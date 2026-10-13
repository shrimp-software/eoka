use eoka::{ActionOptions, Browser, Expectation, LocatorSpec};

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_click_element() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    page.goto(
        r#"data:text/html,
        <button id="btn" onclick="this.textContent = 'Clicked!'">Click Me</button>
    "#,
    )
    .await
    .expect("Failed to navigate");

    // Click the button
    page.with_action_options(ActionOptions::expecting(Expectation::Text {
        target: LocatorSpec::css("#btn"),
        value: "Clicked!".into(),
        contains: false,
    }))
    .click("#btn")
    .await
    .expect("Failed to click");

    // Wait a bit for the click to process
    page.wait(100).await;

    // Check that the button text changed
    let content = page.content().await.expect("Failed to get content");
    assert!(content.contains("Clicked!"));

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_hover() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    // Simpler data URI without newlines
    page.goto("data:text/html,<div id='target' style='width:100px;height:100px;background:blue'></div><script>window.hovered=false;document.getElementById('target').onmouseenter=()=>{window.hovered=true};</script>")
        .await
        .expect("Failed to navigate");

    // Small delay for page to render
    page.wait(200).await;

    // Hover over the element
    page.hover("#target").await.expect("Failed to hover");

    // Check that hover was triggered
    let hovered: bool = page
        .evaluate("window.hovered")
        .await
        .expect("Failed to evaluate");
    assert!(hovered);

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_press_key() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    page.goto(
        r#"data:text/html,
        <input type="text" id="input" autofocus><p id="result"></p>
        <script>
            window.lastKey = '';
            document.getElementById('input').addEventListener('keydown', (e) => {
                window.lastKey = e.key; document.getElementById('result').textContent=e.key;
            });
        </script>
    "#,
    )
    .await
    .expect("Failed to navigate");

    // Wait for autofocus
    page.wait(100).await;

    // Press Enter
    page.with_action_options(ActionOptions::expecting(Expectation::Text {
        target: LocatorSpec::css("#result"),
        value: "Enter".into(),
        contains: false,
    }))
    .press_key("Enter")
    .await
    .expect("Failed to press key");

    let key: String = page
        .evaluate("window.lastKey")
        .await
        .expect("Failed to evaluate");
    assert_eq!(key, "Enter");

    // Press Tab
    page.with_action_options(ActionOptions::expecting(Expectation::Text {
        target: LocatorSpec::css("#result"),
        value: "Tab".into(),
        contains: false,
    }))
    .press_key("Tab")
    .await
    .expect("Failed to press tab");

    let key: String = page
        .evaluate("window.lastKey")
        .await
        .expect("Failed to evaluate");
    assert_eq!(key, "Tab");

    browser.close().await.expect("Failed to close browser");
}
