use crate::cdp::types::*;

use super::*;

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
    pub(super) fn params(&self) -> Result<PageStartScreenRecording, RecordingError> {
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
