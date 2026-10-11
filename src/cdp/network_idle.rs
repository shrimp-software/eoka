//! Native, top-document fetch/XHR accounting on a private CDP session.

use super::Transport;
use crate::{Error, Result};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{sync::watch, time::Instant};

const MAX_ACTIVE: usize = 10_000;
pub(super) type IdleRoutes = Arc<Mutex<HashMap<String, Weak<IdleState>>>>;

struct State {
    frame: String,
    loader: String,
    armed: bool,
    trusted: bool,
    loaded: bool,
    active: HashMap<String, String>,
    last_activity: Instant,
    failure: Option<&'static str>,
}

pub(super) struct IdleState {
    state: Mutex<State>,
    changed: watch::Sender<u64>,
}

impl IdleState {
    fn new(frame: String, loader: String, trusted: bool) -> Self {
        Self {
            state: Mutex::new(State {
                frame,
                loader,
                armed: false,
                trusted,
                loaded: trusted,
                active: HashMap::new(),
                last_activity: Instant::now(),
                failure: None,
            }),
            changed: watch::channel(0).0,
        }
    }

    fn wake(&self) {
        self.changed
            .send_modify(|version| *version = version.wrapping_add(1));
    }

    pub(super) fn fail(&self, reason: &'static str) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).failure = Some(reason);
        self.wake();
    }

    fn event(&self, method: &str, params: &Value) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.failure.is_some() {
            return;
        }
        match method {
            "Page.frameNavigated" => {
                let frame = &params["frame"];
                if frame["id"].as_str() != Some(&state.frame) {
                    return;
                }
                let Some(loader) = frame["loaderId"].as_str() else {
                    state.failure = Some("Network idle monitor received a frame without a loader");
                    drop(state);
                    self.wake();
                    return;
                };
                if loader == state.loader {
                    return;
                }
                state.loader = loader.into();
                state.trusted = state.armed;
                state.loaded = false;
                state
                    .active
                    .retain(|_, request_loader| request_loader == loader);
            }
            "Page.frameStartedLoading" | "Page.frameStoppedLoading" => {
                if params["frameId"].as_str() != Some(&state.frame) {
                    return;
                }
                state.loaded = method == "Page.frameStoppedLoading";
            }
            "Page.lifecycleEvent" => {
                if params["frameId"].as_str() != Some(&state.frame)
                    || params["loaderId"].as_str() != Some(&state.loader)
                    || params["name"] != "load"
                {
                    return;
                }
                state.loaded = true;
            }
            "Network.requestWillBeSent" => {
                if params["frameId"].as_str() != Some(&state.frame)
                    || !matches!(params["type"].as_str(), Some("Fetch" | "XHR"))
                {
                    return;
                }
                let (Some(id), Some(loader)) =
                    (params["requestId"].as_str(), params["loaderId"].as_str())
                else {
                    state.failure =
                        Some("Network idle monitor received an incomplete request identity");
                    drop(state);
                    self.wake();
                    return;
                };
                if state.active.len() >= MAX_ACTIVE && !state.active.contains_key(id) {
                    state.failure = Some("Network idle monitor exceeded 10000 active requests");
                } else {
                    // Redirects reuse an ID; replacement does not increase the count.
                    state.active.insert(id.into(), loader.into());
                }
            }
            "Network.loadingFinished" | "Network.loadingFailed" => {
                let Some(id) = params["requestId"].as_str() else {
                    return;
                };
                if state.active.remove(id).is_none() {
                    return;
                }
            }
            "Inspector.detached" | "Inspector.targetCrashed" => {
                state.failure = Some("Network idle monitoring target disconnected");
            }
            _ => return,
        }
        state.last_activity = Instant::now();
        drop(state);
        self.wake();
    }

    async fn wait(&self, idle: Duration, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        let deadline = start
            .checked_add(timeout)
            .ok_or_else(|| Error::cdp_msg("Network idle timeout is too large"))?;
        let mut changes = self.changed.subscribe();
        loop {
            let wake_at = {
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(reason) = state.failure {
                    return Err(Error::transport(reason));
                }
                if !state.trusted {
                    return Err(Error::cdp_msg("Network idle tracking requires a full navigation or reload after attachment"));
                }
                // A busy executor may resume after both the quiet timer and deadline.
                // Do not turn that expired wait into a successful idle observation.
                if !timeout.is_zero() && Instant::now() > deadline {
                    return Err(Error::Timeout(
                        "Network did not become idle before the deadline".into(),
                    ));
                }
                let quiet_since = state.last_activity.max(start);
                if state.loaded && state.active.is_empty() {
                    if quiet_since.elapsed() >= idle {
                        return Ok(());
                    }
                    quiet_since
                        .checked_add(idle)
                        .unwrap_or(deadline)
                        .min(deadline)
                } else {
                    deadline
                }
            };
            if Instant::now() >= deadline {
                return Err(Error::Timeout(
                    "Network did not become idle before the deadline".into(),
                ));
            }
            let timer = std::pin::pin!(tokio::time::sleep_until(wake_at));
            let changed = std::pin::pin!(changes.changed());
            if let futures_util::future::Either::Right((result, _)) =
                futures_util::future::select(timer, changed).await
            {
                result.map_err(|_| Error::transport("Network idle monitor stopped"))?;
            }
        }
    }
}

