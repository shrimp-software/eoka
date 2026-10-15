//! Native recording and sequential IO schemas from Chromium 155.0.8059.39.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PageStartScreenRecording {
    pub audio: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ScreenRecordingResult {
    pub stream: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct IoRead<'a> {
    pub handle: &'a str,
    pub size: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IoReadResult {
    #[serde(default)]
    pub base64_encoded: bool,
    pub data: String,
    pub eof: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct IoClose<'a> {
    pub handle: &'a str,
}
