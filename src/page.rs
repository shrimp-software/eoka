//! Page Abstraction
//!
//! High-level API for interacting with a browser page.

mod diagnostics;
mod elements;
mod evaluate;
mod frames;
mod input;
mod navigation;
mod network;
mod response_capture;
mod wait;

pub use response_capture::{
    CaptureBodyError, CapturedFrameResponse, FrameResponseCapture, FrameResponseCaptureOptions,
    FrameResponseCaptureReport,
};

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use crate::cdp::Session;
use crate::error::{Error, Result};
use crate::StealthConfig;

pub(crate) use input::{
    coordinated_key_char, coordinated_key_down, coordinated_key_up, coordinated_mouse_down,
    coordinated_mouse_move, coordinated_mouse_up, coordinated_mouse_wheel, DragInput,
    HeldInputState,
};

// Re-export so the historical `eoka::page::Element` / `eoka::page::BoundingBox`
// paths keep resolving after the split into `element.rs`.
pub use crate::element::{BoundingBox, Element};

/// Settle time (ms) used after actions (click, navigate, hover) to let the
/// page react before the next step.
const SETTLE_MS: u64 = 100;

/// Brief pause (ms) between micro-interactions (e.g. focus→type, select_all→delete).
pub(crate) const INTERACTION_DELAY_MS: u64 = 50;

/// Async sleep for the given number of milliseconds.
pub(crate) async fn sleep_ms(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

/// Escape a string for safe use in JavaScript string literals (single pass).
pub(crate) fn escape_js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '`' => out.push_str("\\`"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\x00"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            '$' if chars.peek() == Some(&'{') => {
                out.push_str("\\${");
                chars.next();
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Whether a CDP error message names a missing/stale backend node.
fn is_missing_node_message(message: &str) -> bool {
    message.contains("Could not find node") || message.contains("No node with given id")
}

/// Check if a CDP error indicates the cached node id is no longer valid.
fn is_stale_node_error(e: &Error) -> bool {
    matches!(e, Error::Cdp { message, .. } if is_missing_node_message(message))
}

/// Check if a CDP error is an element-related error (not found, not visible, etc.)
fn is_element_cdp_error(e: &Error) -> bool {
    match e {
        Error::ElementNotFound(_) | Error::ElementNotVisible { .. } => true,
        Error::Cdp { message, .. } => {
            message.contains("box model")
                || is_missing_node_message(message)
                || message.contains("Node is not an element")
        }
        Error::Launch(_)
        | Error::Transport { .. }
        | Error::Navigation(_)
        | Error::Timeout(_)
        | Error::InputState(_)
        | Error::ValueMismatch { .. }
        | Error::Serialization(_)
        | Error::Decode(_)
        | Error::Io(_)
        | Error::ChromeNotFound
        | Error::Patching { .. }
        | Error::RetryExhausted { .. } => false,
    }
}

/// Text matching strategy for find_by_text operations
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextMatch {
    /// Exact match (trimmed, case-sensitive)
    Exact,
    /// Contains the text (case-insensitive) - default
    #[default]
    Contains,
    /// Starts with the text (case-insensitive)
    StartsWith,
    /// Ends with the text (case-insensitive)
    EndsWith,
}

/// A mouse button used by held pointer input methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// Primary (usually left) mouse button.
    Left,
    /// Auxiliary (usually middle) mouse button.
    Middle,
    /// Secondary (usually right) mouse button.
    Right,
    /// Browser-back mouse button.
    Back,
    /// Browser-forward mouse button.
    Forward,
}

/// A browser page with stealth capabilities. Cheap to clone (`Arc`-backed
/// shared state) — clones refer to the same tab and share the document cache.
#[derive(Clone)]
pub struct Page {
    pub(crate) session: Session,
    config: Arc<StealthConfig>,
    /// Cached root `DOM.getDocument` node id (invalidated on navigation).
    /// CDP node IDs are positive, so zero represents an empty cache. Fetching
    /// twice during a race is harmless and avoids a poisonable lock.
    root_node: Arc<AtomicI32>,
    /// Randomized per-page property key for the network-idle request counter.
    net_idle_key: String,
    /// Pointer and keyboard state shared by this Page and all Element/Page clones.
    held_input: Arc<tokio::sync::Mutex<HeldInputState>>,
}

impl Page {
    /// Create a new Page wrapping a CDP session
    pub(crate) fn new(session: Session, config: Arc<StealthConfig>) -> Self {
        let net_idle_key = format!("_{:012x}", fastrand::u64(..) & 0xffff_ffff_ffff);
        let held_input = session.held_input();
        Self {
            session,
            config,
            root_node: Arc::new(AtomicI32::new(0)),
            net_idle_key,
            held_input,
        }
    }

    /// Return the cached root document node id, fetching it once if needed.
    async fn document_node(&self) -> Result<i32> {
        let id = self.root_node.load(Ordering::Acquire);
        if id != 0 {
            return Ok(id);
        }
        let doc = self.session.get_document(Some(0)).await?;
        self.root_node.store(doc.node_id, Ordering::Release);
        Ok(doc.node_id)
    }

    /// Drop the cached document node (call after navigation).
    fn invalidate_document(&self) {
        self.root_node.store(0, Ordering::Release);
    }

    /// Run an operation against the cached document node, retrying once if stale.
    async fn with_document<T, F, Fut>(&self, op: F) -> Result<T>
    where
        F: Fn(i32) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let doc = self.document_node().await?;
        match op(doc).await {
            Err(ref e) if is_stale_node_error(e) => {
                self.invalidate_document();
                let doc = self.document_node().await?;
                op(doc).await
            }
            other => other,
        }
    }

    /// Get the underlying CDP session
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Get the target ID for this page (tab identifier)
    pub fn target_id(&self) -> &str {
        self.session.target_id()
    }
}

