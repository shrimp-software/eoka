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
            input_mode: Default::default(),
        },
        Action::Type {
            target: LocatorSpec::css("#date"),
            text: "2026-10-12".into(),
            input_mode: Default::default(),
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

#[tokio::test]
#[ignore = "requires Chrome; history fixture"]
async fn history_without_an_entry_fails_before_dispatch() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    for action in [Action::Forward, Action::History { delta: -5 }] {
        let error = page.act(action, options()).await.unwrap_err();
        assert!(
            matches!(&error, Error::Action { code, receipt, .. } if code == "history_unavailable"
                && receipt.dispatch == DispatchState::NotSent
                && receipt.outcome == ActionOutcome::NotDispatched),
            "{error:?}"
        );
    }
    assert!(page.forward().await.is_err());

    // A real entry still navigates and verifies.
    page.goto("data:text/html,<p>second</p>").await.unwrap();
    let receipt = page.act(Action::Back, options()).await.unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::VerifiedComplete);
    assert_eq!(receipt.verification.as_deref(), Some("document_ready"));
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn find_by_text_returns_innermost_static_match() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML=`<div id=app><div id=wrap><span id=target>Sign in</span></div><p>Other</p></div>`").await.unwrap();
    let found = page.find_by_text("Sign in").await.unwrap();
    assert_eq!(
        found.get_attribute("id").await.unwrap().as_deref(),
        Some("target")
    );
    let all = page.find_all_by_text("Sign in").await.unwrap();
    assert_eq!(all.len(), 1, "ancestors must not be returned");
    // Quotes and template syntax are data, not code.
    page.execute(r"document.body.innerHTML=`<p id=q>it's \${x}</p>`")
        .await
        .unwrap();
    let quoted = page.find_by_text("it's ${x}").await.unwrap();
    assert_eq!(
        quoted.get_attribute("id").await.unwrap().as_deref(),
        Some("q")
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn fill_reports_a_masked_value_before_the_deadline() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML=`<input id=phone>`;document.querySelector('#phone').addEventListener('input',e=>{e.target.value=e.target.value.replace(/\\D/g,'')})").await.unwrap();
    let started = std::time::Instant::now();
    let error = page
        .act(
            Action::Fill {
                target: LocatorSpec::css("#phone"),
                value: "12ab".into(),
                input_mode: Default::default(),
            },
            ActionOptions {
                timeout_ms: 20_000,
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "waited out the deadline"
    );
    match error {
        Error::Action { code, source, .. } => {
            assert_eq!(code, "value_mismatch");
            let text = source.to_string();
            assert!(
                !text.contains("12ab"),
                "value leaked into the error: {text}"
            );
            assert!(matches!(
                *source,
                Error::ValueMismatch {
                    expected_chars: 4,
                    actual_chars: 2,
                    ..
                }
            ));
        }
        other => panic!("unexpected {other:?}"),
    }
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn goto_to_an_attachment_fails_fast_as_aborted() {
    let fixture = support::HttpFixture::start(|_| async {
        support::Response {
            status: "200 OK",
            headers: vec![
                ("Content-Type".into(), "application/octet-stream".into()),
                (
                    "Content-Disposition".into(),
                    "attachment; filename=x.bin".into(),
                ),
            ],
            body: vec![0; 16],
            body_gate: None,
        }
    })
    .await;
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    let started = std::time::Instant::now();
    let error = page.goto(&fixture.url).await.unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(
        matches!(&error, Error::Action { code, .. } if code == "navigation_aborted"),
        "{error:?}"
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn expectation_survives_a_navigation_started_by_the_click() {
    let fixture = support::HttpFixture::start(|request| async move {
        if request.target.starts_with("/next") {
            support::Response::html("<h1 id=done>Next page</h1>")
        } else {
            support::Response::html("<a id=go href=/next>Go</a>")
        }
    })
    .await;
    let browser = support::browser().await;
    let page = browser.new_page(&fixture.url).await.unwrap();
    let receipt = page
        .act(
            Action::Click {
                target: LocatorSpec::css("#go"),
                human: false,
                double: false,
            },
            ActionOptions {
                expect: Some(Expectation::Visible {
                    target: LocatorSpec::css("#done"),
                }),
                destination: eoka::Destination::Current,
                timeout_ms: 10_000,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::VerifiedComplete);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn with_retry_stops_on_permanent_errors() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    let calls = std::sync::atomic::AtomicU32::new(0);
    let error = page
        .with_retry(5, 1, || async {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err::<(), _>(Error::InvalidUrl("bad".into()))
        })
        .await
        .unwrap_err();
    assert!(matches!(error, Error::InvalidUrl(_)));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

    let calls = std::sync::atomic::AtomicU32::new(0);
    let error = page
        .with_retry(3, 1, || async {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err::<(), _>(Error::Timeout("slow".into()))
        })
        .await
        .unwrap_err();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert!(
        matches!(error, Error::RetryExhausted { attempts: 3, ref source, .. } if matches!(**source, Error::Timeout(_)))
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn locator_covers_hover_focus_type_and_submit() {
    let browser = support::browser().await;
    let page = browser.new_blank_page().await.unwrap();
    page.execute("document.body.innerHTML=`<form id=f onsubmit='event.preventDefault();document.querySelector(\"#h\").textContent=\"sent\"'><input id=q></form><div id=h>hover</div>`").await.unwrap();
    let receipt = page.locator("#h").hover(options()).await.unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::VerifiedComplete);
    let receipt = page.locator("#q").focus(options()).await.unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::VerifiedComplete);
    page.locator("#q")
        .type_text(
            "abc",
            ActionOptions {
                expect: Some(Expectation::Value {
                    target: LocatorSpec::css("#q"),
                    value: "abc".into(),
                }),
                ..options()
            },
        )
        .await
        .unwrap();
    page.locator("#q")
        .submit(ActionOptions {
            expect: Some(Expectation::Text {
                target: LocatorSpec::css("#h"),
                value: "sent".into(),
                contains: false,
            }),
            ..options()
        })
        .await
        .unwrap();
    browser.close().await.unwrap();
}
