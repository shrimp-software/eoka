mod support;

use base64::Engine;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use support::{HttpFixture, Response};

#[tokio::test]
#[ignore = "requires Chrome; local origin and proxy only"]
async fn proxy_credentials_are_not_sent_to_origin_servers() {
    let leaked = Arc::new(AtomicBool::new(false));
    let seen = leaked.clone();
    let origin = HttpFixture::start(move |request| {
        let seen = seen.clone();
        async move {
            if request.headers.contains_key("authorization") {
                seen.store(true, Ordering::SeqCst);
            }
            let mut response = Response::html("authentication denied");
            response.status = "401 Unauthorized";
            response
                .headers
                .push(("WWW-Authenticate".into(), "Basic realm=\"origin\"".into()));
            response
        }
    })
    .await;
    let browser = eoka::Browser::launch_with(|c| {
        c.patch_binary = false;
        c.proxy_username = Some("fixture-user".into());
        c.proxy_password = Some("fixture-password".into());
        c.cdp_timeout = 5;
    })
    .await
    .unwrap();
    let page = browser.new_blank_page().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(8), page.goto(&origin.url))
        .await
        .unwrap()
        .unwrap();
    assert!(!leaked.load(Ordering::SeqCst));
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local proxy only"]
async fn explicit_proxy_challenge_receives_credentials() {
    let authorized = Arc::new(AtomicBool::new(false));
    let seen = authorized.clone();
    let proxy = HttpFixture::start(move |request| {
        let seen = seen.clone();
        async move {
            let expected = format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("fixture-user:fixture-password")
            );
            if request.headers.get("proxy-authorization") == Some(&expected) {
                if request.target.contains("eoka-fixture.invalid") {
                    seen.store(true, Ordering::SeqCst);
                }
                Response::html("<title>proxy success</title>")
            } else {
                let mut response = Response::html("authenticate proxy");
                response.status = "407 Proxy Authentication Required";
                response
                    .headers
                    .push(("Proxy-Authenticate".into(), "Basic realm=\"proxy\"".into()));
                response
            }
        }
    })
    .await;
    let browser = eoka::Browser::launch_with(|c| {
        c.patch_binary = false;
        c.proxy = Some(proxy.url.trim_end_matches('/').into());
        c.proxy_username = Some("fixture-user".into());
        c.proxy_password = Some("fixture-password".into());
        c.cdp_timeout = 5;
    })
    .await
    .unwrap();
    let page = browser
        .new_page("http://eoka-fixture.invalid/")
        .await
        .unwrap();
    assert_eq!(page.title().await.unwrap(), "proxy success");
    assert!(authorized.load(Ordering::SeqCst));
    browser.close().await.unwrap();
}
