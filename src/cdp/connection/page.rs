//! Page commands on a CDP session.

use super::Session;
use crate::cdp::types::*;
use crate::Result;

impl Session {
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

    /// Reload the page
    pub(crate) async fn reload(&self, ignore_cache: bool) -> Result<()> {
        let _scope = self.target.headers.lock().await?;
        self.send_void(
            "Page.reload",
            &PageReload {
                ignore_cache: Some(ignore_cache),
                script_to_evaluate_on_load: None,
            },
        )
        .await
    }

    /// Navigate to a history entry by offset (-1 = back, +1 = forward)
    async fn navigate_history(&self, offset: i32) -> Result<()> {
        let _scope = self.target.headers.lock().await?;
        let history: PageGetNavigationHistoryResult = self
            .send("Page.getNavigationHistory", &PageGetNavigationHistory {})
            .await?;
        let target = history.current_index + offset;
        if target < 0 {
            return Ok(());
        }
        if let Some(entry) = history.entries.get(target as usize) {
            self.send_void(
                "Page.navigateToHistoryEntry",
                &PageNavigateToHistoryEntry { entry_id: entry.id },
            )
            .await?;
        }
        Ok(())
    }

    /// Go back in history
    pub(crate) async fn go_back(&self) -> Result<()> {
        self.navigate_history(-1).await
    }

    /// Go forward in history
    pub(crate) async fn go_forward(&self) -> Result<()> {
        self.navigate_history(1).await
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

    /// Capture a screenshot
    pub(crate) async fn capture_screenshot(
        &self,
        format: Option<&str>,
        quality: Option<u8>,
    ) -> Result<Vec<u8>> {
        // Chrome expects quality 0-100; clamp to valid range
        let quality = quality.map(|q| q.min(100));
        let result: PageCaptureScreenshotResult = self
            .send(
                "Page.captureScreenshot",
                &PageCaptureScreenshot {
                    format: format.map(String::from),
                    quality,
                },
            )
            .await?;

        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&result.data)
            .map_err(|e| crate::error::Error::Decode(e.to_string()))?;
        Ok(bytes)
    }

    /// Get the frame tree
    pub(crate) async fn get_frame_tree(&self) -> Result<FrameTree> {
        let result: PageGetFrameTreeResult =
            self.send("Page.getFrameTree", &PageGetFrameTree {}).await?;
        Ok(result.frame_tree)
    }
}
