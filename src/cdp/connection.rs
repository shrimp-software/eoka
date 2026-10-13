//! CDP Connection/Session Management
//!
//! Manages browser and page sessions over the CDP transport.

mod document;
mod input;
mod network;
mod page;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::transport::Transport;
use super::types::*;
use crate::error::Result;
use crate::page::HeldInputState;

type TargetRegistry = Arc<Mutex<HashMap<String, Arc<TargetState>>>>;

#[derive(Default)]
struct TargetState {
    actions: Arc<crate::action::ActionState>,
    held_input: Arc<tokio::sync::Mutex<HeldInputState>>,
    headers: Arc<super::headers::HeaderState>,
    monitor: tokio::sync::Mutex<Option<Arc<super::network_idle::NetworkMonitor>>>,
}

fn state_for_target(registry: &TargetRegistry, target_id: &str) -> Arc<TargetState> {
    let mut targets = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    targets.entry(target_id.to_owned()).or_default().clone()
}

/// A CDP connection to Chrome
pub struct Connection {
    transport: Arc<Transport>,
    /// Target state survives dropped Page handles, including failed header cleanup.
    /// Explicit target closure or connection teardown releases the retained state.
    targets: TargetRegistry,
}

impl Connection {
    /// Create a new connection wrapping a transport
    pub(crate) fn new(transport: Transport) -> Self {
        Self {
            transport: Arc::new(transport),
            targets: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get a reference to the transport
    pub fn transport(&self) -> &Arc<Transport> {
        &self.transport
    }

    /// Get browser version info
    pub(crate) async fn version(&self) -> Result<BrowserGetVersionResult> {
        self.transport
            .send("Browser.getVersion", &BrowserGetVersion {})
            .await
    }

    /// Create a new target (tab)
    pub(crate) async fn create_target(
        &self,
        url: &str,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<String> {
        let result: TargetCreateTargetResult = self
            .transport
            .send(
                "Target.createTarget",
                &TargetCreateTarget {
                    url: url.to_string(),
                    width,
                    height,
                },
            )
            .await?;
        Ok(result.target_id)
    }

    /// Attach to a target and get a session
    pub(crate) async fn attach_to_target(&self, target_id: &str) -> Result<Session> {
        let result: TargetAttachToTargetResult = self
            .transport
            .send(
                "Target.attachToTarget",
                &TargetAttachToTarget {
                    target_id: target_id.to_string(),
                    flatten: Some(true),
                },
            )
            .await?;

        Ok(Session {
            transport: Arc::clone(&self.transport),
            session_id: result.session_id,
            target_id: target_id.to_string(),
            target: state_for_target(&self.targets, target_id),
            targets: Arc::clone(&self.targets),
        })
    }

    /// Close a target
    pub(crate) async fn close_target(&self, target_id: &str) -> Result<bool> {
        let result: TargetCloseTargetResult = self
            .transport
            .send(
                "Target.closeTarget",
                &TargetCloseTarget {
                    target_id: target_id.to_string(),
                },
            )
            .await?;
        if result.success {
            self.remove_target(target_id).await;
        }
        Ok(result.success)
    }

    /// Get all targets (tabs)
    pub(crate) async fn get_targets(&self) -> Result<Vec<TargetInfo>> {
        let result: TargetGetTargetsResult = self
            .transport
            .send("Target.getTargets", &TargetGetTargets {})
            .await?;
        Ok(result.target_infos)
    }

    async fn remove_target(&self, target_id: &str) {
        let state = self
            .targets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(target_id);
        if let Some(state) = state {
            if let Some(monitor) = state.monitor.lock().await.as_ref() {
                monitor.closed();
            }
        }
    }

    /// Activate (focus) a target
    pub(crate) async fn activate_target(&self, target_id: &str) -> Result<()> {
        self.send_void(
            "Target.activateTarget",
            &TargetActivateTarget {
                target_id: target_id.to_string(),
            },
        )
        .await
    }

    /// Send a fire-and-forget command (discard the response)
    async fn send_void<C: serde::Serialize>(&self, method: &str, params: &C) -> Result<()> {
        self.transport
            .send::<_, serde_json::Value>(method, params)
            .await?;
        Ok(())
    }

    /// Close the browser
    pub(crate) async fn close(&self) -> Result<()> {
        if let Err(error) = self.send_void("Browser.close", &BrowserClose {}).await {
            // Chrome may close the socket before acknowledging Browser.close.
            // Transport::close still terminates and reaps managed Chrome.
            tracing::debug!("Browser.close command did not complete: {}", error);
        }
        if !self
            .transport
            .wait_for_owned_exit(std::time::Duration::from_secs(5))
            .await?
        {
            tracing::warn!("Chrome did not exit after Browser.close; terminating it");
        }
        self.transport.close().await
    }
}

/// A CDP session attached to a specific target. Cheap to clone; clones share
/// the underlying transport and per-target input, header and network state.
#[derive(Clone)]
pub struct Session {
    transport: Arc<Transport>,
    session_id: String,
    target_id: String,
    /// Per-target native input state shared by Page and Human helpers.
    target: Arc<TargetState>,
    targets: TargetRegistry,
}

impl Session {
    pub(crate) async fn send_tracked(
        &self,
        method: &str,
        params: &serde_json::Value,
        dispatch: &super::transport::CommandDispatch,
    ) -> Result<serde_json::Value> {
        self.target.headers.wait().await?;
        self.transport
            .send_to_session_with_dispatch(&self.session_id, method, params, dispatch)
            .await
    }
    pub(crate) async fn navigation_guard(&self) -> Result<tokio::sync::OwnedMutexGuard<()>> {
        self.target.headers.lock().await
    }

    pub(crate) fn action_state(&self) -> Arc<crate::action::ActionState> {
        self.target.actions.clone()
    }
    /// Get the session ID
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Get the target ID
    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    /// Get a reference to the underlying transport (for event polling)
    pub fn transport(&self) -> &Arc<Transport> {
        &self.transport
    }

    /// Return the per-target held-input coordinator state.
    pub(crate) fn held_input(&self) -> Arc<tokio::sync::Mutex<HeldInputState>> {
        Arc::clone(&self.target.held_input)
    }

    pub(crate) async fn monitor_network(&self, fresh: bool) -> Result<()> {
        let mut monitor = self.target.monitor.lock().await;
        if monitor.is_none() {
            *monitor = Some(Arc::new(
                super::network_idle::NetworkMonitor::start(&self.transport, &self.target_id, fresh)
                    .await?,
            ));
        }
        Ok(())
    }

    pub(crate) async fn wait_for_network_idle(&self, idle_ms: u64, timeout_ms: u64) -> Result<()> {
        tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), async {
            self.target.headers.wait().await?;
            let monitor = self.target.monitor.lock().await.clone().ok_or_else(|| {
                crate::Error::cdp_msg("Network idle monitoring is not initialized")
            })?;
            monitor.wait(idle_ms, timeout_ms).await
        })
        .await
        .map_err(|_| {
            crate::Error::Timeout("Network did not become idle before the deadline".into())
        })?
    }

    pub(crate) async fn attach_frame_target(&self, target_id: &str) -> Result<Session> {
        let result: TargetAttachToTargetResult = self
            .transport
            .send(
                "Target.attachToTarget",
                &TargetAttachToTarget {
                    target_id: target_id.to_string(),
                    flatten: Some(true),
                },
            )
            .await?;
        Ok(Session {
            transport: Arc::clone(&self.transport),
            session_id: result.session_id,
            target_id: target_id.to_string(),
            target: state_for_target(&self.targets, target_id),
            targets: Arc::clone(&self.targets),
        })
    }

    pub(crate) async fn detach(&self) -> Result<()> {
        self.transport
            .send::<_, serde_json::Value>(
                "Target.detachFromTarget",
                &serde_json::json!({"sessionId":self.session_id}),
            )
            .await?;
        Ok(())
    }

    /// Send a command to this session
    pub async fn send<C, R>(&self, method: &str, params: &C) -> Result<R>
    where
        C: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        self.target.headers.wait().await?;
        self.transport
            .send_to_session(&self.session_id, method, params)
            .await
    }

    /// Send a fire-and-forget command (discard the response)
    async fn send_void<C: serde::Serialize>(&self, method: &str, params: &C) -> Result<()> {
        self.send::<_, serde_json::Value>(method, params).await?;
        Ok(())
    }
}
