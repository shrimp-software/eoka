//! Page commands on a CDP session.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};

use super::Session;
use crate::cdp::types::*;
use crate::{Error, Result};

type ViewportRegistry = HashMap<String, HashMap<String, Weak<tokio::sync::Mutex<ViewportState>>>>;

// Chromium keeps metrics per CDP session and restores that session's metrics
// after capture. All Eoka handles for a tab must therefore use one controller.
#[derive(Default)]
pub(super) struct ViewportState {
    controller: Option<ViewportController>,
    metrics: Option<EmulationSetDeviceMetricsOverride>,
}

struct ViewportController {
    transport: Weak<crate::cdp::Transport>,
    session_id: Arc<str>,
}

pub(super) fn viewport_for_target(
    endpoint: Option<&str>,
    target_id: &str,
) -> Arc<tokio::sync::Mutex<ViewportState>> {
    static REGISTRY: LazyLock<Mutex<ViewportRegistry>> = LazyLock::new(Mutex::default);
    let Some(endpoint) = endpoint else {
        return Arc::default();
    };
    let mut registry = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(state) = registry
        .get(endpoint)
        .and_then(|targets| targets.get(target_id))
        .and_then(Weak::upgrade)
    {
        return state;
    }
    registry.retain(|_, targets| {
        targets.retain(|_, state| state.strong_count() > 0);
        !targets.is_empty()
    });
    let state = Arc::default();
    registry
        .entry(endpoint.to_owned())
        .or_default()
        .insert(target_id.to_owned(), Arc::downgrade(&state));
    state
}

impl ViewportState {
    async fn connection(
        &mut self,
        session: &Session,
    ) -> Result<(Arc<crate::cdp::Transport>, Arc<str>)> {
        if session.transport.is_closed() {
            return Err(Error::transport("Viewport session is disconnected"));
        }
        if let Some(controller) = &self.controller {
            if let Some(transport) = controller.transport.upgrade().filter(|t| !t.is_closed()) {
                return Ok((transport, Arc::clone(&controller.session_id)));
            }
        }
        let session_id: Arc<str> = if self.controller.is_some() {
            let attached: TargetAttachToTargetResult = session
                .transport
                .send(
                    "Target.attachToTarget",
                    &TargetAttachToTarget {
                        target_id: session.target_id.clone(),
                        flatten: Some(true),
                    },
                )
                .await?;
            self.metrics = None;
            attached.session_id.into()
        } else {
            session.session_id.as_str().into()
        };
        self.controller = Some(ViewportController {
            transport: Arc::downgrade(&session.transport),
            session_id: Arc::clone(&session_id),
        });
        Ok((Arc::clone(&session.transport), session_id))
    }
}

async fn complete_command<C: serde::Serialize>(
    transport: Arc<crate::cdp::Transport>,
    session_id: &Arc<str>,
    method: &str,
    params: &C,
) -> Result<serde_json::Value> {
    let dispatch = crate::cdp::transport::CommandDispatch::new();
    let (written, response) = transport
        .send_to_session_for_completion(session_id, method, params, &dispatch)
        .await?;
    // The response receipt owns channels/maps, not the browser transport.
    // Native completion remains observable beyond the caller's deadline without
    // extending an owned or borrowed browser's lifetime.
    drop(transport);
    let completed = response.wait().await;
    written?;
    completed
}

impl Session {
    async fn viewport_guard(&self) -> Result<tokio::sync::OwnedMutexGuard<ViewportState>> {
        tokio::time::timeout(
            self.transport.command_timeout(),
            Arc::clone(&self.target.viewport).lock_owned(),
        )
        .await
        .map_err(|_| Error::Timeout("Viewport is awaiting native command completion".into()))
    }

    /// Enable the Fetch domain, optionally handling auth challenges (proxy auth).
    /// Must be called per-session (per-tab).
    /// Enable page events
    pub(crate) async fn page_enable(&self) -> Result<()> {
        self.send_void("Page.enable", &PageEnable {}).await
    }

    /// Navigate to a URL, optionally with a custom Referer header
    pub(crate) async fn navigate(
        &self,
        url: &str,
        referrer: Option<&str>,
    ) -> Result<PageNavigateResult> {
        let _scope = self.target.headers.lock().await?;
        self.navigate_unlocked(url, referrer).await
    }

    pub(crate) async fn navigate_unlocked(
        &self,
        url: &str,
        referrer: Option<&str>,
    ) -> Result<PageNavigateResult> {
        self.send(
            "Page.navigate",
            &PageNavigate {
                url: url.to_string(),
                referrer: referrer.map(String::from),
            },
        )
        .await
    }

