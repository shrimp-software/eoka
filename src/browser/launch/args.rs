//! Chrome launch switches: window and screen sizing, stealth flags and the
//! merge of user list switches into eoka's.

use crate::stealth::fingerprint::Fingerprint;
use crate::StealthConfig;

/// Height of Chrome's tab strip and toolbar in CSS pixels. Measured on
/// `--headless=new` (Chrome 155 and 157): a 1920x1080 window has a 1920x993
/// viewport, the same as a headed window without a bookmarks bar.
const BROWSER_CHROME_HEIGHT: u32 = 87;

/// Common desktop resolutions, smallest first.
const SCREEN_SIZES: &[(u32, u32)] = &[
    (1366, 768),
    (1440, 900),
    (1536, 864),
    (1920, 1080),
    (1920, 1200),
    (2560, 1440),
    (2560, 1600),
    (3840, 2160),
];

/// Outer window size whose native viewport is `width` x `height`.
fn window_size_for_viewport(width: u32, height: u32) -> (u32, u32) {
    (width, height.saturating_add(BROWSER_CHROME_HEIGHT))
}

/// Smallest common screen that contains the window, or the window itself
/// when it is larger than every listed screen.
fn screen_for_window(width: u32, height: u32) -> (u32, u32) {
    SCREEN_SIZES
        .iter()
        .copied()
        .find(|&(screen_width, screen_height)| screen_width >= width && screen_height >= height)
        .unwrap_or((width, height))
}

/// Stealth browser arguments (pre-built for zero allocation).
pub(super) fn stealth_args(config: &StealthConfig, fingerprint: &Fingerprint) -> Vec<String> {
    let (window_width, window_height) =
        window_size_for_viewport(config.viewport_width, config.viewport_height);
    let (screen_width, screen_height) = screen_for_window(window_width, window_height);
    let mut args = vec![
        // Core automation hiding
        "--disable-blink-features=AutomationControlled".into(),
        "--disable-features=AutomationControlled,EnableAutomation".into(),
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
        // The window is the viewport plus Chrome's own toolbar area, so
        // `outerHeight` exceeds `innerHeight` as it does in a real browser.
        format!("--window-size={window_width},{window_height}"),
        // New headless mode otherwise reports a fixed 800x600 screen. The
        // screen must contain the window, so it cannot equal the viewport.
        format!("--ozone-override-screen-size={screen_width},{screen_height}"),
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
        // Headless places the window at (10,10), which pushes a full-width
        // window past the right edge of the screen.
        args.push("--window-position=0,0".into());
    }

    // Named profile within user_data_dir
    if let Some(ref profile_dir) = config.profile_dir {
        args.push(format!("--profile-directory={}", profile_dir));
    }

    // Proxy. WebRTC sends UDP straight from the host (STUN reveals the real
    // public IP); with a proxy, keep it to proxied transports only.
    if let Some(ref proxy) = config.proxy {
        args.push(format!("--proxy-server={}", proxy));
        args.push("--webrtc-ip-handling-policy=disable_non_proxied_udp".into());
    }

    // Extra user-supplied args (e.g. --use-fake-ui-for-media-stream)
    merge_extra_args(&mut args, &config.extra_args);

    args
}

/// Switches whose value is a list. Chromium keeps only the last occurrence of a
/// switch, so a user's list is merged into eoka's instead of replacing it.
const LIST_SWITCHES: &[&str] = &[
    "--disable-features",
    "--enable-features",
    "--disable-blink-features",
    "--enable-blink-features",
    "--blink-settings",
];

