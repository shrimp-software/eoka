mod support;

use eoka::{Action, ActionOptions, ActionOutcome, DispatchState, Error, Expectation, LocatorSpec};

fn options() -> ActionOptions {
    ActionOptions {
        timeout_ms: 2000,
        ..Default::default()
    }
}

#[tokio::test]
#[ignore = "requires Chrome; nested scrolling fixture"]
async fn clicks_scroll_nested_clipping_containers_and_still_reject_overlays() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML=`<div id=outer style='width:160px;height:100px;overflow:auto'><div style='width:500px;height:500px;padding:120px 0 0 180px;box-sizing:border-box'><div id=inner style='width:140px;height:80px;overflow:auto'><div style='width:400px;height:350px;padding:200px 0 0 200px;box-sizing:border-box'><button id=go>Go</button></div></div></div></div><p id=status>ready</p>`;window.hits=0;document.querySelector('#go').onclick=()=>{window.hits++;document.querySelector('#status').textContent='done'}").await.unwrap();
    let receipt = page
        .locator("#go")
        .click(ActionOptions {
            expect: Some(Expectation::Text {
                target: LocatorSpec::css("#status"),
                value: "done".into(),
                contains: false,
            }),
            ..options()
        })
        .await
        .unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::VerifiedComplete);
    assert_eq!(page.evaluate::<u32>("window.hits").await.unwrap(), 1);
    assert!(page.evaluate::<bool>("['outer','inner'].every(id=>{const e=document.getElementById(id);return e.scrollTop>0&&e.scrollLeft>0})").await.unwrap());

    page.execute("document.body.insertAdjacentHTML('beforeend',`<div style='position:fixed;inset:0;background:white;z-index:9999'></div>`)").await.unwrap();
    let error = page
        .explore(
            Action::Click {
                target: LocatorSpec::css("#go"),
                human: false,
                double: false,
            },
            ActionOptions {
                timeout_ms: 200,
                ..options()
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::Action { receipt, .. } if receipt.outcome == ActionOutcome::NotDispatched)
    );
    assert_eq!(page.evaluate::<u32>("window.hits").await.unwrap(), 1);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; native input fixture"]
async fn native_temporal_fills_validate_values_emit_events_and_allow_clearing() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    for (kind, requested, expected) in [
        ("date", "2026-10-12", "2026-10-12"),
        ("time", "14:30", "14:30"),
        ("datetime-local", "2026-10-12T14:30:00", "2026-10-12T14:30"),
        ("month", "2026-10", "2026-10"),
        ("week", "2026-W42", "2026-W42"),
    ] {
        page.execute(&format!("document.body.innerHTML='<input id=field required type={kind}>';window.events=[];for(const event of ['input','change'])document.querySelector('#field').addEventListener(event,()=>window.events.push(event))")).await.unwrap();
        let receipt = page
            .locator("#field")
            .fill(requested, options())
            .await
            .unwrap();
        assert_eq!(receipt.outcome, ActionOutcome::VerifiedComplete, "{kind}");
        assert_eq!(
            page.evaluate::<String>("document.querySelector('#field').value")
                .await
                .unwrap(),
            expected,
            "{kind}"
        );
        assert_eq!(
            page.evaluate::<Vec<String>>("window.events").await.unwrap(),
            ["input", "change"],
            "{kind}"
        );
        let receipt = page
            .locator("#field")
            .fill(requested, options())
            .await
            .unwrap();
        assert!(receipt.already_satisfied, "{kind}");
        assert_eq!(receipt.dispatch, DispatchState::NotSent);

        let error = page
            .locator("#field")
            .fill("invalid", options())
            .await
            .unwrap_err();
        assert!(
            matches!(error, Error::Action { code, receipt, .. } if code=="invalid_input_value" && receipt.dispatch==DispatchState::NotSent)
        );
        assert_eq!(
            page.evaluate::<String>("document.querySelector('#field').value")
                .await
                .unwrap(),
            expected
        );
        assert_eq!(
            page.evaluate::<usize>("window.events.length")
                .await
                .unwrap(),
            2
        );

        page.locator("#field").fill("", options()).await.unwrap();
        assert_eq!(
            page.evaluate::<String>("document.querySelector('#field').value")
                .await
                .unwrap(),
            ""
        );
        assert_eq!(
            page.evaluate::<Vec<String>>("window.events").await.unwrap(),
            ["input", "change", "input", "change"]
        );
    }
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; unsupported input fixture"]
async fn unsupported_input_reports_an_error_before_dispatch() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute(
        "document.body.innerHTML='<input id=checkbox type=checkbox><input id=date type=date>'",
    )
    .await
    .unwrap();
    for action in [
        Action::Fill {
            target: LocatorSpec::css("#checkbox"),
            value: "true".into(),
            human: false,
        },
        Action::Type {
            target: LocatorSpec::css("#date"),
            text: "2026-10-12".into(),
            human: false,
        },
    ] {
        let error = page.explore(action, options()).await.unwrap_err();
        assert!(
            matches!(error, Error::Action { code, receipt, .. } if code=="unsupported_input" && receipt.dispatch==DispatchState::NotSent)
        );
    }
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; shared visibility fixture"]
async fn visibility_helpers_agree_with_rendering_and_preserve_transport_errors() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML=`<button id=hidden style='visibility:hidden'>Hidden</button><div style='visibility:hidden'><button id=inherited>Hidden parent</button></div><button id=none style='display:none'>None</button><div id=zero style='width:0;height:0'></div><button id=transparent style='opacity:0'>Transparent</button><button id=offscreen style='position:absolute;top:3000px'>Offscreen</button><button id=disabled disabled>Disabled</button>`").await.unwrap();
    for selector in ["#hidden", "#inherited", "#none", "#zero"] {
        assert!(
            !page
                .find(selector)
                .await
                .unwrap()
                .is_visible()
                .await
                .unwrap(),
            "{selector}"
        );
        page.wait_for_hidden(selector, 500).await.unwrap();
        assert!(
            matches!(
                page.wait_for_visible(selector, 100).await,
                Err(Error::Timeout(_))
            ),
            "{selector}"
        );
    }
    for selector in ["#transparent", "#offscreen", "#disabled"] {
        assert!(
            page.find(selector)
                .await
                .unwrap()
                .is_visible()
                .await
                .unwrap(),
            "{selector}"
        );
        page.wait_for_visible(selector, 500).await.unwrap();
    }
    page.execute(
        "setTimeout(()=>document.querySelector('#hidden').style.visibility='visible',100)",
    )
    .await
    .unwrap();
    let element = page.wait_for_visible("#hidden", 2000).await.unwrap();
    assert!(element.is_visible().await.unwrap());
    browser.close().await.unwrap();
    assert!(element.is_visible().await.is_err());
}
