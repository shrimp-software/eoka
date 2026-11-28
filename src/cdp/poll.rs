//! Fixed-interval polling with an optional deadline.
//!
//! Each helper runs `check` first, then gives up once the deadline has
//! passed, then sleeps for `interval`, so a final check always runs before a
//! timeout is reported.

use std::time::{Duration, Instant};

/// Poll `check` on the current thread until it returns `Some`, or return
/// `None` once `deadline` has passed. `None` as deadline polls forever.
pub(crate) fn poll_blocking<T>(
    deadline: Option<Instant>,
    interval: Duration,
    mut check: impl FnMut() -> Option<T>,
) -> Option<T> {
    loop {
        if let Some(value) = check() {
            return Some(value);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return None;
        }
        std::thread::sleep(interval);
    }
}

/// Async counterpart of [`poll_blocking`] on Tokio's clock, so paused test
/// time applies.
pub(crate) async fn poll_async<T>(
    deadline: Option<tokio::time::Instant>,
    interval: Duration,
    mut check: impl FnMut() -> Option<T>,
) -> Option<T> {
    loop {
        if let Some(value) = check() {
            return Some(value);
        }
        if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            return None;
        }
        tokio::time::sleep(interval).await;
    }
}
