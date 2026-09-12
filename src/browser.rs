//! Browser Launcher
//!
//! Handles Chrome discovery, launching with stealth flags, and binary patching.

mod geo;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Global counter for unique user data directories
static BROWSER_COUNTER: AtomicU64 = AtomicU64::new(0);

use crate::cdp::transport::launch_chrome_with_profile_dir;
use crate::cdp::types::{EmulationSetUserAgentOverride, UserAgentBrandVersion, UserAgentMetadata};
use crate::cdp::{Connection, Transport};
use crate::error::{Error, Result};
use crate::page::Page;
use crate::stealth::fingerprint::Fingerprint;
use crate::stealth::{build_evasion_script_for, find_chrome, ChromePatcher};
use crate::StealthConfig;

fn remove_dir_if_exists(path: &Path) -> std::io::Result<()> {
    const ATTEMPTS: usize = 10;

    for attempt in 0..ATTEMPTS {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if is_directory_not_empty(&error) && attempt + 1 < ATTEMPTS => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error),
        }
    }

    Ok(())
}

fn is_directory_not_empty(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(39) | Some(66) | Some(145))
}

/// Restrict a profile directory to its owner (0700). Profiles contain the
/// cookie store; a default-umask /tmp directory would be world-readable.
#[cfg(unix)]
fn restrict_profile_perms(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o700);
    if let Err(error) = std::fs::set_permissions(path, perms) {
        tracing::warn!(
            "Failed to restrict profile dir {}: {}",
            path.display(),
            error
        );
    }
}

#[cfg(not(unix))]
fn restrict_profile_perms(_path: &Path) {}

async fn discover_browser_ws(port: u16) -> Result<String> {
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
        "--no-sandbox".into(),
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
    ];

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

fn stop_unclaimed_child(child: &mut Child) -> std::io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    if let Err(kill_error) = child.kill() {
        if child.try_wait()?.is_none() {
            return Err(kill_error);
        }
        return Ok(());
    }
    child.wait()?;
    Ok(())
}

/// Own a browser profile until launch fully succeeds. This makes cancellation
/// of `launch_with_config` clean up ephemeral profiles automatically.
struct BrowserProfileGuard {
    path: PathBuf,
    owned: bool,
}

impl BrowserProfileGuard {
    fn create(config: &StealthConfig) -> Result<Self> {
        match config.user_data_dir.as_deref() {
            Some(path) => {
                let path = PathBuf::from(path);
                std::fs::create_dir_all(&path)?;
                restrict_profile_perms(&path);
                Ok(Self { path, owned: false })
            }
            None => {
                let instance_id = BROWSER_COUNTER.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "eoka-browser-{}-{}",
                    std::process::id(),
                    instance_id
                ));
                remove_dir_if_exists(&path)?;
                std::fs::create_dir_all(&path)?;
                restrict_profile_perms(&path);
                Ok(Self { path, owned: true })
            }
        }
    }

    fn browser_owned_path(&mut self) -> Option<PathBuf> {
        if self.owned {
            self.owned = false;
            Some(self.path.clone())
        } else {
            None
        }
    }
}

impl Drop for BrowserProfileGuard {
    fn drop(&mut self) {
        if self.owned {
            if let Err(error) = remove_dir_if_exists(&self.path) {
                tracing::warn!("Failed to remove unclaimed browser profile: {}", error);
            }
        }
    }
}

/// Result of synchronous browser preparation. Until the child is transferred
/// to `Transport`, dropping this value kills Chrome and reclaims its profile.
/// `child` is `None` when we attached to an already-running Chrome instead
/// of spawning one (see `try_attach_existing`).
struct PreparedBrowserLaunch {
    child: Option<Child>,
    ws_url: String,
    fingerprint: Option<Fingerprint>,
    profile: BrowserProfileGuard,
}

