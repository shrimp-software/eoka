use eoka::{Browser, Expectation, LocatorSpec, Page};
fn value(target: &str, value: &str) -> Expectation {
    Expectation::Value {
        target: LocatorSpec::css(target),
        value: value.into(),
    }
}
use serde_json::Value;

async fn fixture() -> (Browser, Page) {
    let browser = Browser::launch().await.unwrap();
    let page = browser.new_blank_page().await.unwrap();
    page.execute_sync(r#"
        document.body.innerHTML='<input id="field" type="search"><textarea id="area"></textarea><div id="editor" contenteditable="true"></div>';
        window.events=[];
        for(const type of ['keydown','keypress','beforeinput','input','keyup']) {
            document.addEventListener(type,event=>events.push({type,key:event.key ?? null,code:event.code ?? null,trusted:event.isTrusted}));
        }
        document.querySelector('#field').focus();
    "#).await.unwrap();
    (browser, page)
}

#[tokio::test]
#[ignore = "requires Chrome; disposable local document"]
async fn printable_keys_emit_native_text_once() {
    let (browser, page) = fixture().await;
    for (key, expected, code) in [
        ("o", "o", "KeyO"),
        ("O", "O", "KeyO"),
        ("7", "7", "Digit7"),
        ("+", "+", "Equal"),
        ("@", "@", "Digit2"),
        ("Space", " ", "Space"),
        (" ", " ", "Space"),
        ("é", "é", ""),
        ("😀", "😀", ""),
        ("Shift+o", "O", "KeyO"),
        ("Shift+1", "!", "Digit1"),
        ("Shift+=", "+", "Equal"),
    ] {
        page.execute_sync("document.querySelector('#field').value='';events=[]")
            .await
            .unwrap();
        page.press_key(key)
            .expecting(value("#field", expected))
            .await
            .unwrap();
        let value: String = page
            .evaluate_sync("document.querySelector('#field').value")
            .await
            .unwrap();
        assert_eq!(value, expected, "key={key}");
        let events: Vec<Value> = page.evaluate_sync("events").await.unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["keydown", "keypress", "beforeinput", "input", "keyup"],
            "key={key}: {events:?}"
        );
        assert!(events.iter().all(|event| event["trusted"] == true));
        assert_eq!(events[0]["key"], expected);
        assert_eq!(events[0]["code"], code);
    }
    page.execute_sync("document.querySelector('#area').focus();events=[]")
        .await
        .unwrap();
    page.press_key("Enter")
        .expecting(value("#area", "\n"))
        .await
        .unwrap();
    assert_eq!(
        page.evaluate_sync::<String>("document.querySelector('#area').value")
            .await
            .unwrap(),
        "\n"
    );
    page.execute_sync("document.querySelector('#editor').focus();events=[]")
        .await
        .unwrap();
    page.press_key("o")
        .expecting(value("#editor", "o"))
        .await
        .unwrap();
    assert_eq!(
        page.evaluate_sync::<String>("document.querySelector('#editor').textContent")
            .await
            .unwrap(),
        "o"
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; disposable local document"]
async fn held_modifiers_shortcuts_and_prevent_default_preserve_native_semantics() {
    let (browser, page) = fixture().await;
    let attached = browser.attach_page(page.target_id()).await.unwrap();
    page.key_down("Shift").await.unwrap();
    attached.dispatch_key("o").await.unwrap();
    attached.key_up("Shift").await.unwrap();
    page.dispatch_key("o").await.unwrap();
    assert_eq!(
        page.evaluate_sync::<String>("document.querySelector('#field').value")
            .await
            .unwrap(),
        "Oo"
    );
    page.dispatch_key(if cfg!(target_os = "macos") {
        "Cmd+A"
    } else {
        "Ctrl+A"
    })
    .await
    .unwrap();
    assert_eq!(
        page.evaluate_sync::<String>("document.querySelector('#field').value")
            .await
            .unwrap(),
        "Oo"
    );
    page.dispatch_key("Backspace").await.unwrap();
    assert_eq!(
        page.evaluate_sync::<String>("document.querySelector('#field').value")
            .await
            .unwrap(),
        ""
    );
    page.key_down("o").await.unwrap();
    assert!(attached.key_down("o").await.is_err());
    attached.release_all_inputs().await.unwrap();
    assert_eq!(
        page.evaluate_sync::<String>("document.querySelector('#field').value")
            .await
            .unwrap(),
        "o"
    );
    page.execute_sync("document.querySelector('#field').value='';events=[];document.addEventListener('keydown',event=>{if(event.key==='q')event.preventDefault()})").await.unwrap();
    page.dispatch_key("q").await.unwrap();
    assert_eq!(
        page.evaluate_sync::<String>("document.querySelector('#field').value")
            .await
            .unwrap(),
        ""
    );
    let events: Vec<Value> = page.evaluate_sync("events").await.unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["keydown", "keyup"]
    );
    page.human().press_key("z").await.unwrap();
    page.human().type_text("ok").await.unwrap();
    assert_eq!(
        page.evaluate_sync::<String>("document.querySelector('#field').value")
            .await
            .unwrap(),
        "zok"
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn human_typing_sends_key_events_and_verifies_the_value() {
    let browser = eoka::Browser::launch_with(|c| {
        c.chrome_path = std::env::var("EOKA_TEST_CHROME_PATH").ok();
        c.patch_binary = false;
    })
    .await
    .unwrap();
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML='<input id=f>';window.keys=[];const f=document.querySelector('#f');for(const t of ['keydown','keyup'])f.addEventListener(t,e=>keys.push([t,e.key,e.code,e.shiftKey,e.isTrusted]))").await.unwrap();
    page.human_fill("#f", "Hi 2!").await.unwrap();
    assert_eq!(
        page.evaluate::<String>("document.querySelector('#f').value")
            .await
            .unwrap(),
        "Hi 2!"
    );
    let keys: Vec<(String, String, String, bool, bool)> = page.evaluate("keys").await.unwrap();
    assert_eq!(keys.iter().filter(|k| k.0 == "keydown").count(), 5);
    assert_eq!(keys.iter().filter(|k| k.0 == "keyup").count(), 5);
    assert!(keys.iter().all(|k| k.4), "events must be trusted");
    assert!(keys.contains(&("keydown".into(), "H".into(), "KeyH".into(), true, true)));
    assert!(keys.contains(&("keydown".into(), "!".into(), "Digit1".into(), true, true)));
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn human_typing_handles_newlines_and_non_ascii() {
    let browser = eoka::Browser::launch_with(|c| {
        c.chrome_path = std::env::var("EOKA_TEST_CHROME_PATH").ok();
    })
    .await
    .unwrap();
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML='<textarea id=t></textarea>'")
        .await
        .unwrap();
    page.human_fill("#t", "café\nnaïve").await.unwrap();
    assert_eq!(
        page.evaluate::<String>("document.querySelector('#t').value")
            .await
            .unwrap(),
        "café\nnaïve"
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn an_invalid_timezone_fails_instead_of_leaking_the_host_zone() {
    let browser = eoka::Browser::launch_with(|c| {
        c.chrome_path = std::env::var("EOKA_TEST_CHROME_PATH").ok();
        c.timezone = Some("Not/AZone".into());
    })
    .await
    .unwrap();
    assert!(browser.new_blank_page().await.is_err());
    browser.close().await.unwrap();
}
