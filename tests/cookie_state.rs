//! Browser-level cookie capture, restore and scoped export.

mod support;

use eoka::{BrowserSession, Page, SessionCookie};
use serde_json::{json, Value};

async fn names(page: &Page) -> Vec<String> {
    let mut names: Vec<_> = page
        .cookies()
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    names.sort();
    names
}

#[tokio::test]
#[ignore = "requires Chrome; local HTTP fixture"]
async fn capture_and_restore_keep_other_sites_cookies() {
    let server = support::HttpFixture::start(|_| async {
        support::Response::html("<!doctype html><title>t</title>")
    })
    .await;
    let port = server.url.rsplit(':').next().unwrap().trim_end_matches('/');
    let browser = support::browser().await;
    let sso = browser
        .new_page(&format!("http://localhost:{port}/"))
        .await
        .unwrap();
    sso.set_cookie("sso", "keep-me", None, None).await.unwrap();
    let app = browser
        .new_page(&format!("http://127.0.0.1:{port}/"))
        .await
        .unwrap();
    app.set_cookie("app", "1", None, None).await.unwrap();

    assert_eq!(names(&app).await, ["app"]);
    let all = app.all_cookies().await.unwrap();
    assert_eq!(all.len(), 2);

    let state = app.capture_state().await.unwrap();
    assert_eq!(state.cookies.len(), 2);

    app.set_cookie("added_later", "1", None, None)
        .await
        .unwrap();
    app.restore_state(&state).await.unwrap();
    assert_eq!(names(&sso).await, ["sso"]);
    assert_eq!(names(&app).await, ["app"], "the restore replaces the jar");

    let after = app.all_cookies().await.unwrap();
    for original in &state.cookies {
        let restored = after
            .iter()
            .find(|c| c.name == original.name)
            .expect("restored cookie");
        assert_eq!(restored.domain, original.domain, "{}", original.name);
        assert_eq!(restored.value, original.value, "{}", original.name);
        assert_eq!(restored.path, original.path, "{}", original.name);
    }
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local HTTP fixture"]
async fn exported_header_matches_what_the_browser_sends() {
    let server = support::HttpFixture::start(|request| async move {
        support::Response::html(
            request
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("cookie"))
                .map_or("", |(_, value)| value),
        )
    })
    .await;
    let port = server.url.rsplit(':').next().unwrap().trim_end_matches('/');
    let url = format!("http://127.0.0.1:{port}/app/page");
    let browser = support::browser().await;
    let page = browser.new_page(&url).await.unwrap();
    page.execute(
        "document.cookie='enc=tok%3Dabc; path=/';\
         document.cookie='spaced=a b,c; path=/';\
         document.cookie='scoped=1; path=/app'",
    )
    .await
    .unwrap();
    page.goto(&url).await.unwrap();
    page.wait_for("body", 2000).await.unwrap();
    let sent: String = page.evaluate("document.body.textContent").await.unwrap();

    let session = BrowserSession::new(
        page.all_cookies().await.unwrap(),
        String::new(),
        url.clone(),
    );
    let mut sent: Vec<&str> = sent.split("; ").collect();
    let header = session.cookie_header_for(&url);
    let mut exported: Vec<&str> = header.split("; ").collect();
    sent.sort();
    exported.sort();
    assert_eq!(exported, sent);
    assert!(exported.contains(&"enc=tok%3Dabc"));
    assert_eq!(
        session.cookie_header_for(&format!("http://localhost:{port}/")),
        ""
    );
    browser.close().await.unwrap();
}

fn cookie(name: &str, domain: &str) -> SessionCookie {
    SessionCookie {
        name: name.into(),
        value: "1".into(),
        domain: domain.into(),
        path: "/".into(),
        secure: false,
        http_only: false,
        same_site: None,
        expires: None,
    }
}

async fn jar(page: &Page) -> Vec<String> {
    let mut names: Vec<_> = page
        .all_cookies()
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    names.sort();
    names
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn all_cookies_reads_the_pages_own_browser_context() {
    let browser = support::browser().await;
    let default_page = browser.new_blank_page().await.unwrap();
    default_page
        .set_cookies_bulk(vec![cookie("default_context", "a.test")])
        .await
        .unwrap();

    let transport = default_page.session().transport();
    let context: Value = transport
        .send("Target.createBrowserContext", &json!({}))
        .await
        .unwrap();
    let target: Value = transport
        .send(
            "Target.createTarget",
            &json!({"url": "about:blank", "browserContextId": context["browserContextId"]}),
        )
        .await
        .unwrap();
    let other_page = browser
        .attach_page(target["targetId"].as_str().unwrap())
        .await
        .unwrap();
    other_page
        .set_cookies_bulk(vec![cookie("other_context", "b.test")])
        .await
        .unwrap();

    assert_eq!(jar(&default_page).await, ["default_context"]);
    assert_eq!(jar(&other_page).await, ["other_context"]);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local HTTP fixture"]
async fn restoring_into_a_live_session_deletes_nothing() {
    let server = support::HttpFixture::start(|_| async {
        support::Response::html("<!doctype html><title>t</title>")
    })
    .await;
    let browser = eoka::Browser::launch_with(|config| {
        config.chrome_path = std::env::var("EOKA_TEST_CHROME_PATH").ok();
        config.patch_binary = false;
        config.live_session = true;
    })
    .await
    .unwrap();
    let page = browser.new_page(&server.url).await.unwrap();
    page.set_cookies_bulk(vec![
        cookie("bank", "bank.test"),
        cookie("mail", "mail.test"),
    ])
    .await
    .unwrap();
    page.set_cookie("app", "1", None, None).await.unwrap();

    let mut state = page.capture_state().await.unwrap();
    state.cookies.retain(|c| c.name == "app");
    page.restore_state(&state).await.unwrap();

    assert_eq!(jar(&page).await, ["app", "bank", "mail"]);
    browser.close().await.unwrap();
}
