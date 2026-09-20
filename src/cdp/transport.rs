//! CDP Transport Layer
//!
//! Handles communication with Chrome via WebSocket (tokio-tungstenite).
//! Includes built-in filtering to block detectable CDP commands.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::error::{Error, Result};

/// The connected WebSocket stream and its split write/read halves.
type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = SplitSink<WsStream, Message>;
type WsSource = SplitStream<WsStream>;

/// Shared write half — used by both `send_impl` and the reader task
/// (proxy-auth replies, pong frames).
type SharedSink = Arc<Mutex<WsSink>>;

/// Pending requests map — accessed from both the async reader task and
/// `send_impl`; the lock is held very briefly so a std Mutex is fine.
type PendingMap = std::sync::Mutex<HashMap<u64, PendingRequest>>;

/// A pending request waiting for a response
type PendingRequest = oneshot::Sender<Result<Value>>;

/// Recover a poisoned pending-request map. Its critical sections only mutate
/// `HashMap` entries, whose memory invariants remain intact during unwinding.
fn lock_pending(pending: &PendingMap) -> std::sync::MutexGuard<'_, HashMap<u64, PendingRequest>> {
    match pending.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::error!("Pending CDP request map was poisoned; recovering its contents");
            poisoned.into_inner()
        }
    }
}

/// One-shot observation token for a single tracked command send.
///
/// A token cannot be reused, including after failure or cancellation. Concurrent
/// attempts with the same token are rejected except for the first polled send.
/// It does not acknowledge delivery or retract an already dispatched command.
#[derive(Default)]
pub struct CommandDispatch {
    state: AtomicU8,
}

impl CommandDispatch {
    /// Create an unused token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the command may have entered the WebSocket sink.
    ///
    /// True is irrevocable and does not mean delivery or success. False permits
    /// safe handoff only after the associated send future has terminated or been
    /// dropped; an active send can still progress and change this observation.
    pub fn may_have_been_sent(&self) -> bool {
        self.state.load(Ordering::Acquire) == 2
    }

    fn claim(&self) -> Result<()> {
        self.state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| Error::transport("CommandDispatch token already used"))
    }
}

struct PendingRegistration<'a> {
    pending: &'a PendingMap,
    id: u64,
}

impl Drop for PendingRegistration<'_> {
    fn drop(&mut self) {
        lock_pending(self.pending).remove(&self.id);
    }
}

/// Fail and remove every pending request, returning the number drained.
fn fail_pending(pending: &PendingMap, context: &str) -> usize {
    let mut requests = lock_pending(pending);
    let count = requests.len();
    for (_id, sender) in requests.drain() {
        drop(sender.send(Err(Error::transport(context))));
    }
    count
}

/// Terminate and reap a managed Chrome child without treating an already
/// exited process as an error.
fn stop_child(child: &mut Child) -> std::io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }

    if let Err(kill_error) = child.kill() {
        if child.try_wait()?.is_none() {
            return Err(kill_error);
        }
        return Ok(());
    }

    child.wait()?;
    Ok(())
}

/// Own a Chrome child until it is handed to `Transport`. If the async
/// WebSocket connection future is cancelled, dropping this guard still kills
/// and reaps the process.
struct ChildCleanupGuard {
    child: Option<Child>,
}

impl ChildCleanupGuard {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn take(&mut self) -> Result<Child> {
        self.child
            .take()
            .ok_or_else(|| Error::transport("Chrome child ownership was already transferred"))
    }
}

impl Drop for ChildCleanupGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if let Err(error) = stop_child(&mut child) {
                tracing::warn!("Failed to clean up an unclaimed Chrome child: {}", error);
            }
        }
    }
}

/// Broadcast capacity for CDP events (multi-consumer, lag observable).
const EVENT_CHANNEL_CAP: usize = 1024;

