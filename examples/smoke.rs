//! Smoke test - verify core functionality works end-to-end
//!
//! Run with: cargo run --example smoke

use eoka::{ActionOptions, Browser, Expectation, LocatorSpec, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let browser = Browser::launch().await?;
    let page = browser.new_page("about:blank").await?;

    // === Element finding ===
    page.goto(r#"data:text/html,<button id="btn">Click Me</button><input type="text" class="input" value="test">"#).await?;
    page.wait_for("#btn", 5000).await?;
    assert!(page.exists("#btn").await);
    assert!(!page.exists("#nope").await);
    let text: String = page
        .evaluate("document.getElementById('btn').textContent")
        .await?;
    assert_eq!(text, "Click Me");
    println!("1. Element finding: ok");

    // === Click ===
    page.goto(r#"data:text/html,<button id="btn" onclick="this.textContent='Clicked!'">Click Me</button>"#).await?;
    page.wait_for("#btn", 5000).await?;
    page.locator("#btn")
        .click(ActionOptions::expecting(Expectation::Text {
            target: LocatorSpec::css("#btn"),
            value: "Clicked!".into(),
            contains: false,
        }))
        .await?;
    page.wait(100).await;
    let text: String = page
        .evaluate("document.getElementById('btn').textContent")
        .await?;
    assert_eq!(text, "Clicked!");
    println!("2. Click: ok");

    // === Text finding + tag_name on focusable element ===
    page.goto(r#"data:text/html,<a href="https://example.com">Sign In</a><button>Submit</button><div>Hello World</div>"#).await?;
    page.wait(500).await;
    let link = page.find_by_text("Sign In").await?;
    let tag = link.tag_name().await?;
    assert_eq!(tag, "a");
    assert!(page.text_exists("Hello World").await);
    assert!(!page.text_exists("Goodbye").await);
    println!("3. Text finding: ok");

    // === Element inspection on UNFOCUSABLE elements (the bug fix) ===
    page.goto("data:text/html,<div id='info' class='test'>Some Text</div><span>Plain</span>")
        .await?;
    page.wait(500).await;
    let div = page.find("#info").await?;
    let tag = div.tag_name().await?;
    assert_eq!(tag, "div");
    let cls = div.get_attribute("class").await?;
    assert_eq!(cls, Some("test".to_string()));
    let enabled = div.is_enabled().await?;
    assert!(enabled);
    // Also test on a bare <span> (no id, no href, not focusable)
    let span = page.find("span").await?;
    let tag = span.tag_name().await?;
    assert_eq!(tag, "span");
    println!("4. Element inspection on unfocusable elements: ok");

    // === Try-click ===
    page.goto(r#"data:text/html,<a href="https://example.com">Sign In</a><button onclick="this.textContent='Submitted'">Submit</button>"#)
        .await?;
    page.wait(500).await;
    let configured = page.with_action_options(ActionOptions::expecting(Expectation::Text {
        target: LocatorSpec::css("button"),
        value: "Submitted".into(),
        contains: false,
    }));
    let found = configured.try_click("#nonexistent").await?;
    assert!(!found);
    let found = configured.try_click_by_text("Submit").await?;
    assert!(found);
    println!("5. Try-click: ok");

    // === Fill ===
    page.goto(r#"data:text/html,<input id="email" type="text" value="old">"#)
        .await?;
    page.wait_for("#email", 5000).await?;
    page.fill("#email", "new@test.com").await?;
    let val: String = page
        .evaluate("document.getElementById('email').value")
        .await?;
    assert_eq!(val, "new@test.com");
    println!("6. Fill: ok");

    // === Type into ===
    page.goto(r#"data:text/html,<input id="inp" type="text">"#)
        .await?;
    page.wait_for("#inp", 5000).await?;
    page.with_action_options(ActionOptions::expecting(Expectation::Value {
        target: LocatorSpec::css("#inp"),
        value: "hello".into(),
    }))
    .type_into("#inp", "hello")
    .await?;
    let val: String = page
        .evaluate("document.getElementById('inp').value")
        .await?;
    assert_eq!(val, "hello");
    println!("7. Type into: ok");

    // === Wait for text ===
    page.goto(r#"data:text/html,<script>setTimeout(() => { document.body.innerHTML = '<div>Loaded!</div>'; }, 200);</script>"#).await?;
    page.wait_for_text("Loaded!", 5000).await?;
    println!("8. Wait for text: ok");

    // === Wait for visible ===
    page.goto(r#"data:text/html,<div id="box" style="display:none">Hidden</div><script>setTimeout(() => document.getElementById('box').style.display='block', 200);</script>"#).await?;
    page.wait_for_visible("#box", 5000).await?;
    println!("9. Wait for visible: ok");

    // === Find any (selector fallback) ===
    page.goto(r#"data:text/html,<input name="email" type="text">"#)
        .await?;
    page.wait(300).await;
    let _el = page.find_any(&["#email", "input[name='email']"]).await?;
    println!("10. Find any: ok");

    // === Debug state ===
    page.goto("data:text/html,<form><input><input><button>Go</button></form><a>Link</a>")
        .await?;
    page.wait(300).await;
    let state = page.debug_state().await?;
    assert_eq!(state.input_count, 2);
    assert_eq!(state.button_count, 1);
    assert_eq!(state.link_count, 1);
    assert_eq!(state.form_count, 1);
    println!(
        "11. Debug state: ok (inputs={}, buttons={}, links={}, forms={})",
        state.input_count, state.button_count, state.link_count, state.form_count
    );

    // === Screenshot ===
    let png = page.screenshot().await?;
    assert!(png.len() > 100);
    assert_eq!(&png[0..4], &[0x89, 0x50, 0x4E, 0x47]);
    println!("12. Screenshot: ok ({} bytes)", png.len());

    // === JavaScript evaluate ===
    let result: i32 = page.evaluate("1 + 2").await?;
    assert_eq!(result, 3);
    let result: String = page.evaluate("'hello' + ' world'").await?;
    assert_eq!(result, "hello world");
    println!("13. JS evaluate: ok");

    // === is_visible + bounding_box ===
    page.goto(r#"data:text/html,<div id="vis">Visible</div><div id="hid" style="display:none">Hidden</div>"#).await?;
    page.wait(300).await;
    let vis = page.find("#vis").await?;
    assert!(vis.is_visible().await?);
    let bbox = vis.bounding_box().await;
    assert!(bbox.is_some());
    let hid = page.find("#hid").await?;
    assert!(!hid.is_visible().await?);
    let bbox = hid.bounding_box().await;
    assert!(bbox.is_none());
    println!("14. is_visible + bounding_box: ok");

    // === is_checked + value ===
    page.goto(r#"data:text/html,<input id="cb" type="checkbox"><input id="txt" type="text" value="hello">"#).await?;
    page.wait(300).await;
    let cb = page.find("#cb").await?;
    assert!(!cb.is_checked().await?);
    page.execute("document.getElementById('cb').checked = true")
        .await?;
    let cb = page.find("#cb").await?;
    assert!(cb.is_checked().await?);
    let txt = page.find("#txt").await?;
    assert_eq!(txt.value().await?, "hello");
    println!("15. is_checked + value: ok");

    // === css computed style ===
    page.goto(r#"data:text/html,<div id="styled" style="color: rgb(255, 0, 0); font-size: 20px;">Red</div>"#).await?;
    page.wait(300).await;
    let el = page.find("#styled").await?;
    let color = el.css("color").await?;
    assert_eq!(color, "rgb(255, 0, 0)");
    println!("16. css: ok");

    // === Element::text ===
    page.goto(r#"data:text/html,<p id="t">Hello World</p>"#)
        .await?;
    page.wait(300).await;
    let el = page.find("#t").await?;
    let text = el.text().await?;
    assert_eq!(text, "Hello World");
    println!("17. Element::text: ok");

    // === wait_for_url_contains ===
    page.wait_for_url_contains("data:", 5000).await?;
    println!("18. wait_for_url_contains: ok");

    // === find_all_by_text ===
    page.goto(r#"data:text/html,<div>Apple</div><span>Apple</span><p>Apple</p><div>Banana</div>"#)
        .await?;
    page.wait(300).await;
    let matches = page.find_all_by_text("Apple").await?;
    assert_eq!(matches.len(), 3);
    println!("19. find_all_by_text: ok (found {})", matches.len());

    // === with_retry ===
    let attempt_count = std::sync::atomic::AtomicU32::new(0);
    let result = page
        .with_retry(3, 100, || {
            let current = attempt_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            async move {
                if current < 3 {
                    Err(eoka::Error::Timeout("not yet".into()))
                } else {
                    Ok("done")
                }
            }
        })
        .await?;
    assert_eq!(result, "done");
    println!("20. with_retry: ok");

    // === content + text (page-level) ===
    page.goto(r#"data:text/html,<!DOCTYPE html><html><body><p>Page text here</p></body></html>"#)
        .await?;
    page.wait(300).await;
    let html = page.content().await?;
    assert!(html.contains("<p>Page text here</p>"));
    let text = page.text().await?;
    assert!(text.contains("Page text here"));
    assert!(!text.contains("<p>"));
    println!("21. content + text: ok");

    // === screenshot_jpeg ===
    let jpeg = page.screenshot_jpeg(80).await?;
    assert!(jpeg.len() > 100);
    assert_eq!(&jpeg[0..2], &[0xFF, 0xD8]);
    println!("22. screenshot_jpeg: ok ({} bytes)", jpeg.len());

    // === Network: real page (last because cross-origin nav disrupts CDP) ===
    page.enable_request_capture().await?;
    page.goto("https://example.com").await?;
    page.wait(2000).await;
    let url = page.url().await?;
    assert!(url.contains("example.com"));
    let title = page.title().await?;
    assert!(!title.is_empty());
    page.disable_request_capture().await?;
    println!("23. Network + real page: ok (url={}, title={})", url, title);

    // === Cookies ===
    page.set_cookie("test_cookie", "test_value", Some("example.com"), Some("/"))
        .await?;
    let cookies = page.cookies().await?;
    let found = cookies
        .iter()
        .any(|c| c.name == "test_cookie" && c.value == "test_value");
    assert!(found, "Cookie not found");
    println!("24. Cookies: ok");

    browser.close().await?;
    println!("\n=== ALL 24 SMOKE TESTS PASSED ===");
    Ok(())
}
