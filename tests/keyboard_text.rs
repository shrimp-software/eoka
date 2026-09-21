use eoka::{Browser, Page};
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
        page.press_key(key).await.unwrap();
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
    page.press_key("Enter").await.unwrap();
    assert_eq!(
        page.evaluate_sync::<String>("document.querySelector('#area').value")
            .await
            .unwrap(),
        "\n"
    );
    page.execute_sync("document.querySelector('#editor').focus();events=[]")
        .await
        .unwrap();
    page.press_key("o").await.unwrap();
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
    attached.press_key("o").await.unwrap();
    attached.key_up("Shift").await.unwrap();
    page.press_key("o").await.unwrap();
    assert_eq!(
        page.evaluate_sync::<String>("document.querySelector('#field').value")
            .await
            .unwrap(),
        "Oo"
    );
    page.select_all().await.unwrap();
    assert_eq!(
        page.evaluate_sync::<String>("document.querySelector('#field').value")
            .await
            .unwrap(),
        "Oo"
    );
    page.press_key("Backspace").await.unwrap();
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
    page.press_key("q").await.unwrap();
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
