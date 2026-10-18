//! Chrome launch preparation, profile ownership and identity selection.

use crate::cdp::transport::{
    chrome_version_output, launch_chrome_with_profile_dir, stop_child, RuntimeDirectory,
};
use crate::stealth::{find_chrome, fingerprint::Fingerprint, ChromePatcher};
use crate::{Error, Result, StealthConfig};
use std::{
    path::{Path, PathBuf},
    process::Child,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

pub(super) struct LaunchCancellation(pub(super) Arc<AtomicBool>);

impl LaunchCancellation {
    pub(super) fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

impl Drop for LaunchCancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub(super) async fn discover_browser_ws(port: u16) -> Result<String> {
    tokio::task::spawn_blocking(move || {
        crate::cdp::discover::discover_browser_ws("127.0.0.1", port)
    })
    .await
    .map_err(|error| Error::transport(format!("Browser discovery worker failed: {}", error)))?
}

/// Stealth browser arguments (pre-built for zero allocation).
fn stealth_args(config: &StealthConfig, fingerprint: &Fingerprint) -> Vec<String> {
    let mut args = vec![
        // Core automation hiding
        "--disable-blink-features=AutomationControlled".into(),
        "--disable-features=IsolateOrigins,site-per-process,AutomationControlled,EnableAutomation"
            .into(),
        "--enable-features=NetworkService,NetworkServiceInProcess".into(),
        // Additional flags to hide automation
        "--disable-infobars".into(),
        "--disable-dev-shm-usage".into(),
        "--disable-ipc-flooding-protection".into(),
        "--disable-renderer-backgrounding".into(),
        "--disable-background-timer-throttling".into(),
        "--disable-backgrounding-occluded-windows".into(),
        // Make browser look natural
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-default-apps".into(),
        "--disable-component-extensions-with-background-pages".into(),
        "--disable-hang-monitor".into(),
        "--disable-popup-blocking".into(),
        "--disable-prompt-on-repost".into(),
        "--disable-sync".into(),
        "--disable-translate".into(),
        "--metrics-recording-only".into(),
        "--safebrowsing-disable-auto-update".into(),
        "--disable-client-side-phishing-detection".into(),
        "--password-store=basic".into(),
        "--use-mock-keychain".into(),
        "--lang=en-US".into(),
        // Don't apply field-trial variations: they feed the `X-Client-Data`
        // header Google properties use to profile installs (see
        // `strip_x_client_data`).
        "--disable-field-trial-config".into(),
        // Window size
        format!(
            "--window-size={},{}",
            config.viewport_width, config.viewport_height
        ),
        // Keep Chromium's virtual screen aligned with the viewport in new
        // headless mode; otherwise it reports a fixed 800x600 screen.
        format!(
            "--ozone-override-screen-size={},{}",
            config.viewport_width, config.viewport_height
        ),
        // Xvfb and headless sessions have no pointing device, so Blink
        // reports `(pointer: none)` and `(hover: none)`. Desktop bot checks
        // (Cloudflare Turnstile included) cross-check the pointer/hover media
        // queries against a desktop UA. Pin the media-query state to what a
        // real desktop browser reports instead of wrapping `matchMedia` in
        // JS, which leaves a detectable proxy/identity behind.
        "--blink-settings=primaryPointerType=4,availablePointerTypes=4,primaryHoverType=2,availableHoverTypes=2".into(),
    ];
    if !config.sandbox {
        args.push("--no-sandbox".into());
    }

    // User agent
    args.push(format!("--user-agent={}", fingerprint.user_agent));

    // Headless mode
    if config.headless {
        args.push("--headless=new".into());
    }

    // Named profile within user_data_dir
    if let Some(ref profile_dir) = config.profile_dir {
        args.push(format!("--profile-directory={}", profile_dir));
    }

    // Proxy
    if let Some(ref proxy) = config.proxy {
        args.push(format!("--proxy-server={}", proxy));
    }

    // Extra user-supplied args (e.g. --use-fake-ui-for-media-stream)
    for arg in &config.extra_args {
        args.push(arg.clone());
    }

    args
}

/// Profile location; ephemeral profiles are owned by the runtime directory.
pub(super) struct BrowserProfile {
    path: PathBuf,
    owned: bool,
}

impl BrowserProfile {
    fn create(config: &StealthConfig, runtime: &RuntimeDirectory) -> Result<Self> {
        let (path, owned) = match config.user_data_dir.as_deref() {
            Some(path) => (PathBuf::from(path), false),
            None => (runtime.path().join("profile"), true),
        };
        if owned {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new().mode(0o700).create(&path)?;
            }
            #[cfg(not(unix))]
            std::fs::create_dir(&path)?;
        } else {
            std::fs::create_dir_all(&path)?;
        }
        Ok(Self { path, owned })
    }
}

/// Result of synchronous browser preparation. Until the child is transferred
/// to `Transport`, dropping this value kills Chrome and reclaims its profile.
/// `child` is `None` when we attached to an already-running Chrome instead
/// of spawning one (see `try_attach_existing`).
pub(super) struct PreparedBrowserLaunch {
    pub(super) child: Option<Child>,
    pub(super) ws_url: String,
    pub(super) fingerprint: Option<Fingerprint>,
    pub(super) runtime: Option<RuntimeDirectory>,
}

impl Drop for PreparedBrowserLaunch {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if let Err(error) = stop_child(&mut child) {
                tracing::warn!("Failed to clean up unclaimed Chrome launch: {}", error);
            }
        }
    }
}