/// Remove `X-Client-Data` from a CDP request-headers object (case-insensitive
/// match, like HTTP header semantics). Returns `None` when the header is
/// absent so callers can continue the request unmodified.
fn strip_client_data_header(headers: &Value) -> Option<serde_json::Map<String, Value>> {
    let map = headers.as_object()?;
    let has_header = map.keys().any(|k| k.eq_ignore_ascii_case("x-client-data"));
    if !has_header {
        return None;
    }
    Some(
        map.iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("x-client-data"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    )
}

/// Check if a command should be blocked (highly detectable by anti-bot)
fn is_blocked(method: &str) -> bool {
    matches!(
        method,
        "Runtime.enable"
            | "Runtime.disable"
            | "HeapProfiler.enable"
            | "HeapProfiler.disable"
            | "Profiler.enable"
            | "Profiler.disable"
            | "Debugger.enable"
            | "Debugger.disable"
            | "Console.enable"
            | "Console.disable"
    )
}

/// Check if a command is risky (potentially detectable)
fn is_risky(method: &str) -> bool {
    matches!(
        method,
        "Emulation.setUserAgentOverride"
            | "Emulation.setTimezoneOverride"
            | "Emulation.setDeviceMetricsOverride"
            | "Page.setBypassCSP"
    )
}

type RequestRoutes = Arc<std::sync::Mutex<HashMap<String, RequestRoute>>>;

struct RequestRoute {
    events: mpsc::Sender<RequestPause>,
    dropped: Arc<AtomicU64>,
}

/// An owned request-stage pause. The consumer must continue or fulfill it once.
pub struct RequestPause {
    /// Exact session that owns this pause.
    pub session_id: String,
    /// Raw Fetch.requestPaused payload; may contain sensitive request metadata.
    pub params: Value,
    continue_params: Value,
}

impl RequestPause {
    /// Continue once with a one-shot dispatch token, preserving header policy.
    ///
    /// A possibly dispatched command must not be retried on timeout, cancellation
    /// or error. False is safe for handoff only after this future is dropped or
    /// terminated. A retry of a never-dispatched attempt needs a fresh token.
    pub async fn continue_request_with_dispatch(
        &self,
        transport: &Transport,
        dispatch: &CommandDispatch,
    ) -> Result<()> {
        transport
            .send_to_session_with_dispatch::<_, Value>(
                &self.session_id,
                "Fetch.continueRequest",
                &self.continue_params,
                dispatch,
            )
            .await?;
        Ok(())
    }

    /// Continue on the originating transport, preserving its header-stripping policy.
    pub async fn continue_request(&self, transport: &Transport) -> Result<()> {
        transport
            .send_to_session::<_, Value>(
                &self.session_id,
                "Fetch.continueRequest",
                &self.continue_params,
            )
            .await?;
        Ok(())
    }
}

fn request_interception_command(
    strip: bool,
    auth: bool,
    patterns: Option<Vec<Value>>,
) -> (&'static str, Value) {
    if patterns.is_none() && !strip && !auth {
        return ("Fetch.disable", json!({}));
    }
    let patterns = if strip || auth {
        vec![json!({"urlPattern":"*","requestStage":"Request"})]
    } else {
        patterns.unwrap_or_default()
    };
    (
        "Fetch.enable",
        json!({"patterns": patterns, "handleAuthRequests":auth}),
    )
}

fn route_request_pause(
    routes: &RequestRoutes,
    session: Option<&str>,
    params: &Value,
    strip: bool,
) -> bool {
    if params.get("responseStatusCode").is_some() || params.get("responseErrorReason").is_some() {
        return false;
    }
    let Some(session) = session else {
        return false;
    };
    let Some(id) = params.get("requestId").and_then(Value::as_str) else {
        return false;
    };
    let routes = routes.lock().unwrap_or_else(|e| e.into_inner());
    let Some(route) = routes.get(session) else {
        return false;
    };
    let mut continue_params = json!({"requestId":id});
    if strip {
        if let Some(headers) = params
            .pointer("/request/headers")
            .and_then(strip_client_data_header)
        {
            continue_params["headers"] = Value::Array(
                headers
                    .into_iter()
                    .map(|(name, value)| json!({"name":name,"value":value}))
                    .collect(),
            );
        }
    }
    let pause = RequestPause {
        session_id: session.into(),
        params: params.clone(),
        continue_params,
    };
    if route.events.try_send(pause).is_ok() {
        return true;
    }
    route.dropped.fetch_add(1, Ordering::Relaxed);
    false
}

/// CDP transport with owned request-pause routing and automatic continuation.
pub struct Transport {
    /// The Chrome child process (None when connecting to an existing instance)
    child: Option<Mutex<Child>>,
    /// WebSocket write half (shared with the reader task for auth/pong).
    writer: SharedSink,
    /// Next message ID
    next_id: AtomicU64,
    /// Pending requests waiting for responses
    pending: Arc<PendingMap>,
    /// Broadcasts parsed events to all subscribers.
    event_tx: broadcast::Sender<CdpMessage>,
    request_routes: RequestRoutes,
    request_base_auth: bool,
    request_base_strip: bool,
    /// Persistent receiver backing `recv_event`/`try_recv_event` (back-compat).
    event_rx: Mutex<broadcast::Receiver<CdpMessage>>,
    /// Timeout for CDP commands
    cmd_timeout: std::time::Duration,
    /// Reader task handle — checked for exit before sending commands.
    reader_handle: Option<tokio::task::JoinHandle<()>>,
    /// When true, drop "detectable" commands (`Runtime.enable`, etc.) silently.
    /// Defaults to true. Disable when you own the browser session and need
    /// full DevTools-equivalent control.
    filter_cdp: bool,
}

/// A parsed CDP message (response or event)
#[derive(Debug, Clone)]
pub enum CdpMessage {
    /// Response to a previously issued command.
    Response {
        /// Command ID this response corresponds to.
        id: u64,
        /// Command result, or the CDP error it returned.
        result: Result<Value>,
    },
    /// An event emitted by Chrome.
    Event {
        /// Event method name (e.g. "Network.requestWillBeSent").
        method: String,
        /// Event payload.
        params: Value,
        /// Session ID the event originated from, if any.
        session_id: Option<String>,
    },
}

impl Transport {
    /// Create a new transport connecting to Chrome via WebSocket
    pub async fn new(child: Child, ws_url: &str) -> Result<Self> {
        Self::new_with_options(child, ws_url, None, 30, false).await
    }

    /// Connect the WebSocket and return the stream.
    async fn ws_connect(ws_url: &str) -> Result<WsStream> {
        if !ws_url.starts_with("ws://") {
            return Err(Error::transport(format!(
                "Invalid WebSocket URL (expected ws://...): {}",
                ws_url
            )));
        }
        let (stream, _resp) = connect_async(ws_url)
            .await
            .map_err(|e| Error::transport(format!("WebSocket connect failed: {}", e)))?;
        tracing::debug!("WebSocket connected to {}", ws_url);
        Ok(stream)
    }

    /// Spawn the reader task and build the Transport from a connected stream.
    fn build(
        child: Option<Child>,
        stream: WsStream,
        proxy_auth: Option<(String, String)>,
        cdp_timeout_secs: u64,
        filter_cdp: bool,
        strip_x_client_data: bool,
    ) -> Self {
        let cmd_timeout = std::time::Duration::from_secs(cdp_timeout_secs);
        tracing::debug!("CDP timeout set to {}s", cdp_timeout_secs);

        let (sink, source) = stream.split();
        let writer: SharedSink = Arc::new(Mutex::new(sink));

        let pending: Arc<PendingMap> = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let (event_tx, event_rx) = broadcast::channel(EVENT_CHANNEL_CAP);

        let pending_clone = Arc::clone(&pending);
        let event_tx_clone = event_tx.clone();
        let reader_writer = Arc::clone(&writer);
        let request_routes = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let reader_requests = Arc::clone(&request_routes);
        let request_base_auth = proxy_auth.is_some();
        let reader_handle = tokio::spawn(async move {
            Self::reader_loop(
                source,
                pending_clone,
                event_tx_clone,
                reader_writer,
                reader_requests,
                proxy_auth,
                strip_x_client_data,
            )
            .await;
        });

        Self {
            child: child.map(Mutex::new),
            writer,
            next_id: AtomicU64::new(1),
            pending,
            event_tx,
            event_rx: Mutex::new(event_rx),
            request_routes,
            request_base_auth,
            request_base_strip: strip_x_client_data,
            cmd_timeout,
            reader_handle: Some(reader_handle),
            filter_cdp,
        }
    }

    /// Create a new transport with proxy auth, X-Client-Data stripping and
    /// configurable CDP timeout.
    pub async fn new_with_options(
        child: Child,
        ws_url: &str,
        proxy_auth: Option<(String, String)>,
        cdp_timeout_secs: u64,
        strip_x_client_data: bool,
    ) -> Result<Self> {
        let mut child = ChildCleanupGuard::new(child);
        let stream = Self::ws_connect(ws_url).await?;
        Ok(Self::build(
            Some(child.take()?),
            stream,
            proxy_auth,
            cdp_timeout_secs,
            true,
            strip_x_client_data,
        ))
    }

    /// Connect to an existing Chrome instance at the given WebSocket URL.
    /// Does not manage a Chrome process — caller owns the browser lifecycle.
    pub async fn connect(ws_url: &str, cdp_timeout_secs: u64) -> Result<Self> {
        let stream = Self::ws_connect(ws_url).await?;
        Ok(Self::build(
            None,
            stream,
            None,
            cdp_timeout_secs,
            true,
            false,
        ))
    }

    /// Connect to an existing Chrome with full options control. Use
    /// `filter_cdp = false` to allow `Runtime.enable` and friends — needed
    /// when driving a user-owned browser where stealth filtering is unwanted.
    pub async fn connect_with_options(
        ws_url: &str,
        cdp_timeout_secs: u64,
        filter_cdp: bool,
        strip_x_client_data: bool,
    ) -> Result<Self> {
        let stream = Self::ws_connect(ws_url).await?;
        Ok(Self::build(
            None,
            stream,
            None,
            cdp_timeout_secs,
            filter_cdp,
            strip_x_client_data,
        ))
    }

    /// Reader task — reads CDP messages off the WebSocket. tokio-tungstenite
    /// handles framing/fragmentation/close; we only reply to pings and parse
    /// text payloads. If `proxy_auth` is set, `Fetch.authRequired` events are
    /// auto-answered with `Fetch.continueWithAuth`. Owned request-stage
    /// `Fetch.requestPaused` events go to bounded consumer queues. Unowned,
    /// overflowed and closed-queue pauses are auto-continued. Consumers must
    /// resolve delivered pauses, including during shutdown.
    /// `strip_x_client_data` removes the header only at the request stage.
    async fn reader_loop(
        mut source: WsSource,
        pending: Arc<PendingMap>,
        event_tx: broadcast::Sender<CdpMessage>,
        writer: SharedSink,
        request_routes: RequestRoutes,
        proxy_auth: Option<(String, String)>,
        strip_x_client_data: bool,
    ) {
        let exit_reason;
        // Separate command ID space for auth/interception auto-responses (won't
        // collide with main IDs for the first ~2 billion commands). Must stay
        // within int32: DevTools silently drops commands with ids outside it.
        let mut auth_cmd_id: u64 = 1_000_000_000;

        loop {
            let text = match source.next().await {
                Some(Ok(Message::Text(t))) => t,
                Some(Ok(Message::Binary(b))) => match String::from_utf8(b.to_vec()) {
                    Ok(s) => s.into(),
                    Err(_) => continue,
                },
                Some(Ok(Message::Ping(payload))) => {
                    // Split streams don't auto-pong; reply via the shared sink.
                    let mut w = writer.lock().await;
                    if let Err(error) = w.send(Message::Pong(payload)).await {
                        exit_reason = format!("WebSocket pong write failed: {}", error);
                        tracing::debug!("{}", exit_reason);
                        break;
                    }
                    continue;
                }
                Some(Ok(Message::Pong(_))) | Some(Ok(Message::Frame(_))) => continue,
                Some(Ok(Message::Close(_))) => {
                    exit_reason = "WebSocket closed by server".to_string();
                    tracing::debug!("{}", exit_reason);
                    break;
                }
                Some(Err(e)) => {
                    exit_reason = format!("WebSocket read error: {}", e);
                    tracing::debug!("{}", exit_reason);
                    break;
                }
                None => {
                    exit_reason = "WebSocket stream ended".to_string();
                    tracing::debug!("{}", exit_reason);
                    break;
                }
            };

            let msg: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Failed to parse CDP message: {} - {}", e, text);
                    continue;
                }
            };

            // Check if response or event
            if let Some(id) = msg.get("id").and_then(|v| v.as_u64()) {
                let result = if let Some(error) = msg.get("error") {
                    Err(Error::cdp(
                        msg.get("method")
                            .and_then(|m| m.as_str())
                            .unwrap_or("unknown"),
                        error.get("code").and_then(|c| c.as_i64()).unwrap_or(-1),
                        error
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("unknown"),
                    ))
                } else {
                    Ok(msg.get("result").cloned().unwrap_or(json!({})))
                };

                let mut pending_guard = lock_pending(&pending);
                if let Some(sender) = pending_guard.remove(&id) {
                    drop(sender.send(result));
                } else {
                    tracing::trace!("Response for unknown id: {}", id);
                }
            } else if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                let params = msg.get("params").cloned().unwrap_or(json!({}));
                let session_id = msg
                    .get("sessionId")
                    .and_then(|s| s.as_str())
                    .map(String::from);

                // Auto-handle proxy auth challenges
                if method == "Fetch.authRequired" {
                    if let Some((ref username, ref password)) = proxy_auth {
                        if let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) {
                            auth_cmd_id += 1;
                            let mut response = json!({
                                "id": auth_cmd_id,
                                "method": "Fetch.continueWithAuth",
                                "params": {
                                    "requestId": request_id,
                                    "authChallengeResponse": {
                                        "response": "ProvideCredentials",
                                        "username": username,
                                        "password": password
                                    }
                                }
                            });
                            // Include sessionId if present
                            if let Some(ref sid) = session_id {
                                response["sessionId"] = json!(sid);
                            }
                            if let Ok(data) = serde_json::to_string(&response) {
                                let mut w = writer.lock().await;
                                if let Err(e) = w.send(Message::Text(data.into())).await {
                                    tracing::error!(
                                        "Failed to send proxy auth response: {} — connection may be broken",
                                        e
                                    );
                                    exit_reason = format!("Proxy auth write failed: {}", e);
                                    // Break so pending requests fail immediately
                                    // instead of hanging.
                                    break;
                                } else {
                                    tracing::debug!("Auto-responded to proxy auth challenge");
                                }
                            }
                            continue; // Don't forward to event channel
                        }
                    }
                }

                // Auto-continue paused requests when Fetch interception is
                // active (proxy auth or X-Client-Data stripping). Interception
                // pauses every matching request; an unanswered pause would
                // hang the page forever.
                if method == "Fetch.requestPaused" {
                    if route_request_pause(
                        &request_routes,
                        session_id.as_deref(),
                        &params,
                        strip_x_client_data,
                    ) {
                        continue;
                    }
                    if let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) {
                        let request_headers = params
                            .pointer("/request/headers")
                            .cloned()
                            .unwrap_or(json!({}));
                        let modified_headers = if strip_x_client_data
                            && params.get("responseStatusCode").is_none()
                            && params.get("responseErrorReason").is_none()
                        {
                            strip_client_data_header(&request_headers)
                        } else {
                            None
                        };
                        auth_cmd_id += 1;
                        let mut response = json!({
                            "id": auth_cmd_id,
                            "method": "Fetch.continueRequest",
                            "params": {
                                "requestId": request_id,
                            }
                        });
                        if let Some(headers) = modified_headers {
                            response["params"]["headers"] = Value::Array(
                                headers
                                    .into_iter()
                                    .map(|(name, value)| json!({"name": name, "value": value}))
                                    .collect(),
                            );
                        }
                        if let Some(ref sid) = session_id {
                            response["sessionId"] = json!(sid);
                        }
                        if let Ok(data) = serde_json::to_string(&response) {
                            let mut w = writer.lock().await;
                            if let Err(e) = w.send(Message::Text(data.into())).await {
                                exit_reason = format!("Fetch continue write failed: {}", e);
                                // Break so pending requests fail immediately
                                // instead of hanging.
                                break;
                            } else {
                                tracing::trace!("Auto-continued paused request");
                            }
                        }
                    }
                    continue; // Don't forward to event channel
                }

                // Publish event; ignore "no active receivers" errors.
                drop(event_tx.send(CdpMessage::Event {
                    method: method.to_string(),
                    params,
                    session_id,
                }));
            }
        }

        // Drain all pending requests so callers fail immediately instead of
        // hanging until the per-command timeout fires.
        let context = format!("WebSocket connection lost: {}", exit_reason);
        let n = fail_pending(&pending, &context);
        if n > 0 {
            tracing::error!(
                "Reader loop exiting ({}), failing {} pending request(s)",
                exit_reason,
                n
            );
        }
        tracing::debug!("CDP reader loop ended ({})", exit_reason);
    }

    /// Internal: send a CDP command with optional session ID
    async fn send_impl<C, R>(
        &self,
        session_id: Option<&str>,
        method: &str,
        params: &C,
        dispatch: Option<&CommandDispatch>,
    ) -> Result<R>
    where
        C: Serialize,
        R: DeserializeOwned,
    {
        if let Some(dispatch) = dispatch {
            dispatch.claim()?;
        }
        // STEALTH: Block detectable commands - return empty object (deserializes via #[serde(default)])
        if self.filter_cdp && is_blocked(method) {
            tracing::debug!("Blocked CDP command: {}", method);
            return serde_json::from_value(json!({})).map_err(Into::into);
        }

        // STEALTH: Warn on risky commands
        if is_risky(method) {
            tracing::warn!("Risky CDP command (may be detectable): {}", method);
        }

        // Check if the reader task has died (exited)
        if let Some(ref handle) = self.reader_handle {
            if handle.is_finished() {
                return Err(Error::transport(
                    "CDP reader task has exited — WebSocket connection is dead",
                ));
            }
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        // Build and serialize BEFORE inserting into the pending map.
        // This way, if serialization or write fails, we don't leak a
        // pending oneshot channel that never gets a response.
        let mut msg = json!({
            "id": id,
            "method": method,
            "params": serde_json::to_value(params)?
        });
        if let Some(sid) = session_id {
            msg["sessionId"] = json!(sid);
        }
        let data = serde_json::to_string(&msg)?;

        // Create response channel and register it
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = lock_pending(&self.pending);
            pending.insert(id, tx);
        }

        let _registration = PendingRegistration {
            pending: &self.pending,
            id,
        };

        // The registration remains owned across writer and response awaits.
        if let Err(e) = {
            let mut writer = self.writer.lock().await;
            async {
                std::future::poll_fn(|cx| writer.poll_ready_unpin(cx)).await?;
                if let Some(dispatch) = dispatch {
                    dispatch.state.store(2, Ordering::Release);
                }
                writer.start_send_unpin(Message::Text(data.into()))?;
                writer.flush().await
            }
            .await
        } {
            return Err(Error::transport(format!("WebSocket write failed: {}", e)));
        }

        tracing::trace!("Sent CDP command: {} (id={})", method, id);

        // Wait for response with timeout to prevent deadlock if reader dies
        let result = tokio::time::timeout(self.cmd_timeout, rx)
            .await
            .map_err(|elapsed| {
                tracing::warn!(
                    "CDP command '{}' timed out after {}s (id={})",
                    method,
                    self.cmd_timeout.as_secs(),
                    id
                );
                Error::transport(format!(
                    "CDP command '{}' timed out after {}s (id={}): {}",
                    method,
                    self.cmd_timeout.as_secs(),
                    id,
                    elapsed
                ))
            })?
            .map_err(|error| Error::transport(format!("Response channel closed: {}", error)))??;

        let response: R = serde_json::from_value(result)?;
        Ok(response)
    }

    /// Send a CDP command and wait for the response
    pub async fn send<C, R>(&self, method: &str, params: &C) -> Result<R>
    where
        C: Serialize,
        R: DeserializeOwned,
    {
        self.send_impl(None, method, params, None).await
    }

    /// Send a CDP command to a specific session
    pub async fn send_to_session<C, R>(
        &self,
        session_id: &str,
        method: &str,
        params: &C,
    ) -> Result<R>
    where
        C: Serialize,
        R: DeserializeOwned,
    {
        self.send_impl(Some(session_id), method, params, None).await
    }

    /// Send one session command with cancellation-aware dispatch observation.
    ///
    /// The token is claimed on first poll, before validation. Reuse/concurrent
    /// use fails without dispatching another command. It is marked irrevocably
    /// immediately before sink insertion, after writer lock and sink readiness.
    /// Cancellation and errors never reset it; see [`CommandDispatch`].
    pub async fn send_to_session_with_dispatch<C, R>(
        &self,
        session_id: &str,
        method: &str,
        params: &C,
        dispatch: &CommandDispatch,
    ) -> Result<R>
    where
        C: Serialize,
        R: DeserializeOwned,
    {
        self.send_impl(Some(session_id), method, params, Some(dispatch))
            .await
    }

    /// Subscribe to CDP events. Each subscriber gets its own receiver; events
    /// published after subscribing are delivered to all live receivers.
    pub fn subscribe(&self) -> broadcast::Receiver<CdpMessage> {
        self.event_tx.subscribe()
    }

    /// Configure manual request patterns while retaining native header/auth interception.
    /// Passing `None` restores the transport's base policy instead of blindly disabling Fetch.
    /// Base header stripping or proxy authentication requires wildcard request-stage
    /// interception; consumers must then apply their own narrower URL filters.
    pub async fn set_request_interception(
        &self,
        session: &str,
        patterns: Option<Vec<Value>>,
    ) -> Result<()> {
        let (method, params) =
            request_interception_command(self.request_base_strip, self.request_base_auth, patterns);
        self.send_to_session::<_, Value>(session, method, &params)
            .await?;
        Ok(())
    }

    /// Claim request pauses for one session; duplicate ownership is rejected.
    /// Full/closed queues fall back to automatic continuation and increment `dropped`.
    /// The consumer must resolve delivered pauses, including during shutdown.
    pub fn install_request_route(
        &self,
        session: &str,
        events: mpsc::Sender<RequestPause>,
        dropped: Arc<AtomicU64>,
    ) -> Result<()> {
        let mut routes = self
            .request_routes
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if routes.contains_key(session) {
            return Err(Error::cdp_msg(
                "Request interception already owned for this session",
            ));
        }
        routes.insert(session.into(), RequestRoute { events, dropped });
        Ok(())
    }

    /// Remove ownership of future pauses. Already-delivered pauses still need resolution.
    pub fn remove_request_route(&self, session: &str) {
        self.request_routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session);
    }

    /// Receive the next event from Chrome (single-consumer back-compat).
    /// Skips over lagged notifications and returns the next available event.
    pub async fn recv_event(&self) -> Option<CdpMessage> {
        let mut rx = self.event_rx.lock().await;
        loop {
            match rx.recv().await {
                Ok(msg) => return Some(msg),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Event receiver lagged, skipped {} event(s)", n);
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Try to receive an event without blocking (single-consumer back-compat).
    pub async fn try_recv_event(&self) -> Option<CdpMessage> {
        let mut rx = self.event_rx.lock().await;
        loop {
            match rx.try_recv() {
                Ok(msg) => return Some(msg),
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    tracing::warn!("Event receiver lagged, skipped {} event(s)", n);
                    continue;
                }
                Err(_) => return None,
            }
        }
    }

    /// Close the transport and kill Chrome
    pub async fn close(&self) -> Result<()> {
        // Send a WebSocket close frame (best-effort).
        {
            let mut writer = self.writer.lock().await;
            if let Err(error) = writer.send(Message::Close(None)).await {
                tracing::debug!("WebSocket close frame was not sent: {}", error);
            }
        }

        if let Some(handle) = &self.reader_handle {
            handle.abort();
        }
        fail_pending(&self.pending, "CDP transport closed");

        if let Some(ref child) = self.child {
            let mut c = child.lock().await;
            stop_child(&mut c)?;
        }
        Ok(())
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        if let Some(handle) = self.reader_handle.take() {
            handle.abort();
        }
        fail_pending(&self.pending, "CDP transport dropped");

        if let Some(ref child) = self.child {
            if let Ok(mut c) = child.try_lock() {
                if let Err(error) = stop_child(&mut c) {
                    tracing::warn!("Failed to stop Chrome while dropping transport: {}", error);
                }
            }
        }
    }
}

/// Launch Chrome and get the WebSocket debugging URL
pub fn launch_chrome(path: &std::path::Path, args: &[String]) -> Result<(Child, String)> {
    launch_chrome_impl(path, args, None)
}

pub(crate) fn launch_chrome_with_profile_dir(
    path: &std::path::Path,
    args: &[String],
    profile_dir: &Path,
) -> Result<(Child, String)> {
    launch_chrome_impl(path, args, Some(profile_dir))
}

fn launch_chrome_impl(
    path: &std::path::Path,
    args: &[String],
    profile_dir: Option<&Path>,
) -> Result<(Child, String)> {
    use std::process::Command;

    let mut cmd = Command::new(path);
    cmd.args(args)
        .args(["--remote-debugging-port=0"]) // Let Chrome pick a free port
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped()); // We need stderr to get the DevTools URL

    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Launch(format!("Failed to launch Chrome: {}", e)))?;

    let stderr = child
        .stderr
        .take()
        .ok_or(Error::Launch("No stderr from Chrome".into()))?;

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        let mut stderr_tail = Vec::new();
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(error) => {
                    let _ = tx.send(Err(format!("failed to read Chrome stderr: {}", error)));
                    return;
                }
            };

            tracing::trace!("Chrome stderr: {}", line);
            stderr_tail.push(line.clone());
            if stderr_tail.len() > 20 {
                stderr_tail.remove(0);
            }

            if line.contains("DevTools listening on") {
                if let Some(url_start) = line.find("ws://") {
                    if tx.send(Ok(line[url_start..].trim().to_string())).is_err() {
                        tracing::debug!("Chrome launch receiver dropped before URL delivery");
                    }
                    return;
                }
            }
        }
        let stderr = stderr_tail.join("\n");
        let message = if stderr.is_empty() {
            "Chrome exited before printing DevTools URL and produced no stderr".to_string()
        } else {
            format!(
                "Chrome exited before printing DevTools URL. stderr:\n{}",
                stderr
            )
        };
        let _ = tx.send(Err(message));
    });

    let ws_url = wait_for_chrome_devtools_url(&mut child, &rx, profile_dir)?;

    tracing::info!("Chrome DevTools URL: {}", ws_url);

    Ok((child, ws_url))
}

