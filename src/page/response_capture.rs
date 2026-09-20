use std::{
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime},
};

use base64::Engine;
use futures_util::future::select;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use super::Page;
use crate::{cdp::Session, Error, Result};

/// Bounded, opt-in response interception for one frame, not its descendants.
/// Matching responses briefly pause while captured, then continue unchanged.
/// Body limits bound retention, not transient CDP messages or decoded bodies.
pub struct FrameResponseCaptureOptions {
    /// CDP Fetch wildcard URL patterns; defaults to all URLs. Must be nonempty.
    pub url_patterns: Vec<String>,
    /// Retain response bytes, which may contain secrets. Disabled by default.
    pub capture_bodies: bool,
    /// Explicit case-insensitive request header allowlist. Empty by default.
    /// CDP may omit some browser-generated headers; absence is not proof they were not sent.
    pub request_headers: Vec<String>,
    /// Explicit case-insensitive response header allowlist. Empty by default.
    /// Chrome can omit Set-Cookie here even when it updates the cookie jar.
    /// Combined retained headers are limited to 8192 bytes per response.
    pub response_headers: Vec<String>,
    /// Maximum retained records; additional responses are counted and continued.
    pub max_responses: usize,
    /// Maximum retained bytes per body. Known oversized bodies are skipped.
    pub max_body_bytes: usize,
    /// Maximum combined retained body bytes over this capture's lifetime.
    pub max_total_body_bytes: usize,
}

impl Default for FrameResponseCaptureOptions {
    fn default() -> Self {
        Self {
            url_patterns: vec!["*".into()],
            capture_bodies: false,
            request_headers: Vec::new(),
            response_headers: Vec::new(),
            max_responses: 128,
            max_body_bytes: 64 * 1024,
            max_total_body_bytes: 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Why a requested body could not be captured. Raw CDP errors are not retained.
pub enum CaptureBodyError {
    /// No body is available (including redirects, failed requests, or CDP errors).
    Unavailable,
    /// Body retrieval exceeded five seconds; the response was released.
    TimedOut,
    /// Chrome returned invalid base64 data.
    InvalidEncoding,
}

#[derive(Clone)]
/// Response captured before delivery to the frame. Debug omits body contents.
/// Headers are opt-in. Explicitly reading bodies or header values may expose secrets.
pub struct CapturedFrameResponse {
    /// HTTP(S) URL without credentials, query, or fragment; at most 4096 bytes.
    pub url: String,
    /// HTTP request method, capped at 32 characters; distinguishes preflights.
    pub method: String,
    /// HTTP status, or None for a network failure.
    pub status: Option<u16>,
    /// Local clock when this response pause was processed, not request dispatch time.
    pub captured_at: SystemTime,
    /// Allowlisted request headers reported at the Fetch response pause.
    /// Chrome may reconstruct Cookie from an already-updated jar, so these are
    /// NOT authoritative wire headers. Use Network.requestWillBeSentExtraInfo
    /// when correlating cookies actually sent. Values are omitted from Debug.
    pub request_headers: Vec<(String, String)>,
    /// Allowlisted response headers; preserves duplicates, values omitted from Debug.
    pub response_headers: Vec<(String, String)>,
    /// At least one allowlisted header was skipped due to the 8192-byte budget.
    pub headers_truncated: bool,
    /// Opt-in decoded bytes. None when disabled, unavailable, or entirely skipped.
    pub body: Option<Vec<u8>>,
    /// A configured byte limit caused all or part of the body to be omitted.
    pub body_truncated: bool,
    /// Body retrieval failure, independent of truncation and opt-out.
    pub body_error: Option<CaptureBodyError>,
    /// The redacted URL or method exceeded its metadata size limit.
    pub metadata_truncated: bool,
}

impl fmt::Debug for CapturedFrameResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapturedFrameResponse")
            .field("url", &self.url)
            .field("method", &self.method)
            .field("status", &self.status)
            .field("captured_at", &self.captured_at)
            .field(
                "request_header_names",
                &self
                    .request_headers
                    .iter()
                    .map(|(n, _)| n)
                    .collect::<Vec<_>>(),
            )
            .field(
                "response_header_names",
                &self
                    .response_headers
                    .iter()
                    .map(|(n, _)| n)
                    .collect::<Vec<_>>(),
            )
            .field("headers_truncated", &self.headers_truncated)
            .field("body_bytes", &self.body.as_ref().map(Vec::len))
            .field("body_truncated", &self.body_truncated)
            .field("body_error", &self.body_error)
            .field("metadata_truncated", &self.metadata_truncated)
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
/// Retained evidence and explicit loss counters. Debug does not print bodies.
pub struct FrameResponseCaptureReport {
    /// Retained responses in capture order.
    pub responses: Vec<CapturedFrameResponse>,
    /// Matching responses omitted due to record limits or cancellation.
    pub dropped_responses: u64,
    /// Response pauses released by transport because the bounded queue was full.
    pub dropped_events: u64,
    /// Errors continuing responses; teardown detaches the session to release pauses.
    pub continuation_errors: u64,
    /// Matching response currently being captured; zero after stop.
    pub in_flight: u64,
    /// The capture task failed to join; populated by stop.
    pub worker_failed: bool,
}

#[derive(Default)]
struct State {
    report: FrameResponseCaptureReport,
    retained_bytes: usize,
}

/// Owns a dedicated CDP session and capture worker. Drop requests cleanup;
/// use stop to await cleanup. Retained evidence survives removal of the frame.
pub struct FrameResponseCapture {
    state: Arc<Mutex<State>>,
    dropped: Arc<AtomicU64>,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<tokio::task::JoinHandle<()>>,
}

impl FrameResponseCapture {
    /// Clone currently retained evidence without stopping the worker.
    pub fn snapshot(&self) -> FrameResponseCaptureReport {
        let mut report = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .report
            .clone();
        report.dropped_events = self.dropped.load(Ordering::Relaxed);
        report
    }

    /// Cancel pending capture, detach its session, and return retained evidence.
    /// Call after the application received its response to avoid interrupting capture.
    pub async fn stop(mut self) -> FrameResponseCaptureReport {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let failed = if let Some(worker) = self.worker.take() {
            worker.await.is_err()
        } else {
            false
        };
        let mut report = self.snapshot();
        report.worker_failed = failed;
        report
    }
}

impl Drop for FrameResponseCapture {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

struct Lease(Option<Session>);

impl Lease {
    async fn close(mut self) {
        if let Some(session) = &self.0 {
            session
                .transport()
                .remove_response_route(session.session_id());
            let _ = session.detach().await;
        }
        self.0 = None;
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            session
                .transport()
                .remove_response_route(session.session_id());
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = session.detach().await;
                });
            }
        }
    }
}

