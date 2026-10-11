//! Browser fingerprint generation
//!
//! Generates realistic, internally consistent browser fingerprints.
//!
//! Ephemeral browsers may use a random identity. A browser backed by a durable
//! user-data directory must not: changing WebGL, CPU, or memory values between
//! visits makes a supposedly persistent login look like a new device.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::json;

mod catalog;
pub(crate) use catalog::GENERIC_FONTS;
use catalog::{
    base_fonts_for, default_voices_for, platform_screen, platform_surfaces, CHROME_VERSIONS,
    US_LAYOUT_ENTRIES,
};

const PERSISTED_FINGERPRINT_FILE: &str = ".eoka-fingerprint.json";

/// Pick a random element from a slice
fn choose<T>(slice: &[T]) -> &T {
    &slice[fastrand::usize(..slice.len())]
}

/// A system text-to-speech voice as reported by `speechSynthesis.getVoices()`.
/// Field names serialize exactly as the Web Speech API spells them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechVoice {
    /// Voice display name, e.g. "Alex".
    pub name: String,
    /// BCP-47 language tag, e.g. "en-US".
    pub lang: String,
    /// Whether the voice is available offline.
    pub local_service: bool,
    /// Whether this is the platform default voice.
    pub default: bool,
    /// Voice identifier (same as `name` for system voices).
    #[serde(rename = "voiceURI")]
    pub voice_uri: String,
}

/// Typical audio/video hardware for one identity, as produced by a fake
/// `mediaDevices.enumerateDevices()` when the real list is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaDevicesSpec {
    /// Microphone count.
    pub audio_inputs: u32,
    /// Speaker/output count.
    pub audio_outputs: u32,
    /// Camera count.
    pub video_inputs: u32,
}

impl Default for MediaDevicesSpec {
    fn default() -> Self {
        Self {
            audio_inputs: 1,
            audio_outputs: 1,
            video_inputs: 1,
        }
    }
}

impl MediaDevicesSpec {
    /// Plausible hardware for a claimed platform. Windows/macOS identities
    /// look like laptops (webcam present); Linux desktops frequently have
    /// none.
    fn for_platform(platform: Platform) -> Self {
        match platform {
            Platform::Linux => Self {
                video_inputs: *choose(&[0, 0, 1]),
                ..Self::default()
            },
            _ => Self {
                audio_inputs: 1,
                audio_outputs: *choose(&[1, 2]),
                video_inputs: 1,
            },
        }
    }
}

/// The UA string for a platform + Chrome full version.
fn ua_for(platform: Platform, chrome_full_version: &str) -> String {
    match platform {
        Platform::MacOS => format!(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{} Safari/537.36",
            chrome_full_version
        ),
        Platform::Windows => format!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{} Safari/537.36",
            chrome_full_version
        ),
        Platform::Linux => format!(
            "Mozilla/5.0 (X11; Linux {}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{} Safari/537.36",
            native_linux_arch(), chrome_full_version
        ),
    }
}

/// Generate a random realistic user agent.
pub fn random_user_agent() -> String {
    Fingerprint::random().user_agent
}

