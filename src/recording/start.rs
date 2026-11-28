use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tokio::sync::watch;

use crate::cdp::types::*;
use crate::{Error, Page};

use super::*;

fn classify_start(error: Error) -> RecordingError {
    use crate::error::classify::{recording_start_failure, RecordingStartFailure};
    let Error::Cdp { message, .. } = &error else {
        return RecordingError::Core(error);
    };
    match recording_start_failure(&error) {
        Some(RecordingStartFailure::Unsupported) => RecordingError::Unsupported(message.clone()),
        Some(RecordingStartFailure::AlreadyActive) => RecordingError::AlreadyActive,
        Some(RecordingStartFailure::Precondition) => RecordingError::Precondition(message.clone()),
        None => RecordingError::Core(error),
    }
}

impl Page {
    /// Start Chrome's native MP4 recorder on a ready, active top-level target.
    /// Options are validated before dispatch. This never falls back to screencast
    /// frames or an external encoder. A cancelled start still owns its cleanup.
    pub async fn start_recording(
        &self,
        options: NativeRecordingOptions,
    ) -> Result<NativeRecording, RecordingError> {
        let params = options.params()?;
        let slot = self.session.recording_slot();
        let (state, transport) = {
            let mut active = slot.lock().unwrap_or_else(|e| e.into_inner());
            if active.is_some() {
                return Err(RecordingError::AlreadyActive);
            }
            let transport = Arc::clone(self.session.transport());
            let (loss, _) = watch::channel(None);
            let state = Arc::new(RecordingState {
                transport: Arc::downgrade(&transport),
                session_id: self.session.session_id().into(),
                target_id: self.session.target_id().into(),
                lifecycle: tokio::sync::Mutex::new(Lifecycle::new()),
                loss,
                finished: AtomicBool::new(false),
                cleanup_requested: AtomicBool::new(false),
                read_in_flight: AtomicBool::new(false),
            });
            *active = Some(Arc::clone(&state));
            (state, transport)
        };
        RecordingState::monitor(&state, &transport);
        owned(async move {
            let mut lifecycle = state.lifecycle.lock().await;
            if lifecycle.phase != Phase::Starting {
                return Err(RecordingError::InvalidState(
                    "start was cancelled before native dispatch".into(),
                ));
            }
            let ready: Result<RuntimeEvaluateResult, _> = state
                .command(
                    "Runtime.evaluate",
                    &RuntimeEvaluate {
                        expression:
                            "({ready:document.readyState==='complete',top:window===window.top})"
                                .into(),
                        object_group: None,
                        return_by_value: Some(true),
                        await_promise: Some(false),
                    },
                )
                .await;
            let preflight = ready.and_then(|ready| {
                if ready.exception_details.is_some()
                    || ready
                        .result
                        .value
                        .as_ref()
                        .is_none_or(|value| value["ready"] != true || value["top"] != true)
                {
                    Err(RecordingError::Precondition(
                        "document must be complete and top-level".into(),
                    ))
                } else {
                    state.check_operable()
                }
            });
            if let Err(error) = preflight {
                state.finish_locked(&mut lifecycle, &slot);
                return Err(error);
            }
            lifecycle.start_dispatched = true;
            let started: Result<ScreenRecordingResult, _> =
                state.command("Page.startScreenRecording", &params).await;
            match started {
                Ok(result) => {
                    lifecycle.streams[0] = Some(result.stream);
                    lifecycle.phase = Phase::Running;
                    if let Err(error) = state.check_operable() {
                        let cleanup = state.cleanup_locked(&mut lifecycle, &slot).await;
                        return Err(match cleanup {
                            Ok(()) => error,
                            Err(cleanup) => RecordingError::CleanupUncertain(format!(
                                "start: {error}; {cleanup}"
                            )),
                        });
                    }
                    drop(lifecycle);
                    Ok(NativeRecording {
                        state,
                        slot,
                        transport,
                    })
                }
                Err(RecordingError::Core(error @ Error::Cdp { .. })) => {
                    // Chromium resets media_recorder_ on every failed Start result.
                    state.finish_locked(&mut lifecycle, &slot);
                    Err(classify_start(error))
                }
                Err(error) => {
                    let cleanup = state.cleanup_locked(&mut lifecycle, &slot).await;
                    Err(match cleanup {
                        Ok(()) => error,
                        Err(cleanup) => {
                            RecordingError::CleanupUncertain(format!("start: {error}; {cleanup}"))
                        }
                    })
                }
            }
        })
        .await
    }
}
