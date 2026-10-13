mod support;

use eoka::{Action, ActionOptions, ActionOutcome, DispatchState, Error, Expectation, LocatorSpec};
use std::time::{Duration, Instant};

fn expected_text(selector: &str, value: &str) -> ActionOptions {
    ActionOptions {
        timeout_ms: 2000,
        ..ActionOptions::expecting(Expectation::Text {
            target: LocatorSpec::css(selector),
            value: value.into(),
            contains: false,
        })
    }
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn actions_reject_ambiguity_and_missing_expectations_without_input() {
    let server=support::HttpFixture::start(|_|async {support::Response::html("<button onclick='window.hits++'>a</button><button>b</button><p id=result>pending</p><script>window.hits=0</script>")}).await;
    let browser = support::browser().await;
    let page = browser.new_page(&server.url).await.unwrap();
    let err = page
        .locator("button")
        .click(expected_text("#result", "done"))
        .await
        .unwrap_err();
    assert!(
        matches!(err,Error::Action {ref code,ref receipt,..} if code=="ambiguous_target" && receipt.outcome==ActionOutcome::NotDispatched)
    );
    let err = page
        .locator("button")
        .first()
        .click(ActionOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err,Error::Action {ref code,..} if code=="expectation_required"));
    assert_eq!(page.evaluate::<u32>("window.hits").await.unwrap(), 0);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn actions_wait_for_replacement_and_verify_one_click() {
    let server=support::HttpFixture::start(|_|async {support::Response::html("<button id=go disabled>Go</button><p id=result>pending</p><script>window.hits=0;setTimeout(()=>{document.querySelector('#go').outerHTML=\"<button id=go onclick=\"+JSON.stringify(\"window.hits++;document.querySelector('#result').textContent='done'\")+\">Go</button>\"},150)</script>")}).await;
    let browser = support::browser().await;
    let page = browser.new_page(&server.url).await.unwrap();
    let receipt = page
        .locator("#go")
        .click(expected_text("#result", "done"))
        .await
        .unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::VerifiedComplete);
    assert_eq!(receipt.dispatch, DispatchState::Acknowledged);
    assert_eq!(page.evaluate::<u32>("window.hits").await.unwrap(), 1);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn fill_select_and_navigation_verify_intrinsic_results() {
    let server=support::HttpFixture::start(|_|async {support::Response::html("<input id=name><select id=choice><option value=a>A</option><option value=b>B</option></select>")}).await;
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.act(
        Action::Navigate {
            url: server.url.clone(),
            referrer: None,
        },
        ActionOptions::default(),
    )
    .await
    .unwrap();
    page.locator("#name")
        .fill("secret", ActionOptions::default())
        .await
        .unwrap();
    let receipt = page
        .locator("#name")
        .fill("secret", ActionOptions::default())
        .await
        .unwrap();
    assert!(receipt.already_satisfied);
    assert_eq!(receipt.dispatch, DispatchState::NotSent);
    page.locator("#choice")
        .select(vec!["b".into()], ActionOptions::default())
        .await
        .unwrap();
    assert_eq!(
        page.evaluate::<String>("document.querySelector('#choice').value")
            .await
            .unwrap(),
        "b"
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn action_deadline_reports_unknown_without_replaying() {
    let server=support::HttpFixture::start(|_|async {support::Response::html("<button id=go onclick='window.hits++'>Go</button><p id=result>pending</p><script>window.hits=0</script>")}).await;
    let browser = support::browser().await;
    let page = browser.new_page(&server.url).await.unwrap();
    let mut options = expected_text("#result", "done");
    options.timeout_ms = 1000;
    let started = Instant::now();
    let err = page.locator("#go").click(options).await.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(
        matches!(err,Error::Action {ref receipt,..} if receipt.outcome==ActionOutcome::OutcomeUnknown)
    );
    assert_eq!(page.evaluate::<u32>("window.hits").await.unwrap(), 1);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn overlays_and_motion_are_waited_out_before_a_single_click() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML=`<button id=go style='position:absolute;left:50px;top:50px' onclick=\"window.hits++;document.getElementById('result').textContent='done'\">Go</button><p id=result>pending</p><div id=cover style='position:fixed;inset:0;background:white'></div>`;window.hits=0;let n=0;const timer=setInterval(()=>{document.getElementById('go').style.left=(50+(n++%2)*50)+'px'},15);setTimeout(()=>{clearInterval(timer);document.getElementById('cover').remove()},200)").await.unwrap();
    page.locator("#go")
        .click(expected_text("#result", "done"))
        .await
        .unwrap();
    assert_eq!(page.evaluate::<u32>("window.hits").await.unwrap(), 1);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn queue_wait_consumes_the_same_deadline_and_does_not_dispatch() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML=`<button id=go onclick='window.hits++'>Go</button><p id=result>pending</p>`;window.hits=0").await.unwrap();
    let first = page.clone();
    let task = tokio::spawn(async move {
        let mut options = expected_text("#result", "done");
        options.timeout_ms = 4000;
        first.locator("#go").click(options).await
    });
    page.wait_for_text("Go", 1000).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while page.evaluate::<u32>("window.hits").await.unwrap() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("first action must dispatch before testing queue timeout");
    let mut options = expected_text("#result", "done");
    options.timeout_ms = 80;
    let start = Instant::now();
    let error = page.locator("#go").click(options).await.unwrap_err();
    assert!(start.elapsed() < Duration::from_millis(300));
    assert!(
        matches!(error,Error::Action{receipt,..} if receipt.outcome==ActionOutcome::NotDispatched)
    );
    assert!(task.await.unwrap().is_err());
    assert_eq!(page.evaluate::<u32>("window.hits").await.unwrap(), 1);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn abort_after_mouse_down_releases_input_before_the_next_action() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML='<button id=go>Go</button><input id=field><p id=result>pending</p>';window.down=0;window.up=0;document.addEventListener('mousedown',()=>window.down++);document.addEventListener('mouseup',()=>window.up++)").await.unwrap();
    let first = page.clone();
    let task = tokio::spawn(async move {
        first
            .locator("#go")
            .human_click(expected_text("#result", "done"))
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while page.evaluate::<u32>("window.down").await.unwrap() == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    page.fill("#field", "next").await.unwrap();
    assert_eq!(page.evaluate::<u32>("window.up").await.unwrap(), 1);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local action fixture"]
async fn pinned_handles_never_click_replacements_and_already_true_expectations_reject() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute(
        "document.body.innerHTML='<button id=go>Go</button><p id=result>pending</p>';window.hits=0",
    )
    .await
    .unwrap();
    let configured = page.with_action_options(expected_text("#result", "done"));
    let original = configured.find("#go").await.unwrap();
    page.execute("document.getElementById('go').outerHTML=`<button id=go onclick=\"window.hits++;document.getElementById('result').textContent='done'\">Replacement</button>`").await.unwrap();
    assert!(original.click().await.is_err());
    assert_eq!(page.evaluate::<u32>("window.hits").await.unwrap(), 0);
    configured.click("#go").await.unwrap();
    let error = configured.click("#go").await.unwrap_err();
    assert!(
        matches!(error,Error::Action{code,receipt,..} if code=="expectation_already_satisfied" && receipt.dispatch==DispatchState::NotSent)
    );
    assert_eq!(page.evaluate::<u32>("window.hits").await.unwrap(), 1);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; local frame action fixture"]
async fn framed_actions_wait_for_parent_interception() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML=`<iframe name=child srcdoc=\"<button onclick='this.textContent=String(Number(this.textContent)+1)'>0</button>\"></iframe><div id=cover style='position:fixed;inset:0;background:white'></div>`").await.unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(frame) = page
                .frames()
                .await
                .unwrap()
                .into_iter()
                .find(|f| f.name.as_deref() == Some("child"))
            {
                break frame.id;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let locator = page.locator("button").in_frame(frame.clone());
    let options = ActionOptions {
        timeout_ms: 2000,
        ..ActionOptions::expecting(Expectation::Text {
            target: locator.specification(),
            value: "1".into(),
            contains: false,
        })
    };
    page.execute("setTimeout(()=>document.getElementById('cover').remove(),150)")
        .await
        .unwrap();
    locator.click(options).await.unwrap();
    assert_eq!(
        page.evaluate_in_frame_id::<String>(&frame, "document.querySelector('button').textContent")
            .await
            .unwrap(),
        "1"
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; durable intent refusal must emit no input"]
async fn refused_dispatch_does_not_send_a_cleanup_mouse_up() {
    struct RefusePrimary;
    impl eoka::DispatchObserver for RefusePrimary {
        fn before_dispatch(
            &self,
            preparation: bool,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = eoka::Result<()>> + Send + '_>>
        {
            Box::pin(async move {
                if preparation {
                    Ok(())
                } else {
                    Err(Error::transport("intent storage unavailable"))
                }
            })
        }
    }
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML='<button id=go>Go</button><input id=field><p id=result>pending</p>';window.inputEvents=0;for(const type of ['mousedown','mouseup','click'])document.addEventListener(type,()=>window.inputEvents++)").await.unwrap();
    let mut options = expected_text("#result", "done");
    options.observer = Some(std::sync::Arc::new(RefusePrimary));
    let error = page.locator("#go").click(options).await.unwrap_err();
    assert!(
        matches!(error,Error::Action{receipt,..} if receipt.outcome==ActionOutcome::NotDispatched && !receipt.cleanup_pending)
    );
    page.fill("#field", "next").await.unwrap();
    assert_eq!(page.evaluate::<u32>("window.inputEvents").await.unwrap(), 0);
    browser.close().await.unwrap();
}
