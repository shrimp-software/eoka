use super::{is_element_cdp_error, sleep_ms, Element, Page};
use crate::error::{Error, Result};

/// Polling interval (ms) used by wait_for_* loop iterations.
const POLL_INTERVAL_MS: u64 = 100;

/// Convert a missing-element result into a polling miss while preserving
/// operational failures such as a closed transport or malformed CDP reply.
fn retry_if_not_found<T>(result: Result<T>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(Error::ElementNotFound(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

impl Page {
    /// Generic polling helper — calls `check` every `POLL_INTERVAL_MS` until it
    /// returns `Ok(Some(value))`, then returns that value. Operational errors
    /// propagate immediately; `Ok(None)` retries until the timeout.
    async fn poll_until<T, F, Fut>(timeout_ms: u64, error_msg: String, check: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<Option<T>>>,
    {
        let timeout = std::time::Duration::from_millis(timeout_ms);
        tokio::time::timeout(timeout, async {
            loop {
                if let Some(val) = check().await? {
                    return Ok(val);
                }
                sleep_ms(POLL_INTERVAL_MS).await;
            }
        })
        .await
        .map_err(|_| Error::Timeout(error_msg))?
    }

    /// Wait for an element to appear in the DOM
    pub async fn wait_for(&self, selector: &str, timeout_ms: u64) -> Result<Element> {
        Self::poll_until(
            timeout_ms,
            format!("Element '{}' not found within {}ms", selector, timeout_ms),
            || async { retry_if_not_found(self.find(selector).await) },
        )
        .await
    }

    /// Wait for a rendered element with a nonzero box and visible CSS visibility.
    /// Actions separately wait for enabled state, stability and hit testing.
    pub async fn wait_for_visible(&self, selector: &str, timeout_ms: u64) -> Result<Element> {
        Self::poll_until(
            timeout_ms,
            format!("Element '{}' not visible within {}ms", selector, timeout_ms),
            || async {
                let Some(element) = retry_if_not_found(self.find(selector).await)? else {
                    return Ok(None);
                };
                match element.is_visible().await {
                    Ok(true) => Ok(Some(element)),
                    Ok(false) => Ok(None),
                    Err(error) if is_element_cdp_error(&error) => Ok(None),
                    Err(error) => Err(error),
                }
            },
        )
        .await
    }

    /// Wait for an element to disappear
    pub async fn wait_for_hidden(&self, selector: &str, timeout_ms: u64) -> Result<()> {
        Self::poll_until(
            timeout_ms,
            format!(
                "Element '{}' still visible after {}ms",
                selector, timeout_ms
            ),
            || async {
                match self.find(selector).await {
                    Err(Error::ElementNotFound(_)) => Ok(Some(())),
                    Ok(el) => match el.is_visible().await {
                        Ok(false) => Ok(Some(())),
                        Ok(true) => Ok(None),
                        Err(error) if is_element_cdp_error(&error) => Ok(Some(())),
                        Err(error) => Err(error),
                    },
                    Err(error) => Err(error),
                }
            },
        )
        .await
    }

    /// Wait for a fixed duration
    pub async fn wait(&self, ms: u64) {
        sleep_ms(ms).await;
    }

    /// Wait for an element with specific text to appear
    pub async fn wait_for_text(&self, text: &str, timeout_ms: u64) -> Result<Element> {
        Self::poll_until(
            timeout_ms,
            format!(
                "Element with text '{}' not found within {}ms",
                text, timeout_ms
            ),
            || async { retry_if_not_found(self.find_by_text(text).await) },
        )
        .await
    }

    /// Wait for the URL to contain a specific string
    pub async fn wait_for_url_contains(&self, pattern: &str, timeout_ms: u64) -> Result<()> {
        Self::poll_until(
            timeout_ms,
            format!("URL did not contain '{}' within {}ms", pattern, timeout_ms),
            || async {
                let url = self.url().await?;
                Ok(url.contains(pattern).then_some(()))
            },
        )
        .await
    }

    /// Wait for URL to change from current URL
    pub async fn wait_for_url_change(&self, timeout_ms: u64) -> Result<String> {
        tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), async {
            let original_url = self.url().await?;
            Self::poll_until(
                timeout_ms,
                format!(
                    "URL did not change from '{}' within {}ms",
                    original_url, timeout_ms
                ),
                || async {
                    let url = self.url().await?;
                    Ok((url != original_url).then_some(url))
                },
            )
            .await
        })
        .await
        .map_err(|_| Error::Timeout("URL did not change before the deadline".into()))?
    }

    /// Wait for any of the given selectors to appear
    ///
    /// Returns the first selector that matches.
    pub async fn wait_for_any(&self, selectors: &[&str], timeout_ms: u64) -> Result<Element> {
        Self::poll_until(
            timeout_ms,
            format!(
                "None of selectors found within {}ms: {:?}",
                timeout_ms, selectors
            ),
            || async { retry_if_not_found(self.find_any(selectors).await) },
        )
        .await
    }

    /// Wait for the loaded top document's fetch/XHR requests to finish and stay
    /// idle for `idle_time_ms`. Includes response bodies and requests started
    /// before this call. WebSockets, workers and subframes are outside this scope.
    /// Newly attached, previously untracked tabs require a full navigation or
    /// reload first; Eoka never reloads automatically. The timeout bounds the
    /// complete wait, including any pending header cleanup.
    pub async fn wait_for_network_idle(&self, idle_time_ms: u64, timeout_ms: u64) -> Result<()> {
        self.session
            .wait_for_network_idle(idle_time_ms, timeout_ms)
            .await
    }

    /// Retry an operation multiple times with delays between attempts
    pub async fn with_retry<F, Fut, T>(
        &self,
        attempts: u32,
        delay_ms: u64,
        operation: F,
    ) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let attempts = attempts.max(1);
        let mut last_error = String::new();

        for attempt in 1..=attempts {
            match operation().await {
                Ok(result) => return Ok(result),
                Err(e @ Error::Action { .. }) => return Err(e),
                Err(e) => {
                    last_error = e.to_string();
                    if attempt < attempts {
                        sleep_ms(delay_ms).await;
                    }
                }
            }
        }

        Err(Error::RetryExhausted {
            attempts,
            last_error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn slow_command_cannot_overrun_poll_deadline() {
        let start = tokio::time::Instant::now();
        let result: Result<()> = Page::poll_until(100, "bounded".into(), || async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            Ok(Some(()))
        })
        .await;
        assert!(matches!(result, Err(Error::Timeout(_))));
        assert_eq!(start.elapsed(), std::time::Duration::from_millis(100));
    }

    #[test]
    fn test_retry_if_not_found_only_retries_absence() {
        assert!(matches!(retry_if_not_found::<()>(Ok(())), Ok(Some(()))));
        assert!(matches!(
            retry_if_not_found::<()>(Err(Error::ElementNotFound("missing".into()))),
            Ok(None)
        ));
        assert!(matches!(
            retry_if_not_found::<()>(Err(Error::transport("disconnected"))),
            Err(Error::Transport { .. })
        ));
    }
}