impl Page {
    /// Capture matching responses in this frame before page JS can remove it.
    /// Supports same-process and OOPIF frames; rejects stale or unrelated IDs.
    /// Uses dedicated Fetch response interception, not page JS instrumentation.
    /// Initialization remains owned until completion if this future is canceled.
    pub async fn capture_frame_responses(
        &self,
        frame_id: &str,
        options: FrameResponseCaptureOptions,
    ) -> Result<FrameResponseCapture> {
        let page = self.clone();
        let frame_id = frame_id.to_owned();
        let (result, received) = oneshot::channel();
        tokio::spawn(async move {
            let capture = page.start_frame_response_capture(&frame_id, options).await;
            let _ = result.send(capture);
        });
        received
            .await
            .map_err(|_| Error::cdp_msg("response capture initialization failed"))?
    }

    async fn start_frame_response_capture(
        &self,
        frame_id: &str,
        options: FrameResponseCaptureOptions,
    ) -> Result<FrameResponseCapture> {
        if options.url_patterns.is_empty() || options.max_responses == 0 {
            return Err(Error::Decode(
                "response capture requires URL patterns and a positive record limit".into(),
            ));
        }
        let target = self.frame_target_id(frame_id).await?;
        let session = self.session.attach_frame_target(&target).await?;
        let lease = Lease(Some(session.clone()));
        let (events, receiver) = mpsc::channel(64);
        let dropped = Arc::new(AtomicU64::new(0));
        session
            .transport()
            .install_response_route(session.session_id(), events, dropped.clone());
        let patterns: Vec<_> = options
            .url_patterns
            .iter()
            .map(|p| json!({"urlPattern":p,"requestStage":"Response"}))
            .collect();
        let _: Value = session
            .send("Fetch.enable", &json!({"patterns":patterns}))
            .await?;
        let (stop, stopped) = oneshot::channel();
        let state = Arc::new(Mutex::new(State::default()));
        let worker_state = state.clone();
        let frame_id = frame_id.to_owned();
        let worker = tokio::spawn(async move {
            {
                let capture = Box::pin(capture_loop(
                    &session,
                    &frame_id,
                    options,
                    receiver,
                    worker_state.clone(),
                ));
                let _ = select(capture, Box::pin(stopped)).await;
            }
            {
                let mut state = worker_state.lock().unwrap_or_else(|e| e.into_inner());
                state.report.dropped_responses += state.report.in_flight;
                state.report.in_flight = 0;
            }
            lease.close().await;
        });
        Ok(FrameResponseCapture {
            state,
            dropped,
            stop: Some(stop),
            worker: Some(worker),
        })
    }
}

