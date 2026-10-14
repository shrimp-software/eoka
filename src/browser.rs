//! Browser Launcher
//!
//! Handles Chrome discovery, launching with stealth flags, and binary patching.

mod geo;
mod launch;

use launch::{discover_browser_ws, prepare_browser_launch, LaunchCancellation};

use std::path::Path;
use std::sync::Arc;

use crate::cdp::types::{EmulationSetUserAgentOverride, UserAgentBrandVersion, UserAgentMetadata};
use crate::cdp::{Connection, Transport};
use crate::error::{Error, Result};
use crate::page::Page;
use crate::stealth::build_evasion_script_for;
use crate::stealth::fingerprint::Fingerprint;
use crate::StealthConfig;

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
    download_slot: Arc<tokio::sync::Semaphore>,
    config: Arc<StealthConfig>,
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
    /// Whether this handle owns the Chrome process, rather than borrowing it.
    pub fn owns_process(&self) -> bool {
        !self.reused
    }

    /// Reserve native download settings and subscribe before triggering a download.
    /// Only supported for owned processes. The directory must already exist.
    pub async fn watch_downloads(&self, directory: &Path) -> Result<crate::DownloadWatch> {
        if !self.owns_process() {
            return Err(Error::cdp_msg(
                "download_unsupported: attached browsers are not supported",
            ));
        }
        crate::DownloadWatch::begin(
            self.connection.transport().clone(),
            self.download_slot.clone(),
            directory,
        )
        .await
    }

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
        let mut config = Arc::new(config);
        let launch_config = Arc::clone(&config);
        let cancellation = LaunchCancellation::new();
        let cancelled = Arc::clone(&cancellation.0);
        let mut prepared =
            tokio::task::spawn_blocking(move || prepare_browser_launch(launch_config, cancelled))
                .await
                .map_err(|error| {
                    Error::Launch(format!("Browser launch worker failed: {}", error))
                })??;

        let spawned = prepared.child.is_some();
        if !spawned {
            let config = Arc::make_mut(&mut config);
            config.live_session = true;
            config.filter_cdp = false;
            config.strip_x_client_data = false;
        }
        let transport = match prepared.child.take() {
            Some(child) => {
                let proxy_auth = match (&config.proxy_username, &config.proxy_password) {
                    (Some(username), Some(password)) => Some((username.clone(), password.clone())),
                    _ => None,
                };
                Transport::new_with_runtime_options(
                    child,
                    &prepared.ws_url,
                    proxy_auth,
                    config.cdp_timeout,
                    config.strip_x_client_data,
                    prepared.runtime.take(),
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
        let evasion_script = build_evasion_script_for(&config, &fingerprint);

        let mut browser = Self {
            connection,
            download_slot: Arc::new(tokio::sync::Semaphore::new(1)),
            config,
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
            download_slot: Arc::new(tokio::sync::Semaphore::new(1)),
            config,
            reused: true,
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

    /// Set up a session with evasion scripts, viewport emulation, and proxy auth.
    /// Common logic shared by new_page, new_blank_page, and attach_page.
    /// Live sessions retain existing viewport metrics and skip evasion injection.
    async fn setup_session(&self, session: &crate::cdp::Session, fresh: bool) -> Result<()> {
        session.monitor_network(fresh).await?;
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
            session
                .set_viewport(
                    self.config.viewport_width,
                    self.config.viewport_height,
                    self.config.device_scale_factor,
                )
                .await?;
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
        self.setup_session(&session, true).await?;

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
        self.setup_session(&session, true).await?;

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
        self.setup_session(&session, false).await?;
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

    /// Gracefully close an owned browser, including owned live-session launches.
    /// Attached browsers are only disconnected, regardless of configuration.
    /// Chrome has a bounded opportunity to flush state before forced termination.
    pub async fn close(self) -> Result<()> {
        if self.reused {
            self.connection.disconnect().await?;
        } else {
            self.connection.close().await?;
        }

        Ok(())
    }

    /// Drop the WebSocket without sending `Browser.close`. Use this when
    /// connected to a user-owned browser to disconnect without killing it.
    pub async fn disconnect(self) -> Result<()> {
        self.connection.disconnect().await
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        if self.owns_process() {
            if let Err(error) = self.connection.transport().cleanup_owned() {
                tracing::warn!(
                    "Failed to clean up owned browser while dropping handle: {}",
                    error
                );
            }
        }
    }
}