/// Browser fingerprint data — a single coherent identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fingerprint {
    /// Full User-Agent string.
    pub user_agent: String,
    /// Operating system platform.
    pub platform: Platform,
    /// Chrome major version, e.g. "140" (drives `Sec-CH-UA` brand versions).
    pub chrome_major: String,
    /// Chrome full version, e.g. "140.0.7312.86".
    pub chrome_full_version: String,
    /// Client-hint OS version, e.g. "15.0.0".
    pub platform_version: String,
    /// Screen width in pixels.
    pub screen_width: u32,
    /// Screen height in pixels.
    pub screen_height: u32,
    /// Screen color depth in bits.
    pub color_depth: u8,
    /// `navigator.hardwareConcurrency` (logical CPU count).
    pub hardware_concurrency: u8,
    /// `navigator.deviceMemory` in gigabytes.
    pub device_memory: u8,
    /// IANA timezone name.
    pub timezone: String,
    /// `navigator.languages` list.
    pub languages: Vec<String>,
    /// Spoofed WebGL unmasked vendor.
    pub webgl_vendor: String,
    /// Spoofed WebGL unmasked renderer.
    pub webgl_renderer: String,
    /// Stable canvas/audio perturbation seed for this identity.
    #[serde(default = "random_noise_seed")]
    pub noise_seed: u32,
    /// Platform-typical fallback for `speechSynthesis.getVoices()` when the
    /// real list is empty (empty for Linux: stock Linux Chrome has none).
    pub voices: Vec<SpeechVoice>,
    /// Fake `mediaDevices.enumerateDevices()` hardware counts when the real
    /// list is empty.
    pub media_devices: MediaDevicesSpec,
    /// System fonts claimed installed for this identity (drives the
    /// `document.fonts.check` spoof). Generic families are always allowed on
    /// top of this set.
    pub fonts: Vec<String>,
}

/// Operating system platform for a fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Platform {
    /// macOS.
    MacOS,
    /// Windows.
    Windows,
    /// Linux.
    Linux,
}

