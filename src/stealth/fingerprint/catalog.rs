//! Platform-specific fingerprint defaults and lookup tables.

use super::{choose, Platform, SpeechVoice};

/// Recent Chrome versions as (major, full) pairs.
pub(super) const CHROME_VERSIONS: &[(&str, &str)] = &[
    ("133", "133.0.6943.142"),
    ("134", "134.0.6998.118"),
    ("135", "135.0.7049.96"),
    ("136", "136.0.7103.114"),
    ("137", "137.0.7151.119"),
    ("138", "138.0.7204.101"),
    ("139", "139.0.7258.139"),
    ("140", "140.0.7312.86"),
    ("150", "150.0.7871.182"),
];

/// macOS client-hint platform versions (UA string is frozen at 10_15_7).
pub(super) const MACOS_CH_VERSIONS: &[&str] =
    &["13.6.0", "14.4.1", "14.5.0", "14.6.1", "15.0.0", "15.1.0"];

/// Windows client-hint platform versions.
pub(super) const WINDOWS_CH_VERSIONS: &[&str] = &["10.0.0", "15.0.0"];

/// Linux client-hint platform versions. Chromium exposes a kernel-style
/// version here rather than advertising the distribution release.
pub(super) const LINUX_CH_VERSIONS: &[&str] = &["5.15.0", "6.8.0"];

/// WebGL renderers for Mac
pub(super) const WEBGL_RENDERERS_MAC: &[&str] = &[
    "ANGLE (Apple, Apple M1 Pro, OpenGL 4.1)",
    "ANGLE (Apple, Apple M1, OpenGL 4.1)",
    "ANGLE (Apple, Apple M2, OpenGL 4.1)",
    "ANGLE (Apple, Apple M3, OpenGL 4.1)",
    "ANGLE (Intel, Intel Iris Pro Graphics 6200, OpenGL 4.1)",
    "ANGLE (Intel, Intel UHD Graphics 630, OpenGL 4.1)",
];

/// WebGL renderers for Windows
pub(super) const WEBGL_RENDERERS_WINDOWS: &[&str] = &[
    "ANGLE (NVIDIA, NVIDIA GeForce RTX 3080 Direct3D11 vs_5_0 ps_5_0, D3D11)",
    "ANGLE (NVIDIA, NVIDIA GeForce RTX 4070 Direct3D11 vs_5_0 ps_5_0, D3D11)",
    "ANGLE (NVIDIA, NVIDIA GeForce GTX 1080 Direct3D11 vs_5_0 ps_5_0, D3D11)",
    "ANGLE (AMD, AMD Radeon RX 6800 XT Direct3D11 vs_5_0 ps_5_0, D3D11)",
    "ANGLE (Intel, Intel(R) UHD Graphics 770 Direct3D11 vs_5_0 ps_5_0, D3D11)",
];

/// Common Chromium-on-Linux software renderer. Native Linux profiles leave
/// WebGL untouched; this is only used when callers explicitly request a
/// synthetic Linux profile.
pub(super) const WEBGL_RENDERERS_LINUX: &[&str] = &[
    "ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (LLVM 16.0.0) (0x0000C0DE)), SwiftShader driver)",
];

/// Platform-specific screen resolutions.
pub(super) const SCREEN_RESOLUTIONS_MAC: &[(u32, u32)] = &[
    (1440, 900),
    (1680, 1050),
    (2560, 1600),
    (1920, 1080),
    (3024, 1964), // MacBook Pro 14"
    (3456, 2234), // MacBook Pro 16"
];
pub(super) const SCREEN_RESOLUTIONS_WINDOWS: &[(u32, u32)] = &[
    (1920, 1080),
    (2560, 1440),
    (3840, 2160),
    (1366, 768),
    (1536, 864),
    (1600, 900),
];
pub(super) const SCREEN_RESOLUTIONS_LINUX: &[(u32, u32)] =
    &[(1920, 1080), (2560, 1440), (1366, 768), (1600, 900)];