fn wait_for_chrome_devtools_url(
    child: &mut Child,
    rx: &std::sync::mpsc::Receiver<std::result::Result<String, String>>,
    profile_dir: Option<&Path>,
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        match rx.try_recv() {
            Ok(Ok(url)) => return Ok(url),
            Ok(Err(stderr)) => {
                let status = child
                    .try_wait()
                    .map(|status| status.map(|s| s.to_string()))
                    .unwrap_or(None)
                    .unwrap_or_else(|| "still running".to_string());
                stop_child_after_failed_launch(child);
                return Err(Error::Launch(format!(
                    "Failed waiting for Chrome DevTools URL: process {}; {}",
                    status, stderr
                )));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                stop_child_after_failed_launch(child);
                return Err(Error::Launch(
                    "Failed waiting for Chrome DevTools URL: stderr reader exited unexpectedly"
                        .into(),
                ));
            }
        }

        if let Some(dir) = profile_dir {
            if let Some(port) = crate::cdp::discover::read_devtools_active_port(dir) {
                return crate::cdp::discover::discover_browser_ws("127.0.0.1", port).map_err(
                    |error| {
                        Error::Launch(format!(
                            "Chrome wrote DevToolsActivePort ({}) but discovery failed: {}",
                            port, error
                        ))
                    },
                );
            }
        }

        if let Ok(Some(status)) = child.try_wait() {
            let stderr = rx
                .recv_timeout(Duration::from_millis(250))
                .ok()
                .and_then(|result| result.err())
                .unwrap_or_else(|| "Chrome exited before printing DevTools URL".to_string());
            stop_child_after_failed_launch(child);
            return Err(Error::Launch(format!(
                "Failed waiting for Chrome DevTools URL: process {}; {}",
                status, stderr
            )));
        }

        if Instant::now() >= deadline {
            stop_child_after_failed_launch(child);
            return Err(Error::Launch(
                "Failed waiting for Chrome DevTools URL after 30s".into(),
            ));
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

fn stop_child_after_failed_launch(child: &mut Child) {
    if let Err(cleanup_error) = stop_child(child) {
        tracing::warn!(
            "Failed to clean up Chrome after launch-channel error: {}",
            cleanup_error
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pending_test_peer() -> (Transport, WebSocketStream<TcpStream>) {
        test_peer_with_header_policy(false).await
    }

    async fn test_peer_with_header_policy(strip: bool) -> (Transport, WebSocketStream<TcpStream>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(socket).await.unwrap()
        });
        let stream = Transport::ws_connect(&url).await.unwrap();
        (
            Transport::build(None, stream, None, 30, false, strip),
            server.await.unwrap(),
        )
    }

    #[tokio::test]
    async fn owned_request_pauses_resolve_once_and_fallback_preserves_header_policy() {
        async fn pause(peer: &mut WebSocketStream<TcpStream>, id: &str) {
            peer.send(Message::Text(json!({
                "method":"Fetch.requestPaused", "sessionId":"owned",
                "params":{"requestId":id,"request":{"headers":{"X-Client-Data":"remove","Other":"keep"}}},
            }).to_string().into())).await.unwrap();
        }
        async fn continuation(peer: &mut WebSocketStream<TcpStream>, id: &str) -> Value {
            let message = tokio::time::timeout(Duration::from_secs(2), peer.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let command: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(command["method"], "Fetch.continueRequest");
            assert_eq!(command["sessionId"], "owned");
            assert_eq!(
                command["params"],
                json!({"requestId":id,"headers":[{"name":"Other","value":"keep"}]})
            );
            command
        }
        let (transport, mut peer) = test_peer_with_header_policy(true).await;
        let (sender, mut receiver) = mpsc::channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        transport
            .install_request_route("owned", sender.clone(), dropped.clone())
            .unwrap();
        assert!(transport
            .install_request_route("owned", sender, dropped.clone())
            .is_err());
        pause(&mut peer, "queued").await;
        pause(&mut peer, "overflow").await;
        continuation(&mut peer, "overflow").await;
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        let delivered = receiver.recv().await.unwrap();
        assert_eq!(delivered.params["requestId"], "queued");
        transport.remove_request_route("owned");
        let dispatch = CommandDispatch::new();
        let reply = async {
            let command = continuation(&mut peer, "queued").await;
            peer.send(Message::Text(
                json!({"id":command["id"],"result":{}}).to_string().into(),
            ))
            .await
            .unwrap();
        };
        let (result, ()) = tokio::join!(
            delivered.continue_request_with_dispatch(&transport, &dispatch),
            reply
        );
        result.unwrap();
        assert!(dispatch.may_have_been_sent());
        pause(&mut peer, "unowned").await;
        continuation(&mut peer, "unowned").await;
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        let (sender, receiver) = mpsc::channel(1);
        drop(receiver);
        transport
            .install_request_route("owned", sender, dropped.clone())
            .unwrap();
        pause(&mut peer, "closed").await;
        continuation(&mut peer, "closed").await;
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
        assert!(tokio::time::timeout(Duration::from_millis(10), peer.next())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn pending_registration_cleans_cancelled_writer_and_reply_waits() {
        let (transport, mut peer) = pending_test_peer().await;
        let writer = transport.writer.lock().await;
        let (unrelated, _receiver) = oneshot::channel();
        lock_pending(&transport.pending).insert(9999, unrelated);
        for _ in 0..8 {
            assert!(tokio::time::timeout(
                Duration::from_millis(10),
                transport.send::<_, Value>("Fixture.wait", &json!({}))
            )
            .await
            .is_err());
            assert_eq!(lock_pending(&transport.pending).len(), 1);
            assert!(lock_pending(&transport.pending).contains_key(&9999));
        }
        drop(writer);
        for _ in 0..8 {
            assert!(tokio::time::timeout(
                Duration::from_millis(10),
                transport.send::<_, Value>("Fixture.wait", &json!({}))
            )
            .await
            .is_err());
            assert_eq!(lock_pending(&transport.pending).len(), 1);
            assert!(peer.next().await.unwrap().is_ok());
        }
        assert!(!transport.reader_handle.as_ref().unwrap().is_finished());
        let reply = async {
            let message = peer.next().await.unwrap().unwrap();
            let command: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            peer.send(Message::Text(
                json!({"id":command["id"], "result":{"ok":true}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        };
        let params = json!({});
        let (response, ()) =
            tokio::join!(transport.send::<_, Value>("Fixture.normal", &params), reply);
        assert_eq!(response.unwrap()["ok"], true);
        assert_eq!(lock_pending(&transport.pending).len(), 1);
    }

    #[tokio::test]
    async fn pending_registration_cleans_internal_timeout_and_write_failure() {
        let (mut transport, _peer) = pending_test_peer().await;
        transport.cmd_timeout = Duration::from_millis(10);
        assert!(transport
            .send::<_, Value>("Fixture.wait", &json!({}))
            .await
            .is_err());
        assert!(lock_pending(&transport.pending).is_empty());
        transport.writer.lock().await.close().await.unwrap();
        assert!(transport
            .send::<_, Value>("Fixture.write", &json!({}))
            .await
            .is_err());
        assert!(lock_pending(&transport.pending).is_empty());
    }

    #[tokio::test]
    async fn command_dispatch_is_one_shot_and_distinguishes_cancelled_writer_wait() {
        let (transport, mut peer) = pending_test_peer().await;
        let writer = transport.writer.lock().await;
        let dispatch = CommandDispatch::new();
        let params = json!({"requestId":"pause"});
        {
            let send = transport.send_to_session_with_dispatch::<_, Value>(
                "session",
                "Fetch.fulfillRequest",
                &params,
                &dispatch,
            );
            tokio::pin!(send);
            assert!(tokio::time::timeout(Duration::from_millis(10), &mut send)
                .await
                .is_err());
            assert!(!dispatch.may_have_been_sent());
            let error = transport
                .send_to_session_with_dispatch::<_, Value>(
                    "session",
                    "Fetch.continueRequest",
                    &params,
                    &dispatch,
                )
                .await
                .unwrap_err();
            assert!(error.to_string().contains("already used"));
        }
        assert!(lock_pending(&transport.pending).is_empty());
        assert!(!dispatch.may_have_been_sent());
        drop(writer);
        let fallback = CommandDispatch::new();
        let response = async {
            let text = peer.next().await.unwrap().unwrap();
            let command: Value = serde_json::from_str(text.to_text().unwrap()).unwrap();
            assert_eq!(command["method"], "Fetch.continueRequest");
            peer.send(Message::Text(
                json!({"id":command["id"],"result":{}}).to_string().into(),
            ))
            .await
            .unwrap();
        };
        let (result, ()) = tokio::join!(
            transport.send_to_session_with_dispatch::<_, Value>(
                "session",
                "Fetch.continueRequest",
                &params,
                &fallback
            ),
            response
        );
        result.unwrap();
        assert!(fallback.may_have_been_sent());
        assert!(transport
            .send_to_session_with_dispatch::<_, Value>(
                "session",
                "Fetch.fulfillRequest",
                &params,
                &fallback
            )
            .await
            .is_err());
        assert!(tokio::time::timeout(Duration::from_millis(10), peer.next())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn command_dispatch_stays_ambiguous_after_lost_reply_error_and_timeout() {
        let (mut transport, mut peer) = pending_test_peer().await;
        let params = json!({"requestId":"pause"});
        let cancelled = CommandDispatch::new();
        {
            let send = transport.send_to_session_with_dispatch::<_, Value>(
                "session",
                "Fetch.fulfillRequest",
                &params,
                &cancelled,
            );
            tokio::pin!(send);
            tokio::select! {
                _ = &mut send => panic!("reply was withheld"),
                message = peer.next() => { assert!(message.unwrap().is_ok()); }
            }
        }
        assert!(cancelled.may_have_been_sent());
        assert!(lock_pending(&transport.pending).is_empty());
        transport.cmd_timeout = Duration::from_millis(10);
        let timed_out = CommandDispatch::new();
        assert!(transport
            .send_to_session_with_dispatch::<_, Value>(
                "session",
                "Fetch.fulfillRequest",
                &params,
                &timed_out
            )
            .await
            .is_err());
        assert!(timed_out.may_have_been_sent());
        peer.next().await.unwrap().unwrap();
        let failed = CommandDispatch::new();
        let response = async {
            let message = peer.next().await.unwrap().unwrap();
            let command: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            peer.send(Message::Text(
                json!({"id":command["id"],"error":{"code":-32000,"message":"unknown result"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        };
        let (result, ()) = tokio::join!(
            transport.send_to_session_with_dispatch::<_, Value>(
                "session",
                "Fetch.fulfillRequest",
                &params,
                &failed
            ),
            response
        );
        assert!(result.is_err());
        assert!(failed.may_have_been_sent());
        assert!(lock_pending(&transport.pending).is_empty());
        transport.writer.lock().await.close().await.unwrap();
        let write_failure = CommandDispatch::new();
        assert!(transport
            .send_to_session_with_dispatch::<_, Value>(
                "session",
                "Fetch.continueRequest",
                &params,
                &write_failure
            )
            .await
            .is_err());
        assert!(lock_pending(&transport.pending).is_empty());
    }

    #[test]
    fn manual_interception_preserves_and_restores_base_auth_and_header_policy() {
        let pattern = vec![json!({"urlPattern":"*/accepted","requestStage":"Request"})];
        for (strip, auth) in [(true, false), (false, true), (true, true)] {
            for patterns in [Some(pattern.clone()), None] {
                let (method, params) = request_interception_command(strip, auth, patterns);
                assert_eq!(method, "Fetch.enable");
                assert_eq!(params["handleAuthRequests"], auth);
                assert_eq!(params["patterns"][0]["urlPattern"], "*");
            }
        }
        assert_eq!(
            request_interception_command(false, false, None).0,
            "Fetch.disable"
        );
        let (_, params) = request_interception_command(false, false, Some(pattern.clone()));
        assert_eq!(params["patterns"], json!(pattern));
        assert_eq!(params["handleAuthRequests"], false);
    }

    #[test]
    fn request_routes_are_scoped_bounded_and_preserve_header_policy() {
        let routes: RequestRoutes = Default::default();
        let (sender, mut receiver) = mpsc::channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        routes.lock().unwrap().insert(
            "owned".into(),
            RequestRoute {
                events: sender,
                dropped: dropped.clone(),
            },
        );
        let request = json!({"requestId":"req", "request":{"headers":{"X-Client-Data":"strip", "Other":"keep"}}});
        assert!(!route_request_pause(&routes, Some("other"), &request, true));
        assert!(!route_request_pause(&routes, None, &request, true));
        let mut response = request.clone();
        response["responseStatusCode"] = json!(200);
        assert!(!route_request_pause(
            &routes,
            Some("owned"),
            &response,
            true
        ));
        assert!(route_request_pause(&routes, Some("owned"), &request, true));
        assert!(!route_request_pause(&routes, Some("owned"), &request, true));
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        let pause = receiver.try_recv().unwrap();
        assert_eq!(pause.session_id, "owned");
        assert_eq!(
            pause.continue_params,
            json!({"requestId":"req", "headers":[{"name":"Other","value":"keep"}]})
        );
        assert!(route_request_pause(&routes, Some("owned"), &request, false));
        assert_eq!(
            receiver.try_recv().unwrap().continue_params,
            json!({"requestId":"req"})
        );
        drop(receiver);
        assert!(!route_request_pause(
            &routes,
            Some("owned"),
            &request,
            false
        ));
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
        routes.lock().unwrap().remove("owned");
        assert!(!route_request_pause(&routes, Some("owned"), &request, true));
    }

    #[test]
    fn strips_x_client_data_case_insensitively() {
        let headers = json!({
            "User-Agent": "test",
            "X-Client-Data": "abc123",
            "Accept": "*/*"
        });
        let stripped = strip_client_data_header(&headers).expect("header present");
        assert_eq!(stripped.len(), 2);
        assert!(stripped.get("User-Agent").is_some());
        assert!(stripped.get("Accept").is_some());
        assert!(strip_client_data_header(&Value::Object(stripped)).is_none());
    }

    #[test]
    fn returns_none_when_header_absent() {
        let headers = json!({ "User-Agent": "test", "Accept": "*/*" });
        assert!(strip_client_data_header(&headers).is_none());
        assert!(strip_client_data_header(&json!({})).is_none());
        assert!(strip_client_data_header(&json!(null)).is_none());
    }

    #[test]
    fn preserves_other_headers_verbatim() {
        let headers = json!({
            "sec-ch-ua": "\"Chromium\";v=\"140\"",
            "x-client-data": "CJKxyz="
        });
        let stripped = strip_client_data_header(&headers).expect("header present");
        assert_eq!(
            stripped.get("sec-ch-ua").and_then(|v| v.as_str()),
            Some("\"Chromium\";v=\"140\"")
        );
    }
}