impl Fingerprint {
    /// Build a platform-consistent UA for the actual Chrome binary Eoka is
    /// launching. This avoids advertising an obsolete Chrome release while
    /// the browser exposes a newer TLS and JavaScript surface.
    pub fn native_chrome_user_agent(chrome_full_version: &str) -> Option<String> {
        let major = chrome_full_version.split('.').next()?;
        if major.is_empty()
            || !major.bytes().all(|byte| byte.is_ascii_digit())
            || chrome_full_version
                .split('.')
                .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return None;
        }
        Some(ua_for(native_platform(), chrome_full_version))
    }

    /// Build a fully coherent identity for a platform + Chrome version.
    fn build(platform: Platform, chrome_major: String, chrome_full_version: String) -> Self {
        let (webgl_vendor, webgl_renderer, platform_version) = platform_surfaces(platform);
        let (screen_width, screen_height) = platform_screen(platform);
        Self {
            user_agent: ua_for(platform, &chrome_full_version),
            platform,
            chrome_major,
            chrome_full_version,
            platform_version,
            screen_width,
            screen_height,
            color_depth: 24,
            hardware_concurrency: *choose(&[4, 8, 10, 12, 16]),
            // Chrome reports deviceMemory capped at 8 (spec: one of
            // 0.25..8); 16/32 are impossible values and an instant tell.
            device_memory: *choose(&[4, 8, 8]),
            timezone: choose(super::evasions::COMMON_TIMEZONES).to_string(),
            languages: vec!["en-US".to_string(), "en".to_string()],
            webgl_vendor: webgl_vendor.to_string(),
            webgl_renderer,
            noise_seed: random_noise_seed(),
            voices: default_voices_for(platform),
            media_devices: MediaDevicesSpec::for_platform(platform),
            fonts: {
                let mut fonts: Vec<String> = base_fonts_for(platform)
                    .iter()
                    .map(|f| (*f).to_string())
                    .collect();
                // Linux font sets differ the most between machines; rotate and
                // trim a random tail so identities don't all look identical.
                if platform == Platform::Linux {
                    let rotate = fastrand::usize(..fonts.len());
                    fonts.rotate_left(rotate);
                    let drop = choose(&[1usize, 1, 2, 3]);
                    fonts.truncate(fonts.len() - drop);
                }
                fonts
            },
        }
    }

    /// Generate a random, internally consistent fingerprint.
    pub fn random() -> Self {
        // 30% Mac, 70% Windows (roughly matches desktop traffic).
        let platform = if fastrand::f64() < 0.3 {
            Platform::MacOS
        } else {
            Platform::Windows
        };
        let (major, full) = *choose(CHROME_VERSIONS);
        Self::build(platform, major.to_string(), full.to_string())
    }

    /// Resolve a fingerprint, honoring an explicit UA override or a random identity.
    pub fn resolve(user_agent: Option<&str>, timezone: Option<&str>) -> Self {
        let mut fp = match user_agent {
            Some(ua) => {
                let platform = if ua.contains("Macintosh") || ua.contains("Mac OS X") {
                    Platform::MacOS
                } else if ua.contains("Linux") {
                    Platform::Linux
                } else {
                    Platform::Windows
                };
                let (major, full) = ua
                    .split("Chrome/")
                    .nth(1)
                    .and_then(|rest| rest.split(' ').next())
                    .filter(|v| !v.is_empty())
                    .map(|v| (v.split('.').next().unwrap_or(v).to_string(), v.to_string()))
                    .unwrap_or_else(|| {
                        let (m, f) = *choose(CHROME_VERSIONS);
                        (m.to_string(), f.to_string())
                    });
                let mut fp = Self::build(platform, major, full);
                fp.user_agent = ua.to_string();
                fp
            }
            None => Self::random(),
        };
        if let Some(tz) = timezone {
            fp.timezone = tz.to_string();
        }
        fp
    }

    /// Resolves an identity for a browser profile, storing it beside the
    /// profile on first use. Explicit UA/timezone values are allowed only when
    /// they agree with an existing identity; silently changing either would
    /// defeat the point of a durable profile.
    pub fn resolve_for_profile(
        user_agent: Option<&str>,
        timezone: Option<&str>,
        profile_dir: Option<&Path>,
    ) -> io::Result<Self> {
        let Some(profile_dir) = profile_dir else {
            return Ok(Self::resolve(user_agent, timezone));
        };

        let path = profile_dir.join(PERSISTED_FINGERPRINT_FILE);
        if path.exists() {
            let contents = fs::read(&path)?;
            let fingerprint: Self = serde_json::from_slice(&contents).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "invalid persisted browser identity {}: {error}",
                        path.display()
                    ),
                )
            })?;
            if let Some(user_agent) = user_agent {
                if user_agent != fingerprint.user_agent {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "configured user agent conflicts with persisted browser identity",
                    ));
                }
            }
            if let Some(timezone) = timezone {
                if timezone != fingerprint.timezone {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "configured timezone conflicts with persisted browser identity",
                    ));
                }
            }
            return Ok(fingerprint);
        }

        let fingerprint = Self::resolve(user_agent, timezone);
        let temporary = profile_dir.join(format!(
            "{PERSISTED_FINGERPRINT_FILE}.tmp-{}",
            std::process::id()
        ));
        let encoded = serde_json::to_vec_pretty(&fingerprint).map_err(io::Error::other)?;
        fs::write(&temporary, encoded)?;
        fs::rename(temporary, path)?;
        Ok(fingerprint)
    }

    /// `navigator.platform` value consistent with this fingerprint.
    pub fn nav_platform(&self) -> &'static str {
        match self.platform {
            Platform::MacOS => "MacIntel",
            Platform::Windows => "Win32",
            Platform::Linux => "Linux x86_64",
        }
    }

    /// Client-hint `architecture` for the claimed platform. `platform_surfaces`
    /// picks Apple-silicon renderers for macOS, so macOS claims are ARM.
    pub fn ch_architecture(&self) -> &'static str {
        match self.platform {
            Platform::MacOS => "arm",
            Platform::Windows | Platform::Linux => "x86",
        }
    }

    /// Assumed OS chrome (taskbar/menu bar) height subtracted from
    /// `screen.height` when spoofing `screen.availHeight`.
    pub fn ui_chrome_height(&self) -> u32 {
        match self.platform {
            Platform::MacOS => 25,
            Platform::Windows => 40,
            Platform::Linux => 30,
        }
    }

    /// JSON `KeyboardLayoutMap` entries for the claimed platform, or `null`
    /// where the Keyboard API doesn't exist in stock Chrome (Linux). All
    /// eoka identities use en-US locales, so the layout is US ANSI.
    pub fn keyboard_map_json(&self) -> String {
        match self.platform {
            Platform::Linux => "null".to_string(),
            Platform::Windows | Platform::MacOS => {
                let mut map = serde_json::Map::new();
                for (code, key) in US_LAYOUT_ENTRIES {
                    map.insert((*code).to_string(), json!((*key).to_string()));
                }
                for letter in b'a'..=b'z' {
                    let key = (letter as char).to_string();
                    map.insert(format!("Key{}", key.to_ascii_uppercase()), json!(key));
                }
                for digit in 0..10u8 {
                    map.insert(format!("Digit{digit}"), json!(digit.to_string()));
                }
                serde_json::to_string(&map).unwrap_or_else(|_| "null".to_string())
            }
        }
    }

    /// JSON device list for the `mediaDevices.enumerateDevices` fallback,
    /// built from the spec and noise seed so IDs are stable per identity.
    pub fn media_devices_json(&self) -> String {
        let mut seed = self.noise_seed as u64 ^ 0x9e37_79b9_7f4a_7c15;
        let mut device_id = move || {
            let mut id = String::with_capacity(12);
            for _ in 0..12 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                id.push(char::from_digit(((seed >> 28) & 0xf) as u32, 16).unwrap_or('0'));
            }
            id
        };

        let mut devices: Vec<serde_json::Value> = Vec::new();
        let audio_group = device_id();
        for index in 0..self.media_devices.audio_inputs {
            devices.push(json!({
                "deviceId": if index == 0 { "default".into() } else { device_id() },
                "groupId": audio_group,
                "kind": "audioinput",
                "label": "",
            }));
        }
        for index in 0..self.media_devices.audio_outputs {
            devices.push(json!({
                "deviceId": if index == 0 { "default".into() } else { device_id() },
                "groupId": audio_group,
                "kind": "audiooutput",
                "label": "",
            }));
        }
        for index in 0..self.media_devices.video_inputs {
            devices.push(json!({
                "deviceId": if index == 0 { "default".into() } else { device_id() },
                "groupId": device_id(),
                "kind": "videoinput",
                "label": "",
            }));
        }
        serde_json::to_string(&devices).unwrap_or_else(|_| "[]".to_string())
    }

    /// `Sec-CH-UA-Platform` value ("macOS" / "Windows").
    pub fn ch_platform(&self) -> &'static str {
        match self.platform {
            Platform::MacOS => "macOS",
            Platform::Windows => "Windows",
            Platform::Linux => "Linux",
        }
    }

    /// Client-hint brand list `(brand, version)` using the major version.
    pub fn ch_brands(&self) -> Vec<(String, String)> {
        vec![
            ("Chromium".to_string(), self.chrome_major.clone()),
            ("Google Chrome".to_string(), self.chrome_major.clone()),
            ("Not?A_Brand".to_string(), "24".to_string()),
        ]
    }

    /// Full-version brand list `(brand, full_version)` for
    /// `Sec-CH-UA-Full-Version-List`.
    pub fn ch_full_version_brands(&self) -> Vec<(String, String)> {
        vec![
            ("Chromium".to_string(), self.chrome_full_version.clone()),
            (
                "Google Chrome".to_string(),
                self.chrome_full_version.clone(),
            ),
            ("Not?A_Brand".to_string(), "24.0.0.0".to_string()),
        ]
    }
}

fn random_noise_seed() -> u32 {
    fastrand::u32(1..=u32::MAX)
}

fn native_platform() -> Platform {
    #[cfg(target_os = "macos")]
    {
        Platform::MacOS
    }
    #[cfg(target_os = "windows")]
    {
        Platform::Windows
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        Platform::Linux
    }
}

fn native_linux_arch() -> &'static str {
    #[cfg(target_arch = "aarch64")]
    {
        "aarch64"
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        "x86_64"
    }
}

#[cfg(test)]
mod tests;
