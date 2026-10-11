//! Network commands on a CDP session.

use super::Session;
use crate::cdp::types::*;
use crate::Result;

impl Session {
    /// Set extra HTTP headers sent with every request on this session.
    /// Pass an empty map to clear previously set headers.
    pub(crate) async fn set_extra_headers(
        &self,
        headers: std::collections::HashMap<String, String>,
    ) -> Result<()> {
        let _scope = self.target.headers.lock().await?;
        self.set_extra_headers_unlocked(headers).await
    }

    pub(crate) async fn set_extra_headers_unlocked(
        &self,
        headers: std::collections::HashMap<String, String>,
    ) -> Result<()> {
        crate::cdp::headers::set_headers(&self.transport, &self.session_id, headers).await
    }

    pub(crate) async fn temporary_headers(&self) -> Result<crate::cdp::headers::TemporaryHeaders> {
        self.target
            .headers
            .temporary(self.transport.clone(), &self.session_id)
            .await
    }

    /// Clear headers, including restoration left pending by a cancelled navigation.
    pub(crate) async fn clear_extra_headers(&self) -> Result<()> {
        self.target
            .headers
            .clear(self.transport.clone(), &self.session_id)
            .await
    }

    /// Clear all browser cookies for this context.
    pub(crate) async fn clear_all_cookies(&self) -> Result<()> {
        self.send_void(
            "Network.clearBrowserCookies",
            &NetworkClearBrowserCookies {},
        )
        .await
    }

    /// Bulk-set multiple cookies at once.
    pub(crate) async fn set_cookies(&self, cookies: Vec<NetworkSetCookie>) -> Result<()> {
        self.send_void("Network.setCookies", &NetworkSetCookies { cookies })
            .await
    }

    /// Enable Fetch domain request interception with URL patterns.
    /// `patterns` — list of URL/resource-type filters; empty matches everything.
    pub(crate) async fn fetch_enable_interception(
        &self,
        patterns: Vec<RequestPattern>,
        handle_auth: bool,
    ) -> Result<()> {
        self.send_void(
            "Fetch.enable",
            &FetchEnable {
                patterns: if patterns.is_empty() {
                    None
                } else {
                    Some(patterns)
                },
                handle_auth_requests: Some(handle_auth),
            },
        )
        .await
    }

    /// Get all cookies
    pub(crate) async fn get_cookies(&self, urls: Option<Vec<String>>) -> Result<Vec<Cookie>> {
        let result: NetworkGetCookiesResult = self
            .send("Network.getCookies", &NetworkGetCookies { urls })
            .await?;
        Ok(result.cookies)
    }

    /// Set a cookie
    pub(crate) async fn set_cookie(
        &self,
        name: &str,
        value: &str,
        url: Option<&str>,
        domain: Option<&str>,
        path: Option<&str>,
    ) -> Result<bool> {
        let result: NetworkSetCookieResult = self
            .send(
                "Network.setCookie",
                &NetworkSetCookie {
                    name: name.to_string(),
                    value: value.to_string(),
                    url: url.map(String::from),
                    domain: domain.map(String::from),
                    path: path.map(String::from),
                    ..Default::default()
                },
            )
            .await?;
        Ok(result.success)
    }

    /// Delete cookies
    pub(crate) async fn delete_cookies(
        &self,
        name: &str,
        url: Option<&str>,
        domain: Option<&str>,
    ) -> Result<()> {
        self.send_void(
            "Network.deleteCookies",
            &NetworkDeleteCookies {
                name: name.to_string(),
                url: url.map(String::from),
                domain: domain.map(String::from),
                ..Default::default()
            },
        )
        .await
    }

    /// Enable network events (request/response capture)
    /// NOTE: This enables Network.enable which may be slightly detectable
    pub(crate) async fn network_enable(&self) -> Result<()> {
        self.send_void(
            "Network.enable",
            &NetworkEnable {
                max_post_data_size: Some(65536), // Capture POST data up to 64KB
            },
        )
        .await
    }

    /// Disable network events
    pub(crate) async fn network_disable(&self) -> Result<()> {
        self.send_void("Network.disable", &NetworkDisable {}).await
    }

    /// Get response body for a request
    pub(crate) async fn get_response_body(&self, request_id: &str) -> Result<(String, bool)> {
        let result: NetworkGetResponseBodyResult = self
            .send(
                "Network.getResponseBody",
                &NetworkGetResponseBody {
                    request_id: request_id.to_string(),
                },
            )
            .await?;
        Ok((result.body, result.base64_encoded))
    }
}
