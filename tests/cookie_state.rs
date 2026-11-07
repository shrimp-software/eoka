//! Browser-level cookie capture, restore and scoped export.

mod support;

use eoka::{BrowserSession, Page};

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