/// Check whether a Chrome instance is already running on `profile_dir` and,
/// if so, return the ws:// URL to attach to instead of spawning a second
/// process. A second `--user-data-dir=<same dir>` launch would silently be
/// handed off to the running instance over Chrome's `SingletonSocket` and
/// exit without ever printing a DevTools URL, so this must run first.
///
/// Returns `Ok(None)` when there's nothing to attach to (no lock, a stale
/// lock that was just cleared, or a platform where reuse detection isn't
/// implemented) — the caller should spawn normally. Returns `Err` when a
/// live instance exists but can't safely be attached to (headless/headed
/// mismatch, no debug port, or an unresponsive port).
fn try_attach_existing(profile_dir: &Path, want_headless: bool) -> Result<Option<String>> {
    use crate::cdp::discover;

    let Some(pid) = discover::read_singleton_lock(profile_dir) else {
        return Ok(None);
    };

    if !discover::pid_is_alive(pid) {
        // Stale lock left behind by a crashed/killed Chrome. Clear it so a
        // normal spawn below can proceed, mirroring what Chrome itself does.
        let _ = std::fs::remove_file(profile_dir.join("SingletonLock"));
        return Ok(None);
    }

    // Check the cheap local signal before touching the network — a
    // headless/headed mismatch always fails the attach, so there's no
    // reason to round-trip to Chrome's DevTools port first.
    if let Some(argv) = discover::read_process_argv(pid) {
        let running_headless = argv.iter().any(|arg| arg.starts_with("--headless"));
        if running_headless != want_headless {
            let (running, requested) = if running_headless {
                ("headless", "headed")
            } else {
                ("non-headless", "headless")
            };
            return Err(Error::Launch(format!(
                "A {} Chrome instance (pid {}) is already running on profile {}; close it \
                 before requesting {} mode, or drop that flag for this launch",
                running,
                pid,
                profile_dir.display(),
                requested
            )));
        }
    }

    let Some(port) = discover::read_devtools_active_port(profile_dir) else {
        return Err(Error::Launch(format!(
            "A Chrome instance (pid {}) is already running on profile {}, but it wasn't \
             launched with a debug port; close it first or relaunch it with \
             --remote-debugging-port",
            pid,
            profile_dir.display()
        )));
    };

    let ws_url = discover::discover_browser_ws("127.0.0.1", port).map_err(|e| {
        Error::Launch(format!(
            "A Chrome instance (pid {}) is already running on profile {}, but its DevTools \
             port {} isn't responding: {}",
            pid,
            profile_dir.display(),
            port,
            e
        ))
    })?;

    Ok(Some(ws_url))
}

