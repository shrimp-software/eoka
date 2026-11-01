//! CDP Transport Layer
//!
//! Handles communication with Chrome via WebSocket (tokio-tungstenite).
//! Includes built-in filtering to block detectable CDP commands.

use std::collections::HashMap;
use std::future::poll_fn;
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::{protocol::WebSocketConfig, Message};
use tokio_tungstenite::{connect_async_with_config, MaybeTlsStream, WebSocketStream};

mod interception;
mod process;
mod reader;
mod runtime;

use super::network_idle::{self, IdleRoutes, IdleState};
use crate::error::{Error, Result};
pub use interception::RequestPause;
use interception::{
    request_interception_command, FetchInterceptor, RequestRoute, RequestRoutes, ResponseRoute,
    ResponseRoutes,
};
pub use process::launch_chrome;
use process::ChildCleanupGuard;
pub(crate) use process::{chrome_version_output, launch_chrome_with_profile_dir, stop_child};
pub(crate) use runtime::RuntimeDirectory;

/// The connected WebSocket stream and its split write/read halves.
type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = SplitSink<WsStream, Message>;
type WsSource = SplitStream<WsStream>;

/// Shared write half — used by both `send_impl` and the reader task
/// (proxy-auth replies, pong frames).
type SharedSink = Arc<Mutex<WsSink>>;

/// Shuts down the shared TCP connection without acquiring the WebSocket writer.
/// Each guard must shut down independently of other retained socket owners.
struct SocketShutdownGuard(Arc<std::net::TcpStream>);

