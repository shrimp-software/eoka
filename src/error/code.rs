//! Stable machine-readable error codes.

use std::fmt;

/// Stable machine-readable code for failures eoka raises itself (as opposed
/// to errors Chrome reports through CDP). Carried by [`crate::Error::Coded`]
/// and [`crate::Error::Action`].
///
/// `#[non_exhaustive]`: match with a `_ =>` arm. Codes compare equal to their
/// snake_case string form, so `code == "history_unavailable"` works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorCode {
    /// The action deadline passed.
    ActionTimeout,
    /// The action failed for a reason without a more specific code.
    ActionFailed,
    /// Held input state was invalid or its cleanup failed.
    InputState,
    /// The target does not support the requested input action.
    UnsupportedInput,
    /// The value cannot be represented by the target control.
    InvalidInputValue,
    /// No element matched, or the element is not rendered.
    TargetUnavailable,
    /// A pinned element handle no longer refers to a live node.
    TargetStale,
    /// More than one element matched a strict locator.
    AmbiguousTarget,
    /// The target stopped being actionable between preparation and input.
    ActionabilityChanged,
    /// No history entry exists at the requested offset.
    HistoryUnavailable,
    /// The action needs an explicit expectation.
    ExpectationRequired,
    /// The expectation already held before dispatch, so it proves nothing.
    ExpectationAlreadySatisfied,
    /// The expectation is malformed.
    InvalidExpectation,
    /// The action timeout is outside the accepted range.
    InvalidTimeout,
    /// A caller-supplied argument is invalid.
    InvalidArgument,
    /// More than one page satisfied the expectation.
    AmbiguousDestination,
    /// Chrome reported a navigation failure.
    NavigationFailed,
    /// Chrome aborted the navigation (it became a download or was replaced).
    NavigationAborted,
    /// A filled control settled on a different value than requested.
    ValueMismatch,
    /// Another download watch owns the browser's download configuration.
    DownloadBusy,
    /// This browser connection cannot observe downloads.
    DownloadUnsupported,
    /// The browser or connection does not support the operation.
    Unsupported,
    /// The target is not in a state that allows the operation (for example
    /// not a ready, active top-level page).
    PreconditionFailed,
    /// Another owner already holds the resource (a recorder on this target).
    Busy,
    /// The handle's own state does not allow the operation.
    InvalidState,
    /// Cleanup was not acknowledged; the resource may still be held.
    CleanupUncertain,
}

impl ErrorCode {
    /// The stable snake_case form, e.g. `"history_unavailable"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ActionTimeout => "action_timeout",
            Self::ActionFailed => "action_failed",
            Self::InputState => "input_state",
            Self::UnsupportedInput => "unsupported_input",
            Self::InvalidInputValue => "invalid_input_value",
            Self::TargetUnavailable => "target_unavailable",
            Self::TargetStale => "target_stale",
            Self::AmbiguousTarget => "ambiguous_target",
            Self::ActionabilityChanged => "actionability_changed",
            Self::HistoryUnavailable => "history_unavailable",
            Self::ExpectationRequired => "expectation_required",
            Self::ExpectationAlreadySatisfied => "expectation_already_satisfied",
            Self::InvalidExpectation => "invalid_expectation",
            Self::InvalidTimeout => "invalid_timeout",
            Self::InvalidArgument => "invalid_argument",
            Self::AmbiguousDestination => "ambiguous_destination",
            Self::NavigationFailed => "navigation_failed",
            Self::NavigationAborted => "navigation_aborted",
            Self::ValueMismatch => "value_mismatch",
            Self::DownloadBusy => "download_busy",
            Self::DownloadUnsupported => "download_unsupported",
            Self::Unsupported => "unsupported",
            Self::PreconditionFailed => "precondition_failed",
            Self::Busy => "busy",
            Self::InvalidState => "invalid_state",
            Self::CleanupUncertain => "cleanup_uncertain",
        }
    }

    /// Whether retrying the same call unchanged can succeed. Usage errors
    /// (invalid arguments, ambiguous selectors, missing expectations) cannot.
    pub const fn is_transient(self) -> bool {
        matches!(
            self,
            Self::ActionTimeout
                | Self::TargetUnavailable
                | Self::ActionabilityChanged
                | Self::NavigationFailed
                | Self::NavigationAborted
                | Self::DownloadBusy
                | Self::Busy
                | Self::PreconditionFailed
                | Self::ActionFailed
        )
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<str> for ErrorCode {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for ErrorCode {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<ErrorCode> for &str {
    fn eq(&self, other: &ErrorCode) -> bool {
        *self == other.as_str()
    }
}

impl serde::Serialize for ErrorCode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
