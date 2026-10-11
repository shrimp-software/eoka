mod support;

use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use support::{HttpFixture, Response};
use tokio::sync::{mpsc, Semaphore};

#[tokio::test]
#[ignore = "requires Chrome; local request lifecycle fixtures"]
async fn native_idle_tracks_bodies_xhr_redirects_and_concurrent_waiters() {
    let body_gate = Arc::new(Semaphore::new(0));
    let gate = body_gate.clone();
    let (started, mut received) = mpsc::channel(32);
    let fixture = HttpFixture::start(move |request| {
        let (gate, started) = (gate.clone(), started.clone());
        async move {
            let mut response = Response::html("<div id='ready'>ready</div>");
            if request.target.starts_with("/slow") {
                response.body_gate = Some(gate);
                started.send(request.target).await.unwrap();
            } else if request.target == "/redirect" {
                response.status = "302 Found";
                response
                    .headers
                    .push(("Location".into(), "/slow-redirect".into()));
            }
            response
        }
    })
    .await;
    let browser = support::browser().await;
    let page = browser.new_page(&fixture.url).await.unwrap();
    page.wait_for_network_idle(20, 3000).await.unwrap();
    let before: String = page.evaluate("fetch.toString()").await.unwrap();
    page.execute("void fetch('/slow-body').then(r=>r.text())")
        .await
        .unwrap();
    received.recv().await.unwrap();
    assert!(matches!(
        page.wait_for_network_idle(10, 80).await,
        Err(eoka::Error::Timeout(_))
    ));
    page.enable_request_capture().await.unwrap();
    page.disable_request_capture().await.unwrap();
    assert!(page.wait_for_network_idle(10, 80).await.is_err());
    let a = tokio::spawn({
        let page = page.clone();
        async move { page.wait_for_network_idle(50, 3000).await }
    });
    let b = tokio::spawn({
        let page = page.clone();
        async move { page.wait_for_network_idle(100, 3000).await }
    });
    body_gate.add_permits(1);
    a.await.unwrap().unwrap();
    b.await.unwrap().unwrap();
    assert_eq!(
        page.evaluate::<String>("fetch.toString()").await.unwrap(),
        before
    );

    // Reusing an XHR must not accumulate completion listeners or negative counts.
    page.execute("window.xhr = new XMLHttpRequest()")
        .await
        .unwrap();
    for _ in 0..3 {
        page.execute("new Promise((resolve,reject)=>{xhr.onload=resolve;xhr.onerror=reject;xhr.open('GET','/fast');xhr.send();})").await.unwrap();
        page.wait_for_network_idle(10, 1000).await.unwrap();
    }
    page.execute("void fetch('/redirect').then(r=>r.text())")
        .await
        .unwrap();
    assert_eq!(received.recv().await.unwrap(), "/slow-redirect");
    assert!(page.wait_for_network_idle(10, 80).await.is_err());
    body_gate.add_permits(1);
    page.wait_for_network_idle(10, 1000).await.unwrap();

    page.execute("window.abort = new AbortController(); void fetch('/slow-abort', {signal:abort.signal}).then(r=>r.text()).catch(()=>{});").await.unwrap();
    received.recv().await.unwrap();
    page.execute("abort.abort()").await.unwrap();
    page.wait_for_network_idle(10, 2000).await.unwrap();
    body_gate.add_permits(1);
    browser.close().await.unwrap();
    assert!(page.wait_for_network_idle(0, 500).await.is_err());
}

