//! Evasion scripts to bypass bot detection
//!
//! These scripts are injected before any page content loads to patch
//! detectable browser properties.

use crate::page::escape_js_string;
use crate::stealth::fingerprint::{Fingerprint, GENERIC_FONTS};
use crate::StealthConfig;

/// Prelude: native-function masking (`_eoka_mark`), the shared prototype patch
/// helpers (`_eoka_patchMethod`/`_eoka_patchGetter`) and the seeded PRNG.
pub const NATIVE_TOSTRING_EVASION: &str = include_str!("evasions/scripts/native_tostring.js");

/// WebDriver evasion: `navigator.webdriver` is false on `Navigator.prototype`
/// (only replaced where the native getter still reports true).
pub const WEBDRIVER_EVASION: &str = include_str!("evasions/scripts/webdriver.js");

/// Formerly hid chromedriver `cdc_` markers, which eoka never produces, by
/// proxying `window.document` (breaking `ownerDocument` identity). Now empty.
#[deprecated(
    since = "0.11.0",
    note = "eoka produces no CDP markers; the script is empty"
)]
pub const CDP_EVASION: &str = "";

/// Chrome runtime object. Stock Chrome (headless/new included) provides
/// `window.chrome` natively; this only fills it where missing, matching a
/// real, extension-less page: `Object.keys(window.chrome)` is
/// `["loadTimes", "csi", "app"]` and `chrome.runtime` is undefined.
pub const CHROME_RUNTIME_EVASION: &str = include_str!("evasions/scripts/chrome_runtime.js");

/// Permissions API consistency: repairs `Notification.permission` "denied"
/// versus `permissions.query` "prompt" only where a browser reports that pair.
pub const PERMISSIONS_EVASION: &str = include_str!("evasions/scripts/permissions.js");

/// Plugins: fills an empty `navigator.plugins` on `Navigator.prototype` with a
/// stable cached reference. Stock Chrome ships the PDF plugins natively.
pub const PLUGINS_EVASION: &str = include_str!("evasions/scripts/plugins.js");

/// Navigator properties (languages, platform, hardware) filled from the fingerprint.
pub const NAVIGATOR_PROPS_EVASION: &str = include_str!("evasions/scripts/navigator_props.js");

/// Headless detection bypass. Screen/window metrics plus native-consistency
/// repairs (PerformanceObserver entry types, isSecureContext, sandbox) that
/// only apply when headless Chrome reports a state stock Chrome cannot.
pub const HEADLESS_EVASION: &str = include_str!("evasions/scripts/headless.js");

/// Battery API fix - Proxy over the real BatteryManager with spoofed values.
pub const BATTERY_EVASION: &str = include_str!("evasions/scripts/battery.js");

/// Navigator connection + userAgentData hygiene
pub const NAVIGATOR_EXTRA_EVASION: &str = include_str!("evasions/scripts/navigator_extra.js");

/// WebGL vendor/renderer spoof, gated by `StealthConfig::webgl_spoof`.
pub const WEBGL_EVASION: &str = include_str!("evasions/scripts/webgl.js");

/// Canvas readback/export noise, gated by `StealthConfig::canvas_spoof`.
pub const CANVAS_EVASION: &str = include_str!("evasions/scripts/canvas.js");

/// Audio buffer noise, gated by `StealthConfig::audio_spoof`.
pub const AUDIO_EVASION: &str = include_str!("evasions/scripts/audio.js");

/// Combined fingerprint evasion (WebGL + Canvas + Audio).
pub const FINGERPRINT_EVASION: &str = concat!(
    include_str!("evasions/scripts/webgl.js"),
    include_str!("evasions/scripts/canvas.js"),
    include_str!("evasions/scripts/audio.js"),
);

/// Formerly stripped STUN/TURN servers from `RTCPeerConnection` configs, which
/// mutated the caller's object without stopping the leak. When a proxy is
/// configured, Chrome's `--webrtc-ip-handling-policy=disable_non_proxied_udp`
/// launch flag now stops WebRTC from bypassing it. Now empty.
#[deprecated(
    since = "0.11.0",
    note = "WebRTC is handled by a Chrome launch flag; the script is empty"
)]
pub const WEBRTC_EVASION: &str = "";

/// Speech synthesis - headless Chrome has 0 voices, real Chrome has many.
/// The fallback list is fingerprint data (`__EOKA_SPEECH_VOICES__`, JSON):
/// platform-aware and stable per identity.
pub const SPEECH_EVASION: &str = include_str!("evasions/scripts/speech.js");

/// Media devices - headless returns empty array. The fake device list is
/// fingerprint data (`__EOKA_MEDIA_DEVICES__`, JSON) derived from the noise
/// seed, so IDs are stable per identity and never shared across sessions.
pub const MEDIA_DEVICES_EVASION: &str = include_str!("evasions/scripts/media_devices.js");

/// Bluetooth API - only filled on platforms whose Chrome ships Web Bluetooth.
/// Stock Linux Chrome exposes no `navigator.bluetooth`, so claiming it on a
/// Linux identity is a platform tell; the script leaves that platform untouched.
pub const BLUETOOTH_EVASION: &str = include_str!("evasions/scripts/bluetooth.js");

