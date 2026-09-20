use eoka::{Browser, FrameResponseCaptureOptions, Page};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Notify,
};

const PAYLOAD: &[u8] = b"{\"cookie\":\"private-fixture-cookie\",\"success\":true}";
const BINARY: &[u8] = &[0, 255, 1, 13, 10, 128, 192];

struct Fixture {
    url: String,
    task: tokio::task::JoinHandle<()>,
    slow_started: Arc<Notify>,
    release: Arc<Notify>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fixture {
    async fn start(isolated: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let slow_started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let (started, released) = (slow_started.clone(), release.clone());
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let (started, released) = (started.clone(), released.clone());
                tokio::spawn(async move {
                    let mut buffer = [0; 8192];
                    let n = socket.read(&mut buffer).await.unwrap_or_default();
                    let request = String::from_utf8_lossy(&buffer[..n]);
                    let path = request.split_whitespace().nth(1).unwrap_or("/");
                    if path.starts_with("/data") || path == "/slow" {
                        let (content_type, payload) = if path.starts_with("/data-binary") {
                            ("application/octet-stream", BINARY)
                        } else {
                            ("application/json", PAYLOAD)
                        };
                        let headers = format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nSet-Cookie: hidden=header-secret\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len());
                        let _ = socket.write_all(headers.as_bytes()).await;
                        if path == "/slow" {
                            started.notify_one();
                            released.notified().await;
                        }
                        let _ = socket.write_all(payload).await;
                        return;
                    }
                    let body = if path.starts_with("/inner") || path == "/sibling" {
                        r#"<button onclick="fetch(this.dataset.path || '/data?token=query-secret').then(r=>r.arrayBuffer()).then(b=>parent.postMessage({bytes:Array.from(new Uint8Array(b)),remove:!document.body.hasAttribute('keep')},'*'))">fetch</button>"#.to_string()
                    } else if path == "/outer" {
                        let host = if isolated { "child.test" } else { "app.test" };
                        format!(
                            r#"<iframe src="http://{host}:{port}/inner"></iframe><iframe src="http://{host}:{port}/sibling"></iframe><script>onmessage=e=>parent.postMessage(e.data,'*')</script>"#
                        )
                    } else {
                        let host = if isolated { "auth.test" } else { "app.test" };
                        format!(
                            r#"<iframe src="http://{host}:{port}/outer"></iframe><script>onmessage=e=>{{document.body.dataset.bytes=JSON.stringify(e.data.bytes);if(e.data.remove)document.querySelector('iframe').remove()}}</script>"#
                        )
                    };
                    let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        Self {
            url: format!("http://app.test:{port}/"),
            task,
            slow_started,
            release,
        }
    }
}

async fn open(fixture: &Fixture) -> (Browser, Page, String) {
    let browser = Browser::launch_with(|c| {
        c.extra_args.extend([
            "--host-resolver-rules=MAP *.test 127.0.0.1".into(),
            "--no-proxy-server".into(),
            "--site-per-process".into(),
        ]);
    })
    .await
    .unwrap();
    let page = browser.new_page(&fixture.url).await.unwrap();
    let id = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(frame) = page
                .frames()
                .await
                .unwrap()
                .into_iter()
                .find(|f| f.url.ends_with("/inner"))
            {
                break frame.id;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    (browser, page, id)
}

async fn received(page: &Page) {
    received_bytes(page, PAYLOAD).await;
}

async fn received_bytes(page: &Page, expected: &[u8]) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let value: String = page
                .evaluate_sync("document.body.dataset.bytes || ''")
                .await
                .unwrap();
            if !value.is_empty() {
                let bytes: Vec<u8> = serde_json::from_str(&value).unwrap();
                assert_eq!(
                    bytes, expected,
                    "capture must not change the delivered response"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("request must not remain paused");
}

fn options() -> FrameResponseCaptureOptions {
    FrameResponseCaptureOptions {
        capture_bodies: true,
        url_patterns: vec!["*/data*".into(), "*/slow".into()],
        ..Default::default()
    }
}

#[tokio::test]
#[ignore = "requires Chrome; local fixture only"]
async fn response_survives_immediate_nested_frame_removal() {
    for (isolated, binary) in [(false, false), (false, true), (true, false), (true, true)] {
        let fixture = Fixture::start(isolated).await;
        let (browser, page, id) = open(&fixture).await;
        let capture = page.capture_frame_responses(&id, options()).await.unwrap();
        if binary {
            page.evaluate_in_frame_id::<bool>(
                &id,
                "(document.querySelector('button').dataset.path='/data-binary',true)",
            )
            .await
            .unwrap();
        }
        page.evaluate_in_frame_id::<bool>(&id, "(document.querySelector('button').click(),true)")
            .await
            .unwrap();
        let expected = if binary { BINARY } else { PAYLOAD };
        received_bytes(&page, expected).await;
        let report = capture.stop().await;
        assert_eq!(report.responses.len(), 1, "{report:?}");
        assert_eq!(report.responses[0].body.as_deref(), Some(expected));
        assert_eq!(report.responses[0].status, Some(200));
        assert_eq!(report.dropped_events + report.dropped_responses, 0);
        let debug = format!("{report:?}");
        for secret in ["private-fixture-cookie", "header-secret", "query-secret"] {
            assert!(!debug.contains(secret));
        }
        assert!(!page.frames().await.unwrap().iter().any(|f| f.id == id));
        assert!(page.capture_frame_responses(&id, options()).await.is_err());
        browser.close().await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires Chrome; local fixture only"]
async fn limits_scope_and_metadata_only_are_explicit() {
    let fixture = Fixture::start(false).await;
    let (browser, page, id) = open(&fixture).await;
    let other = browser.new_page("about:blank").await.unwrap();
    let other_id = other.frames().await.unwrap()[0].id.clone();
    assert!(page
        .capture_frame_responses(&other_id, options())
        .await
        .is_err());
    for body_limit in [None, Some(4), Some(PAYLOAD.len())] {
        let mut opts = options();
        opts.capture_bodies = body_limit.is_some();
        opts.max_body_bytes = body_limit.unwrap_or_default();
        opts.max_total_body_bytes = body_limit.unwrap_or_default();
        opts.max_responses = 2;
        let capture = page.capture_frame_responses(&id, opts).await.unwrap();
        let sibling = page
            .frames()
            .await
            .unwrap()
            .into_iter()
            .find(|f| f.url.ends_with("/sibling"))
            .unwrap();
        page.evaluate_in_frame_id::<bool>(
            &sibling.id,
            "(document.body.setAttribute('keep',''),document.querySelector('button').click(),true)",
        )
        .await
        .unwrap();
        received(&page).await;
        for _ in 0..3 {
            page.execute_sync("delete document.body.dataset.bytes")
                .await
                .unwrap();
            page.evaluate_in_frame_id::<bool>(&id, "(document.body.setAttribute('keep',''),document.querySelector('button').click(),true)").await.unwrap();
            received(&page).await;
        }
        let report = capture.stop().await;
        assert_eq!(report.responses.len(), 2, "{report:?}");
        assert_eq!(report.dropped_responses, 1);
        assert_eq!(report.dropped_events, 0);
        assert_eq!(report.responses[0].body_truncated, body_limit == Some(4));
        if body_limit == Some(PAYLOAD.len()) {
            assert_eq!(report.responses[0].body.as_deref(), Some(PAYLOAD));
            assert!(report.responses[1].body_truncated);
        } else {
            assert!(report.responses.iter().all(|r| r.body.is_none()));
        }
        page.execute_sync("delete document.body.dataset.bytes")
            .await
            .unwrap();
    }
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local fixture only"]
async fn cancellation_releases_paused_responses_and_sessions() {
    let fixture = Fixture::start(true).await;
    let (browser, page, id) = open(&fixture).await;
    let capture = page.capture_frame_responses(&id, options()).await.unwrap();
    page.evaluate_in_frame_id::<bool>(&id, "(document.querySelector('button').dataset.path='/slow',document.querySelector('button').click(),true)").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), fixture.slow_started.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while capture.snapshot().in_flight == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut events = page.session().transport().subscribe();
    drop(capture);
    fixture.release.notify_one();
    received(&page).await;
    let mut detached = false;
    while let Ok(event) = events.try_recv() {
        if let eoka::cdp::transport::CdpMessage::Event { method, .. } = event {
            if method == "Target.detachedFromTarget" {
                detached = true;
            }
        }
    }
    assert!(detached, "capture teardown must detach its session");
    let _: Value = page.evaluate_sync("1").await.unwrap();
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local fixture only"]
async fn allowlisted_headers_are_captured_without_debug_leakage() {
    let fixture = Fixture::start(false).await;
    let (browser, page, id) = open(&fixture).await;
    page.execute_sync("document.cookie='datadome=request-secret; path=/'")
        .await
        .unwrap();
    let mut opts = options();
    opts.request_headers = vec!["Cookie".into(), "User-Agent".into()];
    opts.response_headers = vec!["Set-Cookie".into(), "Content-Type".into()];
    let capture = page.capture_frame_responses(&id, opts).await.unwrap();
    page.evaluate_in_frame_id::<bool>(&id, "(document.querySelector('button').click(),true)")
        .await
        .unwrap();
    received(&page).await;
    let report = capture.stop().await;
    let response = &report.responses[0];
    assert_eq!(response.method, "GET");
    assert!(response
        .request_headers
        .iter()
        .any(|(name, value)| name == "cookie" && value.contains("datadome=request-secret")));
    assert!(response
        .response_headers
        .iter()
        .any(|(name, value)| name == "content-type" && value == "application/json"));
    assert!(response
        .response_headers
        .iter()
        .filter(|(name, _)| name == "set-cookie")
        .all(|(_, value)| value == "hidden=header-secret"));
    assert!(page
        .cookies()
        .await
        .unwrap()
        .iter()
        .any(|c| c.name == "hidden" && c.value == "header-secret"));
    assert!(!response.headers_truncated);
    assert!(!format!("{report:?}").contains("secret"));
    browser.close().await.unwrap();
}
