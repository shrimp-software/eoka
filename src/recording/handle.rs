use std::future::Future;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::Duration;

use base64::Engine;
use tokio::sync::oneshot;

use crate::cdp::types::*;
use crate::cdp::Transport;
use crate::Error;

use super::*;

/// Owned target-bound recorder. Stop, read to EOF, then close to release Chrome's
/// temporary stream storage. Dropping schedules bounded best-effort cancellation.
/// No file policy, transcoding, fallback engine, or whole-video buffering is used.
pub struct NativeRecording {
    pub(super) state: Arc<RecordingState>,
    pub(super) slot: RecordingSlot,
    // Keep the connection available for explicit and drop cleanup, not Chrome ownership.
    pub(super) transport: Arc<Transport>,
}

struct ReadDeliveryGuard<'a> {
    recording: &'a NativeRecording,
    armed: bool,
}

impl Drop for ReadDeliveryGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.recording
                .state
                .loss
                .send_replace(Some(RecordingError::InvalidState(
                    "read waiter cancelled before receiving its stream chunk".into(),
                )));
            schedule_cleanup(
                &self.recording.state,
                &self.recording.slot,
                &self.recording.transport,
            );
        }
        self.recording
            .state
            .read_in_flight
            .store(false, Ordering::Release);
    }
}

fn schedule_cleanup(state: &Arc<RecordingState>, slot: &RecordingSlot, transport: &Arc<Transport>) {
    if state.finished.load(Ordering::Acquire) {
        return;
    }
    state.cleanup_requested.store(true, Ordering::Release);
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        let state = Arc::clone(state);
        let slot = Arc::clone(slot);
        let transport = Arc::clone(transport);
        runtime.spawn(async move {
            let mut lifecycle = state.lifecycle.lock().await;
            if let Err(error) = state.cleanup_locked(&mut lifecycle, &slot).await {
                tracing::warn!("Dropped recording operation cleanup: {error}");
            }
            drop(transport);
        });
    }
}

impl NativeRecording {
    /// Stop capture exactly once and make the native MP4 stream readable.
    /// Cancellation of this waiter does not cancel the owned native transition.
    pub async fn stop(&self) -> Result<(), RecordingError> {
        let state = Arc::clone(&self.state);
        owned(async move {
            let mut lifecycle = state.lifecycle.lock().await;
            state.check_operable()?;
            state.stop_locked(&mut lifecycle).await?;
            state.check_loss()
        })
        .await
    }

    /// Read at most 64 KiB. Call only after stop; EOF remains explicit on repeat.
    /// Overlapping reads are rejected until the preceding chunk is delivered.
    /// A cancelled read waiter cancels the recorder: consumed bytes cannot safely
    /// be recovered without buffering, so a later read cannot silently skip them.
    pub async fn read_chunk(&self) -> Result<RecordingChunk, RecordingError> {
        self.state.check_operable()?;
        self.state
            .read_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                RecordingError::InvalidState(
                    "another read is awaiting delivery of its stream chunk".into(),
                )
            })?;
        let state = Arc::clone(&self.state);
        let slot = Arc::clone(&self.slot);
        let transport = Arc::clone(&self.transport);
        let mut delivery = ReadDeliveryGuard {
            recording: self,
            armed: true,
        };
        let (sent, received) = oneshot::channel();
        tokio::spawn(async move {
            let mut lifecycle = state.lifecycle.lock().await;
            let result = async {
                state.check_operable()?;
                match lifecycle.phase {
                    Phase::Eof => {
                        return Ok(RecordingChunk {
                            data: Vec::new(),
                            eof: true,
                        })
                    }
                    Phase::Streaming => {}
                    _ => {
                        return Err(RecordingError::InvalidState(
                            "stop must succeed before reading".into(),
                        ))
                    }
                }
                let stream = lifecycle
                    .streams
                    .iter()
                    .rev()
                    .find_map(Option::as_deref)
                    .ok_or_else(|| RecordingError::InvalidState("native stream missing".into()))?;
                let response: IoReadResult = state
                    .command(
                        "IO.read",
                        &IoRead {
                            handle: stream,
                            size: CHUNK_BYTES as u32,
                        },
                    )
                    .await?;
                state.check_loss()?;
                let chunk = decode_chunk(response)?;
                if chunk.eof {
                    lifecycle.phase = Phase::Eof
                }
                Ok(chunk)
            }
            .await;
            let failed = result.is_err()
                && !matches!(
                    lifecycle.phase,
                    Phase::Running | Phase::Starting | Phase::Closed
                );
            if sent.send(result).is_err() || failed {
                state.cleanup_requested.store(true, Ordering::Release);
                let _ = state.cleanup_locked(&mut lifecycle, &slot).await;
            }
            drop(transport);
        });
        let result = received.await.map_err(|_| {
            RecordingError::CleanupUncertain("Owned read worker ended without a result".into())
        })?;
        delivery.armed = false;
        result
    }

    /// Stop if necessary, close native stream storage, and release the target.
    /// Cleanup is bounded; failed acknowledgements retain the target reservation.
    pub async fn close(&self) -> Result<(), RecordingError> {
        let state = Arc::clone(&self.state);
        let slot = Arc::clone(&self.slot);
        let transport = Arc::clone(&self.transport);
        state.cleanup_requested.store(true, Ordering::Release);
        bounded_cleanup(async move {
            let mut lifecycle = state.lifecycle.lock().await;
            let result = state.cleanup_locked(&mut lifecycle, &slot).await;
            drop(transport);
            result
        })
        .await
    }

    /// Discard capture with the same truthful, bounded cleanup as close.
    pub async fn cancel(&self) -> Result<(), RecordingError> {
        self.close().await
    }

    /// Wait for target/session loss or connection failure while capture is running.
    pub async fn wait_for_failure(&self) -> RecordingError {
        let mut loss = self.state.loss.subscribe();
        loop {
            if let Some(error) = loss.borrow_and_update().clone() {
                return error;
            }
            if loss.changed().await.is_err() {
                return Error::transport("Recording health observer closed").into();
            }
        }
    }
}

