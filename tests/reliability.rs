mod support;

use eoka::{Action, ActionOptions, Error};
use eoka::{ActionOutcome, Destination, Expectation, InputMode, LocatorSpec};

#[tokio::test]
#[ignore = "requires Chrome; sequential typing target ownership"]
async fn sequential_input_stops_when_the_page_moves_focus() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML='<input id=password><input id=other>';password.oninput=()=>other.focus()")
        .await.unwrap();
    let error = page.fill("#password", "fixture-secret").await.unwrap_err();
    assert!(
        matches!(error, Error::Action { receipt, .. } if receipt.outcome == ActionOutcome::OutcomeUnknown)
    );
    assert_eq!(
        page.evaluate::<String>("password.value").await.unwrap(),
        "f"
    );
    assert_eq!(page.evaluate::<String>("other.value").await.unwrap(), "");
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; login compatibility regression"]
async fn default_fill_delivers_separate_native_input_events_and_preserves_focus() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML='<input id=email><input id=password hidden><button id=submit disabled>Submit</button>';window.inputs=[];window.previous='';email.onblur=()=>{password.hidden=false;password.focus()};password.oninput=e=>{inputs.push({value:e.target.value,trusted:e.isTrusted});submit.disabled=previous.length===0;previous=e.target.value}").await.unwrap();
    page.fill("#email", "test@example.com").await.unwrap();
    assert!(page.evaluate::<bool>("password.hidden").await.unwrap());
    page.explore(
        Action::PressKey { key: "Tab".into() },
        ActionOptions::default(),
    )
    .await
    .unwrap();
    page.fill("#password", "aé😀").await.unwrap();
    assert_eq!(
        page.evaluate::<String>("password.value").await.unwrap(),
        "aé😀"
    );
    assert_eq!(page.evaluate::<usize>("inputs.length").await.unwrap(), 3);
    assert!(page
        .evaluate::<bool>("inputs.every(e=>e.trusted)")
        .await
        .unwrap());
    assert!(!page.evaluate::<bool>("submit.disabled").await.unwrap());
    assert!(page
        .evaluate::<bool>("document.activeElement===password")
        .await
        .unwrap());
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; JavaScript diagnostics regression"]
async fn javascript_exception_preserves_the_actual_exception_description() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    let error = page
        .execute("throw new TypeError('fixture exception detail')")
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("TypeError: fixture exception detail"),
        "{error}"
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; navigation diagnostics regression"]
async fn failed_navigation_retains_chrome_error_text() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    drop(listener);
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    let error = page
        .act(
            Action::Navigate {
                url,
                referrer: None,
            },
            ActionOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(&error, Error::Action { .. }));
    assert!(
        error.to_string().contains("ERR_CONNECTION_REFUSED"),
        "{error}"
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; explicit input and deadline regression"]
async fn bulk_input_is_explicit_and_partial_sequential_input_never_replays() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML='<input id=value><input id=locked readonly>';window.hits=0;document.querySelector('#value').oninput=()=>hits++").await.unwrap();
    page.act(
        Action::Fill {
            target: LocatorSpec::css("#value"),
            value: "bulk".into(),
            input_mode: InputMode::Bulk,
        },
        ActionOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(page.evaluate::<u32>("hits").await.unwrap(), 1);
    let error = page
        .act(
            Action::Fill {
                target: LocatorSpec::css("#locked"),
                value: "x".into(),
                input_mode: InputMode::Sequential,
            },
            ActionOptions {
                timeout_ms: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("readonly"), "{error}");
    let error = page
        .act(
            Action::Fill {
                target: LocatorSpec::css("#value"),
                value: "x".repeat(100),
                input_mode: InputMode::Sequential,
            },
            ActionOptions {
                timeout_ms: 180,
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::Action { receipt, .. } if receipt.outcome == ActionOutcome::OutcomeUnknown)
    );
    let partial: String = page
        .evaluate("document.querySelector('#value').value")
        .await
        .unwrap();
    assert!(!partial.is_empty() && partial.len() < 100);
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(
        page.evaluate::<String>("document.querySelector('#value').value")
            .await
            .unwrap(),
        partial
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; scoped evaluation regression"]
async fn function_evaluation_has_fresh_locals_and_calls_once() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    let function = "(value)=>{const local=value;window.calls=(window.calls||0)+1;return local}";
    for _ in 0..2 {
        let value: String = page
            .evaluate_function(function, &[serde_json::json!("quoted ' \" 😀")])
            .await
            .unwrap();
        assert_eq!(value, "quoted ' \" 😀");
    }
    assert_eq!(page.evaluate::<u32>("calls").await.unwrap(), 2);
    let error = page
        .execute_function("async()=>{throw new Error('promise rejection')}", &[])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("promise rejection"));
    assert!(page
        .evaluate_function::<serde_json::Value>(function, &[serde_json::json!(9007199254740993u64)])
        .await
        .is_err());
    assert_eq!(page.evaluate::<u32>("calls").await.unwrap(), 2);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; popup and application readiness regression"]
async fn click_follows_the_matching_popup_and_waits_for_its_application() {
    let server = support::HttpFixture::start(|_| async { support::Response::html("<button id=ready disabled>Ready</button><script>setTimeout(()=>{document.querySelector('#ready').disabled=false},250)</script>") }).await;
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute_function("url=>{const link=document.createElement('a');link.id='login';link.href=url;link.target='_blank';link.textContent='Login';document.body.append(link)}", &[serde_json::json!(server.url)]).await.unwrap();
    let receipt = page
        .locator("#login")
        .click(ActionOptions {
            expect: Some(Expectation::All {
                conditions: vec![
                    Expectation::Url {
                        value: server.url.clone(),
                        contains: false,
                    },
                    Expectation::Enabled {
                        target: LocatorSpec::css("#ready"),
                        enabled: true,
                    },
                ],
            }),
            timeout_ms: 3000,
            destination: Destination::CurrentOrNew,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::VerifiedComplete);
    assert_ne!(
        receipt.destination_target_id.as_deref(),
        Some(page.target_id())
    );
    assert_eq!(page.url().await.unwrap(), "about:blank");
    let destination = browser
        .attach_page(receipt.destination_target_id.as_deref().unwrap())
        .await
        .unwrap();
    assert!(destination
        .evaluate::<bool>("!document.querySelector('#ready').disabled")
        .await
        .unwrap());
    browser.close().await.unwrap();
}
