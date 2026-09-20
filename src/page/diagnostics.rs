use super::{Page, PageState};
use crate::error::Result;

impl Page {
    /// Get page title
    pub async fn title(&self) -> Result<String> {
        // Use evaluate_sync (awaitPromise=false) so heavy SPAs don't block
        self.evaluate_sync("document.title || ''").await
    }

    /// Get page HTML content
    pub async fn content(&self) -> Result<String> {
        self.evaluate_sync("document.documentElement.outerHTML")
            .await
    }

    /// Get page text content (body innerText)
    pub async fn text(&self) -> Result<String> {
        self.evaluate_sync("document.body?.innerText || ''").await
    }

    /// Capture a screenshot as PNG bytes
    pub async fn screenshot(&self) -> Result<Vec<u8>> {
        self.session.capture_screenshot(Some("png"), None).await
    }

    /// Capture a screenshot as JPEG with quality
    pub async fn screenshot_jpeg(&self, quality: u8) -> Result<Vec<u8>> {
        self.session
            .capture_screenshot(Some("jpeg"), Some(quality))
            .await
    }

    /// Take a debug screenshot and save it with a timestamp
    ///
    /// Saves to `StealthConfig::debug_dir` if set, otherwise current directory.
    /// Useful during development to understand page state.
    pub async fn debug_screenshot(&self, prefix: &str) -> Result<String> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        let filename = match &self.config.debug_dir {
            Some(dir) => {
                // Ensure directory exists
                std::fs::create_dir_all(dir)?;
                format!("{}/{}_{}.png", dir, prefix, timestamp)
            }
            None => format!("{}_{}.png", prefix, timestamp),
        };

        let screenshot = self.screenshot().await?;
        std::fs::write(&filename, screenshot)?;
        Ok(filename)
    }

    /// Log the current page state for debugging
    pub async fn debug_state(&self) -> Result<PageState> {
        let state: PageState = self
            .evaluate(
                r#"({
                url: location.href,
                title: document.title,
                input_count: document.querySelectorAll('input').length,
                button_count: document.querySelectorAll('button').length,
                link_count: document.querySelectorAll('a').length,
                form_count: document.querySelectorAll('form').length
            })"#,
            )
            .await
            .unwrap_or_else(|_| PageState {
                url: "unknown".to_string(),
                title: "unknown".to_string(),
                input_count: 0,
                button_count: 0,
                link_count: 0,
                form_count: 0,
            });
        Ok(state)
    }
}
