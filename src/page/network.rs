use std::collections::HashMap;

use super::{Page, ResponseBody};
use crate::error::{Error, ErrorCode, Result};
use crate::fetch::{BrowserFetchOutcome, BrowserFetchRequest, BrowserFetchResponse};
use crate::session::{BrowserState, SessionCookie};

fn session_cookie(c: crate::cdp::types::Cookie) -> SessionCookie {
    SessionCookie {
        name: c.name,
        value: c.value,
        domain: c.domain,
        path: c.path,
        secure: c.secure,
        http_only: c.http_only,
        same_site: c.same_site,
        // Session cookies have no meaningful expiry.
        expires: if c.session { None } else { Some(c.expires) },
        partition_key: c
            .partition_key
            .map(|key| crate::session::CookiePartitionKey {
                top_level_site: key.top_level_site,
                has_cross_site_ancestor: key.has_cross_site_ancestor,
            }),
    }
}

/// Writes storage only when the page is on the saved URL's (non-opaque) origin.
const RESTORE_STORAGE: &str = "(url, local, session, replace) => {
    const saved = new URL(url, location.href).origin;
    const actual = location.origin;
    if (saved === 'null' || saved !== actual) return { written: false, saved, actual };
    if (replace) { localStorage.clear(); sessionStorage.clear(); }
    for (const [key, value] of Object.entries(local)) localStorage.setItem(key, value);
    for (const [key, value] of Object.entries(session)) sessionStorage.setItem(key, value);
    return { written: true, saved, actual };
}";

#[derive(serde::Deserialize)]
struct StorageRestore {
    written: bool,
    saved: String,
    actual: String,
}

impl Page {
    /// Resolve the current page URL for cookie operations.
    /// Returns None for about:blank (Chrome silently rejects cookies without a URL).
    async fn cookie_url(&self) -> Result<Option<String>> {
        let url = self.url().await?;
        Ok(if url == "about:blank" {
            None
        } else {
            Some(url)
        })
    }

    /// Fetch a URL from inside the page context.
    pub async fn fetch(&self, request: BrowserFetchRequest) -> Result<BrowserFetchResponse> {
        let script = crate::fetch::fetch_script(&request)?;
        self.evaluate(&script).await
    }

    /// Fetch multiple URLs from inside the page context.
    pub async fn fetch_many(
        &self,
        requests: Vec<BrowserFetchRequest>,
    ) -> Result<Vec<BrowserFetchOutcome>> {
        if requests.len() > 100 {
            return Err(Error::coded(
                ErrorCode::InvalidArgument,
                "fetch_many accepts at most 100 requests",
            ));
        }
        let script = crate::fetch::fetch_many_script(&requests)?;
        self.evaluate(&script).await
    }

    /// Get the cookies that apply to the current page URL, including HttpOnly ones.
    /// Use [`all_cookies`](Self::all_cookies) for every site in the browser context.
    pub async fn cookies(&self) -> Result<Vec<SessionCookie>> {
        let cookies = self.session.get_cookies(None).await?;
        Ok(cookies.into_iter().map(session_cookie).collect())
    }

    /// Get every cookie in the browser context, for all sites, including HttpOnly ones.
    pub async fn all_cookies(&self) -> Result<Vec<SessionCookie>> {
        let cookies = self.session.get_all_cookies().await?;
        Ok(cookies.into_iter().map(session_cookie).collect())
    }

    /// Set a cookie
    pub async fn set_cookie(
        &self,
        name: &str,
        value: &str,
        domain: Option<&str>,
        path: Option<&str>,
    ) -> Result<()> {
        let url = self.cookie_url().await?;
        let success = self
            .session
            .set_cookie(name, value, url.as_deref(), domain, path)
            .await?;
        if !success {
            return Err(Error::cdp_msg("Failed to set cookie"));
        }
        Ok(())
    }

    /// Delete a cookie
    pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<()> {
        let url = self.cookie_url().await?;
        self.session
            .delete_cookies(name, url.as_deref(), domain)
            .await
    }

    /// Clear all browser cookies for this context.
    pub async fn clear_all_cookies(&self) -> Result<()> {
        self.session.clear_all_cookies().await
    }

    /// Bulk-import cookies (e.g., restored from a prior `cookies()` dump).
    ///
    /// A `domain` without a leading dot stays host-only, and partitioned
    /// (CHIPS) cookies keep their partition.
    pub async fn set_cookies_bulk(&self, cookies: Vec<SessionCookie>) -> Result<()> {
        let cdp_cookies = cookies
            .into_iter()
            .map(|c| crate::cdp::types::NetworkSetCookie {
                name: c.name,
                value: c.value,
                url: None,
                domain: Some(c.domain),
                path: Some(c.path),
                secure: Some(c.secure),
                http_only: Some(c.http_only),
                same_site: c.same_site,
                expires: c.expires,
                partition_key: c
                    .partition_key
                    .map(|key| crate::cdp::types::CookiePartitionKey {
                        top_level_site: key.top_level_site,
                        has_cross_site_ancestor: key.has_cross_site_ancestor,
                    }),
            })
            .collect();
        self.session.set_cookies(cdp_cookies).await
    }