/// Routes are consumed before broadcasting, and do not retain a transport owner.
pub(super) fn route_event(
    routes: &IdleRoutes,
    session: Option<&str>,
    method: &str,
    params: &Value,
) -> bool {
    let routes = routes.lock().unwrap_or_else(|e| e.into_inner());
    if method == "Target.detachedFromTarget" {
        if let Some(id) = params["sessionId"].as_str() {
            if let Some(state) = routes.get(id) {
                if let Some(state) = state.upgrade() {
                    state.fail("Network idle monitoring session detached");
                }
                return true;
            }
        }
    }
    if let Some(state) = session.and_then(|id| routes.get(id)) {
        if let Some(state) = state.upgrade() {
            state.event(method, params);
        }
        return true;
    }
    false
}

pub(super) fn disconnect(routes: &IdleRoutes) {
    for state in routes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter_map(Weak::upgrade)
    {
        state.fail("Network idle transport disconnected");
    }
}

pub(crate) struct NetworkMonitor {
    transport: Weak<Transport>,
    session: String,
    state: Arc<IdleState>,
}

impl NetworkMonitor {
    pub(crate) async fn start(
        transport: &Arc<Transport>,
        target: &str,
        fresh: bool,
    ) -> Result<Self> {
        // Initialization is owned until the session can be detached on every exit,
        // including cancellation while Chrome is returning the attachment ID.
        let transport = transport.clone();
        let target = target.to_owned();
        let (result, received) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let monitor = Self::start_owned(&transport, &target, fresh).await;
            let _ = result.send(monitor); // A dropped receiver drops/detaches the monitor.
        });
        received
            .await
            .map_err(|_| Error::transport("Network idle monitor initialization stopped"))?
    }

    async fn start_owned(transport: &Arc<Transport>, target: &str, fresh: bool) -> Result<Self> {
        let attached: super::types::TargetAttachToTargetResult = transport
            .send(
                "Target.attachToTarget",
                &json!({"targetId":target,"flatten":true}),
            )
            .await?;
        let monitor = Self {
            transport: Arc::downgrade(transport),
            session: attached.session_id,
            state: Arc::new(IdleState::new(target.into(), String::new(), fresh)),
        };
        transport.install_idle_route(&monitor.session, &monitor.state);
        let tree: Value = transport
            .send_to_session(&monitor.session, "Page.getFrameTree", &json!({}))
            .await?;
        {
            let frame = &tree["frameTree"]["frame"];
            let mut state = monitor
                .state
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            state.frame = frame["id"]
                .as_str()
                .ok_or_else(|| Error::cdp_msg("Network idle monitor is missing its frame"))?
                .into();
            state.loader = frame["loaderId"]
                .as_str()
                .ok_or_else(|| Error::cdp_msg("Network idle monitor is missing its loader"))?
                .into();
        }
        for (method, params) in [
            ("Page.enable", json!({})),
            ("Page.setLifecycleEventsEnabled", json!({"enabled":true})),
            ("Network.enable", json!({"maxPostDataSize":0})),
        ] {
            transport
                .send_to_session::<_, Value>(&monitor.session, method, &params)
                .await?;
        }
        {
            // Navigation during setup may have started requests before Network.enable.
            // An attached document only becomes trusted after a subsequent navigation.
            let mut state = monitor
                .state
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            state.armed = true;
            state.trusted = fresh;
        }
        Ok(monitor)
    }

    pub(crate) async fn wait(&self, idle_ms: u64, timeout_ms: u64) -> Result<()> {
        self.state
            .wait(
                Duration::from_millis(idle_ms),
                Duration::from_millis(timeout_ms),
            )
            .await
    }

    pub(crate) fn closed(&self) {
        self.state.fail("Network idle target closed");
    }
}

