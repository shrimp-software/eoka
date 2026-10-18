use super::*;

#[test]
fn test_build_full_script() {
    let config = StealthConfig::default();
    let script = build_evasion_script(&config);
    assert!(script.contains("webdriver"));
    assert!(script.contains("WebGLRenderingContext"));
    assert!(script.contains("HTMLCanvasElement"));
    assert!(script.contains("AudioBuffer"));
    assert!(!script.contains("const documentProxy"));
}

#[test]
fn test_aggressive_cdp_evasion_is_opt_in() {
    let config = StealthConfig {
        aggressive_cdp_evasion: true,
        ..StealthConfig::default()
    };
    assert!(build_evasion_script(&config).contains("const documentProxy"));
}

#[test]
fn test_placeholders_are_filled() {
    let script = build_evasion_script(&StealthConfig::default());
    assert!(
        !script.contains("__EOKA_"),
        "unresolved placeholder in script"
    );
    assert!(script.contains("_eoka_mark"));
}

#[test]
fn test_script_is_wrapped_in_iife() {
    let config = StealthConfig::default();
    let script = build_evasion_script(&config);
    assert!(script.starts_with("(function()"));
    assert!(script.ends_with("})();"));
}

// Regression: the script must be driven by the fingerprint, not by old
// hardcoded values. Build for a known Windows fingerprint and assert the
// Windows-specific fields flow through while the old Mac constants and
// removed features do not.
#[test]
fn test_build_script_for_known_windows_fp() {
    let fp = Fingerprint::resolve(
        Some("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36"),
        None,
    );
    let config = StealthConfig::default();
    let script = build_evasion_script_for(&config, &fp);

    // navigator.platform comes from the fingerprint (Win32), and the old
    // hardcoded MacIntel must be gone.
    assert!(
        script.contains("'Win32'"),
        "script must contain the fingerprint nav_platform 'Win32'"
    );
    assert!(
        !script.contains("'MacIntel'"),
        "script must not contain the old hardcoded 'MacIntel'"
    );

    // WebGL renderer and hardware values flow from the fingerprint.
    assert!(
        script.contains(&fp.webgl_renderer),
        "script must contain the fingerprint webgl_renderer: {}",
        fp.webgl_renderer
    );
    assert!(
        script.contains(&fp.hardware_concurrency.to_string()),
        "script must contain hardware_concurrency"
    );
    assert!(
        script.contains(&fp.device_memory.to_string()),
        "script must contain device_memory"
    );

    // toString-masking machinery is present.
    assert!(script.contains("_eoka_mark"));
    assert!(script.contains("[native code]"));

    // No leftover placeholders, and the removed beacon must be gone.
    assert!(
        !script.contains("__EOKA_"),
        "script must not contain unresolved placeholders"
    );
    assert!(
        !script.contains("__eoka_pending_requests"),
        "removed request-beacon must not reappear"
    );
}

// Escaping regression: config values with a single quote must be routed
// through escape_js_string, producing an escaped `\'` in the output.
#[test]
fn test_config_values_are_js_escaped() {
    let config = StealthConfig {
        timezone: Some("Foo'Bar".into()),
        ..StealthConfig::default()
    };
    let script = build_evasion_script(&config);
    assert!(
        script.contains(r"Foo\'Bar"),
        "timezone single quote must be escaped via escape_js_string"
    );
    assert!(
        !script.contains("'Foo'Bar'"),
        "unescaped single quote must not appear"
    );
}

// Audit regression: stock Chrome pages expose window.chrome keys as
// ["loadTimes", "csi", "app"] only — chrome.runtime is extension-only and
// must never be injected (verified against stock Chrome 152).
#[test]
fn test_chrome_object_matches_stock() {
    let script = build_evasion_script(&StealthConfig::default());
    assert!(
        script.contains("loadTimes: function"),
        "chrome.loadTimes must be spoofed (present in stock Chrome)"
    );
    assert!(
        script.contains("csi: function"),
        "chrome.csi must be spoofed (present in stock Chrome)"
    );
    assert!(
        !script.contains("onConnect"),
        "chrome.runtime stub must not be re-added: plain pages have no runtime"
    );
    assert!(
        !script.contains("sendMessage: function"),
        "chrome.runtime stub must not be re-added"
    );
}

// Audit regression: stock Linux Chrome has no `navigator.bluetooth`, so a Linux
// identity must not claim it. `navigator.deviceMemory` is different: Linux
// Chrome does expose it, but only in secure contexts, so the spoof is gated on
// the native attribute existing rather than on the platform. Assertions stay on
// single lines so CRLF checkouts (Windows CI) see the same script text.
#[test]
fn test_platform_gated_navigator_apis() {
    let linux_fp = Fingerprint::resolve(
        Some("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36"),
        None,
    );
    let script = build_evasion_script_for(&StealthConfig::default(), &linux_fp);
    assert!(
        script.contains(
            "if (!/Linux/i.test('Linux x86_64') && window.isSecureContext && !navigator.bluetooth)"
        ),
        "Linux script must leave navigator.bluetooth absent"
    );
    assert!(
        script.contains("deviceMemory: { get: () => "),
        "Linux Chrome exposes deviceMemory, so the fingerprint value must replace the host's"
    );
    assert!(
        script.contains("if (!('deviceMemory' in Navigator.prototype))"),
        "deviceMemory must only be spoofed where the native [SecureContext] attribute exists"
    );

    let windows_fp = Fingerprint::resolve(
        Some("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36"),
        None,
    );
    let windows_script = build_evasion_script_for(&StealthConfig::default(), &windows_fp);
    assert!(
        windows_script.contains(
            "if (!/Linux/i.test('Win32') && window.isSecureContext && !navigator.bluetooth)"
        ),
        "Windows script must fill a missing navigator.bluetooth"
    );
    assert!(
        windows_script.contains("deviceMemory: { get: () => "),
        "Windows script must keep the deviceMemory claim"
    );
}