    /// Bypass CSP enforcement for the current page.
    /// Must be called before navigation to take effect.
    pub(crate) async fn set_bypass_csp(&self, enabled: bool) -> Result<()> {
        self.send_void("Page.setBypassCSP", &PageSetBypassCSP { enabled })
            .await
    }

    /// Switch JS execution off/on for the current page.
    /// Must be called before navigation to take effect.
    pub(crate) async fn set_script_execution_disabled(&self, disabled: bool) -> Result<()> {
        self.send_void(
            "Emulation.setScriptExecutionDisabled",
            &EmulationSetScriptExecutionDisabled { value: disabled },
        )
        .await
    }

    pub(crate) async fn set_viewport(
        &self,
        width: u32,
        height: u32,
        device_scale_factor: f64,
    ) -> Result<()> {
        let metrics = EmulationSetDeviceMetricsOverride::new(width, height, device_scale_factor)?;
        self.target.headers.wait().await?;
        let mut viewport = self.viewport_guard().await?;
        let (transport, session_id) = viewport.connection(self).await?;
        let timeout = transport.command_timeout();
        let task = tokio::spawn(async move {
            complete_command(
                transport,
                &session_id,
                "Emulation.setDeviceMetricsOverride",
                &metrics,
            )
            .await?;
            viewport.metrics = Some(metrics);
            Ok(())
        });
        tokio::time::timeout(timeout, task)
            .await
            .map_err(|_| Error::Timeout("Viewport command exceeded its deadline".into()))?
            .map_err(|error| Error::transport(format!("Viewport task failed: {error}")))?
    }

    pub(crate) async fn reset_viewport(&self) -> Result<()> {
        self.target.headers.wait().await?;
        let mut viewport = self.viewport_guard().await?;
        let (transport, session_id) = viewport.connection(self).await?;
        let timeout = transport.command_timeout();
        let task = tokio::spawn(async move {
            let weak_transport = Arc::downgrade(&transport);
            // Clearing an untouched handler is a no-op, even if another session
            // emulated this tab. Prime this handler before restoring native metrics.
            complete_command(
                transport,
                &session_id,
                "Emulation.setDeviceMetricsOverride",
                &EmulationSetDeviceMetricsOverride {
                    width: 0,
                    height: 0,
                    device_scale_factor: 0.0,
                    mobile: false,
                },
            )
            .await?;
            let transport = weak_transport
                .upgrade()
                .ok_or_else(|| Error::transport("Viewport controller disconnected during reset"))?;
            complete_command(
                transport,
                &session_id,
                "Emulation.clearDeviceMetricsOverride",
                &Empty {},
            )
            .await?;
            // Native visual properties can lag the emulation-disable reply.
            // A tiny native surface capture forces renderer work, including on
            // background tabs where animation-frame callbacks are suspended.
            viewport.metrics = None;
            let transport = weak_transport
                .upgrade()
                .ok_or_else(|| Error::transport("Viewport controller disconnected during reset"))?;
            let layout: PageGetLayoutMetricsResult = transport
                .send_to_session(&session_id, "Page.getLayoutMetrics", &Empty {})
                .await?;
            complete_command(
                transport,
                &session_id,
                "Page.captureScreenshot",
                &PageCaptureScreenshot {
                    format: None,
                    quality: None,
                    clip: Some(PageViewport {
                        x: layout.css_visual_viewport.page_x,
                        y: layout.css_visual_viewport.page_y,
                        width: 1.0,
                        height: 1.0,
                        scale: 1.0,
                    }),
                    capture_beyond_viewport: Some(false),
                },
            )
            .await?;
            let transport = weak_transport
                .upgrade()
                .ok_or_else(|| Error::transport("Viewport controller disconnected during reset"))?;
            let _: PageGetLayoutMetricsResult = transport
                .send_to_session(&session_id, "Page.getLayoutMetrics", &Empty {})
                .await?;
            Ok(())
        });
        tokio::time::timeout(timeout, task)
            .await
            .map_err(|_| Error::Timeout("Viewport reset exceeded its deadline".into()))?
            .map_err(|error| Error::transport(format!("Viewport reset task failed: {error}")))?
    }

    /// Override the User-Agent string (and optionally Accept-Language).
    pub(crate) async fn set_user_agent(
        &self,
        user_agent: &str,
        accept_language: Option<&str>,
    ) -> Result<()> {
        self.send_void(
            "Emulation.setUserAgentOverride",
            &EmulationSetUserAgentOverride {
                user_agent: user_agent.to_string(),
                accept_language: accept_language.map(String::from),
                platform: None,
                user_agent_metadata: None,
            },
        )
        .await
    }

