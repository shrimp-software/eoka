//! Network Request Capture
//!
//! [`RequestCapture`] (from [`Page::capture_requests`](crate::Page::capture_requests))
//! records one page's HTTP requests and responses. [`NetworkWatcher`] is the
//! lower-level engine for callers that feed it CDP events themselves.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex};

use crate::cdp::transport::CdpMessage;
use crate::cdp::types::{
    NetworkLoadingFailedEvent, NetworkLoadingFinishedEvent, NetworkRequestWillBeSentEvent,
    NetworkResponseReceivedEvent,
};
use crate::error::{Error, Result};
use crate::page::{CapturedRequest, Page, ResponseBody};
/// Network event types. `Debug` omits header values and URL queries.
#[derive(Clone)]
pub enum NetworkEvent {
    /// A new request was initiated
    RequestStarted(CapturedRequest),
    /// Response headers received
    ResponseReceived {
        /// ID of the request this response belongs to.
        request_id: String,
        /// HTTP status code.
        status: i32,
        /// HTTP status text.
        status_text: String,
        /// Response headers.
        headers: HashMap<String, String>,
        /// Response MIME type, if known.
        mime_type: Option<String>,
    },
    /// Request completed successfully
    RequestCompleted {
        /// ID of the completed request.
        request_id: String,
        /// Total bytes received over the wire.
        encoded_data_length: i64,
    },
    /// Request failed
    RequestFailed {
        /// ID of the failed request.
        request_id: String,
        /// Failure reason.
        error_text: String,
        /// Whether the request was canceled.
        canceled: bool,
    },
}

impl std::fmt::Debug for NetworkEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequestStarted(request) => {
                f.debug_tuple("RequestStarted").field(request).finish()
            }
            Self::ResponseReceived {
                request_id,
                status,
                status_text,
                headers,
                mime_type,
            } => f
                .debug_struct("ResponseReceived")
                .field("request_id", request_id)
                .field("status", status)
                .field("status_text", status_text)
                .field("header_names", &headers.keys())
                .field("mime_type", mime_type)
                .finish(),
            Self::RequestCompleted {
                request_id,
                encoded_data_length,
            } => f
                .debug_struct("RequestCompleted")
                .field("request_id", request_id)
                .field("encoded_data_length", encoded_data_length)
                .finish(),
            Self::RequestFailed {
                request_id,
                error_text,
                canceled,
            } => f
                .debug_struct("RequestFailed")
                .field("request_id", request_id)
                .field("error_text", error_text)
                .field("canceled", canceled)
                .finish(),
        }
    }
}

/// Default bound for [`NetworkWatcher`]. Long-lived connections (WebSocket,
/// SSE, long-polling) never finish, so every store is bounded.
const MAX_TRACKED_REQUESTS: usize = 10_000;

/// Bounded request store, evicting the oldest entry (in-flight or complete).
pub(crate) struct RequestStore {
    capacity: usize,
    next: u64,
    /// Insertion order: sequence number -> request id.
    order: BTreeMap<u64, String>,
    /// Request id -> (sequence number, request).
    requests: HashMap<String, (u64, CapturedRequest)>,
    evicted: u64,
    failed: u64,
}

