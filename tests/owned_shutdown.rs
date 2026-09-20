use eoka::{Browser, SessionCookie, StealthConfig};
use std::path::PathBuf;

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "eoka-shutdown-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
#[ignore = "requires headed Chrome and DISPLAY; verifies owned live shutdown and profile persistence"]
async fn owned_live_close_flushes_netlog_and_persistent_cookies() {
    let directory = Directory::new();
    let profile = directory.0.join("profile");
    let netlog = directory.0.join("netlog.json");
    let launch = |log: bool| {
        let profile = profile.clone();
        let netlog = netlog.clone();
        Browser::launch_with(move |c| {
            c.live_session = true;
            c.headless = false;
            c.user_data_dir = Some(profile.to_string_lossy().into_owned());
            if log {
                c.extra_args
                    .push(format!("--log-net-log={}", netlog.display()));
            }
        })
    };
    let browser = launch(true).await.unwrap();
    let page = browser.new_blank_page().await.unwrap();
    page.set_cookies_bulk(vec![SessionCookie {
        name: "shutdown-fixture".into(),
        value: "fixture-only".into(),
        domain: "fixture.invalid".into(),
        path: "/".into(),
        secure: true,
        http_only: true,
        same_site: Some("Lax".into()),
        expires: Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs_f64()
                + 3600.0,
        ),
    }])
    .await
    .unwrap();
    let before: serde_json::Value = page
        .session()
        .send(
            "Network.getCookies",
            &serde_json::json!({"urls":["https://fixture.invalid/"]}),
        )
        .await
        .unwrap();
    assert_eq!(before["cookies"][0]["value"], "fixture-only");
    browser.close().await.unwrap();
    let document = serde_json::from_slice::<serde_json::Value>(&std::fs::read(&netlog).unwrap());
    assert!(
        document.is_ok(),
        "owned shutdown must flush a complete NetLog: {:?}",
        document.err()
    );
    let browser = launch(false).await.unwrap();
    let page = browser.new_blank_page().await.unwrap();
    let cookies: serde_json::Value = page
        .session()
        .send(
            "Network.getCookies",
            &serde_json::json!({"urls":["https://fixture.invalid/"]}),
        )
        .await
        .unwrap();
    browser.close().await.unwrap();
    assert_eq!(cookies["cookies"][0]["value"], "fixture-only");
}

#[tokio::test]
#[ignore = "requires Chrome; closes only the owned browser"]
async fn borrowed_close_never_terminates_owner_even_with_nonlive_config() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let owner = Browser::launch_with(|c| {
        c.live_session = true;
        c.extra_args.push(format!("--remote-debugging-port={port}"));
    })
    .await
    .unwrap();
    let borrowed = Browser::connect_port_with_config(
        port,
        StealthConfig {
            live_session: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    borrowed.close().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let version = owner.version().await;
    owner.close().await.unwrap();
    assert!(
        version.is_ok(),
        "borrowed close terminated the owned browser"
    );
}