/// A captured HTTP request with its response
#[derive(Debug, Clone)]
pub struct CapturedRequest {
    /// CDP request ID (use with `get_response_body`).
    pub request_id: String,
    /// Request URL.
    pub url: String,
    /// HTTP method.
    pub method: String,
    /// Request headers.
    pub headers: HashMap<String, String>,
    /// Request body, if present.
    pub post_data: Option<String>,
    /// Resource type (e.g. "Document", "XHR", "Script").
    pub resource_type: Option<String>,
    /// Response status code, once received.
    pub status: Option<i32>,
    /// Response status text, once received.
    pub status_text: Option<String>,
    /// Response headers, once received.
    pub response_headers: Option<HashMap<String, String>>,
    /// Response MIME type, once received.
    pub mime_type: Option<String>,
    /// Time the request was initiated (CDP monotonic timestamp).
    pub timestamp: f64,
    /// Whether the response has finished loading.
    pub complete: bool,
}

/// Response body - either text or binary
#[derive(Debug)]
pub enum ResponseBody {
    /// Textual response body.
    Text(String),
    /// Binary response body.
    Binary(Vec<u8>),
}

impl ResponseBody {
    /// Get as text, or `None` for a binary response.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ResponseBody::Text(s) => Some(s),
            ResponseBody::Binary(_) => None,
        }
    }

    /// Get as bytes
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            ResponseBody::Text(s) => s.as_bytes(),
            ResponseBody::Binary(b) => b,
        }
    }
}

/// Information about a frame/iframe
#[derive(Debug, Clone)]
pub struct FrameInfo {
    /// Frame ID
    pub id: String,
    /// Frame URL
    pub url: String,
    /// Frame name (if any)
    pub name: Option<String>,
}

/// Debug information about page state
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PageState {
    /// Current page URL.
    pub url: String,
    /// Current page title.
    pub title: String,
    /// Number of `<input>` elements.
    pub input_count: u32,
    /// Number of `<button>` elements.
    pub button_count: u32,
    /// Number of `<a>` link elements.
    pub link_count: u32,
    /// Number of `<form>` elements.
    pub form_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_js_string() {
        assert_eq!(escape_js_string("hello"), "hello");
        assert_eq!(escape_js_string("it's"), "it\\'s");
        assert_eq!(escape_js_string("line1\nline2"), "line1\\nline2");
        assert_eq!(escape_js_string("back\\slash"), "back\\\\slash");
        assert_eq!(escape_js_string("${var}"), "\\${var}");
        // Null byte escaped as \x00
        assert_eq!(escape_js_string("a\0b"), "a\\x00b");
        assert_eq!(escape_js_string("a\0"), "a\\x00");
        assert_eq!(escape_js_string("a\u{0}1"), "a\\x001");
        assert_eq!(escape_js_string("a\u{2028}b"), "a\\u2028b");
        assert_eq!(escape_js_string("a\u{2029}b"), "a\\u2029b");
    }

    #[test]
    fn test_escape_js_string_null_is_hex_not_octal() {
        // '\0' must become the hex escape \x00, never the octal \0 (which
        // would be ambiguous when followed by a digit and could be parsed
        // as an octal escape by a JS engine).
        assert_eq!(escape_js_string("\0"), "\\x00");
        assert_ne!(escape_js_string("\0"), "\\0");
        // Followed by a digit: must stay "\x001", not "\01".
        assert_eq!(escape_js_string("a\u{0}1"), "a\\x001");
    }

    #[test]
    fn test_escape_js_string_all_special_chars() {
        assert_eq!(escape_js_string("'"), "\\'");
        assert_eq!(escape_js_string("\""), "\\\"");
        assert_eq!(escape_js_string("`"), "\\`");
        assert_eq!(escape_js_string("\\"), "\\\\");
        assert_eq!(escape_js_string("\n"), "\\n");
        assert_eq!(escape_js_string("\r"), "\\r");
        assert_eq!(escape_js_string("${"), "\\${");
        assert_eq!(escape_js_string("\u{2028}"), "\\u2028");
        assert_eq!(escape_js_string("\u{2029}"), "\\u2029");
    }

    #[test]
    fn test_stale_node_error_detection() {
        let stale = Error::cdp(
            "DOM.resolveNode",
            -32000,
            "Could not find node with given id",
        );
        assert!(is_stale_node_error(&stale));
        assert!(is_element_cdp_error(&stale));
    }

    #[test]
    fn test_box_model_error_is_element_but_not_stale() {
        let box_model = Error::cdp("DOM.getBoxModel", -32000, "box model could not be computed");
        // "box model" substring makes it an element error...
        assert!(is_element_cdp_error(&box_model));
        // ...but it is not a stale-node error.
        assert!(!is_stale_node_error(&box_model));
    }

    #[test]
    fn test_element_not_found_is_element_but_not_stale() {
        let not_found = Error::ElementNotFound("x".into());
        assert!(is_element_cdp_error(&not_found));
        assert!(!is_stale_node_error(&not_found));
    }

    #[test]
    fn test_missing_node_message_variants() {
        assert!(is_missing_node_message("Could not find node with given id"));
        assert!(is_missing_node_message("No node with given id found"));
        assert!(!is_missing_node_message("box model could not be computed"));
    }
}
