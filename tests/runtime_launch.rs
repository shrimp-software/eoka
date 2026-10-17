#![cfg(unix)]

use eoka::{Browser, StealthConfig};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("eoka-rt-{:08x}", fastrand::u32(..)));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

async fn failed_launch(root: &Path, sandbox: bool, caller_profile: bool) {
    let runtime = root.join("runtime");
    let profile = root.join("caller-profile");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("keep"), "caller data").unwrap();
    if caller_profile {
        std::fs::write(
            profile.join("DevToolsActivePort"),
            "9\n/devtools/browser/stale\n",
        )
        .unwrap();
    }
    let source = root.join("chrome-fixture");
    let evidence = root.join("launch.txt");
    let body = format!("#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'Chrome 155.0.8059.39\\n'; exit 0; fi\nprintf '%s\\n' \"$TMPDIR\" \"$TMP\" \"$TEMP\" \"$@\" > '{}'\nprintf 'fixture launch failure\\n' >&2\nexit 23\n", evidence.display());
    executable(&source, &body);
    let config = StealthConfig {
        chrome_path: Some(source.to_string_lossy().into_owned()),
        runtime_dir: Some(runtime.to_string_lossy().into_owned()),
        sandbox,
        patch_binary: false,
        user_data_dir: caller_profile.then(|| profile.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let error = Browser::launch_with_config(config)
        .await
        .err()
        .expect("fixture must fail");
    assert!(
        error.to_string().contains("fixture launch failure"),
        "{error}"
    );
    let evidence = std::fs::read_to_string(evidence).unwrap();
    let lines: Vec<_> = evidence.lines().collect();
    let child = Path::new(lines[0]);
    assert_eq!(
        std::fs::canonicalize(child.parent().unwrap()).unwrap(),
        std::fs::canonicalize(&runtime).unwrap()
    );
    assert_eq!(lines[0], lines[1]);
    assert_eq!(lines[0], lines[2]);
    assert!(!child.exists(), "failed launch leaked runtime");
    assert_eq!(lines.contains(&"--no-sandbox"), !sandbox);
    let launched_profile = lines
        .iter()
        .find_map(|arg| arg.strip_prefix("--user-data-dir="))
        .unwrap();
    if caller_profile {
        assert_eq!(Path::new(launched_profile), profile);
    } else {
        assert_eq!(Path::new(launched_profile), child.join("profile"));
    }
    assert!(runtime.exists());
    assert_eq!(std::fs::read_to_string(source).unwrap(), body);
    assert_eq!(
        std::fs::read_to_string(profile.join("keep")).unwrap(),
        "caller data"
    );
}

#[tokio::test]
async fn failed_launch_preserves_caller_data_and_matches_sandbox_policy() {
    for sandbox in [false, true] {
        for caller_profile in [false, true] {
            let root = Directory::new();
            failed_launch(&root.0, sandbox, caller_profile).await;
        }
    }
}

#[tokio::test]
async fn failed_launch_preserves_aliased_runtime_parent() {
    let root = Directory::new();
    let alias = root.0.join("alias");
    std::os::unix::fs::symlink(&root.0, &alias).unwrap();
    failed_launch(&alias, false, true).await;
}

#[tokio::test]
async fn explicit_runtime_ignores_unusable_default_temp() {
    if let Some(root) = std::env::var_os("EOKA_RUNTIME_TEMP_FIXTURE") {
        failed_launch(Path::new(&root), false, false).await;
        return;
    }
    let root = Directory::new();
    let unusable = root.0.join("not-a-directory");
    std::fs::write(&unusable, "occupied").unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "explicit_runtime_ignores_unusable_default_temp",
            "--nocapture",
        ])
        .env("EOKA_RUNTIME_TEMP_FIXTURE", &root.0)
        .env("TMPDIR", &unusable)
        .env("TMP", &unusable)
        .env("TEMP", &unusable)
        .status()
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn invalid_explicit_executables_do_not_create_runtime_or_touch_active_profile() {
    let root = Directory::new();
    let profile = root.0.join("profile");
    std::fs::create_dir(&profile).unwrap();
    std::os::unix::fs::symlink(
        format!("fixture-{}", std::process::id()),
        profile.join("SingletonLock"),
    )
    .unwrap();
    std::fs::write(profile.join("keep"), "keep").unwrap();
    let not_executable = root.0.join("not-executable");
    std::fs::write(&not_executable, "not a browser").unwrap();
    std::fs::set_permissions(&not_executable, std::fs::Permissions::from_mode(0o600)).unwrap();
    for path in [
        root.0.join("missing"),
        root.0.clone(),
        not_executable,
        PathBuf::from("relative-chrome"),
    ] {
        let runtime = root.0.join("runtime");
        let error = Browser::launch_with_config(StealthConfig {
            chrome_path: Some(path.to_string_lossy().into_owned()),
            runtime_dir: Some(runtime.to_string_lossy().into_owned()),
            user_data_dir: Some(profile.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await
        .err()
        .expect("explicit invalid executable must fail");
        assert!(!error.to_string().contains("already running"), "{error}");
        assert!(!runtime.exists());
        assert_eq!(
            std::fs::read_to_string(profile.join("keep")).unwrap(),
            "keep"
        );
        assert!(std::fs::symlink_metadata(profile.join("SingletonLock")).is_ok());
    }
}

#[tokio::test]
#[ignore = "requires Chrome; verifies explicit binary and owned runtime cleanup with a retained Page"]
async fn owned_close_and_drop_clean_runtime_with_live_page_handles() {
    let chrome = eoka::stealth::find_chrome().unwrap();
    let root = Directory::new();
    for close in [false, true] {
        let browser = Browser::launch_with_config(StealthConfig {
            chrome_path: Some(chrome.to_string_lossy().into_owned()),
            runtime_dir: Some(root.0.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await
        .unwrap();
        assert!(browser.owns_process());
        let page = browser.new_blank_page().await.unwrap();
        let children: Vec<_> = std::fs::read_dir(&root.0).unwrap().collect();
        assert_eq!(children.len(), 1);
        if close {
            browser.close().await.unwrap();
        } else {
            drop(browser);
        }
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
        drop(page);
    }
}

#[tokio::test]
async fn explicit_policy_rejects_active_profile_and_sandbox_extra_args() {
    let root = Directory::new();
    let profile = root.0.join("profile");
    std::fs::create_dir(&profile).unwrap();
    std::os::unix::fs::symlink(
        format!("fixture-{}", std::process::id()),
        profile.join("SingletonLock"),
    )
    .unwrap();
    let source = root.0.join("chrome");
    executable(&source, "#!/bin/sh\nexit 99\n");
    for policy in 0..3 {
        let config = StealthConfig {
            chrome_path: (policy == 0).then(|| source.to_string_lossy().into_owned()),
            runtime_dir: (policy == 1)
                .then(|| root.0.join("runtime").to_string_lossy().into_owned()),
            sandbox: policy == 2,
            user_data_dir: Some(profile.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let error = Browser::launch_with_config(config)
            .await
            .err()
            .expect("active profile policy must fail");
        assert!(error.to_string().contains("already running"), "{error}");
        assert!(std::fs::symlink_metadata(profile.join("SingletonLock")).is_ok());
    }
    for sandbox in [false, true] {
        let error = Browser::launch_with_config(StealthConfig {
            chrome_path: Some(source.to_string_lossy().into_owned()),
            runtime_dir: Some(root.0.join("unused-runtime").to_string_lossy().into_owned()),
            sandbox,
            extra_args: vec!["--no-sandbox".into()],
            ..Default::default()
        })
        .await
        .err()
        .expect("sandbox flags must not bypass config");
        assert!(
            error.to_string().contains("StealthConfig.sandbox"),
            "{error}"
        );
        assert!(!root.0.join("unused-runtime").exists());
    }
}

#[tokio::test]
async fn cancellation_cleans_process_and_runtime_before_and_during_websocket_handoff() {
    for phase in 0..3 {
        let websocket_handoff = phase == 2;
        let root = Directory::new();
        let runtime = root.0.join("runtime");
        let source = root.0.join("chrome");
        let pid_file = root.0.join("pid");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let endpoint = if websocket_handoff {
            format!("printf 'DevTools listening on ws://127.0.0.1:{port}/devtools/browser/fixture\\n' >&2\n")
        } else {
            String::new()
        };
        let version_command = if phase == 0 {
            format!(
                "printf '%s\\n' \"$$\" > '{}'; exec /bin/sleep 30",
                pid_file.display()
            )
        } else {
            "printf 'Chrome 155.0.8059.39\\n'; exit 0".into()
        };
        executable(&source, &format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then {}; fi\nprintf '%s\\n' \"$$\" > '{}'\n{}exec /bin/sleep 30\n",
            version_command, pid_file.display(), endpoint,
        ));
        let config = StealthConfig {
            chrome_path: Some(source.to_string_lossy().into_owned()),
            runtime_dir: Some(runtime.to_string_lossy().into_owned()),
            patch_binary: false,
            ..Default::default()
        };
        let launch = tokio::spawn(Browser::launch_with_config(config));
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !pid_file.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let socket = if websocket_handoff {
            Some(
                tokio::time::timeout(std::time::Duration::from_secs(3), listener.accept())
                    .await
                    .unwrap()
                    .unwrap(),
            )
        } else {
            None
        };
        launch.abort();
        assert!(matches!(launch.await, Err(error) if error.is_cancelled()));
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while std::fs::read_dir(&runtime).unwrap().next().is_some() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let pid = std::fs::read_to_string(pid_file).unwrap();
        let alive = Command::new("kill")
            .args(["-0", pid.trim()])
            .output()
            .unwrap()
            .status
            .success();
        assert!(!alive, "cancelled launch retained its owned process");
        drop(socket);
    }
}

#[tokio::test]
#[ignore = "requires Chrome; verifies simultaneous owned runtime and patch isolation"]
async fn concurrent_owned_launches_keep_independent_runtime_state() {
    let chrome = eoka::stealth::find_chrome().unwrap();
    let root = Directory::new();
    let config = StealthConfig {
        chrome_path: Some(chrome.to_string_lossy().into_owned()),
        runtime_dir: Some(root.0.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let (first, second) = tokio::join!(
        Browser::launch_with_config(config.clone()),
        Browser::launch_with_config(config),
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert!(first.owns_process() && second.owns_process());
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 2);
    let page = second.new_blank_page().await.unwrap();
    first.close().await.unwrap();
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
    assert_eq!(
        page.evaluate::<String>("'other-browser-still-usable'")
            .await
            .unwrap(),
        "other-browser-still-usable"
    );
    second.close().await.unwrap();
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
}

#[tokio::test]
#[ignore = "requires headed Chrome and DISPLAY; verifies persistent profile restarts"]
async fn persistent_profile_restarts_preserve_storage_and_caller_files() {
    let chrome = eoka::stealth::find_chrome().unwrap();
    for headless in [true, false] {
        let root = Directory::new();
        let profile = root.0.join("profile");
        let runtime = root.0.join("runtime");
        let document = root.0.join("fixture.html");
        std::fs::create_dir(&profile).unwrap();
        std::fs::write(profile.join("keep"), "caller data").unwrap();
        std::fs::write(
            profile.join("DevToolsActivePort"),
            "9\n/devtools/browser/stale\n",
        )
        .unwrap();
        std::fs::write(&document, "<title>Persistent profile fixture</title>").unwrap();
        let config = StealthConfig {
            chrome_path: Some(chrome.to_string_lossy().into_owned()),
            runtime_dir: Some(runtime.to_string_lossy().into_owned()),
            user_data_dir: Some(profile.to_string_lossy().into_owned()),
            headless,
            ..Default::default()
        };
        for launch in 0..2 {
            let browser = Browser::launch_with_config(config.clone()).await.unwrap();
            assert!(browser.owns_process(), "restart must own a new Chrome");
            let page = browser
                .new_page(&format!("file://{}", document.display()))
                .await
                .unwrap();
            let retained = page
                .evaluate::<Option<String>>("localStorage.getItem('restart-marker')")
                .await
                .unwrap();
            assert_eq!(retained.as_deref(), (launch > 0).then_some("retained"));
            page.execute("localStorage.setItem('restart-marker', 'retained')")
                .await
                .unwrap();
            browser.close().await.unwrap();
            assert_eq!(
                std::fs::read_to_string(profile.join("keep")).unwrap(),
                "caller data"
            );
            assert_eq!(std::fs::read_dir(&runtime).unwrap().count(), 0);
        }
    }
}