impl Drop for NativeRecording {
    fn drop(&mut self) {
        schedule_cleanup(&self.state, &self.slot, &self.transport);
    }
}

pub(super) async fn owned<T: Send + 'static>(
    operation: impl Future<Output = Result<T, RecordingError>> + Send + 'static,
) -> Result<T, RecordingError> {
    tokio::spawn(operation).await.map_err(|_| {
        RecordingError::CleanupUncertain("Owned recording transition ended without a result".into())
    })?
}

pub(crate) async fn cancel_slot(slot: &RecordingSlot) -> Result<(), RecordingError> {
    let state = slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(state) = state else { return Ok(()) };
    cancel_state(state, Arc::clone(slot)).await
}

pub(crate) async fn cancel_owned_slot(
    slot: &RecordingSlot,
    transport: &Arc<Transport>,
    session_id: Option<&str>,
) -> Result<(), RecordingError> {
    let state = slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(state) = state else { return Ok(()) };
    if !Weak::ptr_eq(&state.transport, &Arc::downgrade(transport))
        || session_id.is_some_and(|session_id| state.session_id != session_id)
    {
        return Ok(());
    }
    cancel_state(state, Arc::clone(slot)).await
}

async fn cancel_state(
    state: Arc<RecordingState>,
    slot: RecordingSlot,
) -> Result<(), RecordingError> {
    state.cleanup_requested.store(true, Ordering::Release);
    bounded_cleanup(async move {
        let mut lifecycle = state.lifecycle.lock().await;
        state.cleanup_locked(&mut lifecycle, &slot).await
    })
    .await
}

async fn bounded_cleanup(
    operation: impl Future<Output = Result<(), RecordingError>> + Send + 'static,
) -> Result<(), RecordingError> {
    tokio::time::timeout(Duration::from_secs(40), owned(operation))
        .await
        .map_err(|_| {
            RecordingError::CleanupUncertain(
                "Cleanup acknowledgement exceeded 40 seconds; owned cleanup remains scheduled"
                    .into(),
            )
        })?
}

pub(super) fn decode_chunk(response: IoReadResult) -> Result<RecordingChunk, RecordingError> {
    let data = if response.base64_encoded {
        // Reject an oversized protocol chunk before decoded allocation.
        if response.data.len() > CHUNK_BYTES.div_ceil(3) * 4 {
            return Err(Error::Decode("IO.read exceeded the requested 64 KiB bound".into()).into());
        }
        base64::engine::general_purpose::STANDARD
            .decode(response.data)
            .map_err(|error| Error::Decode(format!("Recording IO.read base64: {error}")))?
    } else {
        response.data.into_bytes()
    };
    if data.len() > CHUNK_BYTES {
        return Err(Error::Decode("IO.read exceeded the requested 64 KiB bound".into()).into());
    }
    Ok(RecordingChunk {
        data,
        eof: response.eof,
    })
}
