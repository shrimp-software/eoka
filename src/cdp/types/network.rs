//! Network domain protocol types.

use super::Empty;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, Serialize)]
pub struct NetworkGetCookies {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub urls: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NetworkGetCookiesResult {
    #[serde(default)]
    pub cookies: Vec<Cookie>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub expires: f64,
    pub size: i32,
    pub http_only: bool,
    pub secure: bool,
    pub session: bool,
    #[serde(default)]
    pub same_site: Option<String>,
    /// CHIPS partition. Chrome before ~119 sent a bare top-level-site string.
    #[serde(default, deserialize_with = "partition_key")]
    pub partition_key: Option<CookiePartitionKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CookiePartitionKey {
    pub top_level_site: String,
    #[serde(default)]
    pub has_cross_site_ancestor: bool,
}

fn partition_key<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<CookiePartitionKey>, D::Error> {
    Ok(
        match Option::<serde_json::Value>::deserialize(deserializer)? {
            Some(serde_json::Value::String(site)) if !site.is_empty() => Some(CookiePartitionKey {
                top_level_site: site,
                has_cross_site_ancestor: false,
            }),
            Some(value @ serde_json::Value::Object(_)) => {
                serde_json::from_value(value).map_err(serde::de::Error::custom)?
            }
            _ => None,
        },
    )
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkSetCookie {
    pub name: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition_key: Option<CookiePartitionKey>,
}

#[cfg(test)]
mod cookie_tests {
    use super::*;

    #[test]
    fn partition_key_accepts_object_legacy_string_and_absence() {
        let base = serde_json::json!({"name":"n","value":"v","domain":"a.test","path":"/","expires":-1,"size":2,"httpOnly":false,"secure":true,"session":true});
        let parse = |key: Option<serde_json::Value>| {
            let mut cookie = base.clone();
            if let Some(key) = key {
                cookie["partitionKey"] = key;
            }
            serde_json::from_value::<Cookie>(cookie)
                .unwrap()
                .partition_key
        };
        assert_eq!(parse(None), None);
        assert_eq!(
            parse(Some(
                serde_json::json!({"topLevelSite":"https://top.test","hasCrossSiteAncestor":true})
            )),
            Some(CookiePartitionKey {
                top_level_site: "https://top.test".into(),
                has_cross_site_ancestor: true
            })
        );
        assert_eq!(
            parse(Some(serde_json::json!("https://top.test"))),
            Some(CookiePartitionKey {
                top_level_site: "https://top.test".into(),
                has_cross_site_ancestor: false
            })
        );
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NetworkSetCookieResult {
    #[serde(default)]
    pub success: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkDeleteCookies {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkEnable {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_post_data_size: Option<i64>,
}

pub type NetworkDisable = Empty;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkGetResponseBody {
    pub request_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkGetResponseBodyResult {
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub base64_encoded: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkRequest {
    pub url: String,
    pub method: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub post_data: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkResponse {
    pub url: String,
    pub status: i32,
    pub status_text: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkRequestWillBeSentEvent {
    pub request_id: String,
    pub request: NetworkRequest,
    pub timestamp: f64,
    #[serde(default)]
    pub r#type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkResponseReceivedEvent {
    pub request_id: String,
    pub response: NetworkResponse,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkLoadingFinishedEvent {
    pub request_id: String,
    pub timestamp: f64,
    pub encoded_data_length: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkLoadingFailedEvent {
    pub request_id: String,
    pub error_text: String,
    #[serde(default)]
    pub canceled: Option<bool>,
}

/// Network.clearBrowserCookies — wipe all cookies for this browser context
pub type NetworkClearBrowserCookies = Empty;

/// Network.setCookies — bulk-set multiple cookies at once
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkSetCookies {
    pub cookies: Vec<NetworkSetCookie>,
}

/// A URL pattern used to filter which requests Fetch.enable intercepts
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPattern {
    /// Glob URL pattern, e.g. "*/api/*" (matches all if omitted)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url_pattern: Option<String>,
    /// Resource type, e.g. "Document", "XHR", "Fetch", "Script"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    /// "Request" or "HeadersReceived"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_stage: Option<String>,
}

/// Fetch.enable — optionally with URL patterns for request interception
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchEnable {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patterns: Option<Vec<RequestPattern>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle_auth_requests: Option<bool>,
}
