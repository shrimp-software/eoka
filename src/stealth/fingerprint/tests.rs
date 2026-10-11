use super::catalog::*;
use super::*;

#[test]
fn test_random_user_agent_format() {
    for _ in 0..20 {
        let ua = random_user_agent();
        assert!(ua.starts_with("Mozilla/5.0"));
        assert!(ua.contains("Chrome/"));
        assert!(ua.contains("Safari/537.36"));
    }
}

#[test]
fn test_device_memory_never_exceeds_chrome_cap() {
    // Regression: 16/32 are impossible values — Chrome's spec buckets end
    // at 8, and an out-of-range report is an instant bot tell.
    for _ in 0..200 {
        let fp = Fingerprint::random();
        assert!(
            fp.device_memory <= 8,
            "device_memory {} exceeds Chrome's cap of 8",
            fp.device_memory
        );
        assert!([0.25_f64, 0.5, 1.0, 2.0, 4.0, 8.0].contains(&(fp.device_memory as f64)));
    }
}

#[test]
fn test_fingerprint_random_is_consistent() {
    for _ in 0..50 {
        let fp = Fingerprint::random();
        assert!(!fp.user_agent.is_empty());
        assert!(fp.screen_width > 0 && fp.screen_height > 0);
        assert!([4, 8, 10, 12, 16].contains(&fp.hardware_concurrency));
        // Chrome caps the reported value at 8 (spec bucket maximum).
        assert!([4, 8].contains(&fp.device_memory));
        match fp.platform {
            Platform::MacOS => {
                assert!(fp.user_agent.contains("Macintosh"));
                assert_eq!(fp.nav_platform(), "MacIntel");
                assert_eq!(fp.ch_platform(), "macOS");
                // Chrome freezes the macOS UA token at 10_15_7.
                assert!(fp.user_agent.contains("Mac OS X 10_15_7"));
            }
            Platform::Windows => {
                assert!(fp.user_agent.contains("Windows NT 10.0"));
                assert_eq!(fp.nav_platform(), "Win32");
                assert_eq!(fp.ch_platform(), "Windows");
            }
            Platform::Linux => {
                unreachable!("random profiles currently target macOS or Windows")
            }
        }
        assert!(fp.user_agent.contains(&fp.chrome_full_version));
        assert!(fp.chrome_full_version.starts_with(&fp.chrome_major));
    }
}

#[test]
fn test_resolve_respects_pinned_ua() {
    let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36";
    let fp = Fingerprint::resolve(Some(ua), None);
    assert_eq!(fp.user_agent, ua);
    assert_eq!(fp.platform, Platform::Windows);
    assert_eq!(fp.chrome_major, "140");
    assert_eq!(fp.nav_platform(), "Win32");
}

// Coherence matrix: whatever the claimed platform, every platform-derived
// surface must agree — architecture, OS chrome height, voices, devices,
// fonts, keyboard.
#[test]
fn test_platform_surfaces_are_coherent() {
    let cases = [
        (
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36",
            "x86",
            40,
            "Microsoft David",
            &["Segoe UI"][..],
        ),
        (
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36",
            "arm",
            25,
            "Alex",
            &["Helvetica"][..],
        ),
        (
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36",
            "x86",
            30,
            "",
            &["DejaVu", "Liberation", "Noto"][..],
        ),
    ];
    for seed in 0..64 {
        fastrand::seed(seed);
        for (ua, architecture, chrome_height, expected_voice, expected_font_prefixes) in cases {
            let fp = Fingerprint::resolve(Some(ua), None);
            assert_eq!(fp.ch_architecture(), architecture);
            assert_eq!(fp.ui_chrome_height(), chrome_height);

            if expected_voice.is_empty() {
                assert!(fp.voices.is_empty(), "Linux claims must have no voices");
            } else {
                assert!(
                    fp.voices.iter().any(|v| v.name.starts_with(expected_voice)),
                    "voices must match the claimed platform (expected {expected_voice})"
                );
            }

            // Linux deliberately varies its installed fonts; a particular family
            // can be absent. Require a platform-appropriate family without requiring
            // DejaVu specifically, and keep rejecting other platforms' signatures.
            assert!(
                fp.fonts.iter().any(|font| expected_font_prefixes
                    .iter()
                    .any(|prefix| font.starts_with(prefix))),
                "fonts must match {expected_font_prefixes:?} (seed {seed}): {:?}",
                fp.fonts
            );
            let disallowed: &[&str] = match fp.platform {
                Platform::Windows => &["Helvetica Neue", "Cantarell", "DejaVu Sans"],
                Platform::MacOS => &["Segoe UI", "Cantarell", "DejaVu Sans"],
                Platform::Linux => &["Segoe UI", "Helvetica Neue"],
            };
            for other_font in disallowed {
                assert!(
                    !fp.fonts.iter().any(|f| f.starts_with(other_font)),
                    "cross-platform font {other_font} must not be claimed here"
                );
            }

            // Keyboard: Linux has no Keyboard API in stock Chrome; Win/macOS
            // get a complete US layout.
            let keyboard = fp.keyboard_map_json();
            if expected_voice.is_empty() {
                assert_eq!(keyboard, "null");
            } else {
                let map: serde_json::Map<String, serde_json::Value> =
                    serde_json::from_str(&keyboard).expect("keyboard map must be valid JSON");
                assert_eq!(map.get("KeyA").and_then(|v| v.as_str()), Some("a"));
                assert_eq!(map.get("Digit1").and_then(|v| v.as_str()), Some("1"));
                assert_eq!(map.get("Minus").and_then(|v| v.as_str()), Some("-"));
                assert_eq!(map.len(), 26 + 10 + 12);
            }

            let devices: serde_json::Value =
                serde_json::from_str(&fp.media_devices_json()).expect("valid device JSON");
            let array = devices.as_array().unwrap();
            let expected_len = (fp.media_devices.audio_inputs
                + fp.media_devices.audio_outputs
                + fp.media_devices.video_inputs) as usize;
            assert_eq!(array.len(), expected_len);
            for device in array {
                assert_eq!(device["label"], "");
                assert!(device["kind"].is_string());
            }
        }
    }
}