    /// Override the User-Agent together with its client-hint metadata.
    pub(crate) async fn set_user_agent_full(
        &self,
        override_opts: EmulationSetUserAgentOverride,
    ) -> Result<()> {
        self.send_void("Emulation.setUserAgentOverride", &override_opts)
            .await
    }

    /// Set the browser-native ICU timezone for this session.
    pub(crate) async fn set_timezone_override(&self, timezone_id: &str) -> Result<()> {
        self.send_void(
            "Emulation.setTimezoneOverride",
            &EmulationSetTimezoneOverride {
                timezone_id: timezone_id.to_string(),
            },
        )
        .await
    }

    /// Ignore TLS certificate errors for this session.
    pub(crate) async fn set_ignore_cert_errors(&self, ignore: bool) -> Result<()> {
        self.send_void(
            "Security.setIgnoreCertificateErrors",
            &SecuritySetIgnoreCertificateErrors { ignore },
        )
        .await
    }

    /// Accept or dismiss a JavaScript dialog (alert / confirm / prompt).
    /// `prompt_text` is only used for prompt() dialogs.
    pub(crate) async fn handle_dialog(
        &self,
        accept: bool,
        prompt_text: Option<&str>,
    ) -> Result<()> {
        self.send_void(
            "Page.handleJavaScriptDialog",
            &PageHandleJavaScriptDialog {
                accept,
                prompt_text: prompt_text.map(String::from),
            },
        )
        .await
    }

    /// Add a script to evaluate on every new document
    pub(crate) async fn add_script_to_evaluate_on_new_document(
        &self,
        source: &str,
    ) -> Result<String> {
        let result: PageAddScriptToEvaluateOnNewDocumentResult = self
            .send(
                "Page.addScriptToEvaluateOnNewDocument",
                &PageAddScriptToEvaluateOnNewDocument {
                    source: source.to_string(),
                    world_name: None,
                    include_command_line_api: None,
                },
            )
            .await?;
        Ok(result.identifier)
    }

    /// Capture a screenshot without adding a second scale to the viewport DPR.
    pub(crate) async fn capture_screenshot(
        &self,
        options: &crate::ScreenshotOptions,
    ) -> Result<Vec<u8>> {
        options.validate()?;
        self.target.headers.wait().await?;
        let mut viewport = self.viewport_guard().await?;
        let (transport, session_id) = viewport.connection(self).await?;
        let options = *options;
        let timeout = transport.command_timeout();
        let task = tokio::spawn(async move {
            let clip = if let Some(clip) = options.clip {
                Some(PageViewport {
                    x: clip.x,
                    y: clip.y,
                    width: clip.width,
                    height: clip.height,
                    scale: 1.0,
                })
            } else if options.full_page {
                let layout: PageGetLayoutMetricsResult = transport
                    .send_to_session(&session_id, "Page.getLayoutMetrics", &Empty {})
                    .await?;
                Some(PageViewport {
                    x: layout.css_content_size.x,
                    y: layout.css_content_size.y,
                    width: layout.css_content_size.width,
                    height: layout.css_content_size.height,
                    scale: 1.0,
                })
            } else if let Some(metrics) = &viewport.metrics {
                let layout: PageGetLayoutMetricsResult = transport
                    .send_to_session(&session_id, "Page.getLayoutMetrics", &Empty {})
                    .await?;
                Some(PageViewport {
                    x: layout.css_visual_viewport.page_x,
                    y: layout.css_visual_viewport.page_y,
                    width: f64::from(metrics.width),
                    height: f64::from(metrics.height),
                    scale: 1.0,
                })
            } else {
                None
            };
            let value = complete_command(
                transport,
                &session_id,
                "Page.captureScreenshot",
                &PageCaptureScreenshot {
                    format: Some(options.format),
                    quality: options.quality,
                    clip,
                    capture_beyond_viewport: Some(options.full_page || options.clip.is_some()),
                },
            )
            .await?;
            // Chrome's actual reply or session/transport teardown, not a local
            // timeout, releases the coordinator after native capture restoration.
            drop(viewport);
            let result: PageCaptureScreenshotResult = serde_json::from_value(value)?;
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(&result.data)
                .map_err(|error| Error::Decode(error.to_string()))
        });
        tokio::time::timeout(timeout, task)
            .await
            .map_err(|_| {
                Error::Timeout(
                    "Screenshot exceeded its deadline; native completion is pending".into(),
                )
            })?
            .map_err(|error| Error::transport(format!("Screenshot task failed: {error}")))?
    }

    /// Get the frame tree
    pub(crate) async fn get_frame_tree(&self) -> Result<FrameTree> {
        let result: PageGetFrameTreeResult =
            self.send("Page.getFrameTree", &PageGetFrameTree {}).await?;
        Ok(result.frame_tree)
    }
}
