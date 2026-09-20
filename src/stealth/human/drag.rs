use std::{future::Future, pin::Pin, sync::Mutex, task::Poll, time::Duration};

use super::{bezier_curve, random_f64_range, random_range, Human};
use crate::error::{Error, Result};
use crate::page::DragInput;

pub(super) enum PendingRelease {
    Task(tokio::task::JoinHandle<Result<()>>),
    Finished(Result<()>),
}

pub(super) struct DragRelease<'a> {
    input: Option<DragInput>,
    pending: &'a Mutex<Vec<PendingRelease>>,
    pub(super) x: f64,
    pub(super) y: f64,
    armed: bool,
}

pub(super) fn validate_drag(x: f64, y: f64, dx: f64) -> Result<()> {
    if !x.is_finite()
        || !y.is_finite()
        || !dx.is_finite()
        || !(x + dx).is_finite()
        || x < 0.0
        || y < 0.0
        || x + dx < 0.0
    {
        return Err(Error::cdp_msg("Invalid drag coordinates"));
    }
    Ok(())
}

async fn release_drag(input: &mut DragInput, x: f64, y: f64) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(3), input.release(x, y))
        .await
        .map_err(|_| Error::cdp_msg("Mouse drag cleanup timed out"))?
}

impl DragRelease<'_> {
    pub(super) async fn move_to(&mut self, x: f64, y: f64) -> Result<()> {
        self.x = x;
        self.y = y;
        self.input
            .as_mut()
            .expect("owned drag input")
            .move_to(x, y)
            .await
    }

    pub(super) async fn press(&mut self) -> Result<()> {
        self.armed = true;
        self.input
            .as_mut()
            .expect("owned drag input")
            .press(self.x, self.y)
            .await
    }

    pub(super) async fn release(mut self) -> Result<()> {
        let result = release_drag(
            self.input.as_mut().expect("owned drag input"),
            self.x,
            self.y,
        )
        .await;
        self.armed = false;
        if result.is_err() {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(PendingRelease::Finished(Err(Error::cdp_msg(
                    "Mouse drag release failed",
                ))));
        }
        result
    }
}

impl Drop for DragRelease<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(mut input) = self.input.take() else {
            return;
        };
        let (x, y) = (self.x, self.y);
        let work = match tokio::runtime::Handle::try_current() {
            Ok(runtime) => PendingRelease::Task(
                runtime.spawn(async move { release_drag(&mut input, x, y).await }),
            ),
            Err(_) => PendingRelease::Finished(Err(Error::cdp_msg(
                "Mouse drag cleanup requires an active runtime",
            ))),
        };
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(work);
    }
}

async fn finish_releases(
    releases: &Mutex<Vec<PendingRelease>>,
    wait: &tokio::sync::Mutex<()>,
) -> Result<()> {
    let _wait = wait.lock().await;
    std::future::poll_fn(|cx| {
        let mut pending = releases.lock().unwrap_or_else(|e| e.into_inner());
        let mut waiting = false;
        for work in pending.iter_mut() {
            if let PendingRelease::Task(task) = work {
                match Pin::new(task).poll(cx) {
                    Poll::Pending => waiting = true,
                    Poll::Ready(result) => {
                        *work = PendingRelease::Finished(result.unwrap_or_else(|_| {
                            Err(Error::cdp_msg("Mouse drag cleanup task failed"))
                        }))
                    }
                }
            }
        }
        if waiting {
            return Poll::Pending;
        }
        let mut result = Ok(());
        for work in std::mem::take(&mut *pending) {
            if let PendingRelease::Finished(Err(error)) = work {
                result = Err(error);
            }
        }
        Poll::Ready(result)
    })
    .await
}

impl Human<'_> {
    /// Await bounded releases scheduled by cancelled drags on this helper.
    /// Keep the helper alive across cancellation, then call this before further input.
    /// Cancelling this wait retains its tasks and results for a subsequent call.
    /// Concurrent cleanup waits on this helper are serialized.
    pub async fn finish_drag_cleanup(&self) -> Result<()> {
        finish_releases(&self.pending_releases, &self.cleanup_wait).await
    }

    pub(super) async fn approach_drag(&self, x: f64, y: f64) -> Result<DragRelease<'_>> {
        self.finish_drag_cleanup().await?;
        let input = DragInput::acquire(self.session).await?;
        self.finish_drag_cleanup().await?;
        let mut release = DragRelease {
            input: Some(input),
            pending: &self.pending_releases,
            x,
            y,
            armed: false,
        };
        let start = (
            (x + random_f64_range(-300.0, 300.0)).max(0.0),
            (y + random_f64_range(-200.0, 200.0)).max(0.0),
        );
        let distance = ((x - start.0).powi(2) + (y - start.1).powi(2)).sqrt();
        let path = bezier_curve(start, (x, y), self.speed.mouse_points(distance));
        let (min, max) = self.speed.move_delay_ms();
        for (px, py) in path {
            release.move_to(px, py).await?;
            tokio::time::sleep(Duration::from_millis(random_range(min, max))).await;
        }
        release.x = x;
        release.y = y;
        Ok(release)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn cancelled_cleanup_wait_retains_completed_failures_and_pending_tasks() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let worker_gate = gate.clone();
        let pending = Mutex::new(vec![
            PendingRelease::Finished(Err(Error::cdp_msg("fixture release failure"))),
            PendingRelease::Task(tokio::spawn(async move {
                worker_gate.notified().await;
                Ok(())
            })),
        ]);
        let wait = tokio::sync::Mutex::new(());
        assert!(
            tokio::time::timeout(Duration::from_millis(10), finish_releases(&pending, &wait))
                .await
                .is_err()
        );
        assert_eq!(pending.lock().unwrap().len(), 2);
        gate.notify_one();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), finish_releases(&pending, &wait))
                .await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("fixture release failure")
        );
        assert!(pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn concurrent_cleanup_waits_both_complete() {
        let pending = Mutex::new(vec![PendingRelease::Task(tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(())
        }))]);
        let wait = tokio::sync::Mutex::new(());
        let (first, second) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(
                finish_releases(&pending, &wait),
                finish_releases(&pending, &wait)
            )
        })
        .await
        .unwrap();
        first.unwrap();
        second.unwrap();
    }
}
