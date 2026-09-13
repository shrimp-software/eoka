//! Hand-written CDP types for the ~20 commands we actually use
//!
//! These replace the massive chromiumoxide-generated types with a minimal set
//! that's just enough for stealth browser automation.
//!
//! Now that this module is crate-private (no longer re-exported), some fields
//! are deserialized from CDP responses but never read internally, and a few
//! protocol structs are only used by escape-hatch wrappers. Allow dead code
//! rather than prune fields the protocol still sends.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Empty params for CDP commands that take no arguments
#[derive(Debug, Clone, Default, Serialize)]
pub struct Empty {}

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
    #[serde(default)]
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

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DOMFocus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<i32>,
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
    pub text: String,
    pub line_number: i32,
    pub column_number: i32,
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

// === File Upload ===

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

// === Enhanced Key Events ===

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

// === Target Discovery (for multi-tab) ===

// === 2026 additions: headers, cookies, CSP bypass, UA, TLS, dialogs, Fetch interception ===

/// Network.setExtraHTTPHeaders — inject extra headers into every request on this session
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkSetExtraHTTPHeaders {
    pub headers: HashMap<String, String>,
}

/// Network.clearBrowserCookies — wipe all cookies for this browser context
pub type NetworkClearBrowserCookies = Empty;

/// Network.setCookies — bulk-set multiple cookies at once
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkSetCookies {
    pub cookies: Vec<NetworkSetCookie>,
}

/// Page.setBypassCSP — disable CSP enforcement for the current page
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageSetBypassCSP {
    pub enabled: bool,
}

/// Emulation.setScriptExecutionDisabled — switch JS execution off/on for the page
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmulationSetScriptExecutionDisabled {
    pub value: bool,
}

/// A single (brand, version) entry in `Sec-CH-UA` client-hint brand lists.
#[derive(Debug, Clone, Serialize)]
pub struct UserAgentBrandVersion {
    pub brand: String,
    pub version: String,
}

/// Emulation.UserAgentMetadata — drives the `Sec-CH-UA*` client-hint headers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserAgentMetadata {
    pub brands: Vec<UserAgentBrandVersion>,
    pub full_version_list: Vec<UserAgentBrandVersion>,
    pub platform: String,
    pub platform_version: String,
    pub architecture: String,
    pub model: String,
    pub mobile: bool,
    pub bitness: String,
    pub wow64: bool,
}

/// Emulation.setUserAgentOverride — override the User-Agent / Accept-Language
/// strings and (optionally) the client-hint metadata.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmulationSetUserAgentOverride {
    pub user_agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accept_language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent_metadata: Option<UserAgentMetadata>,
}

/// Security.setIgnoreCertificateErrors — bypass TLS errors for this session
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecuritySetIgnoreCertificateErrors {
    pub ignore: bool,
}

/// Emulation.setTimezoneOverride — set the browser-native ICU timezone for
/// this session. Unlike a JS shim, this makes `Date.toString()`, `getHours()`,
/// `Intl` and friends natively consistent with the claimed timezone.
#[derive(Debug, Clone, Serialize)]
pub struct EmulationSetTimezoneOverride {
    #[serde(rename = "timezoneId")]
    pub timezone_id: String,
}

/// Page.handleJavaScriptDialog — accept or dismiss an open JS dialog
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageHandleJavaScriptDialog {
    pub accept: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_text: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_box_model_try_center_valid_quad() {
        let bm = BoxModel {
            content: vec![0.0, 0.0, 10.0, 0.0, 10.0, 20.0, 0.0, 20.0],
        };
        assert_eq!(bm.try_center(), Some((5.0, 10.0)));
    }

    #[test]
    fn test_box_model_try_center_empty_is_none() {
        let bm = BoxModel::default();
        assert!(bm.content.is_empty());
        assert_eq!(bm.try_center(), None);
    }

    #[test]
    fn test_box_model_try_center_too_short_is_none() {
        // Fewer than 8 coordinates → degenerate → None.
        let bm = BoxModel {
            content: vec![0.0, 0.0, 10.0, 0.0, 10.0, 20.0],
        };
        assert_eq!(bm.try_center(), None);
    }

    #[test]
    fn test_mouse_event_serializes_held_button_mask() {
        let event = InputDispatchMouseEvent {
            r#type: MouseEventType::MousePressed,
            x: 12.5,
            y: 4.0,
            button: Some(MouseButton::Left),
            click_count: Some(1),
            buttons: Some(1),
            delta_x: None,
            delta_y: None,
        };

        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "type": "mousePressed",
                "x": 12.5,
                "y": 4.0,
                "button": "left",
                "clickCount": 1,
                "buttons": 1,
            })
        );
    }

    #[test]
    fn test_mouse_event_omits_optional_fields() {
        let event = InputDispatchMouseEvent {
            r#type: MouseEventType::MouseMoved,
            x: 0.0,
            y: 0.0,
            button: None,
            click_count: None,
            buttons: None,
            delta_x: None,
            delta_y: None,
        };

        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({ "type": "mouseMoved", "x": 0.0, "y": 0.0 })
        );
    }

    #[test]
    fn test_ua_override_skips_none_fields() {
        let cmd = EmulationSetUserAgentOverride {
            user_agent: "UA/1.0".to_string(),
            accept_language: None,
            platform: None,
            user_agent_metadata: None,
        };
        let value = serde_json::to_value(&cmd).unwrap();
        // Only the userAgent key is present; skip_serializing_if drops the rest.
        assert_eq!(value, json!({ "userAgent": "UA/1.0" }));
    }

    #[test]
    fn test_ua_override_includes_camel_case_fields_when_set() {
        let cmd = EmulationSetUserAgentOverride {
            user_agent: "UA/1.0".to_string(),
            accept_language: Some("en-US".to_string()),
            platform: Some("Linux".to_string()),
            user_agent_metadata: Some(UserAgentMetadata {
                brands: vec![],
                full_version_list: vec![],
                platform: "Linux".to_string(),
                platform_version: "6.0".to_string(),
                architecture: "x86".to_string(),
                model: "".to_string(),
                mobile: false,
                bitness: "64".to_string(),
                wow64: false,
            }),
        };
        let value = serde_json::to_value(&cmd).unwrap();
        let obj = value.as_object().unwrap();
        assert!(obj.contains_key("userAgent"));
        assert!(obj.contains_key("acceptLanguage"));
        assert!(obj.contains_key("platform"));
        assert!(obj.contains_key("userAgentMetadata"));
    }

    #[test]
    fn test_ua_metadata_camel_case_keys() {
        let meta = UserAgentMetadata {
            brands: vec![],
            full_version_list: vec![],
            platform: "Linux".to_string(),
            platform_version: "6.0".to_string(),
            architecture: "x86".to_string(),
            model: "".to_string(),
            mobile: true,
            bitness: "64".to_string(),
            wow64: false,
        };
        let value = serde_json::to_value(&meta).unwrap();
        let obj = value.as_object().unwrap();
        assert!(obj.contains_key("fullVersionList"));
        assert!(obj.contains_key("platformVersion"));
        assert!(obj.contains_key("mobile"));
        assert!(obj.contains_key("bitness"));
        assert!(obj.contains_key("wow64"));
        assert_eq!(obj.get("mobile"), Some(&json!(true)));
    }

    #[test]
    fn test_ua_brand_version_serialization() {
        let bv = UserAgentBrandVersion {
            brand: "Chromium".to_string(),
            version: "120".to_string(),
        };
        let value = serde_json::to_value(&bv).unwrap();
        assert_eq!(value, json!({ "brand": "Chromium", "version": "120" }));
    }
}
