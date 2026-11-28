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
mod writer;

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

/// Write half, owned by the writer task. `close` also locks it for the
/// best-effort close frame; nothing else writes to the socket.
type SharedSink = Arc<Mutex<WsSink>>;

/// Why the connection stopped, recorded by whichever task saw it first.
type CloseReason = Arc<std::sync::Mutex<Option<String>>>;

fn record_close_reason(reason: &CloseReason, text: &str) {
    reason
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(|| text.to_owned());
}

/// One frame for the writer task. Commands carry their dispatch state so a
/// sender cancelled while queued is skipped instead of written later.
struct WriteItem {
    message: Message,
    claim: Option<Arc<AtomicU8>>,
    ack: Option<oneshot::Sender<Result<()>>>,
}

type WriteQueue = mpsc::UnboundedSender<WriteItem>;

/// Dispatch states shared by [`CommandDispatch`] and queued writes.
const DISPATCH_UNUSED: u8 = 0;
const DISPATCH_CLAIMED: u8 = 1;
const DISPATCH_SENT: u8 = 2;
const DISPATCH_WITHDRAWN: u8 = 3;

/// Withdraw a queued command when its sender stops waiting before the
/// writer claims it. After the writer claims it, this does nothing.
struct WithdrawOnDrop(Arc<AtomicU8>);

impl Drop for WithdrawOnDrop {
    fn drop(&mut self) {
        let _ = self.0.compare_exchange(
            DISPATCH_CLAIMED,
            DISPATCH_WITHDRAWN,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

/// Command ids stay below the automatic-reply range; both stay within int32,
/// because DevTools silently drops commands whose ids fall outside it.
const COMMAND_ID_MIN: u64 = 1;
const COMMAND_ID_MAX: u64 = 999_999_999;
pub(super) const AUTO_ID_MIN: u64 = 1_000_000_000;
pub(super) const AUTO_ID_MAX: u64 = i32::MAX as u64;

/// The id after `current` in `min..=max`, wrapping to `min`.
fn wrapping_next(current: u64, min: u64, max: u64) -> u64 {
    if current >= max || current < min {
        min
    } else {
        current + 1
    }
}

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

/// A pending request waiting for a response, with the session it was sent to
/// so a detached or crashed session can fail it at once.
struct PendingRequest {
    sender: oneshot::Sender<Result<Value>>,
    session: Option<Arc<str>>,
}

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
    reason: CloseReason,
    shutdown: SocketShutdownGuard,
    lifecycle: LifecycleSink,
    /// Signalled by the writer when it gives up. Shutting the socket down does
    /// not wake a pending read on every platform (Windows), so the reader also
    /// stops on this.
    writer_failed: Arc<tokio::sync::Notify>,
}

/// Target lifecycle notifications for the connection's per-target state.
/// Delivered on a dedicated channel so a lagging event subscriber cannot lose them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TargetLifecycle {
    /// A session was detached; the target may or may not still exist.
    Detached { target_id: String },
    /// The target no longer exists (sent only while target discovery is on).
    Destroyed { target_id: String },
}

type LifecycleSink = Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<TargetLifecycle>>>>;

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
    // Shared with the writer task, which marks the command sent.
    state: Arc<AtomicU8>,
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
        self.state.load(Ordering::Acquire) == DISPATCH_SENT
    }

    fn claim(&self) -> Result<Arc<AtomicU8>> {
        self.state
            .compare_exchange(
                DISPATCH_UNUSED,
                DISPATCH_CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| Arc::clone(&self.state))
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
    /// Replies carry only the command id; protocol errors get their method here.
    method: Box<str>,
}

impl CommandResponse {
    pub(crate) async fn wait(self) -> Result<Value> {
        let result = self
            .receiver
            .await
            .map_err(|error| Error::transport(format!("Response channel closed: {error}")))?;
        drop(self.registration);
        result.map_err(|error| match error {
            Error::Cdp {
                method: None,
                code: code @ Some(_),
                message,
            } => Error::Cdp {
                method: Some(self.method.into()),
                code,
                message,
            },
            other => other,
        })
    }
}

