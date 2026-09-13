//! # Eoka
//!
//! Stealth browser automation for Rust — a Puppeteer/Playwright alternative that bypasses bot detection.
//!
//! Eoka is a minimal, fast headless Chrome library built from scratch. Unlike Selenium or
//! chromiumoxide, it uses a custom CDP implementation with stealth filtering to avoid detection
//! by Cloudflare, DataDome, PerimeterX, and other anti-bot systems.
//!
//! ## Features
//!
//! - **Stealth by Default** - Binary patching, 15 JS evasions, human-like mouse/typing
//! - **Puppeteer-like API** - `click()`, `type()`, `wait_for()`, `screenshot()`, etc.
//! - **Minimal Dependencies** - 11 direct crates, no chromiumoxide/puppeteer-extra bloat
//! - **AI-Agent Ready** - PageState introspection, element indexing, text extraction
//! - **Fast** - Lazy evasion scripts, mmap patching, stack-allocated paths
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use eoka::Browser;
//!
//! #[tokio::main]
//! async fn main() -> eoka::Result<()> {
//!     // Launch browser (patches Chrome, applies evasions)
//!     let browser = Browser::launch().await?;
//!
//!     // Create page and navigate
//!     let page = browser.new_page("https://example.com").await?;
//!
//!     // Human-like interactions
//!     page.human_click("#button").await?;
//!     page.human_type("#input", "hello world").await?;
//!
//!     // Screenshot
//!     let png = page.screenshot().await?;
//!     std::fs::write("screenshot.png", png)?;
//!
//!     browser.close().await?;
//!     Ok(())
//! }
//! ```
//!
//! ## Configuration
//!
//! ```rust,no_run
//! use eoka::Browser;
//!
//! # #[tokio::main]
//! # async fn main() -> eoka::Result<()> {
//! // The default launch path is stealthy and headless.
//! let browser = Browser::launch().await?;
//! browser.close().await?;
//!
//! // For one-off tweaks, mutate the default stealth config inline.
//! let browser = Browser::launch_with(|config| {
//!     config.proxy = Some("http://127.0.0.1:8080".into());
//! }).await?;
//! browser.close().await?;
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]

pub mod browser;
pub mod cdp;
pub mod element;
pub mod error;
pub mod fetch;
mod keyboard;
pub mod network;
pub mod page;
pub mod session;
pub mod stealth;

// Static assertions: ensure core types are Send + Sync for use across async tasks
#[allow(dead_code)]
const _: () = {
    fn assert_send_sync<T: Send + Sync>() {}
    fn _assertions() {
        assert_send_sync::<browser::Browser>();
        assert_send_sync::<page::Page>();
    }
};

// Re-exports
pub use browser::{Browser, TabInfo};
pub use element::{BoundingBox, Element};
pub use error::{Error, Result};
pub use fetch::{BrowserFetchOutcome, BrowserFetchRequest, BrowserFetchResponse};
pub use network::{NetworkEvent, NetworkWatcher};
pub use page::{CapturedRequest, FrameInfo, MouseButton, Page, PageState, ResponseBody, TextMatch};
pub use session::{BrowserSession, BrowserState, SessionCookie};
pub use stealth::{Fingerprint, HumanSpeed};

