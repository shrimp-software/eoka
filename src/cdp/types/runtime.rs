//! Runtime domain protocol types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeGetProperties {
    pub object_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub own_properties: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeGetPropertiesResult {
    #[serde(default)]
    pub result: Vec<PropertyDescriptor>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyDescriptor {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: Option<RemoteObject>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeEvaluate {
    pub expression: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_by_value: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub await_promise: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeEvaluateResult {
    #[serde(default)]
    pub result: RemoteObject,
    #[serde(default)]
    pub exception_details: Option<ExceptionDetails>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteObject {
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub class_name: Option<String>,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub object_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExceptionDetails {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub line_number: i32,
    #[serde(default)]
    pub column_number: i32,
    #[serde(default)]
    pub exception: Option<RemoteObject>,
}

impl ExceptionDetails {
    /// Format the browser's exception description without including evaluated source.
    pub fn message(&self) -> String {
        let description = self
            .exception
            .as_ref()
            .and_then(|e| e.description.as_deref())
            .unwrap_or(&self.text);
        let description: String = description.chars().take(8192).collect();
        format!(
            "JavaScript error: {description} at {}:{}",
            self.line_number.saturating_add(1),
            self.column_number.saturating_add(1)
        )
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCallFunctionOn {
    pub function_declaration: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<CallArgument>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silent: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_by_value: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub await_promise: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallArgument {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCallFunctionOnResult {
    #[serde(default)]
    pub result: RemoteObject,
    #[serde(default)]
    pub exception_details: Option<ExceptionDetails>,
}