impl RequestStore {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            next: 0,
            order: BTreeMap::new(),
            requests: HashMap::new(),
            evicted: 0,
            failed: 0,
        }
    }

    /// Apply one CDP Network event. Returns whether it was a recognised
    /// network event and, if any, the consumer-facing event.
    pub(crate) fn apply(
        &mut self,
        method: &str,
        params: &serde_json::Value,
    ) -> (bool, Option<NetworkEvent>) {
        match self.apply_inner(method, params) {
            Some(Some(event)) => (true, Some(event)),
            Some(None) => (true, None),
            None => (false, None),
        }
    }

    fn apply_inner(
        &mut self,
        method: &str,
        params: &serde_json::Value,
    ) -> Option<Option<NetworkEvent>> {
        match method {
            "Network.requestWillBeSent" => {
                let e: NetworkRequestWillBeSentEvent =
                    serde_json::from_value(params.clone()).ok()?;
                // Redirects re-fire with the same request_id; keep the original entry.
                if self.requests.contains_key(&e.request_id) {
                    tracing::trace!(
                        "Ignoring redirect requestWillBeSent for {} (keeping original)",
                        e.request_id
                    );
                    return Some(None);
                }
                while self.requests.len() >= self.capacity {
                    let Some((_, oldest)) = self.order.pop_first() else {
                        break;
                    };
                    self.requests.remove(&oldest);
                    self.evicted += 1;
                }
                let request = CapturedRequest {
                    request_id: e.request_id.clone(),
                    url: e.request.url,
                    method: e.request.method,
                    headers: e.request.headers,
                    post_data: e.request.post_data,
                    resource_type: e.r#type,
                    status: None,
                    status_text: None,
                    response_headers: None,
                    mime_type: None,
                    timestamp: e.timestamp,
                    complete: false,
                };
                let sequence = self.next;
                self.next += 1;
                self.order.insert(sequence, e.request_id.clone());
                self.requests
                    .insert(e.request_id, (sequence, request.clone()));
                Some(Some(NetworkEvent::RequestStarted(request)))
            }
            "Network.responseReceived" => {
                let e: NetworkResponseReceivedEvent =
                    serde_json::from_value(params.clone()).ok()?;
                if let Some((_, req)) = self.requests.get_mut(&e.request_id) {
                    req.status = Some(e.response.status);
                    req.status_text = Some(e.response.status_text.clone());
                    req.response_headers = Some(e.response.headers.clone());
                    req.mime_type = e.response.mime_type.clone();
                }
                Some(Some(NetworkEvent::ResponseReceived {
                    request_id: e.request_id,
                    status: e.response.status,
                    status_text: e.response.status_text,
                    headers: e.response.headers,
                    mime_type: e.response.mime_type,
                }))
            }
            "Network.loadingFinished" => {
                let e: NetworkLoadingFinishedEvent = serde_json::from_value(params.clone()).ok()?;
                // Completed entries stay (up to the bound) so their bodies can be read.
                if let Some((_, req)) = self.requests.get_mut(&e.request_id) {
                    req.complete = true;
                }
                Some(Some(NetworkEvent::RequestCompleted {
                    request_id: e.request_id,
                    encoded_data_length: e.encoded_data_length,
                }))
            }
            "Network.loadingFailed" => {
                let e: NetworkLoadingFailedEvent = serde_json::from_value(params.clone()).ok()?;
                if let Some((sequence, _)) = self.requests.remove(&e.request_id) {
                    self.order.remove(&sequence);
                    self.failed += 1;
                }
                Some(Some(NetworkEvent::RequestFailed {
                    request_id: e.request_id,
                    error_text: e.error_text,
                    canceled: e.canceled.unwrap_or(false),
                }))
            }
            _ => None,
        }
    }

    pub(crate) fn get(&self, request_id: &str) -> Option<CapturedRequest> {
        self.requests.get(request_id).map(|(_, r)| r.clone())
    }

    /// Retained requests, oldest first.
    pub(crate) fn all(&self) -> Vec<CapturedRequest> {
        self.order.values().filter_map(|id| self.get(id)).collect()
    }

    /// Remove and return completed requests, oldest first.
    fn drain_completed(&mut self) -> Vec<CapturedRequest> {
        let done: Vec<u64> = self
            .order
            .iter()
            .filter(|(_, id)| self.requests.get(*id).is_some_and(|(_, r)| r.complete))
            .map(|(sequence, _)| *sequence)
            .collect();
        done.into_iter()
            .filter_map(|sequence| self.order.remove(&sequence))
            .filter_map(|id| self.requests.remove(&id).map(|(_, r)| r))
            .collect()
    }

    fn find(&self, predicate: &dyn Fn(&CapturedRequest) -> bool) -> Option<CapturedRequest> {
        self.order
            .values()
            .filter_map(|id| self.requests.get(id))
            .map(|(_, r)| r)
            .find(|r| predicate(r))
            .cloned()
    }

    pub(crate) fn clear(&mut self) {
        self.order.clear();
        self.requests.clear();
    }
}

/// Watches network events and provides a stream of captured requests.
///
/// Feed it events with [`process_event`](Self::process_event). Use
/// [`for_session`](Self::for_session) to ignore events from other CDP sessions;
/// [`new`](Self::new) accepts every session. At most 10,000 requests are kept,
/// evicting the oldest first. Prefer [`Page::capture_requests`](crate::Page::capture_requests),
/// which subscribes and filters for you.
pub struct NetworkWatcher {
    session_id: Option<String>,
    requests: Arc<Mutex<RequestStore>>,
    /// Channel to send events to consumers
    event_tx: mpsc::Sender<NetworkEvent>,
    /// Channel to receive events
    event_rx: Mutex<mpsc::Receiver<NetworkEvent>>,
}

impl NetworkWatcher {
    /// Create a watcher that accepts events from every CDP session.
    pub fn new() -> Self {
        Self::with_filter(None, MAX_TRACKED_REQUESTS)
    }

    /// Create a watcher that ignores events not tagged with `session_id`
    /// (for example from [`Session::session_id`](crate::cdp::Session::session_id)).
    pub fn for_session(session_id: impl Into<String>) -> Self {
        Self::with_filter(Some(session_id.into()), MAX_TRACKED_REQUESTS)
    }

