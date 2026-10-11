//! Dom domain protocol types.

use super::runtime::RemoteObject;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DOMGetDocument {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pierce: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DOMGetDocumentResult {
    #[serde(default)]
    pub root: DOMNode,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DOMNode {
    #[serde(default)]
    pub node_id: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DOMQuerySelector {
    pub node_id: i32,
    pub selector: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DOMQuerySelectorResult {
    #[serde(default)]
    pub node_id: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DOMQuerySelectorAll {
    pub node_id: i32,
    pub selector: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DOMQuerySelectorAllResult {
    #[serde(default)]
    pub node_ids: Vec<i32>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DOMGetBoxModel {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<i32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DOMGetBoxModelResult {
    #[serde(default)]
    pub model: BoxModel,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoxModel {
    #[serde(default)]
    pub content: Vec<f64>,
}

impl BoxModel {
    /// Center of the content quad, or `None` if the box model is degenerate.
    pub fn try_center(&self) -> Option<(f64, f64)> {
        if self.content.len() >= 8 {
            let x = (self.content[0] + self.content[2] + self.content[4] + self.content[6]) / 4.0;
            let y = (self.content[1] + self.content[3] + self.content[5] + self.content[7]) / 4.0;
            Some((x, y))
        } else {
            None
        }
    }

    /// Center of the content quad, or `(0.0, 0.0)` when unavailable.
    #[deprecated(note = "use `try_center` — a silent (0,0) clicks the viewport corner")]
    pub fn center(&self) -> (f64, f64) {
        self.try_center().unwrap_or((0.0, 0.0))
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DOMGetOuterHTML {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<i32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DOMGetOuterHTMLResult {
    #[serde(rename = "outerHTML")]
    pub outer_html: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DOMRequestNode {
    pub object_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DOMRequestNodeResult {
    #[serde(default)]
    pub node_id: i32,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DOMFocus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DOMResolveNode {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_group: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DOMResolveNodeResult {
    #[serde(default)]
    pub object: RemoteObject,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DOMSetFileInputFiles {
    pub files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_node_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<String>,
}
