use crate::support;
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

/// Serves a page whose `go()` navigates it to `/dest` after a delay, so the
/// navigation starts in the page rather than through Eoka.
async fn self_navigating_fixture() -> support::HttpFixture {
    support::HttpFixture::start(|request| async move {
        if request.target == "/dest" {
            support::Response::html("<a href='#'>Destination link</a><p>Destination text</p>")
        } else {
            support::Response::html(
                "<p>Origin</p><script>window.go = () => setTimeout(() => location = '/dest', 300)</script>",
            )
        }
    })
    .await
}

/// Load the origin, cache its document node by finding text on it, then start
/// the page's own navigation.
async fn load_origin(page: &eoka::Page, fixture: &support::HttpFixture) {
    page.goto(&fixture.url).await.expect("Failed to navigate");
    page.find_by_text("Origin").await.expect("origin text");
    page.execute("go()").await.expect("start navigation");
}

/// Wait until the page has navigated itself to the loaded destination, without
/// touching the cached document node.
async fn wait_for_destination(page: &eoka::Page) {
    for _ in 0..100 {
        let loaded = page
            .evaluate::<bool>("location.pathname === '/dest' && document.readyState === 'complete'")
            .await
            .unwrap_or(false);
        if loaded {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("page did not navigate itself to /dest");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_text_lookup_after_page_initiated_navigation() {
    let browser = support::browser().await;
    let page = browser.new_page("about:blank").await.unwrap();
    let fixture = self_navigating_fixture().await;

    load_origin(&page, &fixture).await;
    let link = page
        .wait_for_text("Destination link", 5000)
        .await
        .expect("wait_for_text across a page-initiated navigation");
    assert!(link
        .outer_html()
        .await
        .unwrap()
        .contains("Destination link"));

    load_origin(&page, &fixture).await;
    wait_for_destination(&page).await;
    let link = page
        .find_by_text("Destination link")
        .await
        .expect("find_by_text after a page-initiated navigation");
    assert!(link
        .outer_html()
        .await
        .unwrap()
        .contains("Destination link"));

    load_origin(&page, &fixture).await;
    wait_for_destination(&page).await;
    let matches = page
        .find_all_by_text("Destination")
        .await
        .expect("find_all_by_text after a page-initiated navigation");
    assert!(!matches.is_empty(), "find_all_by_text returned no elements");

    browser.close().await.expect("Failed to close browser");
}