    fn with_filter(session_id: Option<String>, capacity: usize) -> Self {
        let (event_tx, event_rx) = mpsc::channel(256);
        Self {
            session_id,
            requests: Arc::new(Mutex::new(RequestStore::new(capacity))),
            event_tx,
            event_rx: Mutex::new(event_rx),
        }
    }

    /// Process a CDP event.
    /// Returns true if it was a network event for this watcher's session.
    pub async fn process_event(&self, event: &CdpMessage) -> bool {
        let CdpMessage::Event {
            method,
            params,
            session_id,
        } = event
        else {
            return false;
        };
        if self
            .session_id
            .as_ref()
            .is_some_and(|wanted| session_id.as_ref() != Some(wanted))
        {
            return false;
        }
        let (handled, emitted) = self.requests.lock().await.apply(method, params);
        if let Some(event) = emitted {
            self.emit(event);
        }
        handled
    }

    /// Emit an event to consumers without blocking the pump.
    fn emit(&self, event: NetworkEvent) {
        if let Err(e) = self.event_tx.try_send(event) {
            tracing::warn!("Dropping network event (consumer not draining): {}", e);
        }
    }

    /// Receive the next network event
    pub async fn recv(&self) -> Option<NetworkEvent> {
        let mut rx = self.event_rx.lock().await;
        rx.recv().await
    }

    /// Try to receive a network event without blocking
    pub async fn try_recv(&self) -> Option<NetworkEvent> {
        // Use try_lock so a concurrent recv() doesn't block us.
        let mut rx = match self.event_rx.try_lock() {
            Ok(rx) => rx,
            Err(_) => return None,
        };
        rx.try_recv().ok()
    }

    /// Get a captured request by ID
    pub async fn get_request(&self, request_id: &str) -> Option<CapturedRequest> {
        self.requests.lock().await.get(request_id)
    }

    /// Get all retained requests, oldest first.
    pub async fn get_all_requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().await.all()
    }

    /// Clear all captured requests
    pub async fn clear(&self) {
        self.requests.lock().await.clear();
    }
}

