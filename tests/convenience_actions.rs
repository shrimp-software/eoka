//! Shorthand input methods work without configuration and report what is known.

mod support;

use eoka::{Action, ActionOptions, ActionOutcome, DispatchState, Error, Expectation, LocatorSpec};
use std::future::IntoFuture;

const FIXTURE: &str = "<!doctype html><body><script>window.hits=0</script>\
<button id=go onclick=\"window.hits++;document.getElementById('result').textContent='done'\">Go</button>\
<button id=noop onclick=\"window.hits++\">Noop</button>\
<button id=hidden style=display:none onclick=\"window.hits++\">Hidden</button>\
<button id=covered onclick=\"window.hits++\" style=\"position:fixed;left:0;top:300px\">Covered</button>\
<div style=\"position:fixed;left:0;top:290px;width:400px;height:80px;z-index:9;background:#000\"></div>\
<p id=result>pending</p>";

fn done() -> Expectation {
    Expectation::Text {
        target: LocatorSpec::css("#result"),
        value: "done".into(),
        contains: false,
    }
}

async fn open() -> (support::HttpFixture, eoka::Browser, eoka::Page) {
    let server = support::HttpFixture::start(|_| async { support::Response::html(FIXTURE) }).await;
    let browser = support::browser().await;
    let page = browser.new_page(&server.url).await.unwrap();
    page.wait_for("#result", 2000).await.unwrap();
    (server, browser, page)
}

