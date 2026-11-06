//! Stealth regression checks: launch with the default (shipping) config and
//! read the signals common bot detectors look at from a local page.
//!
//! Gaps eoka does not close yet are named `known_gap_*`. CI skips them, and
//! they turn into regular tests once fixed.

mod support;

use serde_json::Value;
use support::{HttpFixture, Response};

const PROBE: &str = r#"(async () => {
  const gl = document.createElement('canvas').getContext('webgl');
  const debug = gl && gl.getExtension('WEBGL_debug_renderer_info');
  const brands = navigator.userAgentData
    ? navigator.userAgentData.brands.map(b => b.brand) : [];
  let permission = null;
  try { permission = (await navigator.permissions.query({ name: 'notifications' })).state; } catch (_) {}
  return {
    webdriver: navigator.webdriver,
    cdcGlobals: Object.keys(window).filter(k => /^\$?cdc_|^\$wdc_/.test(k)),
    chromeKeys: typeof window.chrome === 'object' && window.chrome !== null ? Object.keys(window.chrome).sort() : null,
    plugins: navigator.plugins.length,
    mimeTypes: navigator.mimeTypes.length,
    languages: navigator.languages,
    userAgent: navigator.userAgent,
    appVersion: navigator.appVersion,
    brands,
    notification: typeof Notification === 'undefined' ? null : Notification.permission,
    permission,
    webglVendor: debug ? gl.getParameter(debug.UNMASKED_VENDOR_WEBGL) : null,
    webglRenderer: debug ? gl.getParameter(debug.UNMASKED_RENDERER_WEBGL) : null,
    outer: [window.outerWidth, window.outerHeight],
    inner: [window.innerWidth, window.innerHeight],
    hardwareConcurrency: navigator.hardwareConcurrency,
  };
})()"#;

async fn probe() -> (Value, String) {
    let fixture = HttpFixture::start(|request| async move {
        let accept_language = request
            .headers
            .get("accept-language")
            .cloned()
            .unwrap_or_default();
        Response::html(&format!(
            "<!doctype html><title>probe</title><meta name=al content=\"{accept_language}\">"
        ))
    })
    .await;
    let browser = eoka::Browser::launch().await.unwrap();
    let page = browser.new_page(&fixture.url).await.unwrap();
    let signals: Value = page.evaluate(PROBE).await.unwrap();
    let accept_language: String = page
        .evaluate("document.querySelector('meta[name=al]').content")
        .await
        .unwrap();
    browser.close().await.unwrap();
    eprintln!("signals: {signals:#}\naccept-language: {accept_language}");
    (signals, accept_language)
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn default_launch_hides_automation_signals() {
    let (s, accept_language) = probe().await;

    assert_ne!(s["webdriver"], Value::Bool(true), "navigator.webdriver");
    assert_eq!(s["cdcGlobals"], serde_json::json!([]), "driver globals");
    // Real Chrome exposes window.chrome with app/csi/loadTimes on every page.
    let chrome_keys = s["chromeKeys"].as_array().expect("window.chrome missing");
    for key in ["app", "csi", "loadTimes"] {
        assert!(
            chrome_keys.iter().any(|k| k == key),
            "window.chrome.{key}: {chrome_keys:?}"
        );
    }
    assert!(s["plugins"].as_u64().unwrap() > 0, "navigator.plugins");
    assert!(s["mimeTypes"].as_u64().unwrap() > 0, "navigator.mimeTypes");
    assert!(s["hardwareConcurrency"].as_u64().unwrap() > 0);

    for field in ["userAgent", "appVersion"] {
        let value = s[field].as_str().unwrap();
        assert!(!value.contains("Headless"), "{field}: {value}");
    }
    let brands = s["brands"].as_array().unwrap();
    assert!(
        brands
            .iter()
            .all(|b| !b.as_str().unwrap().contains("Headless")),
        "userAgentData brands: {brands:?}"
    );

    // One well-formed quality value per language (Chrome formats it).
    assert!(
        accept_language
            .split(',')
            .all(|part| part.matches(";q=").count() <= 1),
        "malformed Accept-Language: {accept_language}"
    );
    // The JS language list and the Accept-Language header must agree.
    let first_language = s["languages"][0].as_str().unwrap();
    assert!(
        accept_language.starts_with(first_language),
        "navigator.languages {} vs Accept-Language {accept_language}",
        s["languages"]
    );

    // Headless Chrome reports Notification.permission "denied" while the
    // Permissions API says "prompt"; real browsers agree.
    if let (Some(notification), Some(permission)) =
        (s["notification"].as_str(), s["permission"].as_str())
    {
        let expected = if notification == "default" {
            "prompt"
        } else {
            notification
        };
        assert_eq!(permission, expected, "Notification vs permissions.query");
    }

    assert!(
        s["webglVendor"].is_string(),
        "WebGL vendor: {}",
        s["webglVendor"]
    );
    assert!(
        s["webglRenderer"].is_string(),
        "WebGL renderer: {}",
        s["webglRenderer"]
    );
}

#[tokio::test]
#[ignore = "known gap: window outer size equals inner size, unlike a real window with browser chrome"]
async fn known_gap_outer_window_is_larger_than_viewport() {
    let (s, _) = probe().await;
    assert!(
        s["outer"][1].as_u64() > s["inner"][1].as_u64(),
        "outer {} vs inner {}",
        s["outer"],
        s["inner"]
    );
}

#[tokio::test]
#[ignore = "known gap: WebGL renderer exposes SwiftShader (software rendering)"]
async fn known_gap_webgl_renderer_is_not_software() {
    let (s, _) = probe().await;
    let renderer = s["webglRenderer"].as_str().unwrap_or_default();
    assert!(!renderer.contains("SwiftShader"), "{renderer}");
}
