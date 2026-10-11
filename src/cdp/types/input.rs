//! Input domain protocol types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputDispatchMouseEvent {
    pub r#type: MouseEventType,
    pub x: f64,
    pub y: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button: Option<MouseButton>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub click_count: Option<i32>,
    /// Bit field of currently pressed mouse buttons (left=1, right=2,
    /// middle=4, back=8, forward=16).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buttons: Option<i32>,
    /// Mouse pointer pressure: 0.5 while any button is held, zero otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_y: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
// Variants serialize to the exact CDP event-type strings ("mousePressed", …),
// so the shared `Mouse` prefix is intentional and must not be renamed.
#[allow(clippy::enum_variant_names)]
pub enum MouseEventType {
    MousePressed,
    MouseReleased,
    MouseMoved,
    MouseWheel,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    None,
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputDispatchKeyEvent {
    pub r#type: KeyEventType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeyEventType {
    #[default]
    KeyDown,
    KeyUp,
    RawKeyDown,
    Char,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputInsertText {
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputDispatchKeyEventFull {
    pub r#type: KeyEventType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modifiers: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unmodified_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub windows_virtual_key_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_virtual_key_code: Option<i32>,
}

/// Modifier flags for key events
pub mod modifiers {
    pub const ALT: i32 = 1;
    pub const CTRL: i32 = 2;
    pub const META: i32 = 4; // Cmd on Mac
    pub const SHIFT: i32 = 8;
}
