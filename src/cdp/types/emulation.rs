//! Emulation domain protocol types.

use serde::Serialize;

/// Emulation.setScriptExecutionDisabled — switch JS execution off/on for the page
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmulationSetScriptExecutionDisabled {
    pub value: bool,
}

/// Desktop viewport dimensions in CSS pixels and the physical-pixel scale.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmulationSetDeviceMetricsOverride {
    pub width: i32,
    pub height: i32,
    pub device_scale_factor: f64,
    pub mobile: bool,
}

impl EmulationSetDeviceMetricsOverride {
    pub(crate) fn new(width: u32, height: u32, device_scale_factor: f64) -> crate::Result<Self> {
        if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err(crate::Error::cdp_msg(
                "Viewport dimensions must be positive and no greater than i32::MAX",
            ));
        }
        if !device_scale_factor.is_finite() || device_scale_factor <= 0.0 {
            return Err(crate::Error::cdp_msg(
                "Viewport device scale factor must be positive and finite",
            ));
        }
        Ok(Self {
            width: width as i32,
            height: height as i32,
            device_scale_factor,
            mobile: false,
        })
    }
}

/// A single (brand, version) entry in `Sec-CH-UA` client-hint brand lists.
#[derive(Debug, Clone, Serialize)]
pub struct UserAgentBrandVersion {
    pub brand: String,
    pub version: String,
}

/// Emulation.UserAgentMetadata — drives the `Sec-CH-UA*` client-hint headers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserAgentMetadata {
    pub brands: Vec<UserAgentBrandVersion>,
    pub full_version_list: Vec<UserAgentBrandVersion>,
    pub platform: String,
    pub platform_version: String,
    pub architecture: String,
    pub model: String,
    pub mobile: bool,
    pub bitness: String,
    pub wow64: bool,
}

/// Emulation.setUserAgentOverride — override the User-Agent / Accept-Language
/// strings and (optionally) the client-hint metadata.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmulationSetUserAgentOverride {
    pub user_agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accept_language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent_metadata: Option<UserAgentMetadata>,
}

/// Security.setIgnoreCertificateErrors — bypass TLS errors for this session
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecuritySetIgnoreCertificateErrors {
    pub ignore: bool,
}

/// Emulation.setTimezoneOverride — set the browser-native ICU timezone for
/// this session. Unlike a JS shim, this makes `Date.toString()`, `getHours()`,
/// `Intl` and friends natively consistent with the claimed timezone.
#[derive(Debug, Clone, Serialize)]
pub struct EmulationSetTimezoneOverride {
    #[serde(rename = "timezoneId")]
    pub timezone_id: String,
}

#[cfg(test)]
mod tests {
    use super::EmulationSetDeviceMetricsOverride;

    #[test]
    fn viewport_dimensions_reject_zero_and_signed_protocol_overflow() {
        for (width, height) in [
            (0, 1),
            (1, 0),
            (i32::MAX as u32 + 1, 1),
            (1, i32::MAX as u32 + 1),
            (u32::MAX, u32::MAX),
        ] {
            assert!(EmulationSetDeviceMetricsOverride::new(width, height, 1.0).is_err());
        }
        for (width, height) in [(1, 1), (i32::MAX as u32, i32::MAX as u32)] {
            let metrics = EmulationSetDeviceMetricsOverride::new(width, height, 1.0).unwrap();
            assert_eq!(metrics.width as u32, width);
            assert_eq!(metrics.height as u32, height);
        }
    }

    #[test]
    fn viewport_scale_rejects_nonpositive_and_nonfinite_values() {
        for scale in [0.0, -0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(EmulationSetDeviceMetricsOverride::new(1, 1, scale).is_err());
        }
        for scale in [f64::MIN_POSITIVE, 0.5, 1.0, 2.0, f64::MAX] {
            let metrics = EmulationSetDeviceMetricsOverride::new(1, 1, scale).unwrap();
            assert_eq!(metrics.device_scale_factor, scale);
            assert!(!metrics.mobile);
        }
    }
}
