//! Network Request Capture
//!
//! Provides streaming capture of HTTP requests and responses.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use crate::cdp::transport::CdpMessage;
use crate::cdp::types::{
    NetworkLoadingFailedEvent, NetworkLoadingFinishedEvent, NetworkRequestWillBeSentEvent,
    NetworkResponseReceivedEvent,
};
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

/// Maximum number of in-flight requests tracked before evicting the oldest.
/// Prevents unbounded memory growth from long-lived connections (WebSocket,
/// SSE, long-polling) that never fire loadingFinished/loadingFailed.
const MAX_INFLIGHT_REQUESTS: usize = 10_000;

/// Watches network events and provides a stream of captured requests
pub struct NetworkWatcher {
    /// In-flight requests (request_id -> CapturedRequest)
    requests: Arc<Mutex<HashMap<String, CapturedRequest>>>,
    /// Channel to send events to consumers
    event_tx: mpsc::Sender<NetworkEvent>,
    /// Channel to receive events
    event_rx: Mutex<mpsc::Receiver<NetworkEvent>>,
}

impl NetworkWatcher {
    /// Create a new NetworkWatcher
    pub fn new() -> Self {
        let (event_tx, event_rx) = mpsc::channel(256);
        Self {
            requests: Arc::new(Mutex::new(HashMap::new())),
            event_tx,
            event_rx: Mutex::new(event_rx),
        }
    }

    /// Process a CDP event
    /// Returns true if the event was a network event that was processed
    pub async fn process_event(&self, event: &CdpMessage) -> bool {
        if let CdpMessage::Event { method, params, .. } = event {
            match method.as_str() {
                "Network.requestWillBeSent" => {
                    if let Ok(e) =
                        serde_json::from_value::<NetworkRequestWillBeSentEvent>(params.clone())
                    {
                        self.on_request_will_be_sent(e).await;
                        return true;
                    }
                }
                "Network.responseReceived" => {
                    if let Ok(e) =
                        serde_json::from_value::<NetworkResponseReceivedEvent>(params.clone())
                    {
                        self.on_response_received(e).await;
                        return true;
                    }
                }
                "Network.loadingFinished" => {
                    if let Ok(e) =
                        serde_json::from_value::<NetworkLoadingFinishedEvent>(params.clone())
                    {
                        self.on_loading_finished(e).await;
                        return true;
                    }
                }
                "Network.loadingFailed" => {
                    if let Ok(e) =
                        serde_json::from_value::<NetworkLoadingFailedEvent>(params.clone())
                    {
                        self.on_loading_failed(e).await;
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    async fn on_request_will_be_sent(&self, event: NetworkRequestWillBeSentEvent) {
        let request = {
            let mut requests = self.requests.lock().await;

            // Redirects re-fire with the same request_id; keep the original entry.
            if requests.contains_key(&event.request_id) {
                tracing::trace!(
                    "Ignoring redirect requestWillBeSent for {} (keeping original)",
                    event.request_id
                );
                return;
            }

            // Evict one entry if at capacity (memory safety valve).
            if requests.len() >= MAX_INFLIGHT_REQUESTS {
                if let Some(id) = requests.keys().next().cloned() {
                    requests.remove(&id);
                    tracing::debug!("Evicted a tracked request (cap={})", MAX_INFLIGHT_REQUESTS);
                }
            }

            let request = CapturedRequest {
                request_id: event.request_id.clone(),
                url: event.request.url.clone(),
                method: event.request.method.clone(),
                headers: event.request.headers.clone(),
                post_data: event.request.post_data.clone(),
                resource_type: event.r#type.clone(),
                status: None,
                status_text: None,
                response_headers: None,
                mime_type: None,
                timestamp: event.timestamp,
                complete: false,
            };
            requests.insert(event.request_id.clone(), request.clone());
            request
        };

        // Send event
        self.emit(NetworkEvent::RequestStarted(request));
    }

    async fn on_response_received(&self, event: NetworkResponseReceivedEvent) {
        // Update the stored request
        {
            let mut requests = self.requests.lock().await;
            if let Some(req) = requests.get_mut(&event.request_id) {
                req.status = Some(event.response.status);
                req.status_text = Some(event.response.status_text.clone());
                req.response_headers = Some(event.response.headers.clone());
                req.mime_type = event.response.mime_type.clone();
            }
        }

        // Send event
        self.emit(NetworkEvent::ResponseReceived {
            request_id: event.request_id,
            status: event.response.status,
            status_text: event.response.status_text,
            headers: event.response.headers,
            mime_type: event.response.mime_type,
        });
    }

    async fn on_loading_finished(&self, event: NetworkLoadingFinishedEvent) {
        // Mark complete rather than removing, so get_request() still returns it.
        {
            let mut requests = self.requests.lock().await;
            if let Some(req) = requests.get_mut(&event.request_id) {
                req.complete = true;
            }
        }

        // Send event
        self.emit(NetworkEvent::RequestCompleted {
            request_id: event.request_id,
            encoded_data_length: event.encoded_data_length,
        });
    }

    async fn on_loading_failed(&self, event: NetworkLoadingFailedEvent) {
        // Remove failed request
        {
            let mut requests = self.requests.lock().await;
            requests.remove(&event.request_id);
        }

        // Send event
        self.emit(NetworkEvent::RequestFailed {
            request_id: event.request_id,
            error_text: event.error_text,
            canceled: event.canceled.unwrap_or(false),
        });
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
        let requests = self.requests.lock().await;
        requests.get(request_id).cloned()
    }

    /// Get all captured requests
    pub async fn get_all_requests(&self) -> Vec<CapturedRequest> {
        let requests = self.requests.lock().await;
        requests.values().cloned().collect()
    }

    /// Clear all captured requests
    pub async fn clear(&self) {
        let mut requests = self.requests.lock().await;
        requests.clear();
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
}
