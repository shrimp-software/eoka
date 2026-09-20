use eoka::Browser;

#[tokio::test]
#[ignore = "requires headed Chrome and DISPLAY; disposable profiles"]
async fn explicit_and_live_debug_ports_ignore_stale_profile_discovery() {
    for explicit in [true, false] {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let profile = std::env::temp_dir().join(format!(
            "eoka-port-fixture-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&profile).unwrap();
        std::fs::write(
            profile.join("DevToolsActivePort"),
            "9\n/devtools/browser/stale\n",
        )
        .unwrap();
        let browser = Browser::launch_with(|config| {
            config.live_session = true;
            config.headless = false;
            config.user_data_dir = Some(profile.to_string_lossy().into_owned());
            config
                .extra_args
                .extend(["--no-first-run".into(), "--no-default-browser-check".into()]);
            if explicit {
                config
                    .extra_args
                    .push(format!("--remote-debugging-port={port}"));
            }
        })
        .await
        .unwrap();
        let page = browser.new_page("about:blank").await.unwrap();
        assert!(!page
            .evaluate_sync::<bool>("navigator.webdriver")
            .await
            .unwrap());
        if explicit {
            assert!(eoka::cdp::discover::discover_browser_ws("127.0.0.1", port).is_ok());
        }
        browser.close().await.unwrap();
        std::fs::remove_dir_all(profile).unwrap();
    }
}
