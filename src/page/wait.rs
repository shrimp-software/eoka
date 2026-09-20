use super::{is_element_cdp_error, sleep_ms, Element, Page, INTERACTION_DELAY_MS};
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
    async fn poll_until<T, F, Fut>(&self, timeout_ms: u64, error_msg: String, check: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<Option<T>>>,
    {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(timeout_ms);
        loop {
            if let Some(val) = check().await? {
                return Ok(val);
            }
            if start.elapsed() > timeout {
                return Err(Error::Timeout(error_msg));
            }
            sleep_ms(POLL_INTERVAL_MS).await;
        }
    }

    /// Wait for an element to appear in the DOM
    pub async fn wait_for(&self, selector: &str, timeout_ms: u64) -> Result<Element> {
        self.poll_until(
            timeout_ms,
            format!("Element '{}' not found within {}ms", selector, timeout_ms),
            || async { retry_if_not_found(self.find(selector).await) },
        )
        .await
    }

    /// Wait for an element to be visible and clickable
    pub async fn wait_for_visible(&self, selector: &str, timeout_ms: u64) -> Result<Element> {
        self.poll_until(
            timeout_ms,
            format!("Element '{}' not visible within {}ms", selector, timeout_ms),
            || async {
                let Some(element) = retry_if_not_found(self.find(selector).await)? else {
                    return Ok(None);
                };
                match element.center().await {
                    Ok(_) => Ok(Some(element)),
                    Err(error) if is_element_cdp_error(&error) => Ok(None),
                    Err(error) => Err(error),
                }
            },
        )
        .await
    }

    /// Wait for an element to disappear
    pub async fn wait_for_hidden(&self, selector: &str, timeout_ms: u64) -> Result<()> {
        self.poll_until(
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
        self.poll_until(
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
        self.poll_until(
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
        let original_url = self.url().await?;
        self.poll_until(
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
    }

    /// Wait for any of the given selectors to appear
    ///
    /// Returns the first selector that matches.
    pub async fn wait_for_any(&self, selectors: &[&str], timeout_ms: u64) -> Result<Element> {
        self.poll_until(
            timeout_ms,
            format!(
                "None of selectors found within {}ms: {:?}",
                timeout_ms, selectors
            ),
            || async { retry_if_not_found(self.find_any(selectors).await) },
        )
        .await
    }

    /// Wait for network to become idle (no pending XHR/fetch for `idle_time_ms`)
    pub async fn wait_for_network_idle(&self, idle_time_ms: u64, timeout_ms: u64) -> Result<()> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(timeout_ms);
        let idle_duration = std::time::Duration::from_millis(idle_time_ms);

        let key = &self.net_idle_key;
        let check_idle_js = format!(
            r#"
            (() => {{
                var K = '{key}';
                if (window[K] === undefined) {{
                    Object.defineProperty(window, K, {{
                        value: {{ n: 0 }}, writable: true, enumerable: false, configurable: true
                    }});
                    var st = window[K];
                    const of = window.fetch;
                    const wf = function fetch(...a) {{
                        st.n++;
                        return of.apply(this, a).finally(() => {{ st.n--; }});
                    }};
                    try {{ wf.toString = () => 'function fetch() {{ [native code] }}'; }} catch(e) {{}}
                    window.fetch = wf;
                    const oo = XMLHttpRequest.prototype.open;
                    const os = XMLHttpRequest.prototype.send;
                    XMLHttpRequest.prototype.open = function(...a) {{ this[K] = true; return oo.apply(this, a); }};
                    XMLHttpRequest.prototype.send = function(...a) {{
                        if (this[K]) {{
                            st.n++;
                            this.addEventListener('loadend', () => {{ st.n--; }});
                        }}
                        return os.apply(this, a);
                    }};
                }}
                var pend = window[K].n;
                return (document.readyState === 'complete') ? pend : -1;
            }})()
        "#
        );

        let mut idle_start: Option<std::time::Instant> = None;

        loop {
            let pending: i32 = self.evaluate_sync(&check_idle_js).await?;

            if pending == 0 {
                match idle_start {
                    Some(start) if start.elapsed() >= idle_duration => {
                        return Ok(());
                    }
                    None => {
                        idle_start = Some(std::time::Instant::now());
                    }
                    _ => {}
                }
            } else {
                idle_start = None;
            }

            if start.elapsed() > timeout {
                tracing::warn!(
                    "wait_for_network_idle timed out after {}ms with {} pending request(s)",
                    timeout_ms,
                    pending
                );
                return Err(Error::Timeout(format!(
                    "Network not idle after {}ms ({} pending requests)",
                    timeout_ms, pending
                )));
            }

            sleep_ms(INTERACTION_DELAY_MS).await;
        }
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
