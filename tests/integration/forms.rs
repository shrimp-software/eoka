use eoka::Browser;

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_type_into_input() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    page.goto(
        r#"data:text/html,
        <input type="text" id="input" value="">
    "#,
    )
    .await
    .expect("Failed to navigate");

    // Type into the input
    page.with_action_options(eoka::ActionOptions::expecting(eoka::Expectation::Value {
        target: eoka::LocatorSpec::css("#input"),
        value: "Hello World".into(),
    }))
    .type_into("#input", "Hello World")
    .await
    .expect("Failed to type");

    // Check the value
    let value: String = page
        .evaluate("document.getElementById('input').value")
        .await
        .expect("Failed to evaluate");
    assert_eq!(value, "Hello World");

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_select_dropdown() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    page.goto(
        r#"data:text/html,
        <select id="country">
            <option value="us">United States</option>
            <option value="uk">United Kingdom</option>
            <option value="de">Germany</option>
        </select>
    "#,
    )
    .await
    .expect("Failed to navigate");

    // Select by value
    page.select("#country", "uk")
        .await
        .expect("Failed to select");

    let value: String = page
        .evaluate("document.getElementById('country').value")
        .await
        .expect("Failed to evaluate");
    assert_eq!(value, "uk");

    // Select by text
    page.select_by_text("#country", "Germany")
        .await
        .expect("Failed to select by text");

    let value: String = page
        .evaluate("document.getElementById('country').value")
        .await
        .expect("Failed to evaluate");
    assert_eq!(value, "de");

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_select_invalid_option() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    page.goto(
        r#"data:text/html,
        <select id="country">
            <option value="us">United States</option>
        </select>
    "#,
    )
    .await
    .expect("Failed to navigate");

    // Try to select non-existent value - should error
    let result = page.select("#country", "invalid").await;
    assert!(result.is_err());

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_fill_range_dispatches_events_and_rejects_invalid_values() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");
    page.goto(r#"data:text/html,<input id='range' type='range' min='0' max='10' step='2' value='2'><input id='text' value='old'><script>window.rangeEvents=[];const range=document.getElementById('range');for(const type of ['input','change'])range.addEventListener(type,()=>window.rangeEvents.push(type))</script>"#)
        .await
        .expect("Failed to navigate");

    page.fill("#range", "6")
        .await
        .expect("Failed to fill range");
    assert_eq!(
        page.evaluate::<String>("document.getElementById('range').value")
            .await
            .expect("Failed to read range"),
        "6"
    );
    assert_eq!(
        page.evaluate::<Vec<String>>("window.rangeEvents")
            .await
            .expect("Failed to read range events"),
        vec!["input", "change"]
    );
    assert!(page.fill("#range", "7").await.is_err());
    assert_eq!(
        page.evaluate::<String>("document.getElementById('range').value")
            .await
            .expect("Failed to read rejected range"),
        "6"
    );

    // fill must verify the value assigned before input/change handlers run,
    // not accept the handler's replacement value as its expected result.
    page.execute("document.getElementById('range').addEventListener('input', event => { event.target.value = '2'; }, { once: true })")
        .await
        .expect("Failed to install range listener");
    assert!(page
        .with_action_options(eoka::ActionOptions {
            timeout_ms: 300,
            ..Default::default()
        })
        .fill("#range", "8")
        .await
        .is_err());
    assert_eq!(
        page.evaluate::<String>("document.getElementById('range').value")
            .await
            .expect("Failed to read listener-mutated range"),
        "2"
    );

    page.fill("#text", "new text")
        .await
        .expect("Failed to fill text input");
    assert_eq!(
        page.evaluate::<String>("document.getElementById('text').value")
            .await
            .expect("Failed to read text input"),
        "new text"
    );

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_upload_file() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    page.goto(
        r#"data:text/html,
        <input type="file" id="upload">
        <script>
            window.filename = '';
            document.getElementById('upload').addEventListener('change', (e) => {
                window.filename = e.target.files[0]?.name || '';
            });
        </script>
    "#,
    )
    .await
    .expect("Failed to navigate");

    // Create a temp file
    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join("eoka_test_upload.txt");
    std::fs::write(&temp_file, "test content").expect("Failed to write temp file");

    // Upload the file
    page.upload_file("#upload", temp_file.to_str().unwrap())
        .await
        .expect("Failed to upload file");

    // Check that the file was uploaded
    let filename: String = page
        .evaluate("window.filename")
        .await
        .expect("Failed to evaluate");
    assert_eq!(filename, "eoka_test_upload.txt");

    // Clean up
    std::fs::remove_file(&temp_file).ok();

    browser.close().await.expect("Failed to close browser");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn test_select_multiple() {
    let browser = Browser::launch().await.expect("Failed to launch browser");
    let page = browser
        .new_page("about:blank")
        .await
        .expect("Failed to create page");

    page.goto(
        r#"data:text/html,
        <select id="tags" multiple>
            <option value="rust">Rust</option>
            <option value="async">Async</option>
            <option value="web">Web</option>
        </select>
    "#,
    )
    .await
    .expect("Failed to navigate");

    // Select multiple options
    page.select_multiple("#tags", &["rust", "web"])
        .await
        .expect("Failed to select multiple");

    // Check selected values
    let selected: Vec<String> = page
        .evaluate("Array.from(document.getElementById('tags').selectedOptions).map(o => o.value)")
        .await
        .expect("Failed to evaluate");
    assert_eq!(selected, vec!["rust", "web"]);

    browser.close().await.expect("Failed to close browser");
}
