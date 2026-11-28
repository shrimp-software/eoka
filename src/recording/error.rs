use crate::Error;

/// Native recording failures, including uncertain native cleanup.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum RecordingError {
    /// This Chrome build does not support native recording.
    #[error("Native recording unsupported: {0}")]
    Unsupported(String),
    /// The target is not a ready, active top-level page, or was lost.
    #[error("Recording precondition failed: {0}")]
    Precondition(String),
    /// This target already has an owned recorder or uncertain cleanup.
    #[error("A native recording is already active on this target")]
    AlreadyActive,
    /// Options cannot be represented by native CDP recording parameters.
    #[error("Invalid recording options: {0}")]
    InvalidOptions(String),
    /// The requested operation is invalid for the handle's current state.
    #[error("Invalid recording state: {0}")]
    InvalidState(String),
    /// Cleanup was not acknowledged; the target reservation remains held.
    #[error("Native recording cleanup uncertain: {0}")]
    CleanupUncertain(String),
    /// An underlying protocol, transport or decode failure.
    #[error(transparent)]
    Core(#[from] Error),
}

impl From<RecordingError> for Error {
    /// Folds a recording failure into the crate error so `?` composes.
    /// Protocol failures pass through; the rest become [`Error::Coded`].
    fn from(error: RecordingError) -> Self {
        use crate::ErrorCode;
        let code = match &error {
            RecordingError::Core(inner) => return inner.clone(),
            RecordingError::Unsupported(_) => ErrorCode::Unsupported,
            RecordingError::Precondition(_) => ErrorCode::PreconditionFailed,
            RecordingError::AlreadyActive => ErrorCode::Busy,
            RecordingError::InvalidOptions(_) => ErrorCode::InvalidArgument,
            RecordingError::InvalidState(_) => ErrorCode::InvalidState,
            RecordingError::CleanupUncertain(_) => ErrorCode::CleanupUncertain,
        };
        Error::coded(code, error.to_string())
    }
}
