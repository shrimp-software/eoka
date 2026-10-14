//! Target-bound native Chrome MP4 recording with bounded sequential IO.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Duration;

use base64::Engine;
use tokio::sync::{oneshot, watch};

use crate::cdp::transport::CdpMessage;
use crate::cdp::types::*;
use crate::cdp::Transport;
use crate::{Error, Page};

const CHUNK_BYTES: usize = 64 * 1024;
const COMMAND_LIMIT: Duration = Duration::from_secs(10);

/// Options for Chrome's experimental native screen recorder (without audio).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NativeRecordingOptions {
    /// Maximum frame width, in pixels; Chrome chooses its default when omitted.
    pub max_width: Option<u32>,
    /// Maximum frame height, in pixels; Chrome chooses its default when omitted.
    pub max_height: Option<u32>,
    /// Integral frame rate in the range 1..=120; Chrome defaults to 30.
    pub fps: Option<f64>,
}

impl NativeRecordingOptions {
    fn params(&self) -> Result<PageStartScreenRecording, RecordingError> {
        if [self.max_width, self.max_height]
            .into_iter()
            .flatten()
            .any(|value| value == 0 || value > i32::MAX as u32)
        {
            return Err(RecordingError::InvalidOptions(
                "max_width and max_height must be positive CDP integers".into(),
            ));
        }
        if self.fps.is_some_and(|fps| {
            !fps.is_finite() || !(1.0..=120.0).contains(&fps) || fps.fract() != 0.0
        }) {
            return Err(RecordingError::InvalidOptions(
                "fps must be an integer in 1..=120 (native frameRate is an integer)".into(),
            ));
        }
        Ok(PageStartScreenRecording {
            audio: false,
            max_width: self.max_width,
            max_height: self.max_height,
            frame_rate: self.fps.map(|fps| fps as u32),
        })
    }
}

/// A bounded decoded stream chunk. The final chunk may contain bytes and EOF.
#[derive(Debug, PartialEq, Eq)]
pub struct RecordingChunk {
    /// At most 64 KiB; never a whole-video allocation.
    pub data: Vec<u8>,
    /// True only when Chrome reports end of stream.
    pub eof: bool,
}

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

pub(crate) type RecordingSlot = Arc<Mutex<Option<Arc<RecordingState>>>>;
type RecordingRegistry = HashMap<String, HashMap<String, Weak<Mutex<Option<Arc<RecordingState>>>>>>;

pub(crate) fn recording_for_target(endpoint: Option<&str>, target_id: &str) -> RecordingSlot {
    static REGISTRY: LazyLock<Mutex<RecordingRegistry>> = LazyLock::new(Mutex::default);
    let Some(endpoint) = endpoint else {
        return Arc::default();
    };
    let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(slot) = registry
        .get(endpoint)
        .and_then(|targets| targets.get(target_id))
        .and_then(Weak::upgrade)
    {
        return slot;
    }
    registry.retain(|_, targets| {
        targets.retain(|_, slot| slot.strong_count() > 0);
        !targets.is_empty()
    });
    let slot = Arc::default();
    registry
        .entry(endpoint.into())
        .or_default()
        .insert(target_id.into(), Arc::downgrade(&slot));
    slot
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Starting,
    Running,
    Streaming,
    Eof,
    Broken,
    Closed,
}

struct Lifecycle {
    phase: Phase,
    streams: Vec<String>,
    start_dispatched: bool,
    stop_attempted: bool,
    stopped: bool,
    failure: Option<RecordingError>,
}

impl Lifecycle {
    fn new() -> Self {
        Self {
            phase: Phase::Starting,
            streams: Vec::new(),
            start_dispatched: false,
            stop_attempted: false,
            stopped: false,
            failure: None,
        }
    }

    fn begin_stop(&mut self) -> Result<bool, RecordingError> {
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

    fn add_stream(&mut self, stream: String) {
        if !self.streams.contains(&stream) {
            self.streams.push(stream);
        }
    }
}

pub(crate) struct RecordingState {
    transport: Weak<Transport>,
    session_id: String,
    target_id: String,
    lifecycle: tokio::sync::Mutex<Lifecycle>,
    loss: watch::Sender<Option<RecordingError>>,
    finished: AtomicBool,
    cleanup_requested: AtomicBool,
    read_in_flight: AtomicBool,
}

impl RecordingState {
    async fn command<C, R>(&self, method: &str, params: &C) -> Result<R, RecordingError>
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

