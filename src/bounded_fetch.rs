//! Bounded text inspection through the active browser session.

use crate::{Error, Page, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Maximum retained response bytes accepted by bounded text inspection.
pub const MAX_TEXT_FETCH_BYTES: usize = 16 * 1024 * 1024;

/// Browser-context request for bounded text inspection, not binary downloading.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundedFetchRequest {
    /// HTTP(S) URL or a page-relative URL.
    pub url: String,
    /// HTTP method.
    pub method: String,
    /// Request headers permitted by the browser.
    pub headers: HashMap<String, String>,
    /// Optional request body.
    pub body: Option<String>,
    /// Browser redirect policy: follow, manual, or error.
    pub redirect: String,
    /// Retained raw body-byte limit; zero requests headers only.
    pub max_bytes: usize,
}

impl BoundedFetchRequest {
    /// A credentialed GET retaining at most 8192 response bytes.
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: "GET".into(),
            headers: HashMap::new(),
            body: None,
            redirect: "follow".into(),
            max_bytes: 8192,
        }
    }
}

/// A bounded response. No binary bodies are returned.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundedFetchResponse {
    /// Final URL; may contain credentials or query secrets.
    pub url: String,
    /// HTTP status; zero for an opaque response.
    pub status: u16,
    /// HTTP reason phrase.
    pub status_text: String,
    /// Native fetch response type.
    pub r#type: String,
    /// Whether the browser followed a redirect.
    pub redirected: bool,
    /// Browser-readable response headers.
    pub headers: HashMap<String, String>,
    /// Decoded text; incomplete trailing UTF-8 is omitted on truncation.
    pub body: String,
    /// Reading stopped at the limit before observing EOF.
    pub truncated: bool,
    /// Raw response bytes retained for decoding.
    pub bytes_read: usize,
    /// Why no body was inspected: binary, headers_only, or opaque.
    pub body_omitted: Option<String>,
}

impl Page {
    /// Fetch bounded text with browser credentials. Binary bodies are skipped.
    /// Retention is bounded; native browser/network buffering is not controlled.
    pub async fn fetch_bounded(
        &self,
        request: &BoundedFetchRequest,
    ) -> Result<BoundedFetchResponse> {
        if request.max_bytes > MAX_TEXT_FETCH_BYTES {
            return Err(Error::cdp_msg("max_body must not exceed 16777216 bytes"));
        }
        if !matches!(request.redirect.as_str(), "follow" | "manual" | "error") {
            return Err(Error::cdp_msg("redirect must be follow, manual, or error"));
        }
        let request = serde_json::to_string(request)?;
        let script = format!(
            "(async () => {{ const request = {request}; {} }})()",
            include_str!("bounded_fetch.js")
        );
        self.evaluate(&script).await
    }
}
