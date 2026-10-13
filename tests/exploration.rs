mod support;

use eoka::{Action, ActionOptions, ActionOutcome, DispatchState, Expectation, LocatorSpec};

#[tokio::test]
#[ignore = "requires Chrome; local exploratory action fixture"]
async fn exploratory_dispatch_does_not_claim_completion_and_keeps_verification() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML='<button id=go>Go</button><input id=name><p id=status>ready</p>';window.hits=0;document.querySelector('#go').onclick=()=>{window.hits++;document.querySelector('#status').textContent='done'}").await.unwrap();
    let click = Action::Click {
        target: LocatorSpec::css("#go"),
        human: false,
        double: false,
    };
    let receipt = page
        .explore(click.clone(), ActionOptions::default())
        .await
        .unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::OutcomeUnknown);
    assert_eq!(receipt.dispatch, DispatchState::Acknowledged);
    assert!(receipt.verification.is_none());
    assert_eq!(page.evaluate::<u32>("window.hits").await.unwrap(), 1);
    assert!(page
        .act(click.clone(), ActionOptions::default())
        .await
        .is_err());
    assert_eq!(page.evaluate::<u32>("window.hits").await.unwrap(), 1);
    page.execute("document.querySelector('#status').textContent='ready'")
        .await
        .unwrap();
    let receipt = page
        .explore(
            click,
            ActionOptions::expecting(Expectation::Text {
                target: LocatorSpec::css("#status"),
                value: "done".into(),
                contains: false,
            }),
        )
        .await
        .unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::VerifiedComplete);
    let receipt = page
        .explore(
            Action::Fill {
                target: LocatorSpec::css("#name"),
                value: "Ada".into(),
                human: false,
            },
            ActionOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::VerifiedComplete);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local exploratory action fixture"]
async fn exploratory_click_still_respects_obstructions_and_deadline() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML='<button id=go onclick=window.hits++>Go</button><div style=\"position:fixed;inset:0;background:white\"></div>';window.hits=0").await.unwrap();
    let start = std::time::Instant::now();
    let error = page
        .explore(
            Action::Click {
                target: LocatorSpec::css("#go"),
                human: false,
                double: false,
            },
            ActionOptions {
                timeout_ms: 200,
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
    assert!(
        matches!(error, eoka::Error::Action { receipt, .. } if receipt.outcome == ActionOutcome::NotDispatched)
    );
    assert_eq!(page.evaluate::<u32>("window.hits").await.unwrap(), 0);
    browser.close().await.unwrap();
}
