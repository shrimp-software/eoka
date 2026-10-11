//! Fetch pause ownership and authentication/header policy.

use super::{CommandDispatch, Transport};
use crate::Result;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tokio::sync::mpsc;

pub(super) enum EventAction {
    Forward,
    Consumed,
    Reply { method: &'static str, params: Value },
}

pub(super) struct FetchInterceptor {
    pub responses: ResponseRoutes,
    pub requests: RequestRoutes,
    pub proxy_auth: Option<(String, String)>,
    pub strip: bool,
}

impl FetchInterceptor {
    pub(super) fn on_event(
        &self,
        method: &str,
        session: Option<&str>,
        params: &Value,
    ) -> EventAction {
        match method {
            "Fetch.authRequired" => self.auth_challenge(params),
            "Fetch.requestPaused" => self.paused_request(session, params),
            _ => EventAction::Forward,
        }
    }

    fn auth_challenge(&self, params: &Value) -> EventAction {
        let Some((username, password)) = &self.proxy_auth else {
            return EventAction::Forward;
        };
        let Some(request_id) = params["requestId"].as_str() else {
            return EventAction::Forward;
        };
        let response = if params
            .pointer("/authChallenge/source")
            .and_then(Value::as_str)
            == Some("Proxy")
        {
            json!({"response":"ProvideCredentials", "username":username, "password":password})
        } else {
            json!({"response":"CancelAuth"})
        };
        EventAction::Reply {
            method: "Fetch.continueWithAuth",
            params: json!({"requestId":request_id, "authChallengeResponse":response}),
        }
    }

