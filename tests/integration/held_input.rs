use eoka::Browser;
use eoka::{
    stealth::{Human, HumanSpeed},
    MouseButton,
};

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_held_input_and_release_all() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");
    page.goto(r#"data:text/html,<input id='input'><div id='target' style='width:120px;height:120px'></div><script>window.events=[];const target=document.getElementById('target');for(const type of ['mousedown','mousemove','mouseup'])target.addEventListener(type,e=>window.events.push(`${type}:${e.buttons}`));const input=document.getElementById('input');for(const type of ['keydown','keyup'])input.addEventListener(type,e=>window.events.push(`${type}:${e.key}:${e.shiftKey}`));input.focus()</script>"#)
        .await
        .expect("Failed to navigate");

    let (x, y) = page
        .find("#target")
        .await
        .expect("Missing target")
        .center()
        .await
        .expect("Target is not visible");
    page.mouse_move(x, y).await.expect("Failed to move");
    page.mouse_down(x, y, MouseButton::Left)
        .await
        .expect("Failed to press mouse");
    page.mouse_move(x + 20.0, y).await.expect("Failed to drag");
    page.mouse_up(x + 20.0, y, MouseButton::Left)
        .await
        .expect("Failed to release mouse");
    page.find("#input")
        .await
        .expect("Missing input")
        .focus()
        .await
        .expect("Failed to focus input");
    page.key_down("Shift").await.expect("Failed to press shift");
    page.release_all_inputs()
        .await
        .expect("Failed to release held inputs");

    let events: Vec<String> = page
        .evaluate("window.events")
        .await
        .expect("Failed to read events");
    assert!(
        events.iter().any(|event| event == "mousedown:1"),
        "events: {events:?}"
    );
    assert!(
        events.iter().any(|event| event == "mouseup:0"),
        "events: {events:?}"
    );
    assert!(
        events.iter().any(|event| event == "keydown:Shift:true"),
        "events: {events:?}"
    );
    assert!(
        events.iter().any(|event| event == "keyup:Shift:true"),
        "events: {events:?}"
    );

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_held_input_drag_coordinates_hover_and_human_pointer_through_page_state() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");
    page.goto(r#"data:text/html,<div id='target' style='width:160px;height:160px'></div><script>window.pointerEvents=[];const target=document.getElementById('target');for(const type of ['mousemove','mousedown','mouseup'])target.addEventListener(type,e=>window.pointerEvents.push(`${type}:${e.buttons}`))</script>"#)
        .await
        .expect("Failed to navigate");

    let (x, y) = page
        .find("#target")
        .await
        .expect("Missing target")
        .center()
        .await
        .expect("Target is not visible");
    page.mouse_down(x, y, MouseButton::Right)
        .await
        .expect("Failed to hold right mouse button");

    let clone = page.clone();
    assert!(clone.hover("#target").await.is_err());
    clone
        .mouse_move(x, y)
        .await
        .expect("Failed to move held pointer");
    clone
        .human()
        .move_to(x, y)
        .await
        .expect("Failed to move held pointer");
    clone
        .human()
        .move_and_click(x, y)
        .await
        .expect("Failed to click held pointer");

    let events: Vec<String> = page
        .evaluate("window.pointerEvents")
        .await
        .expect("Failed to read pointer events");
    assert!(
        events.iter().any(|event| event == "mousemove:2"),
        "events: {events:?}"
    );
    let down = events
        .iter()
        .position(|event| event == "mousedown:3")
        .expect("human click must add its left button to held right: {events:?}");
    let up = events
        .iter()
        .position(|event| event == "mouseup:2")
        .expect("human click must preserve held right on left release: {events:?}");
    assert!(down < up, "events: {events:?}");

    page.mouse_up(x, y, MouseButton::Right)
        .await
        .expect("Failed to release right mouse button");
    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_held_input_duplicate_attachment_shares_target_state() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let first = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");
    first
        .goto(r#"data:text/html,<input id='input'><div id='target' style='width:160px;height:160px'></div><script>window.pointerEvents=[];const target=document.getElementById('target');target.addEventListener('mousemove',e=>window.pointerEvents.push(`move:${e.buttons}`));window.keyEvents=[];const input=document.getElementById('input');for(const type of ['keydown','keyup'])input.addEventListener(type,e=>window.keyEvents.push(`${type}:${e.key}:${e.ctrlKey}`))</script>"#)
        .await
        .expect("Failed to navigate");
    let second = browser
        .attach_page(first.target_id())
        .await
        .expect("Failed to attach second Page handle");

    let (x, y) = first
        .find("#target")
        .await
        .expect("Missing target")
        .center()
        .await
        .expect("Target is not visible");
    first
        .mouse_down(x, y, MouseButton::Left)
        .await
        .expect("Failed to hold left mouse button");
    second
        .mouse_move(x, y)
        .await
        .expect("Second Page failed to move held pointer");
    let pointer_events: Vec<String> = first
        .evaluate("window.pointerEvents")
        .await
        .expect("Failed to read pointer events");
    assert!(
        pointer_events.iter().any(|event| event == "move:1"),
        "pointer events: {pointer_events:?}"
    );
    first
        .mouse_up(x, y, MouseButton::Left)
        .await
        .expect("Failed to release left mouse button");

    second
        .find("#input")
        .await
        .expect("Missing input")
        .focus()
        .await
        .expect("Failed to focus input");
    first
        .key_down("Ctrl")
        .await
        .expect("Failed to press control");
    second
        .key_down("a")
        .await
        .expect("Second Page failed to press a");
    second
        .key_up("a")
        .await
        .expect("Second Page failed to release a");
    first
        .key_up("Ctrl")
        .await
        .expect("Failed to release control");
    let key_events: Vec<String> = first
        .evaluate("window.keyEvents")
        .await
        .expect("Failed to read key events");
    assert!(
        key_events.iter().any(|event| event == "keydown:a:true"),
        "key events: {key_events:?}"
    );
    assert!(
        key_events.iter().any(|event| event == "keyup:a:true"),
        "key events: {key_events:?}"
    );

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_held_input_human_session_constructor_and_modifier_key_press() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");
    page.goto(r#"data:text/html,<input id='input'><div id='target' style='width:160px;height:160px'></div><script>window.pointerEvents=[];const target=document.getElementById('target');target.addEventListener('mousemove',e=>window.pointerEvents.push(`move:${e.buttons}`));window.keyEvents=[];const input=document.getElementById('input');for(const type of ['keydown','keyup'])input.addEventListener(type,e=>window.keyEvents.push(`${type}:${e.key}:${e.ctrlKey}`))</script>"#)
        .await
        .expect("Failed to navigate");

    let (x, y) = page
        .find("#target")
        .await
        .expect("Missing target")
        .center()
        .await
        .expect("Target is not visible");
    page.mouse_down(x, y, MouseButton::Right)
        .await
        .expect("Failed to hold right mouse button");
    // This is the source-compatible public constructor. Its moves must share
    // the same target state as Page rather than bypassing the held right mask.
    Human::new(page.session())
        .with_speed(HumanSpeed::Fast)
        .move_to(x, y)
        .await
        .expect("Session Human failed to move");
    let pointer_events: Vec<String> = page
        .evaluate("window.pointerEvents")
        .await
        .expect("Failed to read pointer events");
    assert!(
        pointer_events.iter().any(|event| event == "move:2"),
        "pointer events: {pointer_events:?}"
    );
    page.mouse_up(x, y, MouseButton::Right)
        .await
        .expect("Failed to release right mouse button");

    page.find("#input")
        .await
        .expect("Missing input")
        .focus()
        .await
        .expect("Failed to focus input");
    page.key_down("Ctrl")
        .await
        .expect("Failed to press control");
    page.human()
        .with_speed(HumanSpeed::Fast)
        .press_key("a")
        .await
        .expect("Human key press failed");
    page.key_up("Ctrl")
        .await
        .expect("Failed to release control");
    let key_events: Vec<String> = page
        .evaluate("window.keyEvents")
        .await
        .expect("Failed to read key events");
    assert!(
        key_events.iter().any(|event| event == "keydown:a:true"),
        "key events: {key_events:?}"
    );
    assert!(
        key_events.iter().any(|event| event == "keyup:a:true"),
        "key events: {key_events:?}"
    );

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_held_input_serializes_clone_masks_and_current_key_up_modifiers() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");
    page.goto(r#"data:text/html,<input id='input'><div id='target' style='width:120px;height:120px'></div><script>window.pointerEvents=[];const target=document.getElementById('target');target.addEventListener('mousedown',e=>window.pointerEvents.push(`down:${e.buttons}`));target.addEventListener('mouseup',e=>window.pointerEvents.push(`up:${e.buttons}`));window.keyEvents=[];const input=document.getElementById('input');for(const type of ['keydown','keyup'])input.addEventListener(type,e=>window.keyEvents.push(`${type}:${e.key}:${e.ctrlKey}`))</script>"#)
        .await
        .expect("Failed to navigate");

    let (x, y) = page
        .find("#target")
        .await
        .expect("Missing target")
        .center()
        .await
        .expect("Target is not visible");
    let other_page = page.clone();
    let (left, right) = tokio::join!(
        page.mouse_down(x, y, MouseButton::Left),
        other_page.mouse_down(x, y, MouseButton::Right)
    );
    left.expect("Failed to press left mouse button");
    right.expect("Failed to press right mouse button");
    page.release_all_inputs()
        .await
        .expect("Failed to release mouse buttons");
    let pointer_events: Vec<String> = page
        .evaluate("window.pointerEvents")
        .await
        .expect("Failed to read pointer events");
    assert!(
        pointer_events.iter().any(|event| event == "down:3"),
        "pointer events: {pointer_events:?}"
    );

    page.find("#input")
        .await
        .expect("Missing input")
        .focus()
        .await
        .expect("Failed to focus input");
    page.key_down("Ctrl")
        .await
        .expect("Failed to press control");
    page.key_down("a").await.expect("Failed to press a");
    page.key_up("Ctrl")
        .await
        .expect("Failed to release control");
    page.key_up("a").await.expect("Failed to release a");
    let key_events: Vec<String> = page
        .evaluate("window.keyEvents")
        .await
        .expect("Failed to read key events");
    assert!(
        key_events.iter().any(|event| event == "keydown:a:true"),
        "key events: {key_events:?}"
    );
    assert!(
        key_events.iter().any(|event| event == "keyup:a:false"),
        "key events: {key_events:?}"
    );

    browser.close().await.expect("Failed to close browser");
}