pub(super) fn prepare_browser_launch(
    config: Arc<StealthConfig>,
    cancelled: Arc<AtomicBool>,
) -> Result<PreparedBrowserLaunch> {
    if cancelled.load(Ordering::Acquire) {
        return Err(Error::Launch("Browser launch was cancelled".into()));
    }
    crate::cdp::types::EmulationSetDeviceMetricsOverride::new(
        config.viewport_width,
        config.viewport_height,
        config.device_scale_factor,
    )?;
    validate_sandbox_args(&config)?;
    let explicit_chrome = config
        .chrome_path
        .as_deref()
        .map(|path| validate_executable(Path::new(path)))
        .transpose()?;
    let runtime = RuntimeDirectory::create(config.runtime_dir.as_deref())?;
    let profile = BrowserProfile::create(&config, &runtime)?;

    // Only a durable, user-owned profile can already have a live Chrome on
    // it — ephemeral profiles are freshly created above and can't collide.
    if !profile.owned {
        if (config.chrome_path.is_some() || config.runtime_dir.is_some() || config.sandbox)
            && crate::cdp::discover::read_singleton_lock(&profile.path)
                .is_some_and(crate::cdp::discover::pid_is_alive)
        {
            return Err(Error::Launch(
                "A Chrome instance is already running on this profile; explicit executable, runtime or sandbox policy requires a new process".into(),
            ));
        }
        if let Some(ws_url) = try_attach_existing(&profile.path, config.headless)? {
            tracing::info!("Reusing running Chrome on profile {:?}", profile.path);
            let fingerprint = Fingerprint::resolve_for_profile(
                config.user_agent.as_deref(),
                config.timezone.as_deref(),
                Some(profile.path.as_path()),
            )?;
            return Ok(PreparedBrowserLaunch {
                child: None,
                ws_url,
                fingerprint: Some(fingerprint),
                runtime: None,
            });
        }
    }

    let chrome_path = match explicit_chrome {
        Some(path) => path,
        None => validate_executable(&find_chrome()?)?,
    };

    // Patching a browser binary is not appropriate for a durable user-owned
    // profile. Let that profile provide continuity instead of fresh spoofing.
    let chrome_path = if config.patch_binary && profile.owned {
        ChromePatcher::new_in_runtime(&chrome_path, runtime.path())?.get_patched_path()?
    } else {
        chrome_path
    };

    let detected_user_agent = if config.user_agent.is_none() {
        installed_chrome_user_agent(&chrome_path, runtime.path(), &cancelled)
    } else {
        None
    };
    let fingerprint = Fingerprint::resolve_for_profile(
        config
            .user_agent
            .as_deref()
            .or(detected_user_agent.as_deref()),
        config.timezone.as_deref(),
        (!profile.owned).then_some(profile.path.as_path()),
    )?;

    let mut args = stealth_args(&config, &fingerprint);
    args.push(format!("--user-data-dir={}", profile.path.display()));

    tracing::info!("Launching Chrome from {:?}", chrome_path);
    let (child, ws_url) = launch_chrome_with_profile_dir(
        &chrome_path,
        &args,
        &profile.path,
        config.live_session,
        runtime.path(),
        &cancelled,
    )?;
    Ok(PreparedBrowserLaunch {
        child: Some(child),
        ws_url,
        fingerprint: Some(fingerprint),
        runtime: Some(runtime),
    })
}

fn validate_executable(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(Error::Launch("chrome_path must be absolute".into()));
    }
    let metadata = std::fs::metadata(path).map_err(|error| {
        Error::Launch(format!(
            "Invalid Chrome executable {}: {}",
            path.display(),
            error
        ))
    })?;
    if !metadata.is_file() {
        return Err(Error::Launch(format!(
            "Chrome executable is not a file: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(Error::Launch(format!(
                "Chrome executable is not executable: {}",
                path.display()
            )));
        }
    }
    std::fs::File::open(path).map_err(|error| {
        Error::Launch(format!(
            "Cannot read Chrome executable {}: {}",
            path.display(),
            error
        ))
    })?;
    Ok(std::fs::canonicalize(path)?)
}

