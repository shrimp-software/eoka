//! Deadline-bounded actions with explicit, observable completion conditions.
pub(crate) mod dispatch;
mod locator;
mod perform;
mod runner;
mod types;

pub(crate) use locator::is_visible;
pub use locator::Locator;
pub use types::*;

use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Default)]
pub(crate) struct ActionState {
    pub gate: Arc<Mutex<()>>,
    pub cleanup_failed: std::sync::atomic::AtomicBool,
}