    /// Capture cookies and origin storage needed to restore an authenticated session.
    ///
    /// Like Playwright's `storageState`, cookies cover every site in this page's
    /// browser context, so the result is a credential for all of them. This includes
    /// a browser you attached to, such as a personal Chrome. Unlike `storageState`,
    /// storage covers only the current origin, and includes `sessionStorage`.
    pub async fn capture_state(&self) -> Result<BrowserState> {
        let cookies = self.all_cookies().await?;
        let local_storage = self
            .evaluate("Object.fromEntries(Object.entries(localStorage))")
            .await?;
        let session_storage = self
            .evaluate("Object.fromEntries(Object.entries(sessionStorage))")
            .await?;
        let user_agent = self.evaluate("navigator.userAgent").await?;
        let url = self.url().await?;

        Ok(BrowserState {
            cookies,
            local_storage,
            session_storage,
            user_agent,
            url,
        })
    }

    /// Restore a captured browser state and reload its saved URL.
    ///
    /// Cookies are restored first, so the first request to the saved URL already
    /// carries them. This replaces the browser context's whole cookie jar, so
    /// cookies added after the capture are removed. A live session is never wiped,
    /// because the browser isn't ours: saved cookies and storage entries are added
    /// or overwritten, and its other cookies and storage entries stay.
    ///
    /// Storage is written only when the saved URL loads on its own origin. If it
    /// redirects elsewhere (to a login page, say) and there is storage to restore,
    /// this fails after restoring cookies and writes no storage.
    ///
    /// A different saved user agent is applied with matching client hints and
    /// this page's languages. Values the launch evasions installed, such as
    /// `navigator.platform`, keep the launch fingerprint; launch with the saved
    /// `user_agent` for a fully consistent identity.
    pub async fn restore_state(&self, state: &BrowserState) -> Result<()> {
        self.restore_user_agent(&state.user_agent).await?;
        if !self.config.live_session {
            self.clear_all_cookies().await?;
        }
        self.set_cookies_bulk(state.cookies.clone()).await?;
        self.goto(&state.url).await?;

        let has_storage = !state.local_storage.is_empty() || !state.session_storage.is_empty();
        let replace = !self.config.live_session;
        if !has_storage && !replace {
            return Ok(());
        }
        // Inlined as JSON literals rather than function arguments, which are
        // capped at 1 MiB; origin storage can hold several megabytes.
        let arguments = serde_json::to_string(&serde_json::json!([
            state.url,
            state.local_storage,
            state.session_storage,
            replace,
        ]))?;
        let landed: StorageRestore = self
            .evaluate(&format!("({RESTORE_STORAGE})(...{arguments})"))
            .await?;
        match landed {
            StorageRestore { written: true, .. } => self.goto(&state.url).await,
            StorageRestore { saved, actual, .. } if has_storage => Err(Error::Navigation(format!(
                "restore_state restored cookies, but the saved URL (origin {saved}) loaded on origin {actual}; storage was not written"
            ))),
            _ => Ok(()),
        }
    }

    /// Apply a saved user agent without discarding client-hint metadata.
    async fn restore_user_agent(&self, user_agent: &str) -> Result<()> {
        let current: String = self.evaluate("navigator.userAgent").await?;
        if current == user_agent || user_agent.is_empty() {
            return Ok(());
        }
        let languages: Vec<String> = self.evaluate("Array.from(navigator.languages)").await?;
        let mut fingerprint = crate::stealth::Fingerprint::resolve(Some(user_agent), None);
        if !languages.is_empty() {
            fingerprint.languages = languages;
        }
        self.session
            .set_user_agent_full(crate::browser::ua_override_for(&fingerprint))
            .await
    }

    /// Set extra HTTP headers sent with every subsequent request from this page.
    /// Pass an empty map to clear.
    pub async fn set_extra_headers(&self, headers: HashMap<String, String>) -> Result<()> {
        self.session.set_extra_headers(headers).await
    }

    /// Remove all extra HTTP headers set via set_extra_headers.
    /// Also retries failed temporary-header cleanup on its original CDP session.
    pub async fn clear_extra_headers(&self) -> Result<()> {
        self.session.clear_extra_headers().await
    }

    /// Start recording this page's requests and responses. Subscribe before
    /// triggering the traffic you want; see [`RequestCapture`](crate::network::RequestCapture).
    ///
    /// Enables the CDP Network domain on this page (which advanced anti-bot
    /// scripts may be able to detect); it stays enabled after the capture stops.
    pub async fn capture_requests(
        &self,
        options: crate::network::RequestCaptureOptions,
    ) -> Result<crate::network::RequestCapture> {
        crate::network::RequestCapture::start(self, options).await
    }

    /// Enable the CDP Network domain on this page, so
    /// [`get_response_body`](Self::get_response_body) works for request IDs you
    /// obtain yourself. This records nothing; use
    /// [`capture_requests`](Self::capture_requests) to list requests.
    /// NOTE: Network.enable may be slightly detectable by advanced anti-bot scripts.
    pub async fn enable_request_capture(&self) -> Result<()> {
        self.session.network_enable().await
    }

    /// Disable the CDP Network domain on this page. Chrome then discards
    /// buffered response bodies and headers set with
    /// [`set_extra_headers`](Self::set_extra_headers), and active
    /// [`RequestCapture`](crate::network::RequestCapture)s receive no more events.
    pub async fn disable_request_capture(&self) -> Result<()> {
        self.session.network_disable().await
    }

    /// Get the response body for a request, by
    /// [`CapturedRequest::request_id`](crate::page::CapturedRequest::request_id).
    /// Needs the Network domain enabled (an active capture) and a finished response.
    pub async fn get_response_body(&self, request_id: &str) -> Result<ResponseBody> {
        let (body, base64_encoded) = self.session.get_response_body(request_id).await?;

        if base64_encoded {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&body)
                .map_err(|e| Error::Decode(e.to_string()))?;
            Ok(ResponseBody::Binary(bytes))
        } else {
            Ok(ResponseBody::Text(body))
        }
    }
}