#[test]
fn test_native_chrome_user_agent_uses_supplied_version() {
    let ua = Fingerprint::native_chrome_user_agent("150.0.7871.182").unwrap();
    assert!(ua.contains("Chrome/150.0.7871.182"));
    assert!(Fingerprint::native_chrome_user_agent("not-a-version").is_none());
}

#[test]
fn test_profile_identity_is_stable_across_launches() {
    let dir = std::env::temp_dir().join(format!(
        "eoka-fingerprint-test-{}-{}",
        std::process::id(),
        fastrand::u64(..)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let first =
        Fingerprint::resolve_for_profile(None, Some("America/Los_Angeles"), Some(&dir)).unwrap();
    let second =
        Fingerprint::resolve_for_profile(None, Some("America/Los_Angeles"), Some(&dir)).unwrap();
    assert_eq!(first.user_agent, second.user_agent);
    assert_eq!(first.hardware_concurrency, second.hardware_concurrency);
    assert_eq!(first.device_memory, second.device_memory);
    assert_eq!(first.webgl_renderer, second.webgl_renderer);
    assert_eq!(first.noise_seed, second.noise_seed);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn test_profile_identity_rejects_conflicting_user_agent() {
    let dir = std::env::temp_dir().join(format!(
        "eoka-fingerprint-test-{}-{}",
        std::process::id(),
        fastrand::u64(..)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Fingerprint::resolve_for_profile(Some("Mozilla/5.0 first"), None, Some(&dir)).unwrap();
    assert!(
        Fingerprint::resolve_for_profile(Some("Mozilla/5.0 second"), None, Some(&dir)).is_err()
    );
    std::fs::remove_dir_all(dir).unwrap();
}

// Over many samples, every random fingerprint must be internally coherent:
// the platform, UA string, nav_platform, ch_platform, and Chrome version
// fields all agree with one another.
#[test]
fn test_random_is_internally_coherent_over_samples() {
    for _ in 0..200 {
        let fp = Fingerprint::random();
        match fp.platform {
            Platform::MacOS => {
                assert!(
                    fp.user_agent.contains("Macintosh"),
                    "mac UA missing Macintosh: {}",
                    fp.user_agent
                );
                assert!(
                    fp.user_agent.contains("Mac OS X 10_15_7"),
                    "mac UA missing frozen OS token: {}",
                    fp.user_agent
                );
                assert_eq!(fp.nav_platform(), "MacIntel");
                assert_eq!(fp.ch_platform(), "macOS");
            }
            Platform::Windows => {
                assert!(
                    fp.user_agent.contains("Windows NT 10.0"),
                    "windows UA missing NT token: {}",
                    fp.user_agent
                );
                assert_eq!(fp.nav_platform(), "Win32");
                assert_eq!(fp.ch_platform(), "Windows");
            }
            Platform::Linux => {
                unreachable!("random profiles currently target macOS or Windows")
            }
        }
        // The UA always advertises the full Chrome version, and the full
        // version always begins with the major.
        assert!(
            fp.user_agent.contains(&fp.chrome_full_version),
            "UA missing full version {}: {}",
            fp.chrome_full_version,
            fp.user_agent
        );
        assert!(
            fp.chrome_full_version.starts_with(&fp.chrome_major),
            "full version {} does not start with major {}",
            fp.chrome_full_version,
            fp.chrome_major
        );
    }
}

// Regression: screen resolutions must be partitioned by platform. A Windows
// fingerprint must never surface a Mac-only resolution, and vice versa.
#[test]
fn test_screen_resolution_partitioned_by_platform() {
    // Resolutions unique to each platform's list.
    let mac_only = [(3024u32, 1964u32), (3456, 2234)];
    let windows_only = [(1366u32, 768u32), (1536, 864)];

    // Sanity-check the assumptions against the actual consts.
    for res in &mac_only {
        assert!(SCREEN_RESOLUTIONS_MAC.contains(res));
        assert!(!SCREEN_RESOLUTIONS_WINDOWS.contains(res));
    }
    for res in &windows_only {
        assert!(SCREEN_RESOLUTIONS_WINDOWS.contains(res));
        assert!(!SCREEN_RESOLUTIONS_MAC.contains(res));
    }

    for _ in 0..300 {
        let fp = Fingerprint::random();
        let res = (fp.screen_width, fp.screen_height);
        match fp.platform {
            Platform::Windows => {
                assert!(
                    !mac_only.contains(&res),
                    "windows fingerprint reported mac-only resolution {:?}",
                    res
                );
                assert!(
                    SCREEN_RESOLUTIONS_WINDOWS.contains(&res),
                    "windows resolution {:?} not in windows list",
                    res
                );
            }
            Platform::MacOS => {
                assert!(
                    !windows_only.contains(&res),
                    "mac fingerprint reported windows-only resolution {:?}",
                    res
                );
                assert!(
                    SCREEN_RESOLUTIONS_MAC.contains(&res),
                    "mac resolution {:?} not in mac list",
                    res
                );
            }
            Platform::Linux => {
                unreachable!("random profiles currently target macOS or Windows")
            }
        }
    }
}

#[test]
fn test_resolve_windows_ua_is_coherent() {
    let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36";
    let fp = Fingerprint::resolve(Some(ua), None);
    assert_eq!(fp.platform, Platform::Windows);
    assert_eq!(fp.chrome_major, "140");
    assert_eq!(fp.user_agent, ua, "explicit UA must be preserved verbatim");
    assert!(
        WEBGL_RENDERERS_WINDOWS.contains(&fp.webgl_renderer.as_str()),
        "windows fp got non-windows renderer: {}",
        fp.webgl_renderer
    );
}

#[test]
fn test_resolve_mac_ua_is_coherent() {
    let ua = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36";
    let fp = Fingerprint::resolve(Some(ua), None);
    assert_eq!(fp.platform, Platform::MacOS);
    assert_eq!(fp.chrome_major, "140");
    assert_eq!(fp.user_agent, ua, "explicit UA must be preserved verbatim");
    assert_eq!(fp.nav_platform(), "MacIntel");
    assert!(
        WEBGL_RENDERERS_MAC.contains(&fp.webgl_renderer.as_str()),
        "mac fp got non-mac renderer: {}",
        fp.webgl_renderer
    );
}

#[test]
fn test_resolve_linux_ua_is_coherent() {
    let ua = "Mozilla/5.0 (X11; Linux aarch64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.7871.186 Safari/537.36";
    let fp = Fingerprint::resolve(Some(ua), Some("America/Los_Angeles"));
    assert_eq!(fp.platform, Platform::Linux);
    assert_eq!(fp.nav_platform(), "Linux x86_64");
    assert_eq!(fp.ch_platform(), "Linux");
    assert_eq!(fp.timezone, "America/Los_Angeles");
    assert!(WEBGL_RENDERERS_LINUX.contains(&fp.webgl_renderer.as_str()));
}

#[test]
fn test_resolve_applies_timezone() {
    let fp = Fingerprint::resolve(None, Some("Asia/Tokyo"));
    assert_eq!(fp.timezone, "Asia/Tokyo");
}

#[test]
fn test_resolve_ua_without_chrome_token_falls_back() {
    // A UA with no "Chrome/" token must still yield a plausible Chrome
    // full version via the fallback branch.
    let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Safari/537.36";
    let fp = Fingerprint::resolve(Some(ua), None);
    assert!(
        !fp.chrome_full_version.is_empty(),
        "fallback should populate chrome_full_version"
    );
    assert!(
        fp.chrome_full_version.starts_with(&fp.chrome_major),
        "fallback full version {} must start with major {}",
        fp.chrome_full_version,
        fp.chrome_major
    );
}

#[test]
fn test_ch_brands_shape() {
    let fp = Fingerprint::resolve(
        Some("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36"),
        None,
    );
    let brands = fp.ch_brands();
    assert_eq!(brands.len(), 3);
    assert!(
        brands.contains(&("Google Chrome".to_string(), "140".to_string())),
        "ch_brands must include Google Chrome with major version: {:?}",
        brands
    );
    assert!(
        brands.iter().any(|(b, _)| b == "Not?A_Brand"),
        "ch_brands must include a GREASE Not?A_Brand entry: {:?}",
        brands
    );
}

#[test]
fn test_ch_full_version_brands_shape() {
    let fp = Fingerprint::resolve(
        Some("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36"),
        None,
    );
    let brands = fp.ch_full_version_brands();
    assert_eq!(brands.len(), 3);
    assert!(
        brands.contains(&("Google Chrome".to_string(), "140.0.7312.86".to_string())),
        "ch_full_version_brands must include Google Chrome with full version: {:?}",
        brands
    );
    assert!(
        brands.iter().any(|(b, _)| b == "Not?A_Brand"),
        "ch_full_version_brands must include a GREASE Not?A_Brand entry: {:?}",
        brands
    );
}