/// Fail and remove every pending request, returning the number drained.
fn fail_pending(pending: &PendingMap, context: &str) -> usize {
    let requests = std::mem::take(&mut *lock_pending(pending));
    let count = requests.len();
    for request in requests.into_values() {
        drop(request.sender.send(Err(Error::transport(context))));
    }
    count
}

/// Chrome's reply to a command for a session that no longer exists. Pending
/// commands of a detached session fail with the same error, so callers that
/// already classify it as "target closed" keep working.
const SESSION_GONE_CODE: i64 = -32001;
const SESSION_GONE_MESSAGE: &str = "Session with given id not found.";

/// Fail the pending requests sent to a detached session, returning the number drained.
fn fail_session_pending(pending: &PendingMap, session: &str) -> usize {
    let requests: Vec<PendingRequest> = {
        let mut pending = lock_pending(pending);
        let ids: Vec<u64> = pending
            .iter()
            .filter(|(_, request)| request.session.as_deref() == Some(session))
            .map(|(id, _)| *id)
            .collect();
        ids.iter().filter_map(|id| pending.remove(id)).collect()
    };
    let count = requests.len();
    for request in requests {
        drop(request.sender.send(Err(Error::Cdp {
            method: None,
            code: Some(SESSION_GONE_CODE),
            message: SESSION_GONE_MESSAGE.into(),
        })));
    }
    count
}

/// Largest CDP message accepted, matching Puppeteer. Full-page screenshots and
/// response bodies are base64 JSON and routinely exceed tens of MiB. tungstenite
/// cannot skip an oversized message, so exceeding this closes the connection.
const MAX_CDP_MESSAGE_BYTES: usize = 256 * 1024 * 1024;

