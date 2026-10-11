//! CDP Transport Layer
//!
//! Handles communication with Chrome via WebSocket (tokio-tungstenite).
//! Includes built-in filtering to block detectable CDP commands.

use std::collections::HashMap;
use std::process::Child;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

mod interception;
mod process;
mod reader;

use super::network_idle::{self, IdleRoutes, IdleState};
use crate::error::{Error, Result};
pub use interception::RequestPause;
use interception::{
    request_interception_command, FetchInterceptor, RequestRoute, RequestRoutes, ResponseRoute,
    ResponseRoutes,
};
pub use process::launch_chrome;
use process::ChildCleanupGuard;
pub(crate) use process::{launch_chrome_with_profile_dir, stop_child};

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

/// Broadcast capacity for CDP events (multi-consumer, lag observable).
const EVENT_CHANNEL_CAP: usize = 1024;

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
    response_routes: ResponseRoutes,
    request_routes: RequestRoutes,
    idle_routes: IdleRoutes,
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
    /// Whether the WebSocket reader has exited.
    pub fn is_closed(&self) -> bool {
        self.reader_handle
            .as_ref()
            .is_none_or(|reader| reader.is_finished())
    }

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
        let response_routes = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let reader_responses = Arc::clone(&response_routes);
        let request_routes = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let reader_requests = Arc::clone(&request_routes);
        let idle_routes = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let reader_idle = Arc::clone(&idle_routes);
        let request_base_auth = proxy_auth.is_some();
        let reader_handle = tokio::spawn(async move {
            Self::reader_loop(
                source,
                pending_clone,
                event_tx_clone,
                reader_writer,
                reader_idle,
                FetchInterceptor {
                    responses: reader_responses,
                    requests: reader_requests,
                    proxy_auth,
                    strip: strip_x_client_data,
                },
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
            response_routes,
            idle_routes,
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

    pub(super) fn install_idle_route(&self, session: &str, state: &Arc<IdleState>) {
        self.idle_routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.into(), Arc::downgrade(state));
    }

    pub(super) fn remove_idle_route(&self, session: &str) {
        self.idle_routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session);
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

    pub(crate) fn install_response_route(
        &self,
        session: &str,
        events: mpsc::Sender<Value>,
        dropped: Arc<AtomicU64>,
    ) {
        self.response_routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.to_owned(), ResponseRoute { events, dropped });
    }

    pub(crate) fn remove_response_route(&self, session: &str) {
        self.response_routes
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

    pub(crate) async fn wait_for_owned_exit(&self, timeout: Duration) -> Result<bool> {
        let Some(child) = &self.child else {
            return Ok(true);
        };
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if child.lock().await.try_wait()?.is_some() {
                return Ok(true);
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
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
        network_idle::disconnect(&self.idle_routes);
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
        network_idle::disconnect(&self.idle_routes);
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

#[cfg(test)]
mod tests;
