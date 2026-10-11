mod support;

use std::{collections::HashMap, sync::Arc, time::Duration};
use support::{HttpFixture, Response};
use tokio::sync::{mpsc, Semaphore};

#[tokio::test]
#[ignore = "requires Chrome; local header fixture"]
async fn headers_clear_after_success_error_timeout_and_task_abort() {
    let (started, mut requests) = mpsc::channel(32);
    let gate = Arc::new(Semaphore::new(0));
    let held = gate.clone();
    let fixture = HttpFixture::start(move |request| {
        let (started, held) = (started.clone(), held.clone());
        async move {
            if request.target.starts_with("/slow") {
                started.send(request.clone()).await.unwrap();
                held.acquire().await.unwrap().forget();
            }
            Response::html(
                request
                    .headers
                    .get("x-fixture-secret")
                    .map(String::as_str)
                    .unwrap_or("absent"),
            )
        }
    })
    .await;
    let browser = support::browser().await;
    let page = browser.new_page(&fixture.url).await.unwrap();
    let headers = || HashMap::from([("X-Fixture-Secret".into(), "fixture-only".into())]);
    page.goto_with_headers(&fixture.url, headers())
        .await
        .unwrap();
    assert!(page.text().await.unwrap().contains("fixture-only"));
    assert_eq!(
        page.evaluate::<String>("fetch('/check').then(r=>r.text())")
            .await
            .unwrap(),
        "absent"
    );
    assert!(page
        .goto_with_headers("invalid-protocol://failure", headers())
        .await
        .is_err());
    page.goto(&fixture.url).await.unwrap();
    assert_eq!(page.text().await.unwrap().trim(), "absent");

    for abort in [false, true] {
        let task_page = page.clone();
        let url = format!("{}slow-{abort}", fixture.url);
        let task_headers = headers();
        let task = tokio::spawn(async move {
            if abort {
                task_page
                    .goto_with_headers(&url, task_headers)
                    .await
                    .unwrap();
            } else {
                assert!(tokio::time::timeout(
                    Duration::from_millis(200),
                    task_page.goto_with_headers(&url, task_headers)
                )
                .await
                .is_err());
            }
        });
        let request = tokio::time::timeout(Duration::from_secs(5), requests.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            request.headers.get("x-fixture-secret").unwrap(),
            "fixture-only"
        );
        if abort {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            task.await.unwrap();
        }
        gate.add_permits(1);
        // No extra cleanup API or delay: the first command must fence restoration.
        page.goto(&fixture.url).await.unwrap();
        assert_eq!(page.text().await.unwrap().trim(), "absent");
    }
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local concurrent header fixture"]
async fn duplicate_attachments_serialize_navigation_and_header_changes() {
    let gate = Arc::new(Semaphore::new(0));
    let held = gate.clone();
    let (started, mut received) = mpsc::channel(8);
    let fixture = HttpFixture::start(move |request| {
        let (held, started) = (held.clone(), started.clone());
        async move {
            if request.target == "/slow" {
                started.send(()).await.unwrap();
                held.acquire().await.unwrap().forget();
            }
            Response::html(
                request
                    .headers
                    .get("x-fixture-secret")
                    .map(String::as_str)
                    .unwrap_or("absent"),
            )
        }
    })
    .await;
    let browser = support::browser().await;
    let page = browser.new_page(&fixture.url).await.unwrap();
    let duplicate = browser.attach_page(page.target_id()).await.unwrap();
    let first = tokio::spawn({
        let page = page.clone();
        let url = format!("{}slow", fixture.url);
        async move {
            page.goto_with_headers(
                &url,
                HashMap::from([("X-Fixture-Secret".into(), "temporary".into())]),
            )
            .await
        }
    });
    received.recv().await.unwrap();
    let mut second = tokio::spawn({
        let url = fixture.url.clone();
        async move { duplicate.goto(&url).await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut second)
            .await
            .is_err()
    );
    gate.add_permits(1);
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(page.text().await.unwrap().trim(), "absent");
    browser.close().await.unwrap();
}