/// Append `extra` to `args`, folding list switches into an existing occurrence.
/// Features are deduplicated by name; `--blink-settings` keys take the user's
/// value.
fn merge_extra_args(args: &mut Vec<String>, extra: &[String]) {
    // Feature entries may carry `:params` or `<trial`; blink settings are
    // `key=value`. Either way the identity is the leading name.
    fn key(item: &str) -> &str {
        item.split([':', '<', '='])
            .next()
            .unwrap_or_default()
            .trim()
    }
    for arg in extra {
        let list = LIST_SWITCHES.iter().find_map(|switch| {
            arg.strip_prefix(switch)
                .and_then(|rest| rest.strip_prefix('='))
                .map(|value| (*switch, value))
        });
        let Some((switch, value)) = list else {
            args.push(arg.clone());
            continue;
        };
        let prefix = format!("{switch}=");
        let Some(existing) = args.iter_mut().find(|a| a.starts_with(&prefix)) else {
            args.push(arg.clone());
            continue;
        };
        let mut items: Vec<String> = existing[prefix.len()..]
            .split(',')
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect();
        for item in value.split(',').filter(|item| !item.is_empty()) {
            match items.iter_mut().find(|current| key(current) == key(item)) {
                Some(current) if switch == "--blink-settings" => *current = item.to_string(),
                Some(_) => {}
                None => items.push(item.to_string()),
            }
        }
        *existing = format!("{prefix}{}", items.join(","));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_is_taller_than_the_viewport() {
        assert_eq!(window_size_for_viewport(1920, 1080), (1920, 1167));
        assert_eq!(
            window_size_for_viewport(800, u32::MAX),
            (800, u32::MAX),
            "height must not overflow"
        );
    }

    #[test]
    fn screen_contains_the_window() {
        assert_eq!(screen_for_window(1920, 1167), (1920, 1200));
        assert_eq!(screen_for_window(1280, 807), (1440, 900));
        for &(width, height) in &[(1, 1), (1366, 768), (1920, 1167), (2560, 1527)] {
            let (screen_width, screen_height) = screen_for_window(width, height);
            assert!(screen_width >= width && screen_height >= height);
        }
    }

    #[test]
    fn oversized_window_becomes_the_screen() {
        assert_eq!(screen_for_window(5000, 3000), (5000, 3000));
        assert_eq!(screen_for_window(1000, 4000), (1000, 4000));
    }

    #[test]
    fn default_launch_arguments_size_window_and_screen() {
        let config = StealthConfig::default();
        let fingerprint = Fingerprint::resolve(None, None);
        let args = stealth_args(&config, &fingerprint);
        assert!(args.contains(&"--window-size=1920,1167".to_string()));
        assert!(args.contains(&"--ozone-override-screen-size=1920,1200".to_string()));
        assert!(args.contains(&"--window-position=0,0".to_string()));

        let visible = stealth_args(&StealthConfig::visible(), &fingerprint);
        assert!(!visible
            .iter()
            .any(|arg| arg.starts_with("--window-position")));
    }

    #[test]
    fn stealth_args_disable_field_trials() {
        let config = StealthConfig::default();
        let fp = Fingerprint::random();
        let args = stealth_args(&config, &fp);
        assert!(args.iter().any(|arg| arg == "--disable-field-trial-config"));
    }

    // Regression: Chromium keeps only the last occurrence of a switch, so a
    // user's list switch used to drop eoka's entries.
    #[test]
    fn stealth_args_merge_user_list_switches() {
        let fp = Fingerprint::random();
        let config = StealthConfig {
            extra_args: vec![
                "--disable-features=Translate,AutomationControlled".into(),
                "--enable-features=Foo:param/1".into(),
                "--disable-blink-features=Bar".into(),
                "--blink-settings=imagesEnabled=false,primaryHoverType=1".into(),
                "--use-fake-ui-for-media-stream".into(),
            ],
            ..StealthConfig::default()
        };
        let args = stealth_args(&config, &fp);
        let only = |switch: &str| {
            let prefix = format!("{switch}=");
            let matching: Vec<_> = args.iter().filter(|a| a.starts_with(&prefix)).collect();
            assert_eq!(matching.len(), 1, "{switch} must appear once: {args:?}");
            matching[0].clone()
        };
        assert_eq!(
            only("--disable-features"),
            "--disable-features=AutomationControlled,EnableAutomation,Translate"
        );
        assert_eq!(
            only("--enable-features"),
            "--enable-features=NetworkService,NetworkServiceInProcess,Foo:param/1"
        );
        assert_eq!(
            only("--disable-blink-features"),
            "--disable-blink-features=AutomationControlled,Bar"
        );
        assert_eq!(
            only("--blink-settings"),
            "--blink-settings=primaryPointerType=4,availablePointerTypes=4,primaryHoverType=1,availableHoverTypes=2,imagesEnabled=false"
        );
        assert_eq!(args.last().unwrap(), "--use-fake-ui-for-media-stream");
    }

    // A list switch eoka does not set is appended once, and a repeat of it in
    // extra_args folds into that copy instead of replacing it.
    #[test]
    fn stealth_args_fold_repeated_user_list_switches() {
        let config = StealthConfig {
            extra_args: vec![
                "--enable-blink-features=Foo".into(),
                "--enable-blink-features=Bar,Foo".into(),
                "--disable-features=Translate".into(),
                "--disable-features=Translate<Trial,Other".into(),
            ],
            ..StealthConfig::default()
        };
        let args = stealth_args(&config, &Fingerprint::random());
        let matching = |prefix: &str| -> Vec<&String> {
            args.iter().filter(|a| a.starts_with(prefix)).collect()
        };
        assert_eq!(
            matching("--enable-blink-features="),
            ["--enable-blink-features=Foo,Bar"]
        );
        assert_eq!(
            matching("--disable-features="),
            ["--disable-features=AutomationControlled,EnableAutomation,Translate,Other"]
        );
    }

    // Regression: site isolation was "disabled" with names that are not
    // features (`site-per-process` is a switch), which changed nothing.
    #[test]
    fn stealth_args_do_not_carry_noop_isolation_features() {
        let args = stealth_args(&StealthConfig::default(), &Fingerprint::random());
        assert!(!args
            .iter()
            .any(|a| a.contains("site-per-process") || a.contains("IsolateOrigins")));
    }

    // Regression: WebRTC UDP bypassed a configured proxy (STUN leaks the host
    // IP); only proxied sessions are restricted.
    #[test]
    fn stealth_args_keep_webrtc_behind_a_proxy() {
        let fp = Fingerprint::random();
        let policy = "--webrtc-ip-handling-policy=disable_non_proxied_udp";
        let direct = stealth_args(&StealthConfig::default(), &fp);
        assert!(!direct
            .iter()
            .any(|a| a.starts_with("--webrtc-ip-handling-policy")));
        let proxied = stealth_args(
            &StealthConfig {
                proxy: Some("http://127.0.0.1:8080".into()),
                ..StealthConfig::default()
            },
            &fp,
        );
        assert!(proxied.iter().any(|a| a == policy));
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
}
