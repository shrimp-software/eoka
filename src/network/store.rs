use std::collections::{BTreeMap, HashMap};

use super::NetworkEvent;
use crate::cdp::types::{
    NetworkLoadingFailedEvent, NetworkLoadingFinishedEvent, NetworkRequestWillBeSentEvent,
    NetworkResponseReceivedEvent,
};
use crate::page::CapturedRequest;

/// Bounded request store, evicting the oldest entry (in-flight or complete).
pub(super) struct RequestStore {
    capacity: usize,
    next: u64,
    /// Insertion order: sequence number -> request id.
    order: BTreeMap<u64, String>,
    /// Request id -> (sequence number, request).
    pub(super) requests: HashMap<String, (u64, CapturedRequest)>,
    pub(super) evicted: u64,
    pub(super) failed: u64,
}

impl RequestStore {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            next: 0,
            order: BTreeMap::new(),
            requests: HashMap::new(),
            evicted: 0,
            failed: 0,
        }
    }

    /// Apply one CDP event: `None` if it is not a recognised Network event,
    /// otherwise the consumer-facing event, if any.
    pub(super) fn apply(
        &mut self,
        method: &str,
        params: &serde_json::Value,
    ) -> Option<Option<NetworkEvent>> {
        use serde::Deserialize;
        match method {
            "Network.requestWillBeSent" => {
                let e = NetworkRequestWillBeSentEvent::deserialize(params).ok()?;
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
                let e = NetworkResponseReceivedEvent::deserialize(params).ok()?;
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
                let e = NetworkLoadingFinishedEvent::deserialize(params).ok()?;
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
                let e = NetworkLoadingFailedEvent::deserialize(params).ok()?;
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

    pub(super) fn get(&self, request_id: &str) -> Option<CapturedRequest> {
        self.requests.get(request_id).map(|(_, r)| r.clone())
    }

    /// Retained requests, oldest first.
    pub(super) fn all(&self) -> Vec<CapturedRequest> {
        self.order.values().filter_map(|id| self.get(id)).collect()
    }

    /// Remove and return completed requests, oldest first.
    pub(super) fn drain_completed(&mut self) -> Vec<CapturedRequest> {
        let mut done = Vec::new();
        self.order.retain(|_, id| {
            if !self.requests.get(id).is_some_and(|(_, r)| r.complete) {
                return true;
            }
            done.extend(self.requests.remove(id).map(|(_, r)| r));
            false
        });
        done
    }

    pub(super) fn find(
        &self,
        predicate: &dyn Fn(&CapturedRequest) -> bool,
    ) -> Option<CapturedRequest> {
        self.order
            .values()
            .filter_map(|id| self.requests.get(id))
            .map(|(_, r)| r)
            .find(|r| predicate(r))
            .cloned()
    }

    pub(super) fn clear(&mut self) {
        self.order.clear();
        self.requests.clear();
    }
}