fn validate_sandbox_args(config: &StealthConfig) -> Result<()> {
    for arg in &config.extra_args {
        let name = arg.split('=').next().unwrap_or(arg);
        if matches!(
            name,
            "--no-sandbox"
                | "--no-sandbox-and-elevated"
                | "--disable-sandbox"
                | "--disable-setuid-sandbox"
                | "--disable-seccomp-filter-sandbox"
                | "--disable-namespace-sandbox"
                | "--disable-gpu-sandbox"
        ) {
            return Err(Error::Launch(format!(
                "Sandbox arguments must use StealthConfig.sandbox, not extra_args: {}",
                arg,
            )));
        }
    }
    Ok(())
}

fn installed_chrome_user_agent(
    chrome_path: &Path,
    runtime: &Path,
    cancelled: &AtomicBool,
) -> Option<String> {
    let output = chrome_version_output(chrome_path, runtime, cancelled)?;
    let version = String::from_utf8_lossy(&output);
    let full_version = version.split_whitespace().find(|part| {
        let mut pieces = part.split('.');
        (0..4).all(|_| {
            pieces.next().is_some_and(|piece| {
                !piece.is_empty() && piece.bytes().all(|byte| byte.is_ascii_digit())
            })
        }) && pieces.next().is_none()
    })?;
    Fingerprint::native_chrome_user_agent(full_version)
}

#[cfg(all(test, unix))]
mod try_attach_existing_tests {
    use super::*;
    use crate::browser::{accept_language_for, ua_override_for};
    use crate::stealth::build_evasion_script_for;
    use std::os::unix::fs::symlink;

