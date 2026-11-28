use crate::support;
use eoka::Browser;

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_browser_launch() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_browser_launch_visible() {
    let browser = Browser::launch_visible()
        .await
        .expect("Failed to launch browser");
    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_cookies() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    let fixture =
        support::HttpFixture::start(|_| async { support::Response::html("cookie fixture") }).await;
    page.goto(&fixture.url).await.expect("Failed to navigate");

    // Set a cookie
    page.set_cookie("test_cookie", "test_value", None, Some("/"))
        .await
        .expect("Failed to set cookie");

    // Get cookies
    let cookies = page.cookies().await.expect("Failed to get cookies");
    let test_cookie = cookies.iter().find(|c| c.name == "test_cookie");
    assert!(test_cookie.is_some());
    assert_eq!(test_cookie.unwrap().value, "test_value");

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_multi_tab() {
    let browser = Browser::launch().await.expect("Failed to launch browser");

    // Create two tabs
    let page1 = browser
        .new_page("data:text/html,<title>Tab1</title>")
        .await
        .expect("Failed to create page 1");
    let page2 = browser
        .new_page("data:text/html,<title>Tab2</title>")
        .await
        .expect("Failed to create page 2");

    // Verify target IDs are different
    assert_ne!(page1.target_id(), page2.target_id());

    // List tabs - should have at least 2
    let tabs = browser.tabs().await.expect("Failed to list tabs");
    assert!(tabs.len() >= 2);

    // Find our tabs
    let tab1_found = tabs.iter().any(|t| t.id == page1.target_id());
    let tab2_found = tabs.iter().any(|t| t.id == page2.target_id());
    assert!(tab1_found, "Tab 1 not found in tabs list");
    assert!(tab2_found, "Tab 2 not found in tabs list");

    // Activate tab 1
    browser
        .activate_tab(page1.target_id())
        .await
        .expect("Failed to activate tab");

    // Close tab 2
    browser
        .close_tab(page2.target_id())
        .await
        .expect("Failed to close tab");

    // CDP acknowledges the close request before target destruction completes.
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let tabs = browser.tabs().await.expect("Failed to list tabs");
            if !tabs.iter().any(|t| t.id == page2.target_id()) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Tab 2 should be closed");

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_failed_new_page_closes_its_tab() {
    let browser = support::browser().await;
    let before = browser.tabs().await.unwrap().len();
    // Port 1 refuses connections, so navigation fails after the tab exists.
    let error = browser.new_page("http://127.0.0.1:1/").await;
    assert!(error.is_err(), "navigation to a closed port must fail");
    let mut after = browser.tabs().await.unwrap().len();
    for _ in 0..20 {
        if after == before {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        after = browser.tabs().await.unwrap().len();
    }
    assert_eq!(after, before, "failed new_page left its tab open");
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_new_page_waits_for_navigation() {
    let fixture =
        support::HttpFixture::start(|_| async { support::Response::html("<title>ready</title>") })
            .await;
    let browser = support::browser().await;
    let page = browser.new_page(&fixture.url).await.unwrap();
    // No sleep: the URL is committed when new_page returns.
    assert!(page.url().await.unwrap().starts_with(&fixture.url));
    browser.close().await.unwrap();
}
