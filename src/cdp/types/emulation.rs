//! Emulation domain protocol types.

use serde::Serialize;

/// Emulation.setScriptExecutionDisabled — switch JS execution off/on for the page
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmulationSetScriptExecutionDisabled {
    pub value: bool,
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
