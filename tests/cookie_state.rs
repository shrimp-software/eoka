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
    let header = session.cookie_header_for(&url).unwrap();
    let mut exported: Vec<&str> = header.split("; ").collect();
    sent.sort();
    exported.sort();
    assert_eq!(exported, sent);
    assert!(exported.contains(&"enc=tok%3Dabc"));
    assert_eq!(
        session
            .cookie_header_for(&format!("http://localhost:{port}/"))
            .unwrap(),
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
        partition_key: None,
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

/// Requests seen by a fixture: path and Cookie header.
type Seen = std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>;

async fn recording_fixture(seen: Seen) -> support::HttpFixture {
    support::HttpFixture::start(move |request| {
        let seen = seen.clone();
        async move {
            let cookie = request.headers.get("cookie").cloned().unwrap_or_default();
            seen.lock().unwrap().push((request.target.clone(), cookie));
            if request.target.starts_with("/redirect") {
                let port = request.headers["host"]
                    .rsplit(':')
                    .next()
                    .unwrap()
                    .to_owned();
                return support::Response {
                    status: "302 Found",
                    headers: vec![(
                        "Location".into(),
                        format!("http://localhost:{port}/landing"),
                    )],
                    body: Vec::new(),
                    body_gate: None,
                };
            }
            support::Response::html("<!doctype html><title>t</title>")
        }
    })
    .await
}

fn state_for(url: &str, cookies: Vec<SessionCookie>) -> eoka::BrowserState {
    eoka::BrowserState {
        cookies,
        local_storage: Default::default(),
        session_storage: Default::default(),
        user_agent: String::new(),
        url: url.to_owned(),
    }
}

const CLIENT_HINTS: &str = "(async () => { const d = await navigator.userAgentData.getHighEntropyValues(['platformVersion', 'architecture', 'fullVersionList']); return JSON.stringify([navigator.userAgent, navigator.userAgentData.platform, d.platformVersion, d.architecture, d.fullVersionList]); })()";

/// Regression: restoring the same user agent sent a bare
/// `Emulation.setUserAgentOverride`, which dropped the fingerprint's
/// platform and client-hint metadata.
#[tokio::test]
#[ignore = "requires Chrome; local HTTP fixture"]
async fn restore_keeps_the_fingerprints_client_hints() {
    let server = support::HttpFixture::start(|_| async {
        support::Response::html("<!doctype html><title>t</title>")
    })
    .await;
    let browser = support::browser().await;
    let page = browser.new_page(&server.url).await.unwrap();
    let before: String = page.evaluate(CLIENT_HINTS).await.unwrap();
    let state = page.capture_state().await.unwrap();
    page.restore_state(&state).await.unwrap();
    let after: String = page.evaluate(CLIENT_HINTS).await.unwrap();
    assert_eq!(after, before);
    browser.close().await.unwrap();
}

/// A different saved user agent gets a full override derived from it, so
/// client hints agree with the restored user agent.
#[tokio::test]
#[ignore = "requires Chrome; local HTTP fixture"]
async fn restore_of_another_user_agent_sets_matching_client_hints() {
    let server = support::HttpFixture::start(|_| async {
        support::Response::html("<!doctype html><title>t</title>")
    })
    .await;
    let browser = support::browser().await;
    let page = browser.new_page(&server.url).await.unwrap();
    let mut state = page.capture_state().await.unwrap();
    let original = state.user_agent.clone();
    let version = original
        .split("Chrome/")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap();
    let major = version.split('.').next().unwrap();
    let other_version = format!("{}.0.1.2", major.parse::<u32>().unwrap() - 1);
    state.user_agent = original.replace(version, &other_version);
    page.restore_state(&state).await.unwrap();
    let hints: Value =
        serde_json::from_str(&page.evaluate::<String>(CLIENT_HINTS).await.unwrap()).unwrap();
    assert_eq!(hints[0], state.user_agent.as_str());
    assert!(hints[1].as_str().is_some_and(|p| !p.is_empty()), "{hints}");
    assert!(
        hints[4].to_string().contains(&other_version),
        "full version list follows the restored user agent: {hints}"
    );
    browser.close().await.unwrap();
}

/// Regression: restore navigated to the saved URL before replacing cookies,
/// so the first request carried the old jar.
#[tokio::test]
#[ignore = "requires Chrome; local HTTP fixture"]
async fn restore_sets_cookies_before_loading_the_saved_url() {
    let seen = Seen::default();
    let server = recording_fixture(seen.clone()).await;
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    let url = format!("{}account", server.url);
    let mut auth = cookie("auth", "127.0.0.1");
    auth.value = "restored".into();
    page.restore_state(&state_for(&url, vec![auth]))
        .await
        .unwrap();
    let seen = seen.lock().unwrap().clone();
    let loads: Vec<_> = seen.iter().filter(|(path, _)| path == "/account").collect();
    assert!(!loads.is_empty());
    for (_, cookie) in loads {
        assert_eq!(cookie, "auth=restored");
    }
    browser.close().await.unwrap();
}

/// Regression: storage was written to whatever origin the saved URL
/// redirected to.
#[tokio::test]
#[ignore = "requires Chrome; local HTTP fixture"]
async fn restore_refuses_to_write_storage_to_another_origin() {
    let server = recording_fixture(Seen::default()).await;
    let port = server.url.rsplit(':').next().unwrap().trim_end_matches('/');
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    let mut state = state_for(&format!("http://127.0.0.1:{port}/redirect"), vec![]);
    state.local_storage.insert("token".into(), "secret".into());
    let error = page.restore_state(&state).await.unwrap_err();
    assert!(error.to_string().contains("storage"), "{error}");
    assert!(page.url().await.unwrap().starts_with("http://localhost:"));
    let leaked: Option<String> = page
        .evaluate("localStorage.getItem('token')")
        .await
        .unwrap();
    assert_eq!(leaked, None);

    // Nothing to write: a redirect is not an error.
    state.local_storage.clear();
    page.restore_state(&state).await.unwrap();
    browser.close().await.unwrap();
}

/// Regression: live sessions documented a merge but cleared origin storage.
#[tokio::test]
#[ignore = "requires Chrome; local HTTP fixture"]
async fn restoring_into_a_live_session_keeps_other_storage() {
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
    page.execute("localStorage.setItem('theirs', '1'); sessionStorage.setItem('tab', '1')")
        .await
        .unwrap();
    let mut state = page.capture_state().await.unwrap();
    state.local_storage.clear();
    state.session_storage.clear();
    state.local_storage.insert("ours".into(), "2".into());
    page.restore_state(&state).await.unwrap();
    let kept: Vec<Option<String>> = page
        .evaluate("[localStorage.getItem('theirs'), localStorage.getItem('ours'), sessionStorage.getItem('tab')]")
        .await
        .unwrap();
    assert_eq!(kept, [Some("1".into()), Some("2".into()), Some("1".into())]);
    browser.close().await.unwrap();
}

/// A non-live restore replaces the saved origin's storage.
#[tokio::test]
#[ignore = "requires Chrome; local HTTP fixture"]
async fn restore_replaces_storage_of_the_saved_origin() {
    let server = support::HttpFixture::start(|_| async {
        support::Response::html("<!doctype html><title>t</title>")
    })
    .await;
    let browser = support::browser().await;
    let page = browser.new_page(&server.url).await.unwrap();
    page.execute("localStorage.setItem('token', 'old'); localStorage.setItem('stale', '1')")
        .await
        .unwrap();
    let mut state = page.capture_state().await.unwrap();
    state.local_storage.remove("stale");
    state.local_storage.insert("token".into(), "new".into());
    // Larger than the 1 MiB function-argument limit.
    state
        .local_storage
        .insert("big".into(), "x".repeat(3 * 1024 * 1024));
    page.restore_state(&state).await.unwrap();
    let stored: Vec<Option<String>> = page
        .evaluate("[localStorage.getItem('token'), localStorage.getItem('stale'), String(localStorage.getItem('big')?.length)]")
        .await
        .unwrap();
    assert_eq!(
        stored,
        [
            Some("new".into()),
            None,
            Some((3 * 1024 * 1024).to_string())
        ]
    );
    browser.close().await.unwrap();
}

async fn resolver_browser() -> eoka::Browser {
    eoka::Browser::launch_with(|c| {
        c.chrome_path = std::env::var("EOKA_TEST_CHROME_PATH").ok();
        c.patch_binary = false;
        c.strip_x_client_data = false;
        c.cdp_timeout = 5;
        c.extra_args
            .push("--host-resolver-rules=MAP *.test 127.0.0.1".into());
    })
    .await
    .unwrap()
}

/// Checked rather than changed: Chrome keeps a dotless `domain` host-only, so
/// `set_cookies_bulk` does not widen host-only or `__Host-` cookies to
/// subdomains when it passes `domain`.
#[tokio::test]
#[ignore = "requires Chrome; local HTTP fixture"]
async fn bulk_import_keeps_host_only_cookies_host_only() {
    let server = support::HttpFixture::start(|_| async {
        support::Response::html("<!doctype html><title>t</title>")
    })
    .await;
    let port = server.url.rsplit(':').next().unwrap().trim_end_matches('/');
    let browser = resolver_browser().await;
    let page = browser
        .new_page(&format!("http://app.site.test:{port}/"))
        .await
        .unwrap();
    page.execute(
        "document.cookie='host=1; path=/'; document.cookie='wide=1; domain=site.test; path=/'",
    )
    .await
    .unwrap();
    let mut host_prefixed = cookie("__Host-id", "app.site.test");
    host_prefixed.secure = true;
    let saved: Vec<_> = page
        .all_cookies()
        .await
        .unwrap()
        .into_iter()
        .chain([host_prefixed])
        .collect();
    page.clear_all_cookies().await.unwrap();
    page.set_cookies_bulk(saved.clone()).await.unwrap();

    let mut after = page.all_cookies().await.unwrap();
    after.sort_by(|a, b| a.name.cmp(&b.name));
    let domains: Vec<_> = after
        .iter()
        .map(|c| (c.name.as_str(), c.domain.as_str()))
        .collect();
    assert_eq!(
        domains,
        [
            ("__Host-id", "app.site.test"),
            ("host", "app.site.test"),
            ("wide", ".site.test")
        ]
    );
    page.goto(&format!("http://sub.app.site.test:{port}/"))
        .await
        .unwrap();
    let seen: String = page.evaluate("document.cookie").await.unwrap();
    assert_eq!(seen, "wide=1");
    browser.close().await.unwrap();
}

/// Regression: capture dropped the CHIPS partition key, so a restore turned a
/// partitioned cookie into an unpartitioned one.
#[tokio::test]
#[ignore = "requires Chrome"]
async fn partitioned_cookies_keep_their_partition() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    let mut partitioned = cookie("chips", "embed.test");
    partitioned.secure = true;
    partitioned.same_site = Some("None".into());
    partitioned.partition_key = Some(eoka::session::CookiePartitionKey {
        top_level_site: "https://top.test".into(),
        has_cross_site_ancestor: false,
    });
    page.set_cookies_bulk(vec![partitioned.clone(), cookie("plain", "embed.test")])
        .await
        .unwrap();

    let saved = page.all_cookies().await.unwrap();
    let chips = saved.iter().find(|c| c.name == "chips").unwrap();
    assert_eq!(chips.partition_key, partitioned.partition_key);
    assert_eq!(
        saved
            .iter()
            .find(|c| c.name == "plain")
            .unwrap()
            .partition_key,
        None
    );

    page.clear_all_cookies().await.unwrap();
    page.set_cookies_bulk(saved).await.unwrap();
    let restored = page.all_cookies().await.unwrap();
    let chips = restored.iter().find(|c| c.name == "chips").unwrap();
    assert_eq!(chips.partition_key, partitioned.partition_key);
    browser.close().await.unwrap();
}