    fn temp_profile_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "eoka-browser-test-{}-{}-{}",
            std::process::id(),
            name,
            fastrand::u64(..)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Our own cmdline never contains "--headless" (it's the `cargo test`
    /// binary), so pointing a fake lock at `std::process::id()` gives us a
    /// guaranteed-live pid with a known, non-headless argv — no real Chrome
    /// needed to exercise the liveness/headless-detection branches.
    fn lock_self(dir: &std::path::Path) {
        symlink(
            format!("some-host-{}", std::process::id()),
            dir.join("SingletonLock"),
        )
        .unwrap();
    }

    #[test]
    fn no_lock_returns_none() {
        let dir = temp_profile_dir("no-lock");
        assert!(try_attach_existing(&dir, true).unwrap().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stale_lock_is_cleared_and_returns_none() {
        let dir = temp_profile_dir("stale-lock");
        // A pid this large is never a real live process.
        symlink("some-host-2147483647", dir.join("SingletonLock")).unwrap();

        assert!(try_attach_existing(&dir, true).unwrap().is_none());
        assert!(
            !dir.join("SingletonLock").exists(),
            "stale lock should have been removed"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn headless_mismatch_errors_before_touching_the_network() {
        let dir = temp_profile_dir("headless-mismatch");
        lock_self(&dir);
        // No DevToolsActivePort written — if the headless check didn't run
        // first, this would produce the "no debug port" error instead.
        let error = try_attach_existing(&dir, true).unwrap_err().to_string();
        assert!(
            error.contains("non-headless") && error.contains("requesting headless mode"),
            "expected a headless/headed mismatch error, got: {}",
            error
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn live_lock_without_debug_port_errors() {
        let dir = temp_profile_dir("no-port");
        lock_self(&dir);

        // Matches our own (non-headless) argv, so the headless check passes
        // and the missing-port error is what should surface.
        let error = try_attach_existing(&dir, false).unwrap_err().to_string();
        assert!(
            error.contains("debug port"),
            "expected a missing-debug-port error, got: {}",
            error
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unresponsive_port_errors() {
        let dir = temp_profile_dir("bad-port");
        lock_self(&dir);
        // Port 0 is never a connectable listener.
        std::fs::write(dir.join("DevToolsActivePort"), "0\n/devtools/browser/x\n").unwrap();

        let error = try_attach_existing(&dir, false).unwrap_err().to_string();
        assert!(
            error.contains("isn't responding"),
            "expected an unresponsive-port error, got: {}",
            error
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stealth_args_disable_field_trials() {
        let config = StealthConfig::default();
        let fp = Fingerprint::random();
        let args = stealth_args(&config, &fp);
        assert!(args.iter().any(|arg| arg == "--disable-field-trial-config"));
    }

    // Regression: Xvfb/headless has no pointing device, so Blink reports
    // `(pointer: none)` / `(hover: none)`. Desktop bot checks cross-check those
    // against a desktop UA; the launch args must pin the desktop state.
    #[test]
    fn stealth_args_pin_desktop_pointer_and_hover() {
        let config = StealthConfig::default();
        let fp = Fingerprint::random();
        let args = stealth_args(&config, &fp);
        assert!(args.iter().any(|arg| {
            arg == "--blink-settings=primaryPointerType=4,availablePointerTypes=4,primaryHoverType=2,availableHoverTypes=2"
        }));
    }

    // Privacy regression: profile dirs hold the cookie store and previously
    // landed in /tmp with default umask (world-readable). They must be 0700.
    #[cfg(unix)]
    #[test]
    fn profile_dirs_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let runtime = RuntimeDirectory::create(None).unwrap();
        let owned = BrowserProfile::create(&StealthConfig::default(), &runtime).unwrap();
        let mode = std::fs::metadata(&owned.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "spawned temp profile dir must be 0700");
        std::fs::remove_dir_all(&owned.path).unwrap();

        let custom = temp_profile_dir("user-supplied");
        std::fs::set_permissions(&custom, std::fs::Permissions::from_mode(0o750)).unwrap();
        let guard = BrowserProfile::create(
            &StealthConfig {
                user_data_dir: Some(custom.to_string_lossy().into_owned()),
                ..StealthConfig::default()
            },
            &runtime,
        )
        .unwrap();
        let mode = std::fs::metadata(&guard.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o750,
            "user-supplied profile permissions must be preserved"
        );
        std::fs::remove_dir_all(&custom).unwrap();
    }

    // Audit regression: the Accept-Language header used to be hardcoded
    // en-US while navigator.languages followed the fingerprint/geo align —
    // a header-vs-JS mismatch any server can compare.
    #[test]
    fn accept_language_matches_fingerprint_languages() {
        assert_eq!(
            accept_language_for(&["en-US".into(), "en".into()]),
            "en-US,en;q=0.9"
        );
        assert_eq!(
            accept_language_for(&["de-DE".into(), "de".into(), "en".into()]),
            "de-DE,de;q=0.9,en;q=0.8"
        );

        let fp = Fingerprint::random();
        let override_payload = ua_override_for(&fp);
        assert_eq!(
            override_payload.accept_language.as_deref(),
            Some(accept_language_for(&fp.languages).as_str()),
            "Accept-Language must be derived from navigator.languages"
        );
    }

    // Regression: the fonts and keyboard hooks must be present and driven by
    // fingerprint data, with Linux keyboard passing through untouched.
    #[test]
    fn fonts_and_keyboard_hooks_are_fingerprint_driven() {
        let windows_fp = Fingerprint::resolve(
            Some("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36"),
            None,
        );
        let windows_script = build_evasion_script_for(&StealthConfig::default(), &windows_fp);
        assert!(windows_script.contains("document.fonts.check"));
        assert!(
            windows_script.contains(&serde_json::to_string(&windows_fp.fonts).unwrap()),
            "claimed font set must flow into the script"
        );
        assert!(windows_script.contains("getLayoutMap"));

        let linux_fp = Fingerprint::resolve(
            Some("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36"),
            None,
        );
        let linux_script = build_evasion_script_for(&StealthConfig::default(), &linux_fp);
        assert!(
            linux_script
                .contains("if (navigator.keyboard && navigator.keyboard.getLayoutMap && null)"),
            "Linux keyboard spoof must be inert"
        );
    }
}
