//! Target-bound native Chrome MP4 recording with bounded sequential IO.

mod error;
mod handle;
mod options;
mod registry;
mod start;
mod state;

pub use error::RecordingError;
use handle::owned;
pub use handle::NativeRecording;
pub(crate) use handle::{cancel_owned_slot, cancel_slot};
pub use options::{NativeRecordingOptions, RecordingChunk};
pub(crate) use registry::{recording_for_target, RecordingSlot};
pub(crate) use state::RecordingState;
use state::{Lifecycle, Phase};

use std::time::Duration;
#[cfg(test)]
use {
    crate::cdp::{types::*, Transport},
    crate::Page,
    base64::Engine,
    handle::decode_chunk,
    std::future::Future,
    std::sync::{atomic::Ordering, Arc},
};

const CHUNK_BYTES: usize = 64 * 1024;
const COMMAND_LIMIT: Duration = Duration::from_secs(10);

#[cfg(test)]
mod tests;