/// System-font probing via `document.fonts.check(font, text)`: stock Chrome
/// resolves installed system fonts, so detectors enumerate font names through
/// it. Only the identity's claimed fonts (plus CSS generic families) resolve;
/// everything else reports missing. The claimed set is fingerprint data
/// (`__EOKA_FONTS__`, JSON array).
pub const FONTS_EVASION: &str = include_str!("evasions/scripts/fonts.js");

/// `navigator.keyboard.getLayoutMap()` — stock Chrome exposes the Keyboard
/// API on Windows/macOS/ChromeOS but not Linux, so Linux passes through.
/// The map is fingerprint data (`__EOKA_KEYBOARD_MAP__`, JSON or null):
/// US ANSI, matching the identity's en-US locale.
pub const KEYBOARD_EVASION: &str = include_str!("evasions/scripts/keyboard.js");

/// Formerly replaced `Date.prototype.getTimezoneOffset` and
/// `Intl.DateTimeFormat` with approximations. Every non-live session applies
/// the timezone natively with `Emulation.setTimezoneOverride`, which is exact
/// for DST transitions and keeps native functions intact. Now empty.
#[deprecated(
    since = "0.11.0",
    note = "the timezone is applied natively by Emulation.setTimezoneOverride; the script is empty"
)]
pub const TIMEZONE_EVASION: &str = "";

/// Common timezones for random selection (balanced across regions)
pub(crate) const COMMON_TIMEZONES: &[&str] = &[
    // Americas (4)
    "America/New_York",
    "America/Chicago",
    "America/Los_Angeles",
    "America/Sao_Paulo",
    // Europe (4)
    "Europe/London",
    "Europe/Paris",
    "Europe/Berlin",
    "Europe/Moscow",
    // Asia-Pacific (4)
    "Asia/Tokyo",
    "Asia/Shanghai",
    "Asia/Kolkata",
    "Australia/Sydney",
];

/// Build the complete evasion script for the given config and fingerprint.
pub fn build_evasion_script_for(config: &StealthConfig, fp: &Fingerprint) -> String {
    let mut scripts = vec![
        NATIVE_TOSTRING_EVASION, // must be first: defines _eoka_mark / _eoka_rand
        WEBDRIVER_EVASION,
        CHROME_RUNTIME_EVASION,
        PERMISSIONS_EVASION,
        PLUGINS_EVASION,
        NAVIGATOR_PROPS_EVASION,
        HEADLESS_EVASION,
        BATTERY_EVASION,
        NAVIGATOR_EXTRA_EVASION,
        SPEECH_EVASION,
        MEDIA_DEVICES_EVASION,
        FONTS_EVASION,
        KEYBOARD_EVASION,
        BLUETOOTH_EVASION,
    ];

    // Each fingerprint flag gates only its own surface.
    // `aggressive_cdp_evasion` no longer adds anything (see `CDP_EVASION`).
    for (enabled, script) in [
        (config.webgl_spoof, WEBGL_EVASION),
        (config.canvas_spoof, CANVAS_EVASION),
        (config.audio_spoof, AUDIO_EVASION),
    ] {
        if enabled {
            scripts.push(script);
        }
    }

    let languages_json = format!(
        "[{}]",
        fp.languages
            .iter()
            .map(|l| format!("'{}'", escape_js_string(l)))
            .collect::<Vec<_>>()
            .join(",")
    );
    let language = fp.languages.first().cloned().unwrap_or_default();
    let noise_seed = fp.noise_seed;

    let script = format!("(function(){{{}}})();", scripts.join("\n"));

    // Single fill-point: every placeholder value comes from the fingerprint
    // (or config). No placeholder may survive this fold — guarded by
    // `test_placeholders_are_filled`.
    let placeholders: Vec<(&str, String)> = vec![
        ("__EOKA_PLATFORM__", escape_js_string(fp.nav_platform())),
        ("__EOKA_LANGUAGES__", languages_json),
        ("__EOKA_LANGUAGE__", escape_js_string(&language)),
        ("__EOKA_HW__", fp.hardware_concurrency.to_string()),
        ("__EOKA_MEM__", fp.device_memory.to_string()),
        ("__EOKA_WEBGL_VENDOR__", escape_js_string(&fp.webgl_vendor)),
        (
            "__EOKA_WEBGL_RENDERER__",
            escape_js_string(&fp.webgl_renderer),
        ),
        ("__EOKA_NOISE_SEED__", noise_seed.to_string()),
        (
            "__EOKA_SPEECH_VOICES__",
            serde_json::to_string(&fp.voices).unwrap_or_else(|_| "[]".to_string()),
        ),
        ("__EOKA_MEDIA_DEVICES__", fp.media_devices_json()),
        (
            "__EOKA_FONTS__",
            serde_json::to_string(&fp.fonts).unwrap_or_else(|_| "[]".to_string()),
        ),
        (
            "__EOKA_GENERIC_FONTS__",
            serde_json::to_string(GENERIC_FONTS).unwrap_or_else(|_| "[]".to_string()),
        ),
        ("__EOKA_KEYBOARD_MAP__", fp.keyboard_map_json()),
    ];
    placeholders
        .into_iter()
        .fold(script, |acc, (placeholder, value)| {
            acc.replace(placeholder, &value)
        })
}

/// Build the complete evasion script based on config.
pub fn build_evasion_script(config: &StealthConfig) -> String {
    let fp = Fingerprint::resolve(config.user_agent.as_deref(), config.timezone.as_deref());
    build_evasion_script_for(config, &fp)
}

#[cfg(test)]
mod tests;