    fn paused_request(&self, session: Option<&str>, params: &Value) -> EventAction {
        if route_response_pause(&self.responses, session, params)
            || route_request_pause(&self.requests, session, params, self.strip)
        {
            return EventAction::Consumed;
        }
        match continuation_params(params, self.strip) {
            Some(params) => EventAction::Reply {
                method: "Fetch.continueRequest",
                params,
            },
            None => EventAction::Consumed,
        }
    }
}

/// Both queued and automatically continued pauses use the same header policy.
fn continuation_params(params: &Value, strip: bool) -> Option<Value> {
    let request_id = params["requestId"].as_str()?;
    let mut continuation = json!({"requestId":request_id});
    if strip
        && params.get("responseStatusCode").is_none()
        && params.get("responseErrorReason").is_none()
    {
        if let Some(headers) = params
            .pointer("/request/headers")
            .and_then(strip_client_data_header)
        {
            continuation["headers"] = Value::Array(
                headers
                    .into_iter()
                    .map(|(name, value)| json!({"name":name,"value":value}))
                    .collect(),
            );
        }
    }
    Some(continuation)
}

/// Remove `X-Client-Data` from a CDP request-headers object (case-insensitive
/// match, like HTTP header semantics). Returns `None` when the header is
/// absent so callers can continue the request unmodified.
fn strip_client_data_header(headers: &Value) -> Option<serde_json::Map<String, Value>> {
    let map = headers.as_object()?;
    let has_header = map.keys().any(|k| k.eq_ignore_ascii_case("x-client-data"));
    if !has_header {
        return None;
    }
    Some(
        map.iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("x-client-data"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    )
}

pub(super) struct ResponseRoute {
    pub(super) events: mpsc::Sender<Value>,
    pub(super) dropped: Arc<AtomicU64>,
}

pub(super) type ResponseRoutes = Arc<std::sync::Mutex<HashMap<String, ResponseRoute>>>;
pub(super) type RequestRoutes = Arc<std::sync::Mutex<HashMap<String, RequestRoute>>>;

pub(super) struct RequestRoute {
    pub(super) events: mpsc::Sender<RequestPause>,
    pub(super) dropped: Arc<AtomicU64>,
}

/// An owned request-stage pause. The consumer must continue or fulfill it once.
pub struct RequestPause {
    /// Exact session that owns this pause.
    pub session_id: String,
    /// Raw Fetch.requestPaused payload; may contain sensitive request metadata.
    pub params: Value,
    continue_params: Value,
}

impl RequestPause {
    /// Continue once with a one-shot dispatch token, preserving header policy.
    ///
    /// A possibly dispatched command must not be retried on timeout, cancellation
    /// or error. False is safe for handoff only after this future is dropped or
    /// terminated. A retry of a never-dispatched attempt needs a fresh token.
    pub async fn continue_request_with_dispatch(
        &self,
        transport: &Transport,
        dispatch: &CommandDispatch,
    ) -> Result<()> {
        transport
            .send_to_session_with_dispatch::<_, Value>(
                &self.session_id,
                "Fetch.continueRequest",
                &self.continue_params,
                dispatch,
            )
            .await?;
        Ok(())
    }

    /// Continue on the originating transport, preserving its header-stripping policy.
    pub async fn continue_request(&self, transport: &Transport) -> Result<()> {
        transport
            .send_to_session::<_, Value>(
                &self.session_id,
                "Fetch.continueRequest",
                &self.continue_params,
            )
            .await?;
        Ok(())
    }
}

pub(super) fn request_interception_command(
    strip: bool,
    auth: bool,
    patterns: Option<Vec<Value>>,
) -> (&'static str, Value) {
    if patterns.is_none() && !strip && !auth {
        return ("Fetch.disable", json!({}));
    }
    let patterns = if strip || auth {
        vec![json!({"urlPattern":"*","requestStage":"Request"})]
    } else {
        patterns.unwrap_or_default()
    };
    (
        "Fetch.enable",
        json!({"patterns": patterns, "handleAuthRequests":auth}),
    )
}

fn route_request_pause(
    routes: &RequestRoutes,
    session: Option<&str>,
    params: &Value,
    strip: bool,
) -> bool {
    if params.get("responseStatusCode").is_some() || params.get("responseErrorReason").is_some() {
        return false;
    }
    let Some(session) = session else {
        return false;
    };
    let routes = routes.lock().unwrap_or_else(|e| e.into_inner());
    let Some(route) = routes.get(session) else {
        return false;
    };
    let Some(continue_params) = continuation_params(params, strip) else {
        return false;
    };
    let pause = RequestPause {
        session_id: session.into(),
        params: params.clone(),
        continue_params,
    };
    if route.events.try_send(pause).is_ok() {
        return true;
    }
    route.dropped.fetch_add(1, Ordering::Relaxed);
    false
}

fn route_response_pause(routes: &ResponseRoutes, session: Option<&str>, params: &Value) -> bool {
    if params.get("responseStatusCode").is_none() && params.get("responseErrorReason").is_none() {
        return false;
    }
    let routes = routes.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(route) = session.and_then(|s| routes.get(s)) {
        if route.events.try_send(params.clone()).is_ok() {
            return true;
        }
        route.dropped.fetch_add(1, Ordering::Relaxed);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manual_interception_preserves_and_restores_base_auth_and_header_policy() {
        let pattern = vec![json!({"urlPattern":"*/accepted","requestStage":"Request"})];
        for (strip, auth) in [(true, false), (false, true), (true, true)] {
            for patterns in [Some(pattern.clone()), None] {
                let (method, params) = request_interception_command(strip, auth, patterns);
                assert_eq!(method, "Fetch.enable");
                assert_eq!(params["handleAuthRequests"], auth);
                assert_eq!(params["patterns"][0]["urlPattern"], "*");
            }
        }
        assert_eq!(
            request_interception_command(false, false, None).0,
            "Fetch.disable"
        );
        let (_, params) = request_interception_command(false, false, Some(pattern.clone()));
        assert_eq!(params["patterns"], json!(pattern));
        assert_eq!(params["handleAuthRequests"], false);
    }

    #[test]
    fn request_routes_are_scoped_bounded_and_preserve_header_policy() {
        let routes: RequestRoutes = Default::default();
        let (sender, mut receiver) = mpsc::channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        routes.lock().unwrap().insert(
            "owned".into(),
            RequestRoute {
                events: sender,
                dropped: dropped.clone(),
            },
        );
        let request = json!({"requestId":"req", "request":{"headers":{"X-Client-Data":"strip", "Other":"keep"}}});
        assert!(!route_request_pause(&routes, Some("other"), &request, true));
        assert!(!route_request_pause(&routes, None, &request, true));
        let mut response = request.clone();
        response["responseStatusCode"] = json!(200);
        assert!(!route_request_pause(
            &routes,
            Some("owned"),
            &response,
            true
        ));
        assert!(route_request_pause(&routes, Some("owned"), &request, true));
        assert!(!route_request_pause(&routes, Some("owned"), &request, true));
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        let pause = receiver.try_recv().unwrap();
        assert_eq!(pause.session_id, "owned");
        assert_eq!(
            pause.continue_params,
            json!({"requestId":"req", "headers":[{"name":"Other","value":"keep"}]})
        );
        assert!(route_request_pause(&routes, Some("owned"), &request, false));
        assert_eq!(
            receiver.try_recv().unwrap().continue_params,
            json!({"requestId":"req"})
        );
        drop(receiver);
        assert!(!route_request_pause(
            &routes,
            Some("owned"),
            &request,
            false
        ));
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
        routes.lock().unwrap().remove("owned");
        assert!(!route_request_pause(&routes, Some("owned"), &request, true));
    }

    #[test]
    fn response_routes_leave_unowned_and_overflowed_pauses_to_auto_continue() {
        let (sender, mut receiver) = mpsc::channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let routes = Arc::new(std::sync::Mutex::new(HashMap::from([(
            "owned".into(),
            ResponseRoute {
                events: sender,
                dropped: dropped.clone(),
            },
        )])));
        let response = json!({"requestId":"1", "responseStatusCode":200});
        assert!(!route_response_pause(
            &routes,
            Some("owned"),
            &json!({"requestId":"1"})
        ));
        assert!(!route_response_pause(&routes, Some("other"), &response));
        assert!(!route_response_pause(&routes, None, &response));
        assert!(route_response_pause(&routes, Some("owned"), &response));
        assert!(!route_response_pause(&routes, Some("owned"), &response));
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        assert_eq!(receiver.try_recv().unwrap(), response);
        drop(receiver);
        assert!(!route_response_pause(&routes, Some("owned"), &response));
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn strips_x_client_data_case_insensitively() {
        let headers = json!({
            "User-Agent": "test",
            "X-Client-Data": "abc123",
            "Accept": "*/*"
        });
        let stripped = strip_client_data_header(&headers).expect("header present");
        assert_eq!(stripped.len(), 2);
        assert!(stripped.get("User-Agent").is_some());
        assert!(stripped.get("Accept").is_some());
        assert!(strip_client_data_header(&Value::Object(stripped)).is_none());
    }

    #[test]
    fn returns_none_when_header_absent() {
        let headers = json!({ "User-Agent": "test", "Accept": "*/*" });
        assert!(strip_client_data_header(&headers).is_none());
        assert!(strip_client_data_header(&json!({})).is_none());
        assert!(strip_client_data_header(&json!(null)).is_none());
    }

    #[test]
    fn preserves_other_headers_verbatim() {
        let headers = json!({
            "sec-ch-ua": "\"Chromium\";v=\"140\"",
            "x-client-data": "CJKxyz="
        });
        let stripped = strip_client_data_header(&headers).expect("header present");
        assert_eq!(
            stripped.get("sec-ch-ua").and_then(|v| v.as_str()),
            Some("\"Chromium\";v=\"140\"")
        );
    }
}