    fn check_loss(&self) -> Result<(), RecordingError> {
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

    fn check_operable(&self) -> Result<(), RecordingError> {
        self.check_loss()?;
        if self.cleanup_requested.load(Ordering::Acquire) {
            return Err(RecordingError::InvalidState(
                "recording cleanup has been requested".into(),
            ));
        }
        Ok(())
    }

    fn monitor(state: &Arc<Self>, transport: &Arc<Transport>) {
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
                    match event {
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

    async fn stop_locked(&self, lifecycle: &mut Lifecycle) -> Result<(), RecordingError> {
        if !lifecycle.begin_stop()? {
            return Ok(());
        }
        let result: Result<ScreenRecordingResult, _> =
            self.command("Page.stopScreenRecording", &Empty {}).await;
        match result {
            Ok(result) => {
                lifecycle.add_stream(result.stream);
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

    async fn cleanup_locked(
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
        let mut index = 0;
        while index < lifecycle.streams.len() {
            let closed: Result<serde_json::Value, _> = self
                .command(
                    "IO.close",
                    &IoClose {
                        handle: &lifecycle.streams[index],
                    },
                )
                .await;
            if let Err(error) = closed {
                failure = Some(RecordingError::CleanupUncertain(match failure {
                    Some(previous) => format!("{previous}; IO.close: {error}"),
                    None => format!("IO.close: {error}"),
                }));
                index += 1;
            } else {
                lifecycle.streams.remove(index);
            }
        }
        if let Some(error) = failure {
            lifecycle.phase = Phase::Broken;
            lifecycle.failure = Some(error.clone());
            return Err(error);
        }
        lifecycle.phase = Phase::Closed;
        self.finished.store(true, Ordering::Release);
        let mut active = slot.lock().unwrap_or_else(|e| e.into_inner());
        if active
            .as_ref()
            .is_some_and(|state| std::ptr::eq(state.as_ref(), self))
        {
            *active = None;
        }
        Ok(())
    }
}

/// Owned target-bound recorder. Stop, read to EOF, then close to release Chrome's
/// temporary stream storage. Dropping schedules bounded best-effort cancellation.
/// No file policy, transcoding, fallback engine, or whole-video buffering is used.
pub struct NativeRecording {
    state: Arc<RecordingState>,
    slot: RecordingSlot,
    // Keep the connection available for explicit and drop cleanup, not Chrome ownership.
    transport: Arc<Transport>,
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
                    .last()
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

async fn owned<T: Send + 'static>(
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

fn decode_chunk(response: IoReadResult) -> Result<RecordingChunk, RecordingError> {
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

fn classify_start(error: Error) -> RecordingError {
    if let Error::Cdp { code, message, .. } = &error {
        let text = message.to_ascii_lowercase();
        if *code == Some(-32601) || text.contains("not supported") || text.contains("wasn't found")
        {
            return RecordingError::Unsupported(message.clone());
        }
        if text.contains("already active") {
            return RecordingError::AlreadyActive;
        }
        if text.contains("inactive")
            || text.contains("top-level")
            || text.contains("top target")
            || text.contains("not attached")
        {
            return RecordingError::Precondition(message.clone());
        }
    }
    RecordingError::Core(error)
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
                lifecycle.phase = Phase::Closed;
                state.finished.store(true, Ordering::Release);
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
                return Err(error);
            }
            lifecycle.start_dispatched = true;
            let started: Result<ScreenRecordingResult, _> =
                state.command("Page.startScreenRecording", &params).await;
            match started {
                Ok(result) => {
                    lifecycle.add_stream(result.stream);
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
                    lifecycle.phase = Phase::Closed;
                    state.finished.store(true, Ordering::Release);
                    *slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    Err(classify_start(error))
                }
                Err(error) => {
                    lifecycle.failure = Some(error.clone());
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

#[cfg(test)]
mod tests;
