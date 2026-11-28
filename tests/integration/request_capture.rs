//! Page-scoped request capture against a local HTTP fixture.

use crate::support;

use std::time::Duration;

use eoka::network::RequestCaptureOptions;
use eoka::ResponseBody;

// A data: icon keeps a stray /favicon.ico request out of the capture.
const PAGE: &str = "<!doctype html><link rel=icon href=data:,><title>capture</title>";

async fn fixture() -> support::HttpFixture {
    support::HttpFixture::start(|request| async move {
        if request.target.starts_with("/api") {
            let mut response = support::Response::html(r#"{"ok":true}"#);
            response.headers = vec![("Content-Type".into(), "application/json".into())];
            response
        } else {
            support::Response::html(PAGE)
        }
    })
    .await
}

fn wait() -> Duration {
    Duration::from_secs(5)
}

#[tokio::test]
#[ignore = "requires Chrome; local HTTP fixture"]
async fn captures_requests_with_readable_bodies() {
    let server = fixture().await;
    let browser = support::browser().await;
    let page = browser.new_page(&server.url).await.unwrap();
    let capture = page
        .capture_requests(RequestCaptureOptions::default())
        .await
        .unwrap();

    page.execute("fetch('/api/data', {method: 'POST', body: 'payload'})")
        .await
        .unwrap();
    let api = capture
        .wait_for(|r| r.url.ends_with("/api/data") && r.complete, wait())
        .await
        .unwrap();
    assert_eq!(api.method, "POST");
    assert_eq!(api.post_data.as_deref(), Some("payload"));
    assert_eq!(api.status, Some(200));
    assert_eq!(api.mime_type.as_deref(), Some("application/json"));
    match capture.response_body(&api.request_id).await.unwrap() {
        ResponseBody::Text(body) => assert_eq!(body, r#"{"ok":true}"#),
        other => panic!("unexpected body {other:?}"),
    }

    page.goto(&format!("{}next", server.url)).await.unwrap();
    let document = capture
        .wait_for(|r| r.url.ends_with("/next") && r.complete, wait())
        .await
        .unwrap();
    assert_eq!(document.resource_type.as_deref(), Some("Document"));
    let ids: Vec<_> = capture
        .requests()
        .into_iter()
        .map(|r| r.request_id)
        .collect();
    assert_eq!(ids.first(), Some(&api.request_id), "oldest first");

    let drained = capture.drain_completed();
    assert!(drained.iter().any(|r| r.request_id == api.request_id));
    assert!(capture.get(&api.request_id).is_none());
    assert_eq!(capture.stats().lost_events, 0);
    capture.stop().await;
    browser.close().await.unwrap();
}

/// Another page's traffic shares the transport but must not be recorded.
#[tokio::test]
#[ignore = "requires Chrome; local HTTP fixture"]
async fn capture_ignores_other_pages() {
    let server = fixture().await;
    let browser = support::browser().await;
    let page = browser.new_page(&server.url).await.unwrap();
    let other = browser.new_page(&server.url).await.unwrap();
    let capture = page
        .capture_requests(RequestCaptureOptions::default())
        .await
        .unwrap();
    let other_capture = other
        .capture_requests(RequestCaptureOptions::default())
        .await
        .unwrap();

    other.execute("fetch('/api/other')").await.unwrap();
    other_capture
        .wait_for(|r| r.url.ends_with("/api/other") && r.complete, wait())
        .await
        .unwrap();
    page.execute("fetch('/api/mine')").await.unwrap();
    capture
        .wait_for(|r| r.url.ends_with("/api/mine") && r.complete, wait())
        .await
        .unwrap();
    assert!(capture
        .requests()
        .iter()
        .all(|r| !r.url.ends_with("/api/other")));
    browser.close().await.unwrap();
}

/// The bound evicts the oldest entry, and completed entries count against it.
#[tokio::test]
#[ignore = "requires Chrome; local HTTP fixture"]
async fn capture_evicts_oldest_requests_first() {
    let server = fixture().await;
    let browser = support::browser().await;
    let page = browser.new_page(&server.url).await.unwrap();
    let capture = page
        .capture_requests(RequestCaptureOptions { max_requests: 2 })
        .await
        .unwrap();
    for i in 0..4 {
        page.execute(&format!("fetch('/api/{i}')")).await.unwrap();
        capture
            .wait_for(
                |r| r.url.ends_with(&format!("/api/{i}")) && r.complete,
                wait(),
            )
            .await
            .unwrap();
    }
    let urls: Vec<_> = capture.requests().into_iter().map(|r| r.url).collect();
    assert_eq!(urls.len(), 2, "{urls:?}");
    assert!(
        urls[0].ends_with("/api/2") && urls[1].ends_with("/api/3"),
        "{urls:?}"
    );
    assert_eq!(capture.stats().evicted, 2);
    browser.close().await.unwrap();
}

/// Stopping or dropping a capture leaves Network enabled: bodies stay
/// readable and another capture keeps receiving events.
#[tokio::test]
#[ignore = "requires Chrome; local HTTP fixture"]
async fn stopping_a_capture_keeps_the_network_domain() {
    let server = fixture().await;
    let browser = support::browser().await;
    let page = browser.new_page(&server.url).await.unwrap();
    let first = page
        .capture_requests(RequestCaptureOptions::default())
        .await
        .unwrap();
    let second = page
        .capture_requests(RequestCaptureOptions::default())
        .await
        .unwrap();
    page.execute("fetch('/api/once')").await.unwrap();
    let request = first
        .wait_for(|r| r.url.ends_with("/api/once") && r.complete, wait())
        .await
        .unwrap();
    let retained = first.stop().await;
    assert!(retained.iter().any(|r| r.request_id == request.request_id));
    assert!(page.get_response_body(&request.request_id).await.is_ok());

    page.execute("fetch('/api/after')").await.unwrap();
    second
        .wait_for(|r| r.url.ends_with("/api/after") && r.complete, wait())
        .await
        .unwrap();
    drop(second);

    // Explicitly disabling Network is what discards bodies.
    page.disable_request_capture().await.unwrap();
    assert!(page.get_response_body(&request.request_id).await.is_err());
    browser.close().await.unwrap();
}

/// Regression guard: a capture must not send Network.disable, which also
/// drops headers set with set_extra_headers.
#[tokio::test]
#[ignore = "requires Chrome; local HTTP fixture"]
async fn stopping_a_capture_keeps_extra_headers() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = seen.clone();
    let server = support::HttpFixture::start(move |request| {
        record
            .lock()
            .unwrap()
            .push(request.headers.get("x-probe").cloned());
        async { support::Response::html(PAGE) }
    })
    .await;
    let browser = support::browser().await;
    let page = browser.new_page(&server.url).await.unwrap();
    page.set_extra_headers([("X-Probe".to_string(), "1".to_string())].into())
        .await
        .unwrap();
    let capture = page
        .capture_requests(RequestCaptureOptions::default())
        .await
        .unwrap();
    capture.stop().await;
    seen.lock().unwrap().clear();
    page.goto(&format!("{}after", server.url)).await.unwrap();
    assert_eq!(
        seen.lock().unwrap().first().cloned().flatten().as_deref(),
        Some("1")
    );

    // Documented: an explicit disable drops them.
    page.disable_request_capture().await.unwrap();
    seen.lock().unwrap().clear();
    page.goto(&format!("{}again", server.url)).await.unwrap();
    assert_eq!(seen.lock().unwrap().first().cloned().flatten(), None);
    browser.close().await.unwrap();
}
