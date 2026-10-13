use std::collections::HashMap;

use super::Page;
use crate::error::{Error, Result};

impl Page {
    /// Check a navigation result for errors.
    /// ERR_HTTP_RESPONSE_CODE_FAILURE (4xx/5xx) is ignored — the page still loaded.
    fn check_nav_result(result: &crate::cdp::types::PageNavigateResult) -> Result<()> {
        if let Some(ref error) = result.error_text {
            if error != "net::ERR_HTTP_RESPONSE_CODE_FAILURE" {
                return Err(Error::Navigation(error.clone()));
            }
        }
        Ok(())
    }

    /// Navigate and verify a new document or same-document navigation commit.
    /// The full operation, including the target queue, obeys the configured deadline.
    /// Use an explicit expectation when application readiness requires more than a commit.
    pub async fn goto(&self, url: &str) -> Result<()> {
        self.act(
            crate::Action::Navigate {
                url: url.into(),
                referrer: None,
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }

    /// Navigate an auxiliary tab to an attachment URL. Native events, not this
    /// acknowledgment, determine whether a download actually started.
    pub async fn navigate_for_download(&self, url: &str, referrer: Option<&str>) -> Result<()> {
        let result = self.session.navigate(url, referrer).await?;
        self.invalidate_document();
        if result.is_download == Some(true)
            || result.error_text.as_deref() == Some("net::ERR_ABORTED")
        {
            return Ok(());
        }
        Self::check_nav_result(&result)
    }

    /// Reload the page
    pub async fn reload(&self) -> Result<()> {
        self.act(crate::Action::Reload, self.action_options.clone())
            .await
            .map(|_| ())
    }

    /// Go back in history
    pub async fn back(&self) -> Result<()> {
        self.act(crate::Action::Back, self.action_options.clone())
            .await
            .map(|_| ())
    }

    /// Go forward in history
    pub async fn forward(&self) -> Result<()> {
        self.act(crate::Action::Forward, self.action_options.clone())
            .await
            .map(|_| ())
    }

    /// Get current URL
    pub async fn url(&self) -> Result<String> {
        let frame_tree = self.session.get_frame_tree().await?;
        Ok(frame_tree.frame.url)
    }

    /// Navigate to `url` while sending custom HTTP headers.
    /// Headers are set before navigation and cleared to an empty map afterward,
    /// including on error or cancellation; previous headers are not restored.
    /// Navigation and header changes on the same target are serialized.
    /// Cancelled cleanup continues in the background for up to five seconds.
    /// Later commands wait for it and report any failure until a successful
    /// `clear_extra_headers` explicitly recovers the target.
    pub async fn goto_with_headers(
        &self,
        url: &str,
        headers: HashMap<String, String>,
    ) -> Result<()> {
        self.act(
            crate::Action::NavigateWithHeaders {
                url: url.into(),
                headers,
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }

    /// Navigate to `url` with a custom Referer header.
    pub async fn goto_with_referrer(&self, url: &str, referrer: &str) -> Result<()> {
        self.act(
            crate::Action::Navigate {
                url: url.into(),
                referrer: Some(referrer.into()),
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }

    /// Disable CSP enforcement for the current page.
    /// Must be called before navigation to take effect.
    pub async fn set_bypass_csp(&self, enabled: bool) -> Result<()> {
        self.session.set_bypass_csp(enabled).await
    }

    /// Enable or disable JavaScript execution for the current page.
    /// Must be called before navigation to take effect.
    pub async fn set_javascript_enabled(&self, enabled: bool) -> Result<()> {
        self.session.set_script_execution_disabled(!enabled).await
    }

    /// Override the User-Agent string for this page.
    pub async fn set_user_agent(&self, user_agent: &str) -> Result<()> {
        self.session.set_user_agent(user_agent, None).await
    }

    /// Ignore TLS certificate errors for this session.
    pub async fn ignore_cert_errors(&self, ignore: bool) -> Result<()> {
        self.session.set_ignore_cert_errors(ignore).await
    }

    /// Accept a pending JS dialog (alert / confirm / prompt).
    /// For prompt() dialogs, provide `prompt_text` to fill the input.
    pub async fn accept_dialog(&self, prompt_text: Option<&str>) -> Result<()> {
        self.session.handle_dialog(true, prompt_text).await
    }

    /// Dismiss a pending JS dialog (cancel / close).
    pub async fn dismiss_dialog(&self) -> Result<()> {
        self.session.handle_dialog(false, None).await
    }
}
