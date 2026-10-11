//! Browser domain protocol types.

use super::Empty;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetCreateTarget {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetCreateTargetResult {
    #[serde(default)]
    pub target_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetCloseTarget {
    pub target_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TargetCloseTargetResult {
    #[serde(default)]
    pub success: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetAttachToTarget {
    pub target_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flatten: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetAttachToTargetResult {
    #[serde(default)]
    pub session_id: String,
}

pub type TargetGetTargets = Empty;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetGetTargetsResult {
    #[serde(default)]
    pub target_infos: Vec<TargetInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetActivateTarget {
    pub target_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetInfo {
    pub target_id: String,
    pub r#type: String,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub attached: bool,
}

pub type BrowserGetVersion = Empty;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserGetVersionResult {
    #[serde(default)]
    pub product: String,
    #[serde(default)]
    pub user_agent: String,
}

pub type BrowserClose = Empty;
