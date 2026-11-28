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
// surface must agree — architecture, voices, devices, fonts, keyboard.
#[test]
fn test_platform_surfaces_are_coherent() {
    let cases = [
        (
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36",
            "x86",
            "Microsoft David",
            &["Segoe UI"][..],
        ),
        (
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36",
            "arm",
            "Alex",
            &["Helvetica"][..],
        ),
        (
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7312.86 Safari/537.36",
            "x86",
            "",
            &["DejaVu", "Liberation", "Noto"][..],
        ),
    ];
    for seed in 0..64 {
        fastrand::seed(seed);
        for (ua, architecture, expected_voice, expected_font_prefixes) in cases {
            let fp = Fingerprint::resolve(Some(ua), None);
            assert_eq!(fp.ch_architecture(), architecture);

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

fn temp_profile() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "eoka-fingerprint-test-{}-{}",
        std::process::id(),
        fastrand::u64(..)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn test_profile_identity_is_stable_across_launches() {
    let dir = temp_profile();
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
    let dir = temp_profile();
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
    // Chrome on ARM Linux reports its real CPU in navigator.platform.
    assert_eq!(fp.nav_platform(), "Linux aarch64");
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

#[test]
fn test_webgl_catalog_has_no_software_renderers() {
    for renderer in WEBGL_RENDERERS_MAC
        .iter()
        .chain(WEBGL_RENDERERS_WINDOWS)
        .chain(WEBGL_RENDERERS_LINUX)
    {
        assert!(!is_software_renderer(renderer), "{renderer}");
        assert!(renderer.starts_with("ANGLE ("), "{renderer}");
    }
}

#[test]
fn test_webgl_vendor_matches_renderer() {
    assert_eq!(
        webgl_vendor_for("ANGLE (Intel, Mesa Intel(R) UHD Graphics 630 (CFL GT2), OpenGL 4.6)"),
        "Google Inc. (Intel)"
    );
    assert_eq!(
        webgl_vendor_for(
            "ANGLE (NVIDIA Corporation, NVIDIA GeForce RTX 3060/PCIe/SSE2, OpenGL 4.5.0)"
        ),
        "Google Inc. (NVIDIA Corporation)"
    );
    for platform in [Platform::MacOS, Platform::Windows, Platform::Linux] {
        for _ in 0..50 {
            let fp = Fingerprint::build(platform, "140".into(), "140.0.7312.86".into());
            let vendor = fp
                .webgl_vendor
                .strip_prefix("Google Inc. (")
                .and_then(|vendor| vendor.strip_suffix(')'))
                .unwrap();
            assert!(
                fp.webgl_renderer.starts_with(&format!("ANGLE ({vendor},")),
                "{} vs {}",
                fp.webgl_vendor,
                fp.webgl_renderer
            );
        }
    }
}

#[test]
fn test_persisted_software_renderer_is_replaced_once() {
    let dir = temp_profile();
    let ua = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.7871.182 Safari/537.36";
    let mut stored = Fingerprint::resolve(Some(ua), None);
    stored.webgl_vendor = "Google Inc. (Google)".into();
    stored.webgl_renderer =
        "ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (LLVM 16.0.0) (0x0000C0DE)), SwiftShader driver)".into();
    stored.persist(&dir).unwrap();

    let first = Fingerprint::resolve_for_profile(Some(ua), None, Some(&dir)).unwrap();
    assert!(WEBGL_RENDERERS_LINUX.contains(&first.webgl_renderer.as_str()));
    assert_eq!(first.webgl_vendor, webgl_vendor_for(&first.webgl_renderer));
    // Everything else about the identity is kept.
    assert_eq!(first.noise_seed, stored.noise_seed);
    assert_eq!(first.timezone, stored.timezone);

    let second = Fingerprint::resolve_for_profile(Some(ua), None, Some(&dir)).unwrap();
    assert_eq!(first.webgl_renderer, second.webgl_renderer);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn test_profile_identity_follows_detected_chrome_update() {
    let dir = temp_profile();
    let old = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/155.0.8059.39 Safari/537.36";
    let new = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.8078.80 Safari/537.36";
    let first = Fingerprint::resolve_for_launch(None, Some(old), None, Some(&dir)).unwrap();
    let second = Fingerprint::resolve_for_launch(None, Some(new), None, Some(&dir)).unwrap();
    assert_eq!(second.user_agent, new);
    assert_eq!(second.chrome_major, "156");
    assert_eq!(second.chrome_full_version, "156.0.8078.80");
    assert_eq!(second.noise_seed, first.noise_seed);
    assert_eq!(second.webgl_renderer, first.webgl_renderer);
    assert_eq!(second.hardware_concurrency, first.hardware_concurrency);
    // The update is stored, not recomputed on every launch.
    let third = Fingerprint::resolve_for_profile(None, None, Some(&dir)).unwrap();
    assert_eq!(third.chrome_full_version, "156.0.8078.80");
    // An explicitly configured agent must still agree with the stored one.
    assert!(Fingerprint::resolve_for_launch(Some(old), Some(new), None, Some(&dir)).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn test_mac_identities_claim_only_apple_silicon() {
    for renderer in WEBGL_RENDERERS_MAC {
        assert!(renderer.starts_with("ANGLE (Apple,"), "{renderer}");
    }
    for _ in 0..50 {
        let fp = Fingerprint::build(Platform::MacOS, "156".into(), "156.0.8078.80".into());
        assert_eq!(fp.ch_architecture(), "arm");
        assert_eq!(fp.webgl_vendor, "Google Inc. (Apple)");
    }
}

#[test]
fn test_persisted_mac_intel_renderer_is_replaced() {
    let dir = temp_profile();
    let ua = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.7871.182 Safari/537.36";
    let mut stored = Fingerprint::resolve(Some(ua), None);
    stored.webgl_vendor = "Google Inc. (Intel)".into();
    stored.webgl_renderer = "ANGLE (Intel, Intel UHD Graphics 630, OpenGL 4.1)".into();
    stored.persist(&dir).unwrap();
    let loaded = Fingerprint::resolve_for_profile(Some(ua), None, Some(&dir)).unwrap();
    assert!(loaded.webgl_renderer.starts_with("ANGLE (Apple,"));
    assert_eq!(loaded.noise_seed, stored.noise_seed);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn test_linux_arm_identity_is_coherent() {
    let ua = "Mozilla/5.0 (X11; Linux aarch64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.8078.80 Safari/537.36";
    let fp = Fingerprint::resolve(Some(ua), None);
    assert_eq!(fp.ch_architecture(), "arm");
    assert_eq!(fp.nav_platform(), "Linux aarch64");
    let x86 = Fingerprint::resolve(Some(&ua.replace("aarch64", "x86_64")), None);
    assert_eq!(x86.ch_architecture(), "x86");
    assert_eq!(x86.nav_platform(), "Linux x86_64");
}

#[cfg(unix)]
#[test]
fn test_persisted_identity_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_profile();
    Fingerprint::resolve_for_profile(None, None, Some(&dir)).unwrap();
    let mode = std::fs::metadata(dir.join(PERSISTED_FINGERPRINT_FILE))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
        .collect();
    assert!(leftovers.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}
