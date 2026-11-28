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
    pub error_text: Option<String>,
}

pub type PageEnable = Empty;

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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageViewport {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageVisualViewport {
    pub page_x: f64,
    pub page_y: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageContentSize {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageGetLayoutMetricsResult {
    pub css_visual_viewport: PageVisualViewport,
    pub css_content_size: PageContentSize,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageCaptureScreenshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<crate::ScreenshotFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip: Option<PageViewport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_beyond_viewport: Option<bool>,
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
    /// Identifies the committed document; changes when a new one replaces it.
    #[serde(default)]
    pub loader_id: Option<String>,
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
