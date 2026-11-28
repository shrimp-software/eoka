//! Pending command registry, dispatch tokens and response routing.

use super::*;

/// Dispatch states shared by [`CommandDispatch`] and queued writes.
const DISPATCH_UNUSED: u8 = 0;
pub(super) const DISPATCH_CLAIMED: u8 = 1;
pub(super) const DISPATCH_SENT: u8 = 2;
const DISPATCH_WITHDRAWN: u8 = 3;

/// Withdraw a queued command when its sender stops waiting before the
/// writer claims it. After the writer claims it, this does nothing.
pub(super) struct WithdrawOnDrop(pub(super) Arc<AtomicU8>);

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

/// Pending requests map — accessed from both the async reader task and
/// `send_impl`; the lock is held very briefly so a std Mutex is fine.
pub(super) type PendingMap = std::sync::Mutex<HashMap<u64, PendingRequest>>;

/// A pending request waiting for a response, with the session it was sent to
/// so a detached or crashed session can fail it at once.
pub(super) struct PendingRequest {
    pub(super) sender: oneshot::Sender<Result<Value>>,
    pub(super) session: Option<Arc<str>>,
}

pub(super) type CompletionRoutes = std::sync::Mutex<HashMap<Arc<str>, u64>>;

/// Recover a poisoned pending-request map. Its critical sections only mutate
/// `HashMap` entries, whose memory invariants remain intact during unwinding.
pub(super) fn lock_pending(
    pending: &PendingMap,
) -> std::sync::MutexGuard<'_, HashMap<u64, PendingRequest>> {
    match pending.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::error!("Pending CDP request map was poisoned; recovering its contents");
            poisoned.into_inner()
        }
    }
}

pub(super) fn lock_completions(
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

    pub(super) fn claim(&self) -> Result<Arc<AtomicU8>> {
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

pub(super) struct PendingRegistration {
    pub(super) pending: Arc<PendingMap>,
    pub(super) completion: Option<(Arc<CompletionRoutes>, Arc<str>)>,
    pub(super) id: u64,
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
    pub(super) registration: PendingRegistration,
    pub(super) receiver: oneshot::Receiver<Result<Value>>,
    /// Replies carry only the command id; protocol errors get their method here.
    pub(super) method: Box<str>,
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
pub(super) fn fail_pending(pending: &PendingMap, context: &str) -> usize {
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
pub(super) fn fail_session_pending(pending: &PendingMap, session: &str) -> usize {
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
