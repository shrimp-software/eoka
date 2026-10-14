//! Evasion scripts to bypass bot detection
//!
//! These scripts are injected before any page content loads to patch
//! detectable browser properties.

use crate::page::escape_js_string;
use crate::stealth::fingerprint::{Fingerprint, GENERIC_FONTS};
use crate::StealthConfig;

/// Native-function masking: makes `_eoka_mark`ed functions report native code from `.toString()`.
pub const NATIVE_TOSTRING_EVASION: &str = include_str!("evasions/scripts/native_tostring.js");

/// Core WebDriver evasion - define webdriver=false on prototype (more realistic)
pub const WEBDRIVER_EVASION: &str = include_str!("evasions/scripts/webdriver.js");

/// CDP marker cleanup - simple and effective approach from chaser-oxide
pub const CDP_EVASION: &str = include_str!("evasions/scripts/cdp.js");

/// Chrome runtime object. Matches a real, extension-less Chrome page context:
/// `Object.keys(window.chrome)` is `["loadTimes", "csi", "app"]` and
/// `chrome.runtime` is undefined (verified against stock Chrome 152).
pub const CHROME_RUNTIME_EVASION: &str = include_str!("evasions/scripts/chrome_runtime.js");

/// Permissions API consistency fix
pub const PERMISSIONS_EVASION: &str = include_str!("evasions/scripts/permissions.js");

/// Plugins spoofing - defined on Navigator.prototype with a stable cached reference.
pub const PLUGINS_EVASION: &str = include_str!("evasions/scripts/plugins.js");

/// Navigator properties (languages, platform, hardware) filled from the fingerprint.
pub const NAVIGATOR_PROPS_EVASION: &str = include_str!("evasions/scripts/navigator_props.js");

/// Headless detection bypass
pub const HEADLESS_EVASION: &str = include_str!("evasions/scripts/headless.js");

/// Battery API fix - Proxy over the real BatteryManager with spoofed values.
pub const BATTERY_EVASION: &str = include_str!("evasions/scripts/battery.js");

/// Navigator connection + userAgentData hygiene
pub const NAVIGATOR_EXTRA_EVASION: &str = include_str!("evasions/scripts/navigator_extra.js");

/// Combined fingerprint evasion (WebGL + Canvas + Audio).
pub const FINGERPRINT_EVASION: &str = include_str!("evasions/scripts/fingerprint.js");

/// WebRTC leak protection - prevent real IP from leaking via public STUN.
pub const WEBRTC_EVASION: &str = include_str!("evasions/scripts/webrtc.js");

/// Speech synthesis - headless Chrome has 0 voices, real Chrome has many.
/// The fallback list is fingerprint data (`__EOKA_SPEECH_VOICES__`, JSON):
/// platform-aware and stable per identity.
pub const SPEECH_EVASION: &str = include_str!("evasions/scripts/speech.js");

/// Media devices - headless returns empty array. The fake device list is
/// fingerprint data (`__EOKA_MEDIA_DEVICES__`, JSON) derived from the noise
/// seed, so IDs are stable per identity and never shared across sessions.
pub const MEDIA_DEVICES_EVASION: &str = include_str!("evasions/scripts/media_devices.js");

/// Bluetooth API - headless throws error, real Chrome returns object
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

/// Timezone consistency - ensure Date timezone matches claimed location.
/// The `__EOKA_TIMEZONE__` placeholder is replaced at build time with the
/// actual IANA timezone from StealthConfig.
pub const TIMEZONE_EVASION: &str = include_str!("evasions/scripts/timezone.js");

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
    let timezone = config
        .timezone
        .clone()
        .unwrap_or_else(|| fp.timezone.clone());

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
        WEBRTC_EVASION,
        SPEECH_EVASION,
        MEDIA_DEVICES_EVASION,
        FONTS_EVASION,
        KEYBOARD_EVASION,
        BLUETOOTH_EVASION,
        TIMEZONE_EVASION,
    ];

    // This legacy module globally proxies `window.document` and patches Object
    // reflection methods. Keep it available for callers that explicitly need
    // it, but do not make a page's core DOM behavior depend on it by default.
    if config.aggressive_cdp_evasion {
        scripts.push(CDP_EVASION);
    }

    if config.webgl_spoof || config.canvas_spoof || config.audio_spoof {
        scripts.push(FINGERPRINT_EVASION);
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
        ("__EOKA_TIMEZONE__", escape_js_string(&timezone)),
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