/// Configuration for stealth features
#[derive(Debug, Clone)]
pub struct StealthConfig {
    /// Spoof WebGL renderer/vendor
    pub webgl_spoof: bool,
    /// Spoof canvas fingerprint
    pub canvas_spoof: bool,
    /// Spoof audio fingerprint
    pub audio_spoof: bool,
    /// Use human-like mouse movements
    pub human_mouse: bool,
    /// Use human-like typing
    pub human_typing: bool,
    /// Custom user agent (None = random realistic)
    pub user_agent: Option<String>,
    /// Headless mode
    pub headless: bool,
    /// Path to Chrome/Chromium binary
    pub chrome_path: Option<String>,
    /// Patch Chrome binary to bypass detection
    pub patch_binary: bool,
    /// Viewport width
    pub viewport_width: u32,
    /// Viewport height
    pub viewport_height: u32,
    /// Debug mode - log actions and save screenshots on error
    pub debug: bool,
    /// Directory for debug screenshots (defaults to current directory)
    pub debug_dir: Option<String>,
    /// Proxy server URL (e.g. "http://host:port")
    pub proxy: Option<String>,
    /// Durable Chrome user-data directory. When set, Eoka preserves the
    /// profile and its full fingerprint identity across browser launches.
    ///
    /// If a Chrome instance is already running on this directory, Eoka
    /// attaches to it instead of spawning a second process (which Chrome's
    /// own singleton lock would silently hand off to and exit).
    pub user_data_dir: Option<String>,
    /// Named profile within `user_data_dir` (Chrome's `--profile-directory`,
    /// e.g. "Profile 1"). Only meaningful when `user_data_dir` is set;
    /// defaults to Chrome's implicit "Default" profile when `None`.
    ///
    /// Chrome's singleton lock (and thus Eoka's reuse detection above) is
    /// scoped to the whole `user_data_dir`, not the individual profile —
    /// launching a second `profile_dir` while a Chrome is already running
    /// on the same `user_data_dir` attaches to that same running instance
    /// rather than opening a separate one, matching Chrome's own behavior.
    pub profile_dir: Option<String>,
    /// Proxy username for authenticated proxies
    pub proxy_username: Option<String>,
    /// Proxy password for authenticated proxies
    pub proxy_password: Option<String>,
    /// CDP command timeout in seconds (default: 30, increase for slow proxies)
    pub cdp_timeout: u64,
    /// IANA timezone (default: random from common US/EU timezones).
    /// Set to a specific value like "America/New_York" to control the timezone.
    pub timezone: Option<String>,
    /// Extra Chrome command-line arguments appended after standard stealth args.
    /// E.g. vec!["--use-fake-ui-for-media-stream".into()] to auto-grant camera.
    pub extra_args: Vec<String>,
    /// Enable legacy, invasive CDP-marker hiding that proxies `window.document`
    /// and patches Object reflection APIs. It can break modern SPAs, so it is
    /// disabled by default. Prefer the normal WebDriver and fingerprint evasions.
    pub aggressive_cdp_evasion: bool,
    /// Treat this as a session attached to a user-owned browser. When true,
    /// `Browser::new_page`/`new_blank_page`/`attach_page` skip injecting the
    /// evasion script. Defaults to false (set automatically by `Browser::connect*`).
    pub live_session: bool,
    /// Drop "detectable" CDP commands like `Runtime.enable` silently.
    /// Defaults to true. Set to false in connect mode to get full DevTools-like control.
    pub filter_cdp: bool,
    /// Ignore TLS certificate errors on every page from creation, before the
    /// first navigation. `Page::ignore_cert_errors` is applied too late for
    /// `new_page`'s own initial navigate — this covers self-signed/expired
    /// certs (e.g. .onion services) hit on the very first load. Defaults to
    /// false.
    pub ignore_cert_errors: bool,
    /// Strip Chromium's `X-Client-Data` request header via CDP Fetch
    /// interception. Chrome sends this header with field-trial variation IDs on
    /// requests to Google-owned properties; a fresh automation profile carries
    /// a distinctive (often empty) variation set that server-side bot scoring
    /// can read before any JavaScript runs. Also launches Chrome with
    /// `--disable-field-trial-config`. Defaults to true for spawned stealth
    /// browsers; ignored (never applied) for attached live sessions.
    pub strip_x_client_data: bool,
    /// Resolve timezone and interface language from the browser's apparent
    /// public IP via a one-shot navigation to Cloudflare's trace endpoint
    /// before any real page is created. Keeps `Date`/`Intl` and the injected
    /// `navigator.languages` consistent with IP geolocation instead of a
    /// random timezone. Ignored when `timezone` is set explicitly and when
    /// attaching to an existing browser. Defaults to false.
    pub geo_align: bool,
}

impl Default for StealthConfig {
    fn default() -> Self {
        Self {
            webgl_spoof: true,
            canvas_spoof: true,
            audio_spoof: true,
            human_mouse: true,
            human_typing: true,
            user_agent: None,
            headless: true,
            chrome_path: None,
            patch_binary: true,
            viewport_width: 1920,
            viewport_height: 1080,
            debug: false,
            debug_dir: None,
            proxy: None,
            user_data_dir: None,
            profile_dir: None,
            proxy_username: None,
            proxy_password: None,
            cdp_timeout: 30,
            timezone: None, // Random from common timezones
            extra_args: Vec::new(),
            aggressive_cdp_evasion: false,
            live_session: false,
            filter_cdp: true,
            ignore_cert_errors: false,
            strip_x_client_data: true,
            geo_align: false,
        }
    }
}

impl StealthConfig {
    /// Create a minimal config (no spoofing, no patching)
    pub fn minimal() -> Self {
        Self {
            webgl_spoof: false,
            canvas_spoof: false,
            audio_spoof: false,
            human_mouse: false,
            human_typing: false,
            headless: false,
            patch_binary: false,
            ..Default::default()
        }
    }

    /// Create a visible (non-headless) config
    pub fn visible() -> Self {
        Self {
            headless: false,
            ..Default::default()
        }
    }

    /// Create a debug config (visible, with logging)
    pub fn debug() -> Self {
        Self {
            headless: false,
            debug: true,
            ..Default::default()
        }
    }

    /// Config tuned for attaching to a user-owned browser via `Browser::connect*`.
    ///
    /// Disables anything that touches the user's Chrome state:
    /// - `live_session` — no evasion script injection on attached tabs
    /// - `filter_cdp = false` — full DevTools-equivalent CDP access
    /// - `patch_binary = false` — we don't manage the binary
    /// - all spoofing off — the user's browser already has its own fingerprint
    pub fn live() -> Self {
        Self {
            live_session: true,
            filter_cdp: false,
            patch_binary: false,
            webgl_spoof: false,
            canvas_spoof: false,
            audio_spoof: false,
            human_mouse: true,
            human_typing: true,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StealthConfig;

    #[test]
    fn default_config_is_headless() {
        assert!(StealthConfig::default().headless);
    }

    #[test]
    fn visible_configs_are_explicitly_not_headless() {
        assert!(!StealthConfig::visible().headless);
        assert!(!StealthConfig::debug().headless);
    }
}