fn redacted_url(raw: &str) -> (String, bool) {
    let stripped = raw.split(['?', '#']).next().unwrap_or_default();
    let mut url = if let Some((scheme, rest)) = stripped.split_once("://") {
        if scheme != "http" && scheme != "https" {
            return ("[non-http URL]".into(), false);
        }
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        format!(
            "{scheme}://{}/{path}",
            authority.rsplit('@').next().unwrap_or_default()
        )
    } else {
        "[invalid URL]".into()
    };
    let truncated = url.len() > 4096;
    if truncated {
        let mut end = 4096;
        while !url.is_char_boundary(end) {
            end -= 1;
        }
        url.truncate(end);
    }
    (url, truncated)
}

fn retain_headers<'a>(
    headers: impl Iterator<Item = (&'a str, &'a str)>,
    allowed: &[String],
    remaining: &mut usize,
    truncated: &mut bool,
) -> Vec<(String, String)> {
    let mut retained = Vec::new();
    for (name, value) in headers {
        if !allowed
            .iter()
            .any(|wanted| wanted.eq_ignore_ascii_case(name))
        {
            continue;
        }
        let size = name.len().saturating_add(value.len());
        if size > *remaining {
            *truncated = true;
            continue;
        }
        *remaining -= size;
        retained.push((name.to_ascii_lowercase(), value.to_owned()));
    }
    retained
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Body {
    body: String,
    base64_encoded: bool,
}

async fn read_body(
    session: &Session,
    params: &Value,
    limit: usize,
    response: &mut CapturedFrameResponse,
) {
    let status = response.status.unwrap_or_default();
    if params.get("responseErrorReason").is_some() || (300..400).contains(&status) {
        response.body_error = Some(CaptureBodyError::Unavailable);
        return;
    }
    if status == 204 || status == 205 || params["request"]["method"] == "HEAD" {
        response.body = Some(Vec::new());
        return;
    }
    if limit == 0 {
        response.body_truncated = true;
        return;
    }
    let declared = params["responseHeaders"].as_array().and_then(|headers| {
        headers.iter().find_map(|h| {
            if h["name"].as_str()?.eq_ignore_ascii_case("content-length") {
                h["value"].as_str()?.parse::<u64>().ok()
            } else {
                None
            }
        })
    });
    if declared.is_some_and(|length| length > limit as u64) {
        response.body_truncated = true;
        return;
    }
    let request = json!({"requestId":params["requestId"]});
    let body: Body = match tokio::time::timeout(
        Duration::from_secs(5),
        session.send("Fetch.getResponseBody", &request),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => {
            response.body_error = Some(CaptureBodyError::Unavailable);
            return;
        }
        Err(_) => {
            response.body_error = Some(CaptureBodyError::TimedOut);
            return;
        }
    };
    let bytes = if body.base64_encoded {
        base64::engine::general_purpose::STANDARD.decode(body.body)
    } else {
        Ok(body.body.into_bytes())
    };
    match bytes {
        Ok(mut bytes) => {
            response.body_truncated = bytes.len() > limit;
            bytes.truncate(limit);
            response.body = Some(bytes);
        }
        Err(_) => response.body_error = Some(CaptureBodyError::InvalidEncoding),
    }
}

async fn capture_loop(
    session: &Session,
    frame_id: &str,
    options: FrameResponseCaptureOptions,
    mut events: mpsc::Receiver<Value>,
    state: Arc<Mutex<State>>,
) {
    while let Some(params) = events.recv().await {
        if params["frameId"].as_str() == Some(frame_id) {
            let budget = {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                if state.report.responses.len() >= options.max_responses {
                    state.report.dropped_responses += 1;
                    None
                } else {
                    state.report.in_flight += 1;
                    Some(
                        options.max_body_bytes.min(
                            options
                                .max_total_body_bytes
                                .saturating_sub(state.retained_bytes),
                        ),
                    )
                }
            };
            if let Some(budget) = budget {
                let (url, mut metadata_truncated) =
                    redacted_url(params["request"]["url"].as_str().unwrap_or_default());
                let method = params["request"]["method"].as_str().unwrap_or_default();
                metadata_truncated |= method.chars().count() > 32;
                let mut remaining = 8192;
                let mut headers_truncated = false;
                let request_headers = retain_headers(
                    params["request"]["headers"]
                        .as_object()
                        .into_iter()
                        .flat_map(|h| h.iter())
                        .filter_map(|(n, v)| Some((n.as_str(), v.as_str()?))),
                    &options.request_headers,
                    &mut remaining,
                    &mut headers_truncated,
                );
                let response_headers = retain_headers(
                    params["responseHeaders"]
                        .as_array()
                        .into_iter()
                        .flat_map(|h| h.iter())
                        .filter_map(|h| Some((h["name"].as_str()?, h["value"].as_str()?))),
                    &options.response_headers,
                    &mut remaining,
                    &mut headers_truncated,
                );
                let mut response = CapturedFrameResponse {
                    url,
                    method: method.chars().take(32).collect(),
                    status: params["responseStatusCode"]
                        .as_u64()
                        .and_then(|s| u16::try_from(s).ok()),
                    captured_at: SystemTime::now(),
                    request_headers,
                    response_headers,
                    headers_truncated,
                    body: None,
                    body_truncated: false,
                    body_error: None,
                    metadata_truncated,
                };
                if options.capture_bodies {
                    read_body(session, &params, budget, &mut response).await;
                }
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                state.retained_bytes += response.body.as_ref().map_or(0, Vec::len);
                state.report.responses.push(response);
                state.report.in_flight -= 1;
            }
        }
        let continued: Result<Value> = session
            .send(
                "Fetch.continueRequest",
                &json!({"requestId":params["requestId"]}),
            )
            .await;
        if continued.is_err() {
            state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .report
                .continuation_errors += 1;
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_are_opt_in_case_insensitive_and_bounded() {
        let headers = [
            ("Cookie", "private"),
            ("Authorization", "private"),
            ("Set-Cookie", "one"),
            ("Set-Cookie", "two"),
        ];
        let mut budget = 100;
        let mut truncated = false;
        assert!(retain_headers(headers.into_iter(), &[], &mut budget, &mut truncated).is_empty());
        let kept = retain_headers(
            headers.into_iter(),
            &["cookie".into(), "set-cookie".into()],
            &mut budget,
            &mut truncated,
        );
        assert_eq!(kept.len(), 3);
        assert!(!truncated);
        assert_eq!(budget, 100 - 13 - 13 - 13);
        budget = 13;
        let kept = retain_headers(
            headers.into_iter(),
            &["cookie".into(), "set-cookie".into()],
            &mut budget,
            &mut truncated,
        );
        assert_eq!(kept, vec![("cookie".into(), "private".into())]);
        assert!(truncated);
    }

    #[test]
    fn diagnostics_redact_secrets_and_bound_metadata() {
        assert_eq!(
            redacted_url("https://user:password@example.com/path?cookie=secret#token").0,
            "https://example.com/path"
        );
        assert_eq!(redacted_url("data:text/plain,secret").0, "[invalid URL]");
        let response = CapturedFrameResponse {
            url: "https://example.com/".into(),
            method: "GET".into(),
            status: Some(200),
            captured_at: SystemTime::UNIX_EPOCH,
            request_headers: vec![("cookie".into(), "secret request".into())],
            response_headers: vec![("set-cookie".into(), "secret response".into())],
            headers_truncated: false,
            body: Some(b"secret cookie".to_vec()),
            body_truncated: false,
            body_error: None,
            metadata_truncated: false,
        };
        assert!(!format!("{response:?}").contains("secret"));
        let (url, truncated) = redacted_url(&format!("https://example.com/{}", "é".repeat(3000)));
        assert!(url.len() <= 4096 && truncated);
    }
}