async fn hits(page: &eoka::Page) -> u32 {
    page.evaluate("window.hits").await.unwrap()
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn plain_click_dispatches_and_reports_an_unknown_outcome() {
    let (_server, browser, page) = open().await;
    let receipt = page.click("#go").await.unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::OutcomeUnknown);
    assert_eq!(receipt.dispatch, DispatchState::Acknowledged);
    assert_eq!(receipt.verification, None);
    assert_eq!(hits(&page).await, 1);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn expecting_proves_completion_for_page_and_element_calls() {
    let (_server, browser, page) = open().await;
    let receipt = page.click("#go").expecting(done()).await.unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::VerifiedComplete);
    assert_eq!(receipt.verification.as_deref(), Some("text"));

    let element = page.find("#noop").await.unwrap();
    let receipt = element.click().await.unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::OutcomeUnknown);
    assert_eq!(hits(&page).await, 2);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn an_unmet_expectation_fails_without_replaying_the_click() {
    let (_server, browser, page) = open().await;
    let error = page
        .click("#noop")
        .expecting(done())
        .timeout_ms(400)
        .await
        .unwrap_err();
    let Error::Action { code, receipt, .. } = error else {
        panic!("expected an action error");
    };
    assert_eq!(code, "action_timeout");
    assert_eq!(receipt.outcome, ActionOutcome::OutcomeUnknown);
    assert_eq!(hits(&page).await, 1);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn try_click_reports_whether_anything_was_dispatched() {
    let (_server, browser, page) = open().await;
    assert!(!page.try_click("#missing").await.unwrap());
    assert!(!page.try_click("#hidden").await.unwrap());
    assert!(!page.try_click("#covered").await.unwrap());
    assert_eq!(hits(&page).await, 0);
    assert!(page.try_click("#noop").await.unwrap());
    assert_eq!(hits(&page).await, 1);
    assert!(page.try_click("#go").expecting(done()).await.unwrap());
    assert_eq!(hits(&page).await, 2);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn unavailable_targets_are_errors_and_receive_no_input() {
    let (_server, browser, page) = open().await;
    for selector in ["#missing", "#hidden", "#covered"] {
        let error = page.click(selector).timeout_ms(300).await.unwrap_err();
        let Error::Action { code, receipt, .. } = error else {
            panic!("expected an action error for {selector}");
        };
        assert_eq!(code, "action_timeout", "{selector}");
        assert_eq!(receipt.outcome, ActionOutcome::NotDispatched, "{selector}");
    }
    assert_eq!(hits(&page).await, 0);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn explicit_option_apis_stay_strict() {
    let (_server, browser, page) = open().await;
    let click = Action::Click {
        target: LocatorSpec::css("#go"),
        human: false,
        double: false,
    };
    for error in [
        page.act(click, ActionOptions::default()).await.unwrap_err(),
        page.locator("#go")
            .click(ActionOptions::default())
            .await
            .unwrap_err(),
    ] {
        assert!(matches!(error, Error::Action { ref code, .. } if code == "expectation_required"));
    }
    assert_eq!(hits(&page).await, 0);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn replaced_elements_are_not_retargeted() {
    let (_server, browser, page) = open().await;
    let original = page.find("#go").await.unwrap();
    page.execute("document.getElementById('go').outerHTML=`<button id=go onclick=\"window.hits++\">Go</button>`")
        .await
        .unwrap();
    assert!(original.click().await.is_err());
    assert_eq!(hits(&page).await, 0);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn calls_can_move_to_another_task_and_do_nothing_until_awaited() {
    let (_server, browser, page) = open().await;
    let unused = page.click("#go");
    drop(unused);
    assert_eq!(hits(&page).await, 0);

    let call = page.click("#noop");
    tokio::spawn(call.into_future()).await.unwrap().unwrap();
    assert!(tokio::spawn(page.try_click("#noop").into_future())
        .await
        .unwrap()
        .unwrap());
    assert_eq!(hits(&page).await, 2);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn handle_defaults_apply_to_shorthand_and_found_elements() {
    let (_server, browser, page) = open().await;
    let quick = page.with_action_defaults(eoka::ActionDefaults {
        timeout_ms: 300,
        ..Default::default()
    });
    let started = std::time::Instant::now();
    assert!(quick.click("#missing").await.is_err());
    assert!(quick.fill("#missing", "x").await.is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(5));

    let element = quick.find("#noop").await.unwrap();
    let receipt = element.click().await.unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::OutcomeUnknown);
    assert_eq!(hits(&page).await, 1);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn type_text_requires_an_editable_focus() {
    let server = support::HttpFixture::start(|_| async {
        support::Response::html(
            "<!doctype html><input id=field><input id=locked readonly><button id=go>Go</button>",
        )
    })
    .await;
    let browser = support::browser().await;
    let page = browser.new_page(&server.url).await.unwrap();
    page.wait_for("#go", 2000).await.unwrap();

    for focus in [
        "document.activeElement.blur()",
        "document.getElementById('locked').focus()",
        "document.getElementById('go').focus()",
    ] {
        page.execute(focus).await.unwrap();
        let error = page.type_text("lost").await.unwrap_err();
        assert!(
            matches!(&error, Error::Action { code, receipt, .. }
                if code == "input_state" && receipt.outcome == ActionOutcome::NotDispatched),
            "{focus}: {error}"
        );
    }

    page.execute("document.getElementById('field').focus()")
        .await
        .unwrap();
    let receipt = page.type_text("kept").await.unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::OutcomeUnknown);
    let value: String = page
        .evaluate("document.getElementById('field').value")
        .await
        .unwrap();
    assert_eq!(value, "kept");
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn try_click_on_a_covered_target_sends_no_input_but_may_scroll() {
    let server = support::HttpFixture::start(|_| async {
        support::Response::html(
            "<!doctype html><body style=\"height:6000px\">\
             <button id=go style=\"position:absolute;top:3000px;left:10px\" onclick=\"window.hits=1\">Go</button>\
             <div style=\"position:absolute;top:2990px;left:0;width:500px;height:80px;z-index:9;background:black\"></div>",
        )
    })
    .await;
    let browser = support::browser().await;
    let page = browser.new_page(&server.url).await.unwrap();
    page.wait_for("#go", 2000).await.unwrap();
    assert!(!page.try_click("#go").await.unwrap());
    assert_eq!(
        page.evaluate::<Option<u8>>("window.hits ?? null")
            .await
            .unwrap(),
        None
    );
    assert!(page.evaluate::<f64>("scrollY").await.unwrap() > 0.0);
    browser.close().await.unwrap();
}