#[tokio::test]
#[ignore = "requires Chrome; local attachment fixture"]
async fn attached_documents_require_navigation_and_managed_reattach_reuses_tracking() {
    let fixture = HttpFixture::start(|_| async { Response::html("<div>ready</div>") }).await;
    let browser = support::browser().await;
    let managed = browser.new_page(&fixture.url).await.unwrap();
    let duplicate = browser.attach_page(managed.target_id()).await.unwrap();
    duplicate.wait_for_network_idle(10, 1000).await.unwrap();
    let managed_id = managed.target_id().to_owned();
    drop(duplicate);
    drop(managed);
    let managed = browser.attach_page(&managed_id).await.unwrap();
    managed.wait_for_network_idle(10, 1000).await.unwrap();
    let target: Value = managed
        .session()
        .transport()
        .send("Target.createTarget", &json!({"url":fixture.url}))
        .await
        .unwrap();
    // Establish the fixture's loaded document before Eoka installs a monitor.
    // Target.createTarget acknowledges before its initial navigation completes.
    let transport = managed.session().transport();
    let raw: Value = transport
        .send(
            "Target.attachToTarget",
            &json!({"targetId":target["targetId"],"flatten":true}),
        )
        .await
        .unwrap();
    let raw_session = raw["sessionId"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let result: Value = transport.send_to_session(raw_session, "Runtime.evaluate", &json!({
                "expression":format!("location.href === {} && document.readyState === 'complete'", serde_json::to_string(&fixture.url).unwrap()),
                "returnByValue":true
            })).await.unwrap();
            if result["result"]["value"] == true { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("untracked document loaded");
    transport
        .send::<_, Value>("Target.detachFromTarget", &json!({"sessionId":raw_session}))
        .await
        .unwrap();
    let attached = browser
        .attach_page(target["targetId"].as_str().unwrap())
        .await
        .unwrap();
    assert!(attached
        .wait_for_network_idle(0, 1000)
        .await
        .unwrap_err()
        .to_string()
        .contains("full navigation"));
    attached
        .goto(&format!("{}#same-document", fixture.url))
        .await
        .unwrap();
    assert!(attached
        .wait_for_network_idle(0, 1000)
        .await
        .unwrap_err()
        .to_string()
        .contains("full navigation"));
    attached.reload().await.unwrap();
    // Wait for the actual new document; reload itself only acknowledges dispatch.
    attached.execute("new Promise(resolve=>{if(document.readyState==='complete')resolve();else addEventListener('load',resolve,{once:true});})").await.unwrap();
    attached.wait_for_network_idle(20, 2000).await.unwrap();
    browser.close_tab(attached.target_id()).await.unwrap();
    assert!(attached.wait_for_network_idle(0, 500).await.is_err());
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local quiet-period fixture"]
async fn a_new_request_restarts_the_quiet_period_and_cancelled_waits_do_not_stop_tracking() {
    let gate = Arc::new(Semaphore::new(0));
    let held = gate.clone();
    let (started, mut received) = mpsc::channel(4);
    let fixture = HttpFixture::start(move |request| {
        let (held, started) = (held.clone(), started.clone());
        async move {
            let mut response = Response::html("ready");
            if request.target.starts_with("/slow") {
                started.send(()).await.unwrap();
                response.body_gate = Some(held);
            }
            response
        }
    })
    .await;
    let browser = support::browser().await;
    let page = browser.new_page(&fixture.url).await.unwrap();
    let wait = tokio::spawn({
        let page = page.clone();
        async move { page.wait_for_network_idle(300, 3000).await }
    });
    page.execute("void fetch('/slow-first').then(r=>r.text())")
        .await
        .unwrap();
    received.recv().await.unwrap();
    wait.abort();
    assert!(wait.await.unwrap_err().is_cancelled());
    assert!(page.wait_for_network_idle(10, 80).await.is_err());
    gate.add_permits(1);
    page.wait_for_network_idle(10, 1000).await.unwrap();
    let mut wait = tokio::spawn({
        let page = page.clone();
        async move { page.wait_for_network_idle(300, 3000).await }
    });
    tokio::time::sleep(Duration::from_millis(80)).await; // Deliberately enter the measured quiet period.
    page.execute("void fetch('/slow-second').then(r=>r.text())")
        .await
        .unwrap();
    received.recv().await.unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(320), &mut wait)
        .await
        .is_err());
    gate.add_permits(1);
    wait.await.unwrap().unwrap();
    browser.close().await.unwrap();
}
