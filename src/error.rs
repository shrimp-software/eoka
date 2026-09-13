//! Error types for eoka

use thiserror::Error;

/// Result type for eoka operations
pub type Result<T> = std::result::Result<T, Error>;

/// Error type for eoka.
///
/// `#[non_exhaustive]`: match with a `_ =>` arm so new variants are additive.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// Failed to launch Chrome
    #[error("Failed to launch Chrome: {0}")]
    Launch(String),

    /// Transport error
    #[error(fmt = transport_fmt)]
    Transport {
        /// Description of what failed.
        context: String,
        /// Underlying IO error, if any.
        #[source]
        source: Option<std::io::Error>,
    },

    /// CDP protocol error. `method`/`code` carry command context when the error
    /// came from a command response, and are `None` for context-free failures.
    #[error(fmt = cdp_fmt)]
    Cdp {
        /// CDP command method that failed, if known.
        method: Option<String>,
        /// CDP error code, if known.
        code: Option<i64>,
        /// Error message.
        message: String,
    },

    /// Navigation error
    #[error("Navigation error: {0}")]
    Navigation(String),

    /// Element not found in DOM
    #[error("Element not found: {0}")]
    ElementNotFound(String),

    /// Element exists in DOM but is not visible/rendered
    #[error("Element not visible: '{selector}' exists in DOM but is not rendered (hidden, display:none, or off-screen)")]
    ElementNotVisible {
        /// Selector that matched the hidden element.
        selector: String,
    },

    /// Timeout
    #[error("Timeout: {0}")]
    Timeout(String),

    /// Input action could not be completed because its held-input state was invalid.
    #[error("Input state error: {0}")]
    InputState(String),

    /// A form control did not contain the requested value after filling.
    #[error("Value mismatch for '{selector}': expected '{expected}', got '{actual}'")]
    ValueMismatch {
        /// Selector supplied to the fill operation.
        selector: String,
        /// Value requested by the caller.
        expected: String,
        /// Value read back from the DOM.
        actual: String,
    },

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Decode error (e.g., base64)
    #[error("Decode error: {0}")]
    Decode(String),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Chrome not found
    #[error("Chrome not found")]
    ChromeNotFound,

    /// Binary patching error
    #[error("Patching error in {operation}: {message}")]
    Patching {
        /// Patching operation that failed.
        operation: String,
        /// Error message.
        message: String,
    },

    /// Retry exhausted
    #[error("Retry exhausted after {attempts} attempts: {last_error}")]
    RetryExhausted {
        /// Number of attempts made before giving up.
        attempts: u32,
        /// The last error encountered.
        last_error: String,
    },
}

// `std::io::Error` and `serde_json::Error` aren't `Clone`, so we can't derive it.
// Reconstruct both best-effort (kind + message) so `Error` — and thus the
// broadcast `CdpMessage` — is cloneable.
impl Clone for Error {
    fn clone(&self) -> Self {
        use serde::de::Error as _;
        match self {
            Self::Launch(s) => Self::Launch(s.clone()),
            Self::Transport { context, source } => Self::Transport {
                context: context.clone(),
                source: source
                    .as_ref()
                    .map(|e| std::io::Error::new(e.kind(), e.to_string())),
            },
            Self::Cdp {
                method,
                code,
                message,
            } => Self::Cdp {
                method: method.clone(),
                code: *code,
                message: message.clone(),
            },
            Self::Navigation(s) => Self::Navigation(s.clone()),
            Self::ElementNotFound(s) => Self::ElementNotFound(s.clone()),
            Self::ElementNotVisible { selector } => Self::ElementNotVisible {
                selector: selector.clone(),
            },
            Self::Timeout(s) => Self::Timeout(s.clone()),
            Self::InputState(s) => Self::InputState(s.clone()),
            Self::ValueMismatch {
                selector,
                expected,
                actual,
            } => Self::ValueMismatch {
                selector: selector.clone(),
                expected: expected.clone(),
                actual: actual.clone(),
            },
            Self::Serialization(e) => Self::Serialization(serde_json::Error::custom(e.to_string())),
            Self::Decode(s) => Self::Decode(s.clone()),
            Self::Io(e) => Self::Io(std::io::Error::new(e.kind(), e.to_string())),
            Self::ChromeNotFound => Self::ChromeNotFound,
            Self::Patching { operation, message } => Self::Patching {
                operation: operation.clone(),
                message: message.clone(),
            },
            Self::RetryExhausted {
                attempts,
                last_error,
            } => Self::RetryExhausted {
                attempts: *attempts,
                last_error: last_error.clone(),
            },
        }
    }
}

/// Display for the `Cdp` variant, including method/code context when present.
fn cdp_fmt(
    method: &Option<String>,
    code: &Option<i64>,
    message: &str,
    f: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    match (method, code) {
        (Some(m), Some(c)) => write!(f, "CDP error in {}: {} (code {})", m, message, c),
        (Some(m), None) => write!(f, "CDP error in {}: {}", m, message),
        _ => write!(f, "CDP error: {}", message),
    }
}

/// Display for the `Transport` variant, including the io error cause when present.
fn transport_fmt(
    context: &str,
    source: &Option<std::io::Error>,
    f: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    match source {
        Some(err) => write!(f, "Transport error: {}: {}", context, err),
        None => write!(f, "Transport error: {}", context),
    }
}

impl Error {
    /// Create a transport error with context
    pub fn transport(context: impl Into<String>) -> Self {
        Self::Transport {
            context: context.into(),
            source: None,
        }
    }

    /// Create a transport error with IO source
    pub fn transport_io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Transport {
            context: context.into(),
            source: Some(source),
        }
    }

    /// Create a CDP error with full command context.
    pub fn cdp(method: impl Into<String>, code: i64, message: impl Into<String>) -> Self {
        Self::Cdp {
            method: Some(method.into()),
            code: Some(code),
            message: message.into(),
        }
    }

    /// Create a CDP error without command context.
    pub fn cdp_msg(message: impl Into<String>) -> Self {
        Self::Cdp {
            method: None,
            code: None,
            message: message.into(),
        }
    }

    /// Create a patching error
    pub fn patching(operation: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Patching {
            operation: operation.into(),
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as StdError;

    #[test]
    fn test_transport_io_display_includes_context_and_cause() {
        let err = Error::transport_io(
            "Failed to connect",
            std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "boom"),
        );
        let msg = format!("{}", err);
        // Both the context and the underlying io cause must appear.
        assert!(
            msg.contains("Failed to connect"),
            "missing context in: {msg}"
        );
        assert!(msg.contains("boom"), "missing io cause in: {msg}");
    }

    #[test]
    fn test_transport_io_exposes_source() {
        let err = Error::transport_io(
            "Failed to connect",
            std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "boom"),
        );
        assert!(
            err.source().is_some(),
            "transport_io error should expose its io source"
        );
    }

    #[test]
    fn test_cdp_display_with_and_without_context() {
        let full = Error::cdp("DOM.resolveNode", -1, "boom");
        assert_eq!(
            format!("{full}"),
            "CDP error in DOM.resolveNode: boom (code -1)"
        );
        let simple = Error::cdp_msg("no value");
        assert_eq!(format!("{simple}"), "CDP error: no value");
    }

    #[test]
    fn test_transport_display_plain_message() {
        let err = Error::transport("plain msg");
        let msg = format!("{}", err);
        assert!(msg.contains("plain msg"), "missing message in: {msg}");
        // No source for the context-only constructor.
        assert!(err.source().is_none());
    }
}
