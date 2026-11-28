//! Error types for eoka

use thiserror::Error;

pub(crate) mod classify;
mod code;
pub use code::ErrorCode;

/// Result type for eoka operations
pub type Result<T> = std::result::Result<T, Error>;

/// Error type for eoka.
///
/// `#[non_exhaustive]`: match with a `_ =>` arm so new variants are additive.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// A failed high-level action, including whether its effect is uncertain.
    #[error("Action {code}: {source}")]
    Action {
        /// Stable machine-readable action error code.
        code: ErrorCode,
        /// Dispatch and verification evidence retained on failure.
        receipt: Box<crate::ActionReceipt>,
        #[source]
        /// Underlying cause of the failure.
        source: Box<Error>,
    },
    /// A failure eoka raised itself (usage, policy or state), with a stable
    /// code. Chrome's own protocol errors stay [`Error::Cdp`].
    #[error("{code}: {message}")]
    Coded {
        /// Stable machine-readable code.
        code: ErrorCode,
        /// Human-readable detail. Never contains caller-supplied secrets.
        message: String,
    },

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

    /// A URL could not be used. The message omits the URL, which may hold secrets.
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    /// Timeout
    #[error("Timeout: {0}")]
    Timeout(String),

    /// Input action could not be completed because its held-input state was invalid.
    #[error("Input state error: {0}")]
    InputState(String),

    /// Screenshot encoding or extent options were invalid.
    #[error("Invalid screenshot options: {0}")]
    InvalidScreenshotOptions(String),

    /// A form control settled on a different value than requested, for
    /// example because of a mask, `maxlength` or trimming. Values are not
    /// included: they may be passwords. Only their lengths are.
    #[error("Value mismatch for {selector}: the control holds {actual_chars} characters after filling {expected_chars}")]
    ValueMismatch {
        /// Description of the filled control.
        selector: String,
        /// Character count of the requested value.
        expected_chars: usize,
        /// Character count of the value read back from the DOM.
        actual_chars: usize,
    },

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// A checked JavaScript result exceeded its UTF-8 JSON byte limit.
    #[error("Evaluation result exceeds its JSON byte limit of {maximum_bytes}")]
    EvaluationTooLarge {
        /// Maximum encoded result size accepted by the caller.
        maximum_bytes: usize,
    },

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
        /// Display text of the last error encountered.
        last_error: String,
        /// The last error encountered.
        #[source]
        source: Box<Error>,
    },
}

// `std::io::Error` and `serde_json::Error` aren't `Clone`, so we can't derive it.
// Reconstruct both best-effort (kind + message) so `Error` — and thus the
// broadcast `CdpMessage` — is cloneable.
impl Clone for Error {
    fn clone(&self) -> Self {
        use serde::de::Error as _;
        match self {
            Self::Action {
                code,
                receipt,
                source,
            } => Self::Action {
                code: *code,
                receipt: receipt.clone(),
                source: source.clone(),
            },
            Self::Coded { code, message } => Self::Coded {
                code: *code,
                message: message.clone(),
            },
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
            Self::InvalidUrl(s) => Self::InvalidUrl(s.clone()),
            Self::Timeout(s) => Self::Timeout(s.clone()),
            Self::InputState(s) => Self::InputState(s.clone()),
            Self::InvalidScreenshotOptions(s) => Self::InvalidScreenshotOptions(s.clone()),
            Self::ValueMismatch {
                selector,
                expected_chars,
                actual_chars,
            } => Self::ValueMismatch {
                selector: selector.clone(),
                expected_chars: *expected_chars,
                actual_chars: *actual_chars,
            },
            Self::Serialization(e) => Self::Serialization(serde_json::Error::custom(e.to_string())),
            Self::EvaluationTooLarge { maximum_bytes } => Self::EvaluationTooLarge {
                maximum_bytes: *maximum_bytes,
            },
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
                source,
            } => Self::RetryExhausted {
                attempts: *attempts,
                last_error: last_error.clone(),
                source: source.clone(),
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
    /// Decode a CDP JavaScript exception without exposing protocol implementation types.
    pub fn javascript_exception(details: &serde_json::Value) -> Self {
        match serde_json::from_value::<crate::cdp::types::ExceptionDetails>(details.clone()) {
            Ok(details) => Self::cdp_msg(details.message()),
            Err(_) => Self::cdp_msg("JavaScript exception details unavailable"),
        }
    }
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

    /// Create an error eoka raised itself, with a stable code.
    pub fn coded(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Coded {
            code,
            message: message.into(),
        }
    }

    /// The stable code of a [`Error::Coded`] or [`Error::Action`] error.
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            Self::Coded { code, .. } | Self::Action { code, .. } => Some(*code),
            Self::ValueMismatch { .. } => Some(ErrorCode::ValueMismatch),
            _ => None,
        }
    }

    /// Whether retrying the same call unchanged can succeed.
    ///
    /// Usage errors (invalid URLs, arguments or options, serialization,
    /// oversized results, a missing Chrome) are permanent. An action error is
    /// transient only when no input was sent and its code is transient, so a
    /// retry never repeats input that may have landed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Action { code, receipt, .. } => {
                receipt.dispatch == crate::DispatchState::NotSent && code.is_transient()
            }
            Self::Coded { code, .. } => code.is_transient(),
            Self::InvalidUrl(_)
            | Self::InvalidScreenshotOptions(_)
            | Self::Serialization(_)
            | Self::EvaluationTooLarge { .. }
            | Self::ChromeNotFound
            | Self::ValueMismatch { .. }
            | Self::RetryExhausted { .. } => false,
            _ => true,
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
    fn test_error_codes_compare_with_their_string_form() {
        let error = Error::coded(ErrorCode::HistoryUnavailable, "no entry");
        assert_eq!(error.code(), Some(ErrorCode::HistoryUnavailable));
        assert!(ErrorCode::HistoryUnavailable == "history_unavailable");
        assert_eq!(error.to_string(), "history_unavailable: no entry");
        assert_eq!(
            serde_json::to_value(ErrorCode::ValueMismatch).unwrap(),
            serde_json::json!("value_mismatch")
        );
    }

    #[test]
    fn test_permanent_errors_are_not_retryable() {
        assert!(!Error::InvalidUrl("x".into()).is_retryable());
        assert!(!Error::coded(ErrorCode::AmbiguousTarget, "x").is_retryable());
        assert!(!Error::ChromeNotFound.is_retryable());
        assert!(Error::Timeout("x".into()).is_retryable());
        assert!(Error::coded(ErrorCode::ActionabilityChanged, "x").is_retryable());
    }

    #[test]
    fn test_value_mismatch_hides_values() {
        let error = Error::ValueMismatch {
            selector: "#password".into(),
            expected_chars: 8,
            actual_chars: 6,
        };
        let text = error.to_string();
        assert!(text.contains("#password") && text.contains('8') && text.contains('6'));
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