/// Platform-typical fallback voices for `speechSynthesis.getVoices()`. Stock
/// Linux Chrome has none (empty list = pass-through); Alex/Samantha are
/// macOS-only, David/Zira are Windows-only.
pub(super) fn default_voices_for(platform: Platform) -> Vec<SpeechVoice> {
    let voices: &[(&str, &str, bool, bool)] = match platform {
        Platform::MacOS => &[
            ("Alex", "en-US", true, true),
            ("Samantha", "en-US", true, false),
            ("Victoria", "en-US", true, false),
            ("Daniel", "en-GB", true, false),
            ("Google US English", "en-US", false, false),
        ],
        Platform::Windows => &[
            (
                "Microsoft David - English (United States)",
                "en-US",
                true,
                true,
            ),
            (
                "Microsoft Zira - English (United States)",
                "en-US",
                true,
                false,
            ),
            (
                "Microsoft Mark - English (United States)",
                "en-US",
                true,
                false,
            ),
            ("Google US English", "en-US", false, false),
        ],
        Platform::Linux => &[],
    };
    voices
        .iter()
        .map(|(name, lang, local, default)| SpeechVoice {
            name: (*name).to_string(),
            lang: (*lang).to_string(),
            local_service: *local,
            default: *default,
            voice_uri: (*name).to_string(),
        })
        .collect()
}

/// Base system font set for a claimed platform, as seen by
/// `document.fonts.check` probing. Order-insensitive; generics are always
/// allowed on top of these.
pub(super) fn base_fonts_for(platform: Platform) -> &'static [&'static str] {
    match platform {
        Platform::Windows => &[
            "Segoe UI",
            "Arial",
            "Calibri",
            "Cambria",
            "Consolas",
            "Courier New",
            "Georgia",
            "Impact",
            "Tahoma",
            "Times New Roman",
            "Trebuchet MS",
            "Verdana",
            "Microsoft Sans Serif",
            "Comic Sans MS",
            "Candara",
            "Franklin Gothic Medium",
            "Rockwell",
        ],
        Platform::MacOS => &[
            "Helvetica",
            "Helvetica Neue",
            "Arial",
            "Menlo",
            "Monaco",
            "Courier New",
            "Georgia",
            "Times New Roman",
            "Palatino",
            "Avenir",
            "Avenir Next",
            "Futura",
            "Geneva",
            "Lucida Grande",
            "Optima",
            "Baskerville",
            "American Typewriter",
        ],
        Platform::Linux => &[
            "DejaVu Sans",
            "DejaVu Serif",
            "DejaVu Sans Mono",
            "Liberation Sans",
            "Liberation Serif",
            "Liberation Mono",
            "Ubuntu",
            "Cantarell",
            "FreeSans",
            "FreeSerif",
            "Noto Sans",
            "Noto Serif",
            "Noto Mono",
        ],
    }
}

/// Font families every real Chrome resolves regardless of installed fonts.
pub(crate) const GENERIC_FONTS: &[&str] = &[
    "serif",
    "sans-serif",
    "monospace",
    "cursive",
    "fantasy",
    "system-ui",
    "ui-sans-serif",
    "ui-serif",
    "ui-monospace",
    "ui-rounded",
    "math",
    "emoji",
    "none",
];

/// US ANSI layout: `KeyboardLayoutMap.get(code)` entries for the punctuation
/// and space keys. Letters and digits are generated programmatically.
pub(super) const US_LAYOUT_ENTRIES: &[(&str, &str)] = &[
    ("Backquote", "`"),
    ("Minus", "-"),
    ("Equal", "="),
    ("BracketLeft", "["),
    ("BracketRight", "]"),
    ("Backslash", "\\"),
    ("Semicolon", ";"),
    ("Quote", "'"),
    ("Comma", ","),
    ("Period", "."),
    ("Slash", "/"),
    ("Space", " "),
];

/// WebGL vendor, WebGL renderer and client-hint platform version for a platform.
pub(super) fn platform_surfaces(platform: Platform) -> (&'static str, String, String) {
    match platform {
        Platform::MacOS => (
            "Google Inc. (Apple)",
            choose(WEBGL_RENDERERS_MAC).to_string(),
            choose(MACOS_CH_VERSIONS).to_string(),
        ),
        Platform::Windows => (
            "Google Inc. (NVIDIA)",
            choose(WEBGL_RENDERERS_WINDOWS).to_string(),
            choose(WINDOWS_CH_VERSIONS).to_string(),
        ),
        Platform::Linux => (
            "Google Inc. (Google)",
            choose(WEBGL_RENDERERS_LINUX).to_string(),
            choose(LINUX_CH_VERSIONS).to_string(),
        ),
    }
}

/// A plausible screen resolution for the platform.
pub(super) fn platform_screen(platform: Platform) -> (u32, u32) {
    match platform {
        Platform::MacOS => *choose(SCREEN_RESOLUTIONS_MAC),
        Platform::Windows => *choose(SCREEN_RESOLUTIONS_WINDOWS),
        Platform::Linux => *choose(SCREEN_RESOLUTIONS_LINUX),
    }
}