impl Drop for PreparedBrowserLaunch {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if let Err(error) = stop_unclaimed_child(&mut child) {
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

fn prepare_browser_launch(config: Arc<StealthConfig>) -> Result<PreparedBrowserLaunch> {
    let profile = BrowserProfileGuard::create(&config)?;

    // Only a durable, user-owned profile can already have a live Chrome on
    // it — ephemeral profiles are freshly created above and can't collide.
    if !profile.owned {
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
                profile,
            });
        }
    }

    let chrome_path = match &config.chrome_path {
        Some(path) => PathBuf::from(path),
        None => find_chrome()?,
    };

    // Patching a browser binary is not appropriate for a durable user-owned
    // profile. Let that profile provide continuity instead of fresh spoofing.
    let chrome_path = if config.patch_binary && profile.owned {
        ChromePatcher::new(&chrome_path)?.get_patched_path()?
    } else {
        chrome_path
    };

    let detected_user_agent = if config.user_agent.is_none() {
        installed_chrome_user_agent(&chrome_path)
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
    let (child, ws_url) = launch_chrome_with_profile_dir(&chrome_path, &args, &profile.path)?;
    Ok(PreparedBrowserLaunch {
        child: Some(child),
        ws_url,
        fingerprint: Some(fingerprint),
        profile,
    })
}

/// Info about an open tab
#[derive(Debug, Clone)]
pub struct TabInfo {
    /// Target ID of the tab.
    pub id: String,
    /// Tab title.
    pub title: String,
    /// URL currently loaded in the tab.
    pub url: String,
}

/// Build the `Emulation.setUserAgentOverride` payload from the resolved fingerprint.
fn ua_override_for(fp: &Fingerprint) -> EmulationSetUserAgentOverride {
    let to_brands = |v: Vec<(String, String)>| -> Vec<UserAgentBrandVersion> {
        v.into_iter()
            .map(|(brand, version)| UserAgentBrandVersion { brand, version })
            .collect()
    };
    let architecture = fp.ch_architecture();
    EmulationSetUserAgentOverride {
        user_agent: fp.user_agent.clone(),
        // Chrome sends the first language bare and the rest at q=0.9; the
        // header must match `navigator.languages` (filled from the same list).
        accept_language: Some(accept_language_for(&fp.languages)),
        platform: Some(fp.nav_platform().to_string()),
        user_agent_metadata: Some(UserAgentMetadata {
            brands: to_brands(fp.ch_brands()),
            full_version_list: to_brands(fp.ch_full_version_brands()),
            platform: fp.ch_platform().to_string(),
            platform_version: fp.platform_version.clone(),
            architecture: architecture.to_string(),
            model: String::new(),
            mobile: false,
            bitness: "64".to_string(),
            wow64: false,
        }),
    }
}