impl Default for NetworkWatcher {
    fn default() -> Self {
        Self::new()
    }
}

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
                            if worker_shared.store().apply(method, params).0 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_network_watcher_creation() {
        let watcher = NetworkWatcher::new();
        assert!(watcher.get_all_requests().await.is_empty());
    }

    fn event(method: &str, params: serde_json::Value) -> CdpMessage {
        CdpMessage::Event {
            method: method.to_string(),
            params,
            session_id: None,
        }
    }

    fn request_will_be_sent(id: &str, url: &str) -> CdpMessage {
        event(
            "Network.requestWillBeSent",
            serde_json::json!({
                "requestId": id,
                "request": {
                    "url": url,
                    "method": "GET",
                    "headers": {},
                },
                "timestamp": 1.0,
                "type": "Document",
            }),
        )
    }

    /// Regression: a completed request (finished loading) must be retained so
    /// `get_request` still returns it with `complete == true` and the response
    /// status populated. Previously loadingFinished removed the entry.
    #[tokio::test]
    async fn test_completed_request_is_retained() {
        let watcher = NetworkWatcher::new();
        let id = "req-1";

        assert!(
            watcher
                .process_event(&request_will_be_sent(id, "https://example.com/"))
                .await
        );
        assert!(
            watcher
                .process_event(&event(
                    "Network.responseReceived",
                    serde_json::json!({
                        "requestId": id,
                        "response": {
                            "url": "https://example.com/",
                            "status": 200,
                            "statusText": "OK",
                            "headers": {},
                            "mimeType": "text/html",
                        },
                    }),
                ))
                .await
        );
        assert!(
            watcher
                .process_event(&event(
                    "Network.loadingFinished",
                    serde_json::json!({
                        "requestId": id,
                        "timestamp": 2.0,
                        "encodedDataLength": 1234,
                    }),
                ))
                .await
        );

        let req = watcher.get_request(id).await.expect("request retained");
        assert!(req.complete);
        assert_eq!(req.status, Some(200));
        assert_eq!(req.status_text.as_deref(), Some("OK"));
    }

    /// Regression: a redirect re-fires requestWillBeSent with the same
    /// request_id. The original entry (first URL) must be kept and there must
    /// be exactly one stored entry.
    #[tokio::test]
    async fn test_redirect_keeps_original_entry() {
        let watcher = NetworkWatcher::new();
        let id = "req-redirect";

        assert!(
            watcher
                .process_event(&request_will_be_sent(id, "https://first.example/"))
                .await
        );
        assert!(
            watcher
                .process_event(&request_will_be_sent(id, "https://second.example/"))
                .await
        );

        let req = watcher.get_request(id).await.expect("request present");
        assert_eq!(req.url, "https://first.example/");
        assert_eq!(watcher.get_all_requests().await.len(), 1);
    }

    /// A failed request (loadingFailed) must be removed from the store.
    #[tokio::test]
    async fn test_loading_failed_removes_entry() {
        let watcher = NetworkWatcher::new();
        let id = "req-fail";

        assert!(
            watcher
                .process_event(&request_will_be_sent(id, "https://example.com/"))
                .await
        );
        assert!(watcher.get_request(id).await.is_some());

        assert!(
            watcher
                .process_event(&event(
                    "Network.loadingFailed",
                    serde_json::json!({
                        "requestId": id,
                        "errorText": "net::ERR_FAILED",
                        "canceled": false,
                    }),
                ))
                .await
        );

        assert!(watcher.get_request(id).await.is_none());
    }

    /// Regression: the pump must not block when events are never drained.
    /// Feeding well over the channel capacity without ever calling `recv()`
    /// should complete (proving `emit` uses non-blocking `try_send`) and the
    /// store should reflect every request.
    #[tokio::test]
    async fn test_pump_does_not_block_when_events_not_drained() {
        let watcher = NetworkWatcher::new();
        const N: usize = 300;

        for i in 0..N {
            let id = format!("req-{i}");
            let url = format!("https://example.com/{i}");
            assert!(
                watcher
                    .process_event(&request_will_be_sent(&id, &url))
                    .await
            );
        }

        // Reaching here without hanging proves the pump never blocked.
        assert_eq!(watcher.get_all_requests().await.len(), N);
        assert!(watcher.get_request("req-0").await.is_some());
        assert!(watcher
            .get_request(&format!("req-{}", N - 1))
            .await
            .is_some());
    }

    fn on_session(message: CdpMessage, id: &str) -> CdpMessage {
        match message {
            CdpMessage::Event { method, params, .. } => CdpMessage::Event {
                method,
                params,
                session_id: Some(id.to_string()),
            },
            other => other,
        }
    }

    /// Regression: the watcher accepted events from every CDP session.
    #[tokio::test]
    async fn test_for_session_ignores_other_sessions() {
        let watcher = NetworkWatcher::for_session("mine");
        let theirs = on_session(request_will_be_sent("a", "https://a.test/"), "theirs");
        let untagged = request_will_be_sent("b", "https://b.test/");
        let mine = on_session(request_will_be_sent("c", "https://c.test/"), "mine");
        assert!(!watcher.process_event(&theirs).await);
        assert!(!watcher.process_event(&untagged).await);
        assert!(watcher.process_event(&mine).await);
        let ids: Vec<_> = watcher
            .get_all_requests()
            .await
            .into_iter()
            .map(|r| r.request_id)
            .collect();
        assert_eq!(ids, ["c"]);
    }

    /// Regression: eviction removed an arbitrary HashMap entry instead of the
    /// oldest, and completed requests were never removed at all.
    #[test]
    fn test_store_evicts_oldest_first_including_completed() {
        let mut store = RequestStore::new(3);
        let apply = |store: &mut RequestStore, message: CdpMessage| {
            let CdpMessage::Event { method, params, .. } = message else {
                unreachable!()
            };
            store.apply(&method, &params)
        };
        for i in 0..5 {
            let id = format!("r{i}");
            apply(
                &mut store,
                request_will_be_sent(&id, &format!("https://x.test/{i}")),
            );
            apply(
                &mut store,
                event(
                    "Network.loadingFinished",
                    serde_json::json!({"requestId": id, "timestamp": 2.0, "encodedDataLength": 1}),
                ),
            );
        }
        let ids: Vec<_> = store.all().into_iter().map(|r| r.request_id).collect();
        assert_eq!(ids, ["r2", "r3", "r4"]);
        assert_eq!(store.evicted, 2);

        // A failed request frees its slot without disturbing the order.
        apply(
            &mut store,
            event(
                "Network.loadingFailed",
                serde_json::json!({"requestId": "r3", "errorText": "net::ERR_FAILED"}),
            ),
        );
        apply(&mut store, request_will_be_sent("r5", "https://x.test/5"));
        let ids: Vec<_> = store.all().into_iter().map(|r| r.request_id).collect();
        assert_eq!(ids, ["r2", "r4", "r5"]);
        assert_eq!((store.evicted, store.failed), (2, 1));

        let drained: Vec<_> = store
            .drain_completed()
            .into_iter()
            .map(|r| r.request_id)
            .collect();
        assert_eq!(drained, ["r2", "r4"], "r5 is still in flight");
        assert_eq!(store.all().len(), 1);
    }
}
