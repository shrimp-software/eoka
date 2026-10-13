//! Best Buy add-to-cart example using stealthy automation
//!
//! Run with: cargo run --example bestbuy_cart
//! Or visible: cargo run --example bestbuy_cart -- --visible

use eoka::{Browser, Result};

const PRODUCT_URL: &str = "https://www.bestbuy.com/product/steelseries-apex-3-full-size-wired-membrane-whisper-quiet-switch-gaming-keyboard-with-10-zone-rgb-backlighting-black/J3LG47VT9S";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("eoka=debug".parse().unwrap()),
        )
        .init();

    let visible = std::env::args().any(|a| a == "--visible");

    println!("=== Best Buy Add to Cart ===\n");
    println!("Mode: {}", if visible { "visible" } else { "headless" });
    println!("Product URL: {}\n", PRODUCT_URL);

    println!("Launching browser...");
    let browser = if visible {
        Browser::launch_visible().await?
    } else {
        Browser::launch().await?
    };

    let page = browser.new_page(PRODUCT_URL).await?;

    // Wait for page to fully load and network to settle
    println!("Waiting for page to load...");
    page.wait_for_network_idle(1000, 15000).await?;

    // Handle any cookie consent/privacy banners
    println!("Checking for cookie banners...");
    let _ = page.try_click_by_text("Accept").await;
    let _ = page.try_click_by_text("Close").await;
    page.wait(500).await;

    // Take initial screenshot
    let screenshot = page.screenshot().await?;
    std::fs::write("bestbuy_1_loaded.png", screenshot)?;
    println!("Screenshot: bestbuy_1_loaded.png");

    // Check page title to verify we're on the right page
    let title = page.title().await?;
    println!("Page title: {}", title);

    if title.contains("Not Found") || title.contains("Error") {
        println!("Product page not found!");
        browser.close().await?;
        return Ok(());
    }

    // Scroll down a bit to get the Add to Cart button area in view
    println!("\nScrolling to Add to Cart section...");
    page.execute("window.scrollBy(0, 300)").await?;
    page.wait(500).await;

    // Look for the Add to Cart button - Best Buy uses various selectors
    let add_to_cart_selectors = [
        "[data-button-state='ADD_TO_CART']",
        "button.add-to-cart-button",
        "button[data-sku-id]",
        ".fulfillment-add-to-cart-button button",
        ".add-to-cart-button",
    ];

    println!("Looking for Add to Cart button...");

    let mut clicked = false;

    let expected: eoka::Expectation =
        serde_json::from_str(&std::env::var("EOKA_CART_CONFIRMATION").map_err(|_| {
            eoka::Error::cdp_msg(
                "Set EOKA_CART_CONFIRMATION to the JSON expectation for a new cart confirmation",
            )
        })?)?;
    let action_page = page.with_action_options(eoka::ActionOptions::expecting(expected));

    // First try specific selectors
    for selector in &add_to_cart_selectors {
        if let Ok(elem) = action_page.find(selector).await {
            println!("Found button with selector: {}", selector);

            // Check if visible
            if elem.is_visible().await.unwrap_or(false) {
                println!("Button is visible, scrolling into view...");
                elem.scroll_into_view().await?;
                page.wait(300).await;

                // Take screenshot before click
                let screenshot = page.screenshot().await?;
                std::fs::write("bestbuy_2_before_click.png", screenshot)?;
                println!("Screenshot: bestbuy_2_before_click.png");

                // Use human-like click for stealth
                println!("Clicking Add to Cart...");
                elem.human_click().await?;
                clicked = true;
                break;
            }
        }
    }

    // Fallback: try clicking by text
    if !clicked {
        println!("Trying to find button by text...");
        if let Ok(elem) = action_page.find_by_text("Add to Cart").await {
            if elem.is_visible().await.unwrap_or(false) {
                println!("Found visible 'Add to Cart' text element");
                elem.scroll_into_view().await?;
                page.wait(300).await;

                let screenshot = page.screenshot().await?;
                std::fs::write("bestbuy_2_before_click.png", screenshot)?;

                elem.human_click().await?;
                clicked = true;
            }
        }
    }

    if !clicked {
        println!("Could not find clickable Add to Cart button!");
        let screenshot = page.screenshot().await?;
        std::fs::write("bestbuy_error.png", screenshot)?;
        println!("Error screenshot saved: bestbuy_error.png");

        let state = page.debug_state().await?;
        println!("Page state: {:?}", state);

        browser.close().await?;
        return Ok(());
    }

    // Wait for cart update / modal to appear
    println!("\nWaiting for cart response...");
    page.wait(3000).await;

    // Take screenshot after clicking
    let screenshot = page.screenshot().await?;
    std::fs::write("bestbuy_3_clicked.png", screenshot)?;
    println!("Screenshot: bestbuy_3_clicked.png");

    // Check for success indicators
    let page_text = page.text().await?;
    let success_indicators = [
        "Added to Cart",
        "added to cart",
        "Go to Cart",
        "View Cart",
        "Continue Shopping",
        "item in cart",
    ];

    let mut success = false;
    for indicator in &success_indicators {
        if page_text.to_lowercase().contains(&indicator.to_lowercase()) {
            println!("\n✓ Success! Found: '{}'", indicator);
            success = true;
            break;
        }
    }

    // Check if a modal appeared and handle it
    if page.text_exists("Go to Cart").await || page.text_exists("View Cart").await {
        println!("Cart modal detected!");

        // Option to go to cart
        if page.try_click_by_text("Go to Cart").await? {
            println!("Clicking 'Go to Cart'...");
            page.wait(2000).await;
        } else if page.try_click_by_text("View Cart").await? {
            println!("Clicking 'View Cart'...");
            page.wait(2000).await;
        }
    }

    // Final screenshot
    let screenshot = page.screenshot().await?;
    std::fs::write("bestbuy_4_final.png", screenshot)?;
    println!("Screenshot: bestbuy_4_final.png");

    // Get final URL
    let final_url = page.url().await?;
    println!("\nFinal URL: {}", final_url);

    if final_url.contains("cart") {
        println!("\n✓ Successfully navigated to cart!");
        success = true;
    }

    if success {
        println!("\n=============================");
        println!("  ADD TO CART SUCCESSFUL!");
        println!("=============================");
    } else {
        println!("\n⚠ Could not confirm cart addition - check screenshots");
    }

    // Keep browser open if visible mode
    if visible {
        println!("\nBrowser staying open for 60s - press Ctrl+C to close early...");
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }

    browser.close().await?;
    println!("\nDone!");

    Ok(())
}