/// Chrome-style `Accept-Language` value for a language list:
/// `de-DE,de;q=0.9,en;q=0.8` — first entry bare, each following entry one
/// quality step lower (minimum 0.1).
fn accept_language_for(languages: &[String]) -> String {
    languages
        .iter()
        .enumerate()
        .map(|(index, language)| match index {
            0 => language.clone(),
            _ => format!("{};q={:.1}", language, (1.0 - 0.1 * index as f64).max(0.1)),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// The main stealth browser
pub struct Browser {
    connection: Connection,
    config: Arc<StealthConfig>,
    /// User data directory (cleaned up on close; None when connecting to existing instance)
    user_data_dir: Option<PathBuf>,
    /// True when we attached to a Chrome we didn't spawn (see
    /// `try_attach_existing`). `close()` must never send `Browser.close` to
    /// a process we don't own, regardless of `config.live_session`.
    reused: bool,
    /// Resolved fingerprint (None in live-session mode).
    fingerprint: Option<Fingerprint>,
    /// Evasion script (cached)
    evasion_script: String,
}

impl Browser {
    /// Launch a new stealth browser with default config
    pub async fn launch() -> Result<Self> {
        Self::launch_with_config(StealthConfig::default()).await
    }

    /// Launch a new stealth browser with a visible window.
    pub async fn launch_visible() -> Result<Self> {
        Self::launch_with_config(StealthConfig::visible()).await
    }

    /// Launch a new visible stealth browser with debug logging behavior enabled.
    pub async fn launch_debug() -> Result<Self> {
        Self::launch_with_config(StealthConfig::debug()).await
    }

    /// Launch with the default stealth config after applying inline changes.
    ///
    /// This avoids constructing a full [`StealthConfig`] when only one or two
    /// options need to change.
    pub async fn launch_with(configure: impl FnOnce(&mut StealthConfig)) -> Result<Self> {
        let mut config = StealthConfig::default();
        configure(&mut config);
        Self::launch_with_config(config).await
    }

    /// Launch with custom config
    pub async fn launch_with_config(config: StealthConfig) -> Result<Self> {
        let config = Arc::new(config);
        let launch_config = Arc::clone(&config);
        let mut prepared =
            tokio::task::spawn_blocking(move || prepare_browser_launch(launch_config))
                .await
                .map_err(|error| {
                    Error::Launch(format!("Browser launch worker failed: {}", error))
                })??;

        let spawned = prepared.child.is_some();
        let transport = match prepared.child.take() {
            Some(child) => {
                let proxy_auth = match (&config.proxy_username, &config.proxy_password) {
                    (Some(username), Some(password)) => Some((username.clone(), password.clone())),
                    _ => None,
                };
                Transport::new_with_options(
                    child,
                    &prepared.ws_url,
                    proxy_auth,
                    config.cdp_timeout,
                    config.strip_x_client_data,
                )
                .await?
            }
            None => {
                Transport::connect_with_options(
                    &prepared.ws_url,
                    config.cdp_timeout,
                    config.filter_cdp,
                    config.strip_x_client_data,
                )
                .await?
            }
        };
        let connection = Connection::new(transport);

        let version = connection.version().await?;
        tracing::info!("Connected to Chrome: {}", version.product);

        let fingerprint = prepared
            .fingerprint
            .take()
            .ok_or_else(|| Error::Launch("Browser launch lost its fingerprint".into()))?;
        // browser_owned_path() is already None for a user-supplied (and thus
        // possibly attached-to) profile, so a reused Chrome's profile dir is
        // never deleted on close/drop.
        let user_data_dir = prepared.profile.browser_owned_path();

        let evasion_script = build_evasion_script_for(&config, &fingerprint);

        let mut browser = Self {
            connection,
            config,
            user_data_dir,
            reused: !spawned,
            fingerprint: Some(fingerprint),
            evasion_script,
        };
        if spawned && browser.config.geo_align && browser.config.timezone.is_none() {
            browser.align_geo().await;
        }
        Ok(browser)
    }

    /// Align timezone and languages with the browser's apparent public IP.
    /// Lookup failure preserves the current fingerprint.
    async fn align_geo(&mut self) {
        let outcome = async {
            let page = self.new_blank_page().await?;
            let result = geo::lookup(&page).await;
            let _ = self.close_tab(page.target_id()).await;
            result
        }
        .await;

        match outcome {
            Ok(geo) => {
                if let Some(fp) = self.fingerprint.as_mut() {
                    if let Some(timezone) = geo.timezone {
                        fp.timezone = timezone;
                    }
                    fp.languages = geo::languages_for_country(&geo.country);
                    self.evasion_script = build_evasion_script_for(&self.config, fp);
                }
                tracing::info!(
                    "Geo-aligned languages/timezone (IP country {})",
                    geo.country
                );
            }
            Err(error) => {
                tracing::warn!("Geo alignment failed, keeping random timezone: {}", error);
            }
        }
    }

    /// Connect to an existing Chrome instance at the given WebSocket CDP URL.
    /// Obtain the URL from `curl http://localhost:9222/json/version` or use
    /// `Browser::connect_port` for HTTP discovery.
    ///
    /// Defaults to `StealthConfig::live()` so attaching to a user's real browser
    /// leaves their tabs untouched (no evasion script injection, full CDP access).
    pub async fn connect(ws_url: &str) -> Result<Self> {
        Self::connect_with_config(ws_url, StealthConfig::live()).await
    }

    /// Connect to an existing Chrome instance with a custom config.
    /// Allows customizing CDP timeout, evasion scripts, proxy auth, etc.
    pub async fn connect_with_config(ws_url: &str, config: StealthConfig) -> Result<Self> {
        let config = Arc::new(config);
        let transport = crate::cdp::transport::Transport::connect_with_options(
            ws_url,
            config.cdp_timeout,
            config.filter_cdp,
            config.strip_x_client_data && !config.live_session,
        )
        .await?;
        let connection = Connection::new(transport);
        let version = connection.version().await?;
        tracing::info!("Connected to Chrome: {}", version.product);
        // Skip the evasion script and fingerprint for live sessions.
        let (fingerprint, evasion_script) = if config.live_session {
            (None, String::new())
        } else {
            let fp = Fingerprint::resolve(config.user_agent.as_deref(), config.timezone.as_deref());
            let script = build_evasion_script_for(&config, &fp);
            (Some(fp), script)
        };
        Ok(Self {
            connection,
            config,
            user_data_dir: None,
            reused: false,
            fingerprint,
            evasion_script,
        })
    }

    /// Discover the DevTools URL on `127.0.0.1:<port>` and connect.
    /// Equivalent to `curl http://127.0.0.1:<port>/json/version` then `Browser::connect`.
    pub async fn connect_port(port: u16) -> Result<Self> {
        let ws_url = discover_browser_ws(port).await?;
        Self::connect(&ws_url).await
    }

    /// `connect_port` with a custom config.
    pub async fn connect_port_with_config(port: u16, config: StealthConfig) -> Result<Self> {
        let ws_url = discover_browser_ws(port).await?;
        Self::connect_with_config(&ws_url, config).await
    }

    /// Set up a session with evasion scripts and proxy auth.
    /// Common logic shared by new_page, new_blank_page, and attach_page.
    /// In live-session mode, skips the evasion-script injection so we don't
    /// pollute the user's tab with extra `addScriptToEvaluateOnNewDocument`.
    async fn setup_session(&self, session: &crate::cdp::Session) -> Result<()> {
        session.page_enable().await?;

        let proxy_auth =
            self.config.proxy_username.is_some() && self.config.proxy_password.is_some();
        if proxy_auth || self.config.strip_x_client_data {
            session
                .fetch_enable_interception(vec![], proxy_auth)
                .await?;
        }

        if self.config.ignore_cert_errors {
            session.set_ignore_cert_errors(true).await?;
        }

        if !self.config.live_session {
            if let Some(ref fp) = self.fingerprint {
                session.set_user_agent_full(ua_override_for(fp)).await?;
                // Native ICU override: makes `Date.toString()`, `getHours()` and
                // friends consistent with the claimed timezone (a JS shim alone
                // leaves them reporting the host timezone).
                if let Err(error) = session.set_timezone_override(&fp.timezone).await {
                    tracing::warn!("Timezone override failed for {}: {}", fp.timezone, error);
                }
            }
            session
                .add_script_to_evaluate_on_new_document(&self.evasion_script)
                .await?;
        }

        Ok(())
    }

    /// Create a new page and navigate to URL
    pub async fn new_page(&self, url: &str) -> Result<Page> {
        let target_id = self
            .connection
            .create_target("about:blank", None, None)
            .await?;

        let session = self.connection.attach_to_target(&target_id).await?;
        self.setup_session(&session).await?;

        // Navigate to URL
        let nav_result = session.navigate(url, None).await?;
        if let Some(error) = nav_result.error_text {
            return Err(Error::Navigation(error));
        }

        // Brief settle time for the initial page load to start.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        Ok(Page::new(session, Arc::clone(&self.config)))
    }

    /// Create a new page without navigation (at about:blank)
    pub async fn new_blank_page(&self) -> Result<Page> {
        let target_id = self
            .connection
            .create_target("about:blank", None, None)
            .await?;

        let session = self.connection.attach_to_target(&target_id).await?;
        self.setup_session(&session).await?;

        Ok(Page::new(session, Arc::clone(&self.config)))
    }

    /// Get the browser version
    pub async fn version(&self) -> Result<String> {
        let v = self.connection.version().await?;
        Ok(v.product)
    }

    /// List all open tabs
    pub async fn tabs(&self) -> Result<Vec<TabInfo>> {
        let targets = self.connection.get_targets().await?;
        Ok(targets
            .into_iter()
            .filter(|t| t.r#type == "page")
            .map(|t| TabInfo {
                id: t.target_id,
                title: t.title,
                url: t.url,
            })
            .collect())
    }

    /// Attach to an existing browser target (e.g., a popup opened by window.open()).
    /// Use `tabs()` to discover popup target IDs, then call this to get a Page handle.
    pub async fn attach_page(&self, target_id: &str) -> Result<Page> {
        let session = self.connection.attach_to_target(target_id).await?;
        self.setup_session(&session).await?;
        Ok(Page::new(session, Arc::clone(&self.config)))
    }

    /// Activate (focus) a tab by target ID
    pub async fn activate_tab(&self, target_id: &str) -> Result<()> {
        self.connection.activate_target(target_id).await
    }

    /// Close a specific tab by target ID
    pub async fn close_tab(&self, target_id: &str) -> Result<()> {
        self.connection.close_target(target_id).await?;
        Ok(())
    }

    /// Close the browser. In live-session mode, or when we attached to a
    /// Chrome we didn't spawn (see `try_attach_existing`), this is
    /// equivalent to `disconnect()` — we never send `Browser.close` to a
    /// Chrome we don't own.
    pub async fn close(self) -> Result<()> {
        if self.config.live_session || self.reused {
            self.connection.transport().close().await?;
        } else {
            self.connection.close().await?;
        }

        // Clean up user data directory (None when connecting).
        if let Some(ref dir) = self.user_data_dir {
            remove_dir_if_exists(dir)?;
        }

        Ok(())
    }

    /// Drop the WebSocket without sending `Browser.close`. Use this when
    /// connected to a user-owned browser to disconnect without killing it.
    pub async fn disconnect(self) -> Result<()> {
        self.connection.transport().close().await
    }
}

/// Read Chrome's executable version and derive a coherent native UA. Failure
/// is intentionally non-fatal: callers fall back to a current fingerprint.
fn installed_chrome_user_agent(chrome_path: &std::path::Path) -> Option<String> {
    let output = Command::new(chrome_path).arg("--version").output().ok()?;
    let version = String::from_utf8_lossy(&output.stdout);
    let full_version = version.split_whitespace().find(|part| {
        let pieces: Vec<_> = part.split('.').collect();
        pieces.len() == 4
            && pieces
                .iter()
                .all(|piece| !piece.is_empty() && piece.bytes().all(|byte| byte.is_ascii_digit()))
    })?;
    Fingerprint::native_chrome_user_agent(full_version)
}

impl Drop for Browser {
    fn drop(&mut self) {
        // Best-effort cleanup of the temp user-data-dir if close() wasn't called.
        if let Some(ref dir) = self.user_data_dir {
            if let Err(error) = remove_dir_if_exists(dir) {
                tracing::warn!(
                    "Failed to remove temporary browser profile while dropping browser: {}",
                    error
                );
            }
        }
    }
}

#[cfg(all(test, unix))]
mod try_attach_existing_tests {
    use super::*;
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

    // Privacy regression: profile dirs hold the cookie store and previously
    // landed in /tmp with default umask (world-readable). They must be 0700.
    #[cfg(unix)]
    #[test]
    fn profile_dirs_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let owned = BrowserProfileGuard::create(&StealthConfig::default()).unwrap();
        let mode = std::fs::metadata(&owned.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "spawned temp profile dir must be 0700");
        std::fs::remove_dir_all(&owned.path).unwrap();

        let custom = temp_profile_dir("user-supplied");
        let guard = BrowserProfileGuard::create(&StealthConfig {
            user_data_dir: Some(custom.to_string_lossy().into_owned()),
            ..StealthConfig::default()
        })
        .unwrap();
        let mode = std::fs::metadata(&guard.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "user-supplied profile dir must be tightened to 0700"
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
