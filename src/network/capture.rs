use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, oneshot, watch};

use super::RequestStore;
use crate::cdp::transport::CdpMessage;
use crate::error::{Error, Result};
use crate::page::{CapturedRequest, Page, ResponseBody};

/// Options for [`Page::capture_requests`](crate::Page::capture_requests).
#[derive(Debug, Clone)]
pub struct RequestCaptureOptions {
    /// Maximum retained requests, in-flight or complete; the oldest is evicted
    /// first. Defaults to 1000. Zero is treated as one.
    pub max_requests: usize,
}

impl Default for RequestCaptureOptions {
    fn default() -> Self {
        Self { max_requests: 1000 }
    }
}

/// Loss counters for a [`RequestCapture`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestCaptureStats {
    /// Requests evicted to stay within `max_requests`.
    pub evicted: u64,
    /// Requests removed because loading failed or was canceled.
    pub failed: u64,
    /// CDP events missed because the capture fell behind the transport.
    /// Nonzero means retained requests may be incomplete.
    pub lost_events: u64,
}

struct Shared {
    store: std::sync::Mutex<RequestStore>,
    changed: watch::Sender<u64>,
    lost: AtomicU64,
    closed: AtomicBool,
}

impl Shared {
    fn store(&self) -> std::sync::MutexGuard<'_, RequestStore> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Requests made by one page's top-level CDP session, captured from
/// `Network` events. Out-of-process iframes and workers have their own
/// sessions and are not included.
///
/// Starting a capture enables the page's CDP Network domain. [`stop`](Self::stop)
/// or Drop ends collection but leaves the domain enabled, because other
/// features (such as [`Page::set_extra_headers`](crate::Page::set_extra_headers))
/// rely on it and `Network.disable` discards their state.
/// [`Page::disable_request_capture`](crate::Page::disable_request_capture)
/// turns it off explicitly.
pub struct RequestCapture {
    shared: Arc<Shared>,
    page: Page,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<tokio::task::JoinHandle<()>>,
}

impl RequestCapture {
    pub(crate) async fn start(page: &Page, options: RequestCaptureOptions) -> Result<Self> {
        let session = page.session();
        let session_id = session.session_id().to_owned();
        // Subscribe before enabling so no early event is missed.
        let mut events = session.transport().subscribe();
        session.network_enable().await?;
        let (changed, _) = watch::channel(0);
        let shared = Arc::new(Shared {
            store: std::sync::Mutex::new(RequestStore::new(options.max_requests)),
            changed,
            lost: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        });
        let (stop, mut stopped) = oneshot::channel::<()>();
        let worker_shared = shared.clone();
        let worker = tokio::spawn(async move {
            let pump = async {
                loop {
                    match events.recv().await.as_deref() {
                        Ok(CdpMessage::Event {
                            method,
                            params,
                            session_id: Some(source),
                        }) if *source == session_id && method.starts_with("Network.") => {
                            if worker_shared.store().apply(method, params).is_some() {
                                worker_shared
                                    .changed
                                    .send_modify(|version| *version = version.wrapping_add(1));
                            }
                        }
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            worker_shared.lost.fetch_add(*skipped, Ordering::Relaxed);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            };
            let _ = futures_util::future::select(std::pin::pin!(pump), &mut stopped).await;
            worker_shared.closed.store(true, Ordering::Release);
            worker_shared
                .changed
                .send_modify(|version| *version = version.wrapping_add(1));
        });
        Ok(Self {
            shared,
            page: page.clone(),
            stop: Some(stop),
            worker: Some(worker),
        })
    }

    /// Retained requests, oldest first, without removing them.
    pub fn requests(&self) -> Vec<CapturedRequest> {
        self.shared.store().all()
    }

    /// A retained request by its CDP request ID.
    pub fn get(&self, request_id: &str) -> Option<CapturedRequest> {
        self.shared.store().get(request_id)
    }

    /// Remove and return requests that finished loading, oldest first.
    /// In-flight requests stay and keep receiving updates.
    pub fn drain_completed(&self) -> Vec<CapturedRequest> {
        self.shared.store().drain_completed()
    }

    /// Loss counters since the capture started.
    pub fn stats(&self) -> RequestCaptureStats {
        let store = self.shared.store();
        RequestCaptureStats {
            evicted: store.evicted,
            failed: store.failed,
            lost_events: self.shared.lost.load(Ordering::Relaxed),
        }
    }

    /// Wait until a retained request satisfies `predicate`, such as
    /// `|r| r.url.ends_with("/api") && r.complete`, and return it.
    pub async fn wait_for(
        &self,
        predicate: impl Fn(&CapturedRequest) -> bool,
        timeout: Duration,
    ) -> Result<CapturedRequest> {
        let mut changes = self.shared.changed.subscribe();
        tokio::time::timeout(timeout, async {
            loop {
                if let Some(found) = self.shared.store().find(&predicate) {
                    return Ok(found);
                }
                if self.shared.closed.load(Ordering::Acquire) || changes.changed().await.is_err() {
                    return Err(Error::transport("Request capture stopped"));
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout("No captured request matched before the deadline".into()))?
    }

    /// Body of a captured request, available once it is `complete` and while
    /// capture is active. Chrome evicts bodies under memory pressure.
    pub async fn response_body(&self, request_id: &str) -> Result<ResponseBody> {
        self.page.get_response_body(request_id).await
    }

    /// Stop collecting and return retained requests, oldest first. The Network
    /// domain stays enabled, so their bodies remain readable through
    /// [`Page::get_response_body`](crate::Page::get_response_body).
    pub async fn stop(mut self) -> Vec<CapturedRequest> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.await;
        }
        self.requests()
    }
}

impl Drop for RequestCapture {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

impl std::fmt::Debug for RequestCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestCapture")
            .field("retained", &self.shared.store().requests.len())
            .field("stats", &self.stats())
            .finish()
    }
}
