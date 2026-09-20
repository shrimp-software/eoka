use std::collections::HashMap;

use super::{sleep_ms, Page, SETTLE_MS};
use crate::error::{Error, Result};

/// Preserve the primary operation error when both an operation and its
/// mandatory cleanup fail. A cleanup-only failure must still reach the caller.
fn finish_temporary_headers<T>(operation: Result<T>, cleanup: Result<()>) -> Result<T> {
    match (operation, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(operation_error), Err(cleanup_error)) => {
            tracing::error!(
                "Failed to clear temporary HTTP headers after navigation error: {}",
                cleanup_error
            );
            Err(operation_error)
        }
    }
}

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

    /// Start navigation to a URL.
    ///
    /// This returns once Chrome accepts the navigation rather than waiting for
    /// `document.readyState`. A page can legally remain in `loading` forever
    /// (for example, a single-page app with a long-lived resource), and making
    /// navigation wait on that state turns a best-effort convenience into a
    /// 30-second command stall. Call an explicit wait method when a later
    /// action actually needs a particular element or condition.
    pub async fn goto(&self, url: &str) -> Result<()> {
        self.navigate_impl(url, None).await
    }

    /// Reload the page
    pub async fn reload(&self) -> Result<()> {
        self.session.reload(false).await?;
        self.invalidate_document();
        Ok(())
    }

    /// Go back in history
    pub async fn back(&self) -> Result<()> {
        self.session.go_back().await?;
        self.invalidate_document();
        Ok(())
    }

    /// Go forward in history
    pub async fn forward(&self) -> Result<()> {
        self.session.go_forward().await?;
        self.invalidate_document();
        Ok(())
    }

    /// Get current URL
    pub async fn url(&self) -> Result<String> {
        let frame_tree = self.session.get_frame_tree().await?;
        Ok(frame_tree.frame.url)
    }

    /// Navigate to `url` while sending custom HTTP headers.
    /// Headers are set before navigation and cleared afterward.
    pub async fn goto_with_headers(
        &self,
        url: &str,
        headers: HashMap<String, String>,
    ) -> Result<()> {
        self.session.set_extra_headers(headers).await?;
        let navigation = self.goto(url).await;
        let cleanup = self.session.clear_extra_headers().await;
        finish_temporary_headers(navigation, cleanup)
    }

    /// Navigate to `url` with a custom Referer header.
    pub async fn goto_with_referrer(&self, url: &str, referrer: &str) -> Result<()> {
        self.navigate_impl(url, Some(referrer)).await
    }

    /// Shared navigation impl
    async fn navigate_impl(&self, url: &str, referrer: Option<&str>) -> Result<()> {
        let result = self.session.navigate(url, referrer).await?;
        Self::check_nav_result(&result)?;
        self.invalidate_document();
        // Give Chrome a brief opportunity to install the new document, but do
        // not probe readyState here. A Runtime.evaluate issued while a SPA is
        // tearing down its previous document can itself wait for the full CDP
        // command timeout and block a persistent automation daemon.
        sleep_ms(SETTLE_MS).await;
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temporary_header_cleanup_error_reaches_caller() {
        let result = finish_temporary_headers(Ok(()), Err(Error::transport("cleanup failed")));
        assert!(matches!(result, Err(Error::Transport { .. })));
    }

    #[test]
    fn test_navigation_error_wins_when_header_cleanup_also_fails() {
        let result = finish_temporary_headers::<()>(
            Err(Error::Navigation("navigation failed".into())),
            Err(Error::transport("cleanup failed")),
        );
        assert!(matches!(result, Err(Error::Navigation(_))));
    }
}
