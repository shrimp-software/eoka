use std::collections::HashMap;

use super::{Page, ResponseBody};
use crate::error::{Error, Result};
use crate::fetch::{BrowserFetchOutcome, BrowserFetchRequest, BrowserFetchResponse};
use crate::session::{BrowserState, SessionCookie};

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
            return Err(Error::cdp_msg("fetch_many accepts at most 100 requests"));
        }
        let script = crate::fetch::fetch_many_script(&requests)?;
        self.evaluate(&script).await
    }

    /// Get all cookies as domain `SessionCookie`s (no `cdp::types` leak).
    pub async fn cookies(&self) -> Result<Vec<SessionCookie>> {
        let cookies = self.session.get_cookies(None).await?;
        Ok(cookies
            .into_iter()
            .map(|c| SessionCookie {
                name: c.name,
                value: c.value,
                domain: c.domain,
                path: c.path,
                secure: c.secure,
                http_only: c.http_only,
                same_site: c.same_site,
                // Session cookies have no meaningful expiry.
                expires: if c.session { None } else { Some(c.expires) },
            })
            .collect())
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
            })
            .collect();
        self.session.set_cookies(cdp_cookies).await
    }

    /// Capture cookies and origin storage needed to restore an authenticated session.
    pub async fn capture_state(&self) -> Result<BrowserState> {
        let cookies = self.cookies().await?;
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
    pub async fn restore_state(&self, state: &BrowserState) -> Result<()> {
        self.set_user_agent(&state.user_agent).await?;
        self.goto(&state.url).await?;
        self.clear_all_cookies().await?;
        self.set_cookies_bulk(state.cookies.clone()).await?;

        let storage = serde_json::to_string(&serde_json::json!({
            "localStorage": state.local_storage,
            "sessionStorage": state.session_storage,
        }))?;
        self.execute(&format!(
            "(() => {{ const state = {storage}; localStorage.clear(); sessionStorage.clear(); for (const [key, value] of Object.entries(state.localStorage)) localStorage.setItem(key, value); for (const [key, value] of Object.entries(state.sessionStorage)) sessionStorage.setItem(key, value); }})()"
        ))
        .await?;
        self.goto(&state.url).await
    }

    /// Set extra HTTP headers sent with every subsequent request from this page.
    /// Pass an empty map to clear.
    pub async fn set_extra_headers(&self, headers: HashMap<String, String>) -> Result<()> {
        self.session.set_extra_headers(headers).await
    }

    /// Remove all extra HTTP headers set via set_extra_headers.
    pub async fn clear_extra_headers(&self) -> Result<()> {
        self.session.clear_extra_headers().await
    }

    /// Enable network request capture
    /// NOTE: This enables Network.enable which may be slightly detectable by advanced anti-bot
    pub async fn enable_request_capture(&self) -> Result<()> {
        self.session.network_enable().await
    }

    /// Disable network request capture
    pub async fn disable_request_capture(&self) -> Result<()> {
        self.session.network_disable().await
    }

    /// Get response body for a captured request
    /// The request_id comes from CapturedRequest.request_id
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
