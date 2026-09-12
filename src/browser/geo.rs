use serde_json::Value;

use crate::error::{Error, Result};
use crate::page::Page;

/// One-shot navigation targets for geo alignment. The API returns JSON with
/// `timezone.id` and `country_code`; the trace endpoint is a fallback that
/// only yields the country (`loc=`).
const GEO_API_URL: &str = "https://ipwho.is/";
const GEO_TRACE_URL: &str = "https://www.cloudflare.com/cdn-cgi/trace";

/// Geo info resolved from the browser's apparent public IP.
#[derive(Debug, Clone)]
pub(super) struct GeoInfo {
    pub(super) timezone: Option<String>,
    pub(super) country: String,
}

/// Parse an ipwho.is response into [`GeoInfo`].
fn parse_geo_api(text: &str) -> Option<GeoInfo> {
    let value: Value = serde_json::from_str(text).ok()?;
    let timezone = value.pointer("/timezone/id")?.as_str()?.to_string();
    let country = value.get("country_code")?.as_str()?.to_string();
    if timezone.is_empty()
        || country.len() != 2
        || !country.bytes().all(|byte| byte.is_ascii_alphabetic())
    {
        return None;
    }
    Some(GeoInfo {
        timezone: Some(timezone),
        country: country.to_uppercase(),
    })
}

/// Parse a Cloudflare trace response into [`GeoInfo`] (country only — the
/// trace response has no timezone field).
fn parse_geo_trace(text: &str) -> Option<GeoInfo> {
    let mut country = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("loc=") {
            country = Some(value.trim().to_string());
        }
    }
    let country = country.filter(|c| c.len() == 2 && c.chars().all(|c| c.is_ascii_alphabetic()))?;
    Some(GeoInfo {
        timezone: None,
        country: country.to_uppercase(),
    })
}

/// Browser `Accept-Language`-style list for an IP country code, matching how
/// Chrome orders locales for a default install in that country.
pub(super) fn languages_for_country(country: &str) -> Vec<String> {
    let languages: &[&str] = match country {
        "US" => &["en-US", "en"],
        "GB" => &["en-GB", "en"],
        "AU" | "NZ" => &["en-AU", "en"],
        "CA" => &["en-CA", "fr-CA", "en", "fr"],
        "IE" => &["en-IE", "en"],
        "IN" => &["en-IN", "hi-IN", "en", "hi"],
        "DE" | "AT" => &["de-DE", "de"],
        "CH" => &["de-CH", "fr-CH", "it-CH", "de", "fr", "it"],
        "FR" | "LU" => &["fr-FR", "fr"],
        "BE" => &["nl-BE", "fr-BE", "nl", "fr"],
        "ES" | "MX" | "AR" | "CL" | "CO" => &["es-ES", "es"],
        "BR" => &["pt-BR", "pt"],
        "PT" => &["pt-PT", "pt"],
        "IT" => &["it-IT", "it"],
        "NL" => &["nl-NL", "nl"],
        "SE" => &["sv-SE", "sv"],
        "NO" => &["nb-NO", "no"],
        "DK" => &["da-DK", "da"],
        "FI" => &["fi-FI", "fi"],
        "PL" => &["pl-PL", "pl"],
        "CZ" | "SK" => &["cs-CZ", "cs"],
        "HU" => &["hu-HU", "hu"],
        "RO" => &["ro-RO", "ro"],
        "GR" => &["el-GR", "el"],
        "TR" => &["tr-TR", "tr"],
        "RU" | "BY" | "KZ" => &["ru-RU", "ru"],
        "UA" => &["uk-UA", "ru", "uk"],
        "JP" => &["ja-JP", "ja"],
        "KR" => &["ko-KR", "ko"],
        "CN" => &["zh-CN", "zh"],
        "TW" | "HK" => &["zh-TW", "zh"],
        "VN" => &["vi-VN", "vi"],
        "TH" => &["th-TH", "th"],
        "ID" => &["id-ID", "id"],
        "SA" | "AE" | "EG" => &["ar-SA", "ar"],
        "IL" => &["he-IL", "he"],
        _ => &["en-US", "en"],
    };
    languages.iter().map(|s| (*s).to_string()).collect()
}

/// Navigate a page to `url` and return the body text (used for geo lookups).
async fn read_remote_text(page: &Page, url: &str) -> Result<String> {
    page.goto(url).await?;
    page.text().await
}

pub(super) async fn lookup(page: &Page) -> Result<GeoInfo> {
    if let Some(geo) = read_remote_text(page, GEO_API_URL)
        .await
        .ok()
        .and_then(|text| parse_geo_api(&text))
    {
        return Ok(geo);
    }
    let text = read_remote_text(page, GEO_TRACE_URL).await?;
    parse_geo_trace(&text).ok_or_else(|| Error::Launch("geo response missing loc".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_geo_api_response() {
        let body = r#"{"ip":"203.0.113.9","country_code":"DE","timezone":{"id":"Europe/Berlin","abbr":"CEST"}}"#;
        let geo = parse_geo_api(body).expect("valid geo api response");
        assert_eq!(geo.timezone.as_deref(), Some("Europe/Berlin"));
        assert_eq!(geo.country, "DE");
        assert!(parse_geo_api("not json").is_none());
        assert!(parse_geo_api(r#"{"success":false,"message":"rate limited"}"#).is_none());
    }

    #[test]
    fn parses_geo_trace_response() {
        let trace = "fl=123\n\nip=203.0.113.9\nloc=DE\nsnf=oia\n";
        let geo = parse_geo_trace(trace).expect("valid trace");
        assert_eq!(geo.timezone, None);
        assert_eq!(geo.country, "DE");
    }

    #[test]
    fn rejects_malformed_geo_trace() {
        assert!(parse_geo_trace("fl=1\nip=1.2.3.4\n").is_none());
        assert!(parse_geo_trace("loc=\n").is_none());
        assert!(parse_geo_trace("loc=DEU\n").is_none());
        assert!(parse_geo_trace("").is_none());
    }

    #[test]
    fn languages_match_ip_country() {
        assert_eq!(languages_for_country("DE"), vec!["de-DE", "de"]);
        assert_eq!(languages_for_country("JP"), vec!["ja-JP", "ja"]);
        assert_eq!(
            languages_for_country("CA"),
            vec!["en-CA", "fr-CA", "en", "fr"]
        );
        assert_eq!(languages_for_country("ZZ"), vec!["en-US", "en"]);
    }
}
