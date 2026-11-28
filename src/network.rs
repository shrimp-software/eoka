//! Network Request Capture
//!
//! [`RequestCapture`] (from [`Page::capture_requests`](crate::Page::capture_requests))
//! records one page's HTTP requests and responses. [`NetworkWatcher`] is the
//! lower-level engine for callers that feed it CDP events themselves.

mod capture;
mod store;

pub use capture::{RequestCapture, RequestCaptureOptions, RequestCaptureStats};
use store::RequestStore;

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use crate::cdp::transport::CdpMessage;
use crate::page::CapturedRequest;
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
        let Some(emitted) = self.requests.lock().await.apply(method, params) else {
            return false;
        };
        if let Some(event) = emitted {
            self.emit(event);
        }
        true
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
