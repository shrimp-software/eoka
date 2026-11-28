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

mod connect;
mod interception;
mod pending;
mod process;
mod reader;
mod runtime;
mod shutdown;
mod writer;

use super::network_idle::{self, IdleRoutes, IdleState};
use crate::error::{Error, Result};
pub use interception::RequestPause;
use interception::{
    request_interception_command, FetchInterceptor, RequestRoute, RequestRoutes, ResponseRoute,
    ResponseRoutes,
};
pub use pending::CommandDispatch;
use pending::{
    fail_pending, fail_session_pending, lock_completions, lock_pending, CommandResponse,
    CompletionRoutes, PendingMap, PendingRegistration, PendingRequest, WithdrawOnDrop,
    DISPATCH_CLAIMED, DISPATCH_SENT,
};
pub use process::launch_chrome;
use process::ChildCleanupGuard;
pub(crate) use process::{chrome_version_output, launch_chrome_with_profile_dir, stop_child};
pub(crate) use runtime::RuntimeDirectory;
use shutdown::{duplicate_socket, SocketShutdownGuard};

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
}
#[cfg(test)]
mod tests;