// Audit regression: the old matchMedia wrapper reported matches:false for
// BOTH prefers-reduced-motion variants — an impossible state (one always
// matches in real Chrome) — and returned a non-MediaQueryList object.
#[test]
fn test_match_media_is_not_wrapped() {
    let script = build_evasion_script(&StealthConfig::default());
    assert!(
        !script.contains("originalMatchMedia"),
        "matchMedia must not be wrapped: stock headless/new reports the correct pair"
    );
}

// Audit regression: fallback voices must match the claimed platform.
// Alex/Samantha are macOS-only; Microsoft David/Zira are Windows-only;
// stock Linux Chrome has no built-in voices (pass-through).
#[test]
fn test_speech_voices_match_platform() {
    let voices_for = |ua: &str| {
        let fp = Fingerprint::resolve(Some(ua), None);
        let script = build_evasion_script_for(&StealthConfig::default(), &fp);
        let start = script
            .find("const defaultVoices = ")
            .expect("voices array present")
            + "const defaultVoices = ".len();
        let end = script[start..]
            .find(".map(v =>")
            .expect("voices array terminated");
        script[start..start + end].trim().to_string()
    };

    let linux = voices_for("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36");
    assert_eq!(
        linux, "[]",
        "Linux identities must pass through with no voices"
    );

    let mac = voices_for("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36");
    let mac_json: serde_json::Value =
        serde_json::from_str(&mac).expect("macOS voices must be valid JSON");
    let mac_names: Vec<&str> = mac_json
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert!(mac_names.contains(&"Alex"), "macOS list must contain Alex");
    assert!(!mac_names.iter().any(|n| n.starts_with("Microsoft")));

    let win = voices_for("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36");
    let win_json: serde_json::Value =
        serde_json::from_str(&win).expect("Windows voices must be valid JSON");
    let win_names: Vec<&str> = win_json
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert!(
        win_names.iter().any(|n| n.starts_with("Microsoft David")),
        "Windows list must contain Microsoft David"
    );
    assert!(!win_names.contains(&"Alex"), "Alex is macOS-only");
}

// Audit regression: fake media-device IDs used to be one hardcoded
// constant shared by every eoka session, linking all identities. They are
// now fingerprint data (spec + noise seed), stable per identity and
// distinct across identities.
#[test]
fn test_media_device_ids_are_seeded_per_fingerprint() {
    let devices_json = |script: &str| -> serde_json::Value {
        let start = script
            .find("const fakeDevices = ")
            .expect("fake device list present")
            + "const fakeDevices = ".len();
        let end = script[start..].find(';').expect("device list terminated");
        serde_json::from_str(&script[start..start + end])
            .expect("media device list must be valid JSON")
    };

    let script = build_evasion_script(&StealthConfig::default());
    assert!(
        !script.contains("5f8b2c1e9a3d"),
        "hardcoded cross-session groupId must not reappear"
    );

    let devices = devices_json(&script).as_array().cloned().unwrap();
    assert!(!devices.is_empty());
    let mut ids: Vec<&str> = devices
        .iter()
        .map(|d| d["deviceId"].as_str().unwrap())
        .collect();
    let defaults = ids.iter().filter(|id| **id == "default").count();
    ids.retain(|id| *id != "default");
    assert_eq!(
        ids.len(),
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        "non-default device IDs must be unique within an identity"
    );
    assert!(
        ids.iter().all(|id| id.len() == 12),
        "device IDs must be 12 hex chars like real enumerated IDs"
    );
    assert!(defaults >= 2, "audio in/out pair should include 'default'");

    // Different fingerprints must present different hardware IDs. The
    // identity (and therefore the seed) is randomized per build.
    let fps_a = Fingerprint::random();
    let fps_b = Fingerprint::random();
    let ids_a =
        devices_json(&build_evasion_script_for(&StealthConfig::default(), &fps_a)).to_string();
    let ids_b =
        devices_json(&build_evasion_script_for(&StealthConfig::default(), &fps_b)).to_string();
    assert_ne!(
        ids_a, ids_b,
        "distinct identities must not share device IDs (linkability)"
    );
}

// Audit regression: canvas reads through getImageData must carry the same
// noise as toDataURL/toBlob, or a page comparing both flags the hook.
#[test]
fn test_get_image_data_is_noised_with_canvas_spoof() {
    let on = build_evasion_script(&StealthConfig::default());
    assert!(
        on.contains("CanvasRenderingContext2D.prototype.getImageData"),
        "getImageData must be hooked when canvas spoofing is on"
    );

    let off = build_evasion_script(&StealthConfig {
        webgl_spoof: false,
        canvas_spoof: false,
        audio_spoof: false,
        ..StealthConfig::default()
    });
    assert!(
        !off.contains("getImageData"),
        "canvas hooks must be absent when all fingerprint spoofs are off"
    );
}

// Audit regression: the Intl shim must not mutate the caller's options
// object (observable side effect).
#[test]
fn test_intl_shim_clones_options() {
    let script = build_evasion_script(&StealthConfig::default());
    assert!(
        script.contains("Object.assign({}, options)"),
        "Intl.DateTimeFormat shim must clone options before injecting timeZone"
    );
}