impl Drop for SocketShutdownGuard {
    fn drop(&mut self) {
        // Shutdown affects every duplicated descriptor, including the socket
        // retained by SplitSink. Repeated shutdown after EOF is harmless.
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

fn duplicate_socket(stream: &WsStream) -> Result<Arc<std::net::TcpStream>> {
    let MaybeTlsStream::Plain(socket) = stream.get_ref() else {
        return Err(Error::transport(
            "CDP transport requires a plain ws:// socket",
        ));
    };
    #[cfg(unix)]
    let socket = {
        use std::os::fd::AsFd;
        socket.as_fd().try_clone_to_owned()
    };
    #[cfg(windows)]
    let socket = {
        use std::os::windows::io::AsSocket;
        socket.as_socket().try_clone_to_owned()
    };
    let socket = socket
        .map(std::net::TcpStream::from)
        .map_err(|error| Error::transport(format!("Failed to duplicate CDP socket: {error}")))?;
    Ok(Arc::new(socket))
}

/// Pending requests map — accessed from both the async reader task and
/// `send_impl`; the lock is held very briefly so a std Mutex is fine.
type PendingMap = std::sync::Mutex<HashMap<u64, PendingRequest>>;

/// A pending request waiting for a response
type PendingRequest = oneshot::Sender<Result<Value>>;

type CompletionRoutes = std::sync::Mutex<HashMap<Arc<str>, u64>>;

#[derive(Serialize)]
struct CdpCommand<'a> {
    id: u64,
    method: &'a str,
    params: &'a Value,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
}

struct ReaderCompletionState {
    closed: Arc<AtomicBool>,
    routes: Arc<CompletionRoutes>,
    write_timeout: Duration,
    shutdown: SocketShutdownGuard,
}

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

fn lock_completions(
    routes: &CompletionRoutes,
) -> std::sync::MutexGuard<'_, HashMap<Arc<str>, u64>> {
    match routes.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::error!("Native completion routes were poisoned; recovering their contents");
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

struct PendingRegistration {
    pending: Arc<PendingMap>,
    completion: Option<(Arc<CompletionRoutes>, Arc<str>)>,
    id: u64,
}

impl Drop for PendingRegistration {
    fn drop(&mut self) {
        lock_pending(&self.pending).remove(&self.id);
        if let Some((routes, session)) = &self.completion {
            let mut routes = lock_completions(routes);
            if routes.get(session.as_ref()) == Some(&self.id) {
                routes.remove(session.as_ref());
            }
        }
    }
}

pub(crate) struct CommandResponse {
    registration: PendingRegistration,
    receiver: oneshot::Receiver<Result<Value>>,
}

impl CommandResponse {
    pub(crate) async fn wait(self) -> Result<Value> {
        let result = self
            .receiver
            .await
            .map_err(|error| Error::transport(format!("Response channel closed: {error}")))?;
        drop(self.registration);
        result
    }
}

/// Fail and remove every pending request, returning the number drained.
fn fail_pending(pending: &PendingMap, context: &str) -> usize {
    let requests = std::mem::take(&mut *lock_pending(pending));
    let count = requests.len();
    for sender in requests.into_values() {
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
    target_discovery: tokio::sync::OnceCell<()>,
    /// The Chrome child process (None when connecting to an existing instance)
    child: Option<std::sync::Mutex<Child>>,
    runtime: std::sync::Mutex<Option<RuntimeDirectory>>,
    browser_endpoint: Option<String>,
    closed: Arc<AtomicBool>,
    /// WebSocket write half (shared with the reader task for auth/pong).
    writer: SharedSink,
    /// Duplicated socket for lock-independent shutdown, even under backpressure.
    shutdown_socket: Arc<std::net::TcpStream>,
    /// Next message ID
    next_id: AtomicU64,
    /// Pending requests waiting for responses
    pending: Arc<PendingMap>,
    completion_routes: Arc<CompletionRoutes>,
    /// Weak reference: only the reader keeps the event stream open.
    event_tx: broadcast::WeakSender<CdpMessage>,
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
    pub(crate) async fn ensure_target_discovery(&self) -> Result<()> {
        self.target_discovery
            .get_or_try_init(|| async {
                self.send::<_, Value>(
                    "Target.setDiscoverTargets",
                    &serde_json::json!({"discover":true}),
                )
                .await?;
                Ok::<_, Error>(())
            })
            .await?;
        Ok(())
    }
    pub(crate) fn browser_endpoint(&self) -> Option<&str> {
        self.browser_endpoint.as_deref()
    }

    pub(crate) fn command_timeout(&self) -> Duration {
        self.cmd_timeout
    }

    /// Whether the WebSocket reader has exited.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
            || self
                .reader_handle
                .as_ref()
                .is_none_or(|reader| reader.is_finished())
    }

    /// Create a new transport connecting to Chrome via WebSocket
    pub async fn new(child: Child, ws_url: &str) -> Result<Self> {
        Self::new_with_options(child, ws_url, None, 30, false).await
    }

    /// Connect the WebSocket and return the stream.
    async fn ws_connect(ws_url: &str, timeout: Duration) -> Result<WsStream> {
        if !ws_url.starts_with("ws://") {
            return Err(Error::transport(format!(
                "Invalid WebSocket URL (expected ws://...): {}",
                ws_url
            )));
        }
        let max_message_bytes = 64 * 1024 * 1024;
        let config = WebSocketConfig::default()
            .max_message_size(Some(max_message_bytes))
            .max_frame_size(Some(max_message_bytes));
        let (stream, _resp) = tokio::time::timeout(
            timeout,
            connect_async_with_config(ws_url, Some(config), false),
        )
        .await
        .map_err(|_| Error::Timeout("WebSocket connection exceeded its deadline".into()))?
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
    ) -> Result<Self> {
        let mut child = child.map(ChildCleanupGuard::new);
        let shutdown_socket = duplicate_socket(&stream)?;
        let child = child.as_mut().map(ChildCleanupGuard::take).transpose()?;
        let cmd_timeout = std::time::Duration::from_secs(cdp_timeout_secs);
        tracing::debug!("CDP timeout set to {}s", cdp_timeout_secs);

        let (sink, source) = stream.split();
        let writer: SharedSink = Arc::new(Mutex::new(sink));

        let pending: Arc<PendingMap> = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let completion_routes = Arc::new(CompletionRoutes::default());
        let reader_completions = Arc::clone(&completion_routes);
        let closed = Arc::new(AtomicBool::new(false));
        let reader_closed = Arc::clone(&closed);
        let (reader_events, event_rx) = broadcast::channel(EVENT_CHANNEL_CAP);
        let event_tx = reader_events.downgrade();

        let pending_clone = Arc::clone(&pending);
        let reader_writer = Arc::clone(&writer);
        let response_routes = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let reader_responses = Arc::clone(&response_routes);
        let request_routes = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let reader_requests = Arc::clone(&request_routes);
        let idle_routes = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let reader_idle = Arc::clone(&idle_routes);
        let request_base_auth = proxy_auth.is_some();
        // Construct the guard before spawning: abort-before-first-poll must
        // still shut down the socket while Transport retains the writer.
        let completion_state = ReaderCompletionState {
            closed: reader_closed,
            routes: reader_completions,
            write_timeout: cmd_timeout,
            shutdown: SocketShutdownGuard(Arc::clone(&shutdown_socket)),
        };
        let reader_handle = tokio::spawn(async move {
            Self::reader_loop(
                source,
                pending_clone,
                completion_state,
                reader_events,
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

        Ok(Self {
            target_discovery: tokio::sync::OnceCell::new(),
            child: child.map(std::sync::Mutex::new),
            runtime: std::sync::Mutex::new(None),
            browser_endpoint: None,
            closed,
            writer,
            shutdown_socket,
            next_id: AtomicU64::new(1),
            pending,
            completion_routes,
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
        })
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
        Self::new_with_runtime_options(
            child,
            ws_url,
            proxy_auth,
            cdp_timeout_secs,
            strip_x_client_data,
            None,
        )
        .await
    }

    pub(crate) async fn new_with_runtime_options(
        child: Child,
        ws_url: &str,
        proxy_auth: Option<(String, String)>,
        cdp_timeout_secs: u64,
        strip_x_client_data: bool,
        runtime: Option<RuntimeDirectory>,
    ) -> Result<Self> {
        let mut child = ChildCleanupGuard::new(child);
        let stream = Self::ws_connect(ws_url, Duration::from_secs(cdp_timeout_secs)).await?;
        let mut transport = Self::build(
            Some(child.take()?),
            stream,
            proxy_auth,
            cdp_timeout_secs,
            true,
            strip_x_client_data,
        )?;
        transport.browser_endpoint = Some(ws_url.to_owned());
        *transport
            .runtime
            .get_mut()
            .expect("new runtime mutex is unpoisoned") = runtime;
        Ok(transport)
    }

    /// Connect to an existing Chrome instance at the given WebSocket URL.
    /// Does not manage a Chrome process — caller owns the browser lifecycle.
    /// `cdp_timeout_secs` bounds connection establishment and each CDP command.
    pub async fn connect(ws_url: &str, cdp_timeout_secs: u64) -> Result<Self> {
        Self::connect_with_options(ws_url, cdp_timeout_secs, true, false).await
    }

    /// Connect to an existing Chrome with full options control. Use
    /// `filter_cdp = false` to allow `Runtime.enable` and friends — needed
    /// when driving a user-owned browser where stealth filtering is unwanted.
    /// `cdp_timeout_secs` bounds connection establishment and each CDP command.
    pub async fn connect_with_options(
        ws_url: &str,
        cdp_timeout_secs: u64,
        filter_cdp: bool,
        strip_x_client_data: bool,
    ) -> Result<Self> {
        let stream = Self::ws_connect(ws_url, Duration::from_secs(cdp_timeout_secs)).await?;
        let mut transport = Self::build(
            None,
            stream,
            None,
            cdp_timeout_secs,
            filter_cdp,
            strip_x_client_data,
        )?;
        transport.browser_endpoint = Some(ws_url.to_owned());
        Ok(transport)
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
        tokio::time::timeout(
            self.cmd_timeout,
            self.send_inner(session_id, method, params, dispatch),
        )
        .await
        .map_err(|_| Error::Timeout(format!("CDP command {method} exceeded its deadline")))?
    }

    async fn send_inner<C, R>(
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

        let (data, response) = self.prepare_command(session_id, method, params, None)?;
        let id = response.registration.id;
        self.write_command(data, dispatch).await?;
        tracing::trace!("Sent CDP command: {} (id={})", method, id);
        let result = tokio::time::timeout(self.cmd_timeout, response.wait())
            .await
            .map_err(|elapsed| {
                Error::transport(format!(
                    "CDP command '{method}' timed out after {}s (id={id}): {elapsed}",
                    self.cmd_timeout.as_secs(),
                ))
            })??;
        serde_json::from_value(result).map_err(Into::into)
    }

    fn prepare_command<C: Serialize>(
        &self,
        session_id: Option<&str>,
        method: &str,
        params: &C,
        completion_session: Option<Arc<str>>,
    ) -> Result<(String, CommandResponse)> {
        if self.is_closed() {
            return Err(Error::transport("CDP transport is closed"));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let params = serde_json::to_value(params)?;
        let data = serde_json::to_string(&CdpCommand {
            id,
            method,
            params: &params,
            session_id,
        })?;
        let mut pending = lock_pending(&self.pending);
        if self.is_closed() {
            return Err(Error::transport("CDP transport is closed"));
        }
        let completion = if let Some(session) = completion_session {
            let mut routes = lock_completions(&self.completion_routes);
            if routes.contains_key(session.as_ref()) {
                return Err(Error::cdp_msg(
                    "Native command completion is already owned for this session",
                ));
            }
            routes.insert(Arc::clone(&session), id);
            Some((Arc::clone(&self.completion_routes), session))
        } else {
            None
        };
        let (sender, receiver) = oneshot::channel();
        pending.insert(id, sender);
        drop(pending);
        Ok((
            data,
            CommandResponse {
                registration: PendingRegistration {
                    pending: Arc::clone(&self.pending),
                    completion,
                    id,
                },
                receiver,
            },
        ))
    }

    async fn write_command(&self, data: String, dispatch: Option<&CommandDispatch>) -> Result<()> {
        let mut writer = self.writer.lock().await;
        if self.is_closed() {
            return Err(Error::transport("CDP transport is closed"));
        }
        async {
            poll_fn(|cx| writer.poll_ready_unpin(cx)).await?;
            if let Some(dispatch) = dispatch {
                dispatch.state.store(2, Ordering::Release);
            }
            writer.start_send_unpin(Message::Text(data.into()))?;
            writer.flush().await
        }
        .await
        .map_err(|error| Error::transport(format!("WebSocket write failed: {error}")))
    }

    pub(crate) async fn send_to_session_for_completion<C: Serialize>(
        &self,
        session_id: &Arc<str>,
        method: &str,
        params: &C,
        dispatch: &CommandDispatch,
    ) -> Result<(Result<()>, CommandResponse)> {
        dispatch.claim()?;
        if is_risky(method) {
            tracing::warn!("Risky CDP command (may be detectable): {}", method);
        }
        let (data, response) = self.prepare_command(
            Some(session_id),
            method,
            params,
            Some(Arc::clone(session_id)),
        )?;
        let written =
            tokio::time::timeout(self.cmd_timeout, self.write_command(data, Some(dispatch)))
                .await
                .map_err(|_| {
                    Error::Timeout(format!("CDP command {method} write exceeded its deadline"))
                })
                .and_then(|result| result);
        match written {
            Err(error) if !dispatch.may_have_been_sent() => Err(error),
            written => Ok((written, response)),
        }
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
    /// A subscription made after closure is already closed.
    pub fn subscribe(&self) -> broadcast::Receiver<CdpMessage> {
        if !self.is_closed() {
            if let Some(sender) = self.event_tx.upgrade() {
                return sender.subscribe();
            }
        }
        let (_, receiver) = broadcast::channel(1);
        receiver
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
        // Check under the same lock used by shutdown to prevent late ownership.
        if self.is_closed() {
            return Err(Error::transport("CDP transport is closed"));
        }
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
        let mut routes = self
            .response_routes
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !self.is_closed() {
            routes.insert(session.to_owned(), ResponseRoute { events, dropped });
        }
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
    /// Returns `None` if another consumer currently owns the persistent receiver.
    pub async fn try_recv_event(&self) -> Option<CdpMessage> {
        let Ok(mut rx) = self.event_rx.try_lock() else {
            return None;
        };
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
            if child
                .lock()
                .map_err(|_| Error::transport("Chrome ownership mutex poisoned"))?
                .try_wait()?
                .is_some()
            {
                return Ok(true);
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Close the transport and terminate any owned Chrome process.
    ///
    /// Waiting for the writer and sending the best-effort WebSocket close frame
    /// share the configured command timeout. TCP shutdown follows that attempt,
    /// independently of the writer lock, and also occurs if this future is
    /// cancelled. Pending requests are failed and owned cleanup is attempted
    /// even if the deadline expires. Borrowed Chrome processes are not terminated.
    pub async fn close(&self) -> Result<()> {
        self.closed.store(true, Ordering::Release);
        interception::close_routes(&self.request_routes, &self.response_routes);
        // If this future is cancelled during the graceful attempt, shutdown
        // must still happen without waiting for the writer or the reader task.
        let shutdown = SocketShutdownGuard(Arc::clone(&self.shutdown_socket));
        // Bound both writer acquisition and the best-effort close-frame write
        // so a stalled sink cannot prevent pending failure or owned cleanup.
        match tokio::time::timeout(self.cmd_timeout, async {
            self.writer.lock().await.send(Message::Close(None)).await
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::debug!("WebSocket close frame was not sent: {}", error);
            }
            Err(error) => {
                tracing::debug!("WebSocket close frame exceeded its deadline: {}", error);
            }
        }
        drop(shutdown);

        if let Some(handle) = &self.reader_handle {
            handle.abort();
        }
        network_idle::disconnect(&self.idle_routes);
        fail_pending(&self.pending, "CDP transport closed");

        self.cleanup_owned()
    }

    pub(crate) fn cleanup_owned(&self) -> Result<()> {
        if let Some(child) = &self.child {
            let mut child = child
                .lock()
                .map_err(|_| Error::transport("Chrome ownership mutex poisoned"))?;
            stop_child(&mut child)?;
        }
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| Error::transport("Chrome runtime mutex poisoned"))?;
        if let Some(runtime) = runtime.as_mut() {
            runtime.cleanup()?;
        }
        Ok(())
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        interception::close_routes(&self.request_routes, &self.response_routes);
        let _ = self.shutdown_socket.shutdown(std::net::Shutdown::Both);
        if let Some(handle) = self.reader_handle.take() {
            handle.abort();
        }
        network_idle::disconnect(&self.idle_routes);
        fail_pending(&self.pending, "CDP transport dropped");

        if let Err(error) = self.cleanup_owned() {
            tracing::warn!(
                "Failed to clean up Chrome while dropping transport: {}",
                error
            );
        }
    }
}

#[cfg(test)]
mod tests;
