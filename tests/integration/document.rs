use eoka::Browser;

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_page_navigation() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    // Navigate to a simple page
    page.goto("data:text/html,<h1>Hello</h1>")
        .await
        .expect("Failed to navigate");

    // Check title
    let content = page.content().await.expect("Failed to get content");
    assert!(content.contains("Hello"));

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_page_title() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    page.goto("data:text/html,<title>Test Title</title><body>Content</body>")
        .await
        .expect("Failed to navigate");

    let title = page.title().await.expect("Failed to get title");
    assert_eq!(title, "Test Title");

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_page_url() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    page.goto("data:text/html,<h1>Test</h1>")
        .await
        .expect("Failed to navigate");

    let url = page.url().await.expect("Failed to get URL");
    assert!(url.starts_with("data:"));

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_screenshot() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    page.goto("data:text/html,<body style='background:red'><h1>Red</h1></body>")
        .await
        .expect("Failed to navigate");

    let png = page.screenshot().await.expect("Failed to take screenshot");

    // Check PNG magic bytes
    assert!(png.len() > 100);
    assert_eq!(&png[0..4], &[0x89, 0x50, 0x4E, 0x47]); // PNG signature

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_screenshot_jpeg() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    page.goto("data:text/html,<body style='background:blue'><h1>Blue</h1></body>")
        .await
        .expect("Failed to navigate");

    let jpeg = page
        .screenshot_jpeg(80)
        .await
        .expect("Failed to take screenshot");

    // Check JPEG magic bytes
    assert!(jpeg.len() > 100);
    assert_eq!(&jpeg[0..2], &[0xFF, 0xD8]); // JPEG SOI

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_element_finding() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    page.goto(
        r#"data:text/html,
        <button id="btn">Click Me</button>
        <input type="text" class="input" value="test">
    "#,
    )
    .await
    .expect("Failed to navigate");

    // Find by ID
    let btn = page.find("#btn").await.expect("Failed to find button");
    let html = btn.outer_html().await.expect("Failed to get HTML");
    assert!(html.contains("Click Me"));

    // Find by class
    let input = page.find(".input").await.expect("Failed to find input");
    let html = input.outer_html().await.expect("Failed to get HTML");
    assert!(html.contains("type=\"text\""));

    // Find all
    let all = page.find_all("*").await.expect("Failed to find all");
    assert!(all.len() > 2);

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_element_not_found() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    page.goto("data:text/html,<div>Simple</div>")
        .await
        .expect("Failed to navigate");

    let result = page.find("#nonexistent").await;
    assert!(result.is_err());

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_evaluate_javascript() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    // Evaluate simple expression
    let result: i32 = page.evaluate("1 + 2").await.expect("Failed to evaluate");
    assert_eq!(result, 3);

    // Evaluate string
    let result: String = page
        .evaluate("'hello' + ' world'")
        .await
        .expect("Failed to evaluate");
    assert_eq!(result, "hello world");

    // Evaluate array
    let result: Vec<i32> = page
        .evaluate("[1, 2, 3]")
        .await
        .expect("Failed to evaluate");
    assert_eq!(result, vec![1, 2, 3]);

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_wait_for_element() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    // Page with delayed element
    page.goto(
        r#"data:text/html,
        <script>
            setTimeout(() => {
                document.body.innerHTML = '<div id="delayed">Loaded!</div>';
            }, 100);
        </script>
    "#,
    )
    .await
    .expect("Failed to navigate");

    // Wait for element to appear
    let element = page
        .wait_for("#delayed", 5000)
        .await
        .expect("Element not found");
    let html = element.outer_html().await.expect("Failed to get HTML");
    assert!(html.contains("Loaded!"));

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_wait_for_element_timeout() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    page.goto("data:text/html,<div>No delayed element</div>")
        .await
        .expect("Failed to navigate");

    // Should timeout
    let result = page.wait_for("#never-exists", 500).await;
    assert!(result.is_err());

    browser.close().await.expect("Failed to close browser");
}