impl Drop for NetworkMonitor {
    fn drop(&mut self) {
        self.closed();
        let Some(transport) = self.transport.upgrade() else {
            return;
        };
        let session = self.session.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let result = tokio::time::timeout(
                    Duration::from_secs(5),
                    transport
                        .send::<_, Value>("Target.detachFromTarget", &json!({"sessionId":session})),
                )
                .await;
                if !matches!(result, Ok(Ok(_))) {
                    tracing::debug!("Network monitor detach was not acknowledged");
                }
                transport.remove_idle_route(&session);
            });
        } else {
            transport.remove_idle_route(&session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::Future, task::Context};

    fn state(trusted: bool) -> IdleState {
        let state = IdleState::new("main".into(), "loader".into(), trusted);
        state.state.lock().unwrap().armed = true;
        state
    }
    fn request(id: &str, kind: &str, frame: &str) -> Value {
        json!({"requestId":id,"loaderId":"loader","frameId":frame,"type":kind})
    }

    #[tokio::test(start_paused = true)]
    async fn request_identity_scope_and_limits_fail_without_false_idle() {
        let state = state(true);
        state.event(
            "Network.requestWillBeSent",
            &request("child", "Fetch", "child"),
        );
        state.event(
            "Network.requestWillBeSent",
            &request("socket", "WebSocket", "main"),
        );
        state.wait(Duration::ZERO, Duration::ZERO).await.unwrap();
        for _ in 0..3 {
            state.event(
                "Network.requestWillBeSent",
                &request("redirect", "Fetch", "main"),
            );
        }
        assert_eq!(state.state.lock().unwrap().active.len(), 1);
        state.event("Network.loadingFailed", &json!({"requestId":"redirect"}));
        state.event("Network.loadingFinished", &json!({"requestId":"redirect"}));
        state.wait(Duration::ZERO, Duration::ZERO).await.unwrap();
        for index in 0..MAX_ACTIVE {
            state.event(
                "Network.requestWillBeSent",
                &request(&index.to_string(), "XHR", "main"),
            );
        }
        state.event(
            "Network.requestWillBeSent",
            &request("overflow", "XHR", "main"),
        );
        assert_eq!(state.state.lock().unwrap().active.len(), MAX_ACTIVE);
        assert!(state
            .wait(Duration::ZERO, Duration::ZERO)
            .await
            .unwrap_err()
            .to_string()
            .contains("10000"));
    }

    #[tokio::test(start_paused = true)]
    async fn attached_epochs_ignore_same_document_and_stale_lifecycle_events() {
        let state = state(false);
        assert!(state.wait(Duration::ZERO, Duration::ZERO).await.is_err());
        state.event("Page.navigatedWithinDocument", &json!({"frameId":"main"}));
        state.event(
            "Page.frameNavigated",
            &json!({"frame":{"id":"main","loaderId":"loader"}}),
        );
        assert!(state.wait(Duration::ZERO, Duration::ZERO).await.is_err());
        state.event(
            "Page.frameNavigated",
            &json!({"frame":{"id":"main","loaderId":"new"}}),
        );
        state.event(
            "Page.lifecycleEvent",
            &json!({"frameId":"main","loaderId":"loader","name":"load"}),
        );
        assert!(state.wait(Duration::ZERO, Duration::ZERO).await.is_err());
        state.event(
            "Page.lifecycleEvent",
            &json!({"frameId":"main","loaderId":"new","name":"load"}),
        );
        state.wait(Duration::ZERO, Duration::ZERO).await.unwrap();
        state.fail("fixture disconnection");
        assert!(state
            .wait(Duration::ZERO, Duration::ZERO)
            .await
            .unwrap_err()
            .to_string()
            .contains("disconnection"));
    }

    #[tokio::test]
    async fn navigation_during_initialization_cannot_establish_tracking() {
        let state = IdleState::new("main".into(), "old".into(), false);
        state.event(
            "Page.frameNavigated",
            &json!({"frame":{"id":"main","loaderId":"during-setup"}}),
        );
        state.event("Page.frameStoppedLoading", &json!({"frameId":"main"}));
        state.state.lock().unwrap().armed = true;
        assert!(state
            .wait(Duration::ZERO, Duration::ZERO)
            .await
            .unwrap_err()
            .to_string()
            .contains("full navigation"));
        state.event(
            "Page.frameNavigated",
            &json!({"frame":{"id":"main","loaderId":"after-setup"}}),
        );
        state.event("Page.frameStoppedLoading", &json!({"frameId":"main"}));
        state.wait(Duration::ZERO, Duration::ZERO).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn each_wait_requires_its_own_quiet_period_and_obeys_its_deadline() {
        let state = state(true);
        tokio::time::advance(Duration::from_secs(1)).await;
        let start = Instant::now();
        state
            .wait(Duration::from_millis(100), Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(start.elapsed(), Duration::from_millis(100));
        let start = Instant::now();
        assert!(matches!(
            state
                .wait(Duration::from_secs(1), Duration::from_millis(100))
                .await,
            Err(Error::Timeout(_))
        ));
        assert_eq!(start.elapsed(), Duration::from_millis(100));
    }

    #[tokio::test(start_paused = true)]
    async fn a_late_poll_cannot_succeed_after_the_deadline() {
        let state = state(true);
        let wait = state.wait(Duration::from_millis(100), Duration::from_millis(150));
        tokio::pin!(wait);
        assert!(wait
            .as_mut()
            .poll(&mut Context::from_waker(
                futures_util::task::noop_waker_ref()
            ))
            .is_pending());
        tokio::time::advance(Duration::from_millis(200)).await;
        assert!(matches!(wait.await, Err(Error::Timeout(_))));
    }
}
