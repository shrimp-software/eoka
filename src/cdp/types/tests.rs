#[test]
fn outer_html_uses_the_protocol_acronym_and_rejects_missing_markup() {
    let response: super::DOMGetOuterHTMLResult =
        serde_json::from_value(serde_json::json!({"outerHTML":"<b>markup</b>"})).unwrap();
    assert_eq!(response.outer_html, "<b>markup</b>");
    for value in [
        serde_json::json!({}),
        serde_json::json!({"outerHtml":"wrong"}),
        serde_json::json!({"outerHTML":null}),
    ] {
        assert!(serde_json::from_value::<super::DOMGetOuterHTMLResult>(value).is_err());
    }
}

use super::*;
use serde_json::json;

#[test]
fn test_box_model_try_center_valid_quad() {
    let bm = BoxModel {
        content: vec![0.0, 0.0, 10.0, 0.0, 10.0, 20.0, 0.0, 20.0],
    };
    assert_eq!(bm.try_center(), Some((5.0, 10.0)));
}

#[test]
fn test_box_model_try_center_empty_is_none() {
    let bm = BoxModel::default();
    assert!(bm.content.is_empty());
    assert_eq!(bm.try_center(), None);
}

#[test]
fn test_box_model_try_center_too_short_is_none() {
    // Fewer than 8 coordinates → degenerate → None.
    let bm = BoxModel {
        content: vec![0.0, 0.0, 10.0, 0.0, 10.0, 20.0],
    };
    assert_eq!(bm.try_center(), None);
}

#[test]
fn test_mouse_event_serializes_held_button_mask() {
    let event = InputDispatchMouseEvent {
        r#type: MouseEventType::MousePressed,
        x: 12.5,
        y: 4.0,
        button: Some(MouseButton::Left),
        click_count: Some(1),
        buttons: Some(1),
        force: Some(0.5),
        delta_x: None,
        delta_y: None,
    };

    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "mousePressed",
            "x": 12.5,
            "y": 4.0,
            "button": "left",
            "clickCount": 1,
            "buttons": 1,
            "force": 0.5,
        })
    );
}

#[test]
fn test_mouse_event_omits_optional_fields() {
    let event = InputDispatchMouseEvent {
        r#type: MouseEventType::MouseMoved,
        x: 0.0,
        y: 0.0,
        button: None,
        click_count: None,
        buttons: None,
        force: None,
        delta_x: None,
        delta_y: None,
    };

    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({ "type": "mouseMoved", "x": 0.0, "y": 0.0 })
    );
}

#[test]
fn test_ua_override_skips_none_fields() {
    let cmd = EmulationSetUserAgentOverride {
        user_agent: "UA/1.0".to_string(),
        accept_language: None,
        platform: None,
        user_agent_metadata: None,
    };
    let value = serde_json::to_value(&cmd).unwrap();
    // Only the userAgent key is present; skip_serializing_if drops the rest.
    assert_eq!(value, json!({ "userAgent": "UA/1.0" }));
}

#[test]
fn test_ua_override_includes_camel_case_fields_when_set() {
    let cmd = EmulationSetUserAgentOverride {
        user_agent: "UA/1.0".to_string(),
        accept_language: Some("en-US".to_string()),
        platform: Some("Linux".to_string()),
        user_agent_metadata: Some(UserAgentMetadata {
            brands: vec![],
            full_version_list: vec![],
            platform: "Linux".to_string(),
            platform_version: "6.0".to_string(),
            architecture: "x86".to_string(),
            model: "".to_string(),
            mobile: false,
            bitness: "64".to_string(),
            wow64: false,
        }),
    };
    let value = serde_json::to_value(&cmd).unwrap();
    let obj = value.as_object().unwrap();
    assert!(obj.contains_key("userAgent"));
    assert!(obj.contains_key("acceptLanguage"));
    assert!(obj.contains_key("platform"));
    assert!(obj.contains_key("userAgentMetadata"));
}

#[test]
fn test_ua_metadata_camel_case_keys() {
    let meta = UserAgentMetadata {
        brands: vec![],
        full_version_list: vec![],
        platform: "Linux".to_string(),
        platform_version: "6.0".to_string(),
        architecture: "x86".to_string(),
        model: "".to_string(),
        mobile: true,
        bitness: "64".to_string(),
        wow64: false,
    };
    let value = serde_json::to_value(&meta).unwrap();
    let obj = value.as_object().unwrap();
    assert!(obj.contains_key("fullVersionList"));
    assert!(obj.contains_key("platformVersion"));
    assert!(obj.contains_key("mobile"));
    assert!(obj.contains_key("bitness"));
    assert!(obj.contains_key("wow64"));
    assert_eq!(obj.get("mobile"), Some(&json!(true)));
}

#[test]
fn test_ua_brand_version_serialization() {
    let bv = UserAgentBrandVersion {
        brand: "Chromium".to_string(),
        version: "120".to_string(),
    };
    let value = serde_json::to_value(&bv).unwrap();
    assert_eq!(value, json!({ "brand": "Chromium", "version": "120" }));
}