/// Broadcast capacity for CDP events; see [`Transport::subscribe`] for lag.
pub const EVENT_CHANNEL_CAPACITY: usize = 4096;

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
    /// The Chrome child process (None when connecting to an existing instance
    /// or once owned cleanup has reaped it).
    child: std::sync::Mutex<Option<Child>>,
    runtime: std::sync::Mutex<Option<RuntimeDirectory>>,
    browser_endpoint: Option<String>,
    closed: Arc<AtomicBool>,
    /// WebSocket write half, owned by the writer task.
    writer: SharedSink,
    /// Normal-priority lane to the writer task (automatic replies use their own).
    commands: WriteQueue,
    writer_handle: tokio::task::JoinHandle<()>,
    lifecycle: LifecycleSink,
    /// Duplicated socket for lock-independent shutdown, even under backpressure.
    shutdown_socket: Arc<std::net::TcpStream>,
    /// Next message ID
    next_id: AtomicU64,
    /// Pending requests waiting for responses
    pending: Arc<PendingMap>,
    completion_routes: Arc<CompletionRoutes>,
    /// Weak reference: only the reader keeps the event stream open.
    event_tx: broadcast::WeakSender<Arc<CdpMessage>>,
    response_routes: ResponseRoutes,
    request_routes: RequestRoutes,
    idle_routes: IdleRoutes,
    request_base_auth: bool,
    request_base_strip: bool,
    /// Timeout for CDP commands
    cmd_timeout: std::time::Duration,
    /// Reader task handle — checked for exit before sending commands.
    reader_handle: tokio::task::JoinHandle<()>,
    /// When true, refuse "detectable" commands (`Runtime.enable`, etc.) with an
    /// error instead of sending them. Defaults to true. Disable when you own the
    /// browser session and need full DevTools-equivalent control.
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

    /// Whether the WebSocket reader or writer has exited.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
            || self.reader_handle.is_finished()
            || self.writer_handle.is_finished()
    }

    /// Attach to a target with a flattened session and return the session id.
    pub(crate) async fn attach_to_target(&self, target_id: &str) -> Result<String> {
        let attached: super::types::TargetAttachToTargetResult = self
            .send(
                "Target.attachToTarget",
                &super::types::TargetAttachToTarget {
                    target_id: target_id.to_owned(),
                    flatten: Some(true),
                },
            )
            .await?;
        Ok(attached.session_id)
    }

    /// Receive target lifecycle notifications. Replaces any earlier receiver.
    pub(crate) fn watch_target_lifecycle(&self) -> mpsc::UnboundedReceiver<TargetLifecycle> {
        let (sender, receiver) = mpsc::unbounded_channel();
        *self.lifecycle.lock().unwrap_or_else(|e| e.into_inner()) = Some(sender);
        receiver
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
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_CDP_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_CDP_MESSAGE_BYTES));
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
        let closed = Arc::new(AtomicBool::new(false));
        let reason = CloseReason::default();
        let lifecycle = LifecycleSink::default();
        let (reader_events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let event_tx = reader_events.downgrade();
        let (commands, command_queue) = mpsc::unbounded_channel();
        let (replies, reply_queue) = mpsc::unbounded_channel();

        let pending_clone = Arc::clone(&pending);
        let response_routes = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let reader_responses = Arc::clone(&response_routes);
        let request_routes = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let reader_requests = Arc::clone(&request_routes);
        let idle_routes = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let reader_idle = Arc::clone(&idle_routes);
        let request_base_auth = proxy_auth.is_some();
        // Construct the guard before spawning: abort-before-first-poll must
        // still shut down the socket while Transport retains the writer.
        let writer_failed = Arc::new(tokio::sync::Notify::new());
        let completion_state = ReaderCompletionState {
            closed: Arc::clone(&closed),
            reason: Arc::clone(&reason),
            shutdown: SocketShutdownGuard(Arc::clone(&shutdown_socket)),
            lifecycle: Arc::clone(&lifecycle),
            writer_failed: Arc::clone(&writer_failed),
        };
        let writer_state = writer::WriterState {
            closed: Arc::clone(&closed),
            reason,
            write_timeout: cmd_timeout,
            shutdown: SocketShutdownGuard(Arc::clone(&shutdown_socket)),
            failed: writer_failed,
        };
        let writer_handle = tokio::spawn(writer::writer_loop(
            Arc::clone(&writer),
            reply_queue,
            command_queue,
            writer_state,
        ));
        let reader_handle = tokio::spawn(async move {
            Self::reader_loop(
                source,
                pending_clone,
                completion_state,
                reader_events,
                replies,
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
            child: std::sync::Mutex::new(child),
            runtime: std::sync::Mutex::new(None),
            browser_endpoint: None,
            closed,
            writer,
            commands,
            writer_handle,
            lifecycle,
            shutdown_socket,
            next_id: AtomicU64::new(COMMAND_ID_MIN),
            pending,
            completion_routes,
            event_tx,
            request_routes,
            response_routes,
            idle_routes,
            request_base_auth,
            request_base_strip: strip_x_client_data,
            cmd_timeout,
            reader_handle,
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
            true,
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
        filter_cdp: bool,
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
            filter_cdp,
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
        Self::connect_with_proxy_auth(
            ws_url,
            None,
            cdp_timeout_secs,
            filter_cdp,
            strip_x_client_data,
        )
        .await
    }

    /// Connect with proxy credentials answered by the reader, as on launch.
    pub(crate) async fn connect_with_proxy_auth(
        ws_url: &str,
        proxy_auth: Option<(String, String)>,
        cdp_timeout_secs: u64,
        filter_cdp: bool,
        strip_x_client_data: bool,
    ) -> Result<Self> {
        let stream = Self::ws_connect(ws_url, Duration::from_secs(cdp_timeout_secs)).await?;
        let mut transport = Self::build(
            None,
            stream,
            proxy_auth,
            cdp_timeout_secs,
            filter_cdp,
            strip_x_client_data,
        )?;
        transport.browser_endpoint = Some(ws_url.to_owned());
        Ok(transport)
    }

    /// Refuse commands the stealth filter blocks with an error, not an empty result.
    fn check_filter(&self, method: &str) -> Result<()> {
        if self.filter_cdp && is_blocked(method) {
            tracing::debug!("Blocked CDP command: {}", method);
            return Err(Error::Cdp {
                method: Some(method.to_owned()),
                code: None,
                message: "blocked by the stealth command filter (StealthConfig::filter_cdp)".into(),
            });
        }
        if is_risky(method) {
            tracing::warn!("Risky CDP command (may be detectable): {}", method);
        }
        Ok(())
    }

    /// Internal: send a CDP command with optional session ID. One deadline
    /// covers queueing, writing and the reply.
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
        let claim = match dispatch {
            Some(dispatch) => dispatch.claim()?,
            None => Arc::new(AtomicU8::new(DISPATCH_CLAIMED)),
        };
        let _withdraw = WithdrawOnDrop(Arc::clone(&claim));
        self.check_filter(method)?;
        let session = session_id.map(Arc::<str>::from);
        let (data, response) = self.prepare_command(session, method, params, false)?;
        let id = response.registration.id;
        let result = tokio::time::timeout(self.cmd_timeout, async {
            self.write_command(data, claim).await?;
            tracing::trace!("Sent CDP command: {} (id={})", method, id);
            response.wait().await
        })
        .await
        .map_err(|_| {
            // Transport, not Timeout: callers treat a dead command as a
            // connection-level failure, not as an action or wait timeout.
            Error::transport(format!(
                "CDP command {method} (id={id}) exceeded its {:?} deadline",
                self.cmd_timeout
            ))
        })??;
        serde_json::from_value(result).map_err(Into::into)
    }

    /// Register a pending reply and serialize the command. Ids are unique among
    /// pending commands and wrap within the command range.
    fn prepare_command<C: Serialize>(
        &self,
        session: Option<Arc<str>>,
        method: &str,
        params: &C,
        completion: bool,
    ) -> Result<(String, CommandResponse)> {
        if self.is_closed() {
            return Err(Error::transport("CDP transport is closed"));
        }
        let params = serde_json::to_value(params)?;
        let mut pending = lock_pending(&self.pending);
        if self.is_closed() {
            return Err(Error::transport("CDP transport is closed"));
        }
        let mut id = self.next_command_id();
        while pending.contains_key(&id) {
            id = self.next_command_id();
        }
        let completion = match (&session, completion) {
            (Some(session), true) => {
                let mut routes = lock_completions(&self.completion_routes);
                if routes.contains_key(session.as_ref()) {
                    return Err(Error::coded(
                        crate::ErrorCode::Busy,
                        "Native command completion is already owned for this session",
                    ));
                }
                routes.insert(Arc::clone(session), id);
                Some((Arc::clone(&self.completion_routes), Arc::clone(session)))
            }
            _ => None,
        };
        let (sender, receiver) = oneshot::channel();
        pending.insert(
            id,
            PendingRequest {
                sender,
                session: session.clone(),
            },
        );
        drop(pending);
        let response = CommandResponse {
            registration: PendingRegistration {
                pending: Arc::clone(&self.pending),
                completion,
                id,
            },
            receiver,
            method: method.into(),
        };
        let data = serde_json::to_string(&CdpCommand {
            id,
            method,
            params: &params,
            session_id: session.as_deref(),
        })?;
        Ok((data, response))
    }

    /// `next_id` starts at `COMMAND_ID_MIN` and only ever holds in-range ids.
    fn next_command_id(&self) -> u64 {
        let mut current = self.next_id.load(Ordering::Relaxed);
        loop {
            let next = wrapping_next(current, COMMAND_ID_MIN, COMMAND_ID_MAX);
            match self.next_id.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return current,
                Err(actual) => current = actual,
            }
        }
    }

    /// Queue one command for the writer task and wait until it is written.
    /// Dropping this future before the writer claims the command withdraws it.
    async fn write_command(&self, data: String, claim: Arc<AtomicU8>) -> Result<()> {
        if self.is_closed() {
            return Err(Error::transport("CDP transport is closed"));
        }
        let (ack, acked) = oneshot::channel();
        self.commands
            .send(WriteItem {
                message: Message::Text(data.into()),
                claim: Some(claim),
                ack: Some(ack),
            })
            .map_err(|_| Error::transport("CDP transport is closed"))?;
        acked
            .await
            .map_err(|_| Error::transport("CDP writer stopped before writing the command"))?
    }

    pub(crate) async fn send_to_session_for_completion<C: Serialize>(
        &self,
        session_id: &Arc<str>,
        method: &str,
        params: &C,
        dispatch: &CommandDispatch,
    ) -> Result<(Result<()>, CommandResponse)> {
        let claim = dispatch.claim()?;
        let withdraw = WithdrawOnDrop(Arc::clone(&claim));
        self.check_filter(method)?;
        let (data, response) =
            self.prepare_command(Some(Arc::clone(session_id)), method, params, true)?;
        let id = response.registration.id;
        let written = tokio::time::timeout(self.cmd_timeout, self.write_command(data, claim))
            .await
            .unwrap_or_else(|_| {
                Err(Error::Timeout(format!(
                    "CDP command {method} (id={id}) write exceeded its deadline"
                )))
            });
        // Withdraw before deciding, so the writer cannot send it afterwards.
        drop(withdraw);
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
    /// published after subscribing are delivered to all live receivers as
    /// shared `Arc`s.
    ///
    /// A subscriber more than [`EVENT_CHANNEL_CAPACITY`] events behind receives
    /// `RecvError::Lagged(n)` once, then resumes with the oldest retained event.
    /// Treat a lag as possible loss of any event. A subscription made after
    /// closure is already closed.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<CdpMessage>> {
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
            return Err(Error::coded(
                crate::ErrorCode::Busy,
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

    pub(crate) async fn wait_for_owned_exit(&self, timeout: Duration) -> Result<bool> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let exited = match self
                .child
                .lock()
                .map_err(|_| Error::transport("Chrome ownership mutex poisoned"))?
                .as_mut()
            {
                None => true,
                Some(child) => child.try_wait()?.is_some(),
            };
            if exited {
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
    /// Reaping Chrome and removing its runtime directory run on a blocking thread.
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
        self.stop_tasks("CDP transport closed");
        self.cleanup_owned_blocking().await
    }

    fn stop_tasks(&self, context: &str) {
        self.reader_handle.abort();
        self.writer_handle.abort();
        network_idle::disconnect(&self.idle_routes);
        fail_pending(&self.pending, context);
    }

    /// Move owned cleanup off the async runtime; leftovers are restored on failure
    /// so a later attempt (including Drop) can retry.
    async fn cleanup_owned_blocking(&self) -> Result<()> {
        let mut child = self
            .child
            .lock()
            .map_err(|_| Error::transport("Chrome ownership mutex poisoned"))?
            .take();
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| Error::transport("Chrome runtime mutex poisoned"))?
            .take();
        if child.is_none() && runtime.is_none() {
            return Ok(());
        }
        let (result, child, runtime) = tokio::task::spawn_blocking(move || {
            let result = cleanup_parts(&mut child, &mut runtime);
            (result, child, runtime)
        })
        .await
        .map_err(|error| Error::transport(format!("Owned Chrome cleanup task failed: {error}")))?;
        if child.is_some() {
            if let Ok(mut slot) = self.child.lock() {
                *slot = child;
            }
        }
        if runtime.is_some() {
            if let Ok(mut slot) = self.runtime.lock() {
                *slot = runtime;
            }
        }
        result
    }

    /// Synchronous owned cleanup, for Drop where awaiting is impossible.
    pub(crate) fn cleanup_owned(&self) -> Result<()> {
        let mut child = self
            .child
            .lock()
            .map_err(|_| Error::transport("Chrome ownership mutex poisoned"))?;
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| Error::transport("Chrome runtime mutex poisoned"))?;
        cleanup_parts(&mut child, &mut runtime)
    }
}

/// Reap the child, then remove the runtime directory. Each part becomes `None`
/// once cleaned, so a failed attempt keeps what still needs cleanup.
fn cleanup_parts(child: &mut Option<Child>, runtime: &mut Option<RuntimeDirectory>) -> Result<()> {
    if let Some(process) = child.as_mut() {
        stop_child(process)?;
        *child = None;
    }
    if let Some(directory) = runtime.as_mut() {
        directory.cleanup()?;
        *runtime = None;
    }
    Ok(())
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        interception::close_routes(&self.request_routes, &self.response_routes);
        let _ = self.shutdown_socket.shutdown(std::net::Shutdown::Both);
        self.stop_tasks("CDP transport dropped");

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
