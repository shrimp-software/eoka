//! Page domain protocol types.

use super::Empty;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageNavigate {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referrer: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageNavigateResult {
    /// Whether Chrome converted this navigation into a download.
    #[serde(default)]
    pub is_download: Option<bool>,
    #[serde(default)]
    pub frame_id: String,
    #[serde(default)]
    pub loader_id: Option<String>,
    #[serde(default)]
    pub error_text: Option<String>,
    /// HTTP status code of the final response (e.g. 200, 404). Not set for non-HTTP schemes.
    #[serde(default)]
    pub http_status_code: Option<u16>,
}

pub type PageEnable = Empty;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageReload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore_cache: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_to_evaluate_on_load: Option<String>,
}

pub type PageGetNavigationHistory = Empty;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageGetNavigationHistoryResult {
    pub current_index: i32,
    pub entries: Vec<NavigationEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigationEntry {
    pub id: i32,
    pub url: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageNavigateToHistoryEntry {
    pub entry_id: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageAddScriptToEvaluateOnNewDocument {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub world_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_command_line_api: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageAddScriptToEvaluateOnNewDocumentResult {
    #[serde(default)]
    pub identifier: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageCaptureScreenshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<u8>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PageCaptureScreenshotResult {
    #[serde(default)]
    pub data: String,
}

pub type PageGetFrameTree = Empty;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageGetFrameTreeResult {
    #[serde(default)]
    pub frame_tree: FrameTree,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameTree {
    #[serde(default)]
    pub frame: Frame,
    #[serde(default)]
    pub child_frames: Vec<FrameTree>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub url: String,
}

/// Page.setBypassCSP — disable CSP enforcement for the current page
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageSetBypassCSP {
    pub enabled: bool,
}

/// Page.handleJavaScriptDialog — accept or dismiss an open JS dialog
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageHandleJavaScriptDialog {
    pub accept: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_text: Option<String>,
}
