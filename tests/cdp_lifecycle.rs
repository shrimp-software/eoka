mod support;

use eoka::cdp::transport::CdpMessage;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use support::{HttpFixture, Response};

#[tokio::test]
#[ignore = "requires Chrome"]
async fn commands_on_a_closed_tab_fail_at_detachment_not_at_the_deadline() {
    let browser = support::browser().await;
    let page = browser.new_page("about:blank").await.unwrap();
    let transport = page.session().transport().clone();
    let session = page.session().session_id().to_owned();
    let never = tokio::spawn({
        let transport = transport.clone();
        let session = session.clone();
        async move {
            transport
                .send_to_session::<_, Value>(
                    &session,
                    "Runtime.evaluate",
                    &json!({"expression":"new Promise(() => {})","awaitPromise":true}),
                )
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    // Bypass eoka's close path, as when the site or a crash closes the tab.
    transport
        .send::<_, Value>("Target.closeTarget", &json!({"targetId": page.target_id()}))
        .await
        .unwrap();
    let error = never.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("detached"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn launched_browsers_honour_filter_cdp() {
    let filtered = support::browser().await;
    let page = filtered.new_page("about:blank").await.unwrap();
    let error = page
        .session()
        .send::<_, Value>("Runtime.enable", &json!({}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("filter_cdp"), "{error}");
    filtered.close().await.unwrap();

    let open = eoka::Browser::launch_with(|c| {
        c.chrome_path = std::env::var("EOKA_TEST_CHROME_PATH").ok();
        c.patch_binary = false;
        c.strip_x_client_data = false;
        c.cdp_timeout = 5;
        c.filter_cdp = false;
    })
    .await
    .unwrap();
    let page = open.new_page("about:blank").await.unwrap();
    let mut events = page.session().transport().subscribe();
    page.session()
        .send::<_, Value>("Runtime.enable", &json!({}))
        .await
        .unwrap();
    // The command really reached Chrome: it reports existing contexts.
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let CdpMessage::Event { method, .. } = &*events.recv().await.unwrap() {
                if method == "Runtime.executionContextCreated" {
                    break;
                }
            }
        }
    })
    .await
    .expect("Runtime.enable must be sent when filter_cdp is false");
    open.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local header fixture"]
async fn clearing_headers_does_not_re_enable_disabled_network_capture() {
    let fixture = HttpFixture::start(|request| async move {
        Response::html(
            request
                .headers
                .get("x-fixture-secret")
                .map(String::as_str)
                .unwrap_or("absent"),
        )
    })
    .await;
    let browser = support::browser().await;
    let page = browser.new_page(&fixture.url).await.unwrap();
    page.enable_request_capture().await.unwrap();
    page.disable_request_capture().await.unwrap();
    page.clear_extra_headers().await.unwrap();
    let mut events = page.session().transport().subscribe();
    page.goto(&format!("{}after-clear", fixture.url))
        .await
        .unwrap();
    assert_eq!(page.text().await.unwrap().trim(), "absent");
    let session = page.session().session_id().to_owned();
    while let Ok(event) = events.try_recv() {
        if let CdpMessage::Event {
            method, session_id, ..
        } = &*event
        {
            assert!(
                !(method.starts_with("Network.") && session_id.as_deref() == Some(&session)),
                "clearing headers re-enabled Network: {method}"
            );
        }
    }
    // Setting headers still works: Chrome applies them only with Network enabled.
    page.goto_with_headers(
        &format!("{}with-header", fixture.url),
        std::collections::HashMap::from([("X-Fixture-Secret".into(), "present".into())]),
    )
    .await
    .unwrap();
    assert!(page.text().await.unwrap().contains("present"));
    browser.close().await.unwrap();
}
