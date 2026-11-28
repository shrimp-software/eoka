use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use tokio::sync::watch;

use crate::cdp::transport::CdpMessage;
use crate::cdp::types::*;
use crate::cdp::Transport;
use crate::Error;

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Starting,
    Running,
    Streaming,
    Eof,
    Broken,
    Closed,
}

pub(super) struct Lifecycle {
    pub(super) phase: Phase,
    // Start handle, then a distinct stop handle; clear only after IO.close succeeds.
    pub(super) streams: [Option<String>; 2],
    pub(super) start_dispatched: bool,
    pub(super) stop_attempted: bool,
    pub(super) stopped: bool,
    pub(super) failure: Option<RecordingError>,
}

impl Lifecycle {
    pub(super) fn new() -> Self {
        Self {
            phase: Phase::Starting,
            streams: [None, None],
            start_dispatched: false,
            stop_attempted: false,
            stopped: false,
            failure: None,
        }
    }

    pub(super) fn begin_stop(&mut self) -> Result<bool, RecordingError> {
        match self.phase {
            Phase::Streaming | Phase::Eof => Ok(false),
            Phase::Closed => Err(RecordingError::InvalidState("recording is closed".into())),
            _ if self.stop_attempted => Err(self.failure.clone().unwrap_or_else(|| {
                RecordingError::CleanupUncertain("stop completion was not acknowledged".into())
            })),
            _ => {
                self.stop_attempted = true;
                Ok(true)
            }
        }
    }
}

pub(crate) struct RecordingState {
    pub(super) transport: Weak<Transport>,
    pub(super) session_id: String,
    pub(super) target_id: String,
    pub(super) lifecycle: tokio::sync::Mutex<Lifecycle>,
    pub(super) loss: watch::Sender<Option<RecordingError>>,
    pub(super) finished: AtomicBool,
    pub(super) cleanup_requested: AtomicBool,
    pub(super) read_in_flight: AtomicBool,
}

impl RecordingState {
    pub(super) async fn command<C, R>(&self, method: &str, params: &C) -> Result<R, RecordingError>
    where
        C: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        let transport = self
            .transport
            .upgrade()
            .ok_or_else(|| Error::transport("Recording connection dropped"))?;
        tokio::time::timeout(
            COMMAND_LIMIT,
            transport.send_to_session(&self.session_id, method, params),
        )
        .await
        .map_err(|_| Error::Timeout(format!("{method} did not complete within 10 seconds")))?
        .map_err(RecordingError::Core)
    }

    pub(super) fn check_loss(&self) -> Result<(), RecordingError> {
        if let Some(error) = self.loss.borrow().as_ref() {
            return Err(error.clone());
        }
        if self
            .transport
            .upgrade()
            .is_none_or(|transport| transport.is_closed())
        {
            return Err(Error::transport("Recording connection disconnected").into());
        }
        Ok(())
    }

    pub(super) fn check_operable(&self) -> Result<(), RecordingError> {
        self.check_loss()?;
        if self.cleanup_requested.load(Ordering::Acquire) {
            return Err(RecordingError::InvalidState(
                "recording cleanup has been requested".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn monitor(state: &Arc<Self>, transport: &Arc<Transport>) {
        let mut events = transport.subscribe();
        let weak = Arc::downgrade(state);
        tokio::spawn(async move {
            loop {
                let event = tokio::time::timeout(Duration::from_millis(100), events.recv()).await;
                let Some(state) = weak.upgrade() else { break };
                if state.finished.load(Ordering::Acquire) {
                    break;
                }
                let error = if state
                    .transport
                    .upgrade()
                    .is_none_or(|transport| transport.is_closed())
                {
                    Some(RecordingError::Core(Error::transport(
                        "Recording connection disconnected",
                    )))
                } else {
                    match event.as_ref().map(|event| event.as_deref()) {
                        Ok(Ok(CdpMessage::Event {
                            method,
                            params,
                            session_id,
                        })) if (method == "Target.targetDestroyed"
                            && params["targetId"] == state.target_id)
                            || (method == "Target.detachedFromTarget"
                                && params["sessionId"] == state.session_id)
                            || (matches!(
                                method.as_str(),
                                "Inspector.detached" | "Inspector.targetCrashed"
                            ) && session_id.as_deref() == Some(state.session_id.as_str())) =>
                        {
                            Some(RecordingError::Precondition(format!(
                                "recording target lost: {method}"
                            )))
                        }
                        Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                            Some(RecordingError::CleanupUncertain(
                                "Recording target event observation lagged".into(),
                            ))
                        }
                        _ => None,
                    }
                };
                if let Some(error) = error {
                    state.loss.send_replace(Some(error));
                    break;
                }
            }
        });
    }

    pub(super) fn finish_locked(&self, lifecycle: &mut Lifecycle, slot: &RecordingSlot) {
        lifecycle.phase = Phase::Closed;
        self.finished.store(true, Ordering::Release);
        let mut active = slot.lock().unwrap_or_else(|e| e.into_inner());
        if active
            .as_ref()
            .is_some_and(|state| std::ptr::eq(state.as_ref(), self))
        {
            *active = None;
        }
    }

    pub(super) async fn stop_locked(
        &self,
        lifecycle: &mut Lifecycle,
    ) -> Result<(), RecordingError> {
        if !lifecycle.begin_stop()? {
            return Ok(());
        }
        let result: Result<ScreenRecordingResult, _> =
            self.command("Page.stopScreenRecording", &Empty {}).await;
        match result {
            Ok(result) => {
                if lifecycle.streams[0].as_ref() != Some(&result.stream) {
                    lifecycle.streams[1] = Some(result.stream);
                }
                lifecycle.stopped = true;
                lifecycle.phase = Phase::Streaming;
                Ok(())
            }
            Err(error) => {
                lifecycle.phase = Phase::Broken;
                let error = RecordingError::CleanupUncertain(format!(
                    "stop was dispatched exactly once: {error}"
                ));
                lifecycle.failure = Some(error.clone());
                Err(error)
            }
        }
    }

    pub(super) async fn cleanup_locked(
        &self,
        lifecycle: &mut Lifecycle,
        slot: &RecordingSlot,
    ) -> Result<(), RecordingError> {
        if lifecycle.phase == Phase::Closed {
            return Ok(());
        }
        let mut failure = None;
        if !lifecycle.start_dispatched {
            lifecycle.stopped = true;
        } else if !lifecycle.stopped {
            if let Err(error) = self.stop_locked(lifecycle).await {
                failure = Some(error)
            }
        }
        // Both start and stop declare a stream. Close every distinct handle even
        // when stop failed, without ever reissuing a potentially dispatched stop.
        for stream in &mut lifecycle.streams {
            let Some(handle) = stream.as_deref() else {
                continue;
            };
            let closed: Result<serde_json::Value, _> =
                self.command("IO.close", &IoClose { handle }).await;
            if let Err(error) = closed {
                failure = Some(RecordingError::CleanupUncertain(match failure {
                    Some(previous) => format!("{previous}; IO.close: {error}"),
                    None => format!("IO.close: {error}"),
                }));
            } else {
                *stream = None;
            }
        }
        if let Some(error) = failure {
            lifecycle.phase = Phase::Broken;
            lifecycle.failure = Some(error.clone());
            return Err(error);
        }
        self.finish_locked(lifecycle, slot);
        Ok(())
    }
}
