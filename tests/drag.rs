use base64::{engine::general_purpose::STANDARD, Engine};
use eoka::{Browser, Page};
use serde_json::Value;

const PAGE: &str = r#"<!doctype html><html><body style="margin:0">
<div id="track" style="position:absolute;left:100px;top:100px;width:400px;height:40px;background:#eee">
  <div id="handle" style="position:absolute;left:0;top:0;width:40px;height:40px;background:#888"></div>
</div>
<script>
window.__log = [];
for (const t of ['mousedown','mousemove','mouseup']) {
  document.addEventListener(t, e => window.__log.push({t, x: e.clientX, y: e.clientY, b: e.buttons}), true);
}
window.__pointerLog=[];
for (const t of ['pointerdown','pointermove','pointerup']) {
  document.addEventListener(t,e=>window.__pointerLog.push({t,mx:e.movementX,button:e.button,buttons:e.buttons,pressure:e.pressure,trusted:e.isTrusted,focus:document.hasFocus(),visibility:document.visibilityState}),true);
}
</script></body></html>"#;

async fn fixture(browser: &Browser) -> Page {
    browser
        .new_page(&format!("data:text/html;base64,{}", STANDARD.encode(PAGE)))
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn drag_by_produces_human_shaped_trajectory() {
    let browser = Browser::launch().await.unwrap();
    let page = fixture(&browser).await;
    let dx = 150.0;
    page.human_drag("#handle", dx).await.expect("drag");

    let log: Vec<Value> = page.evaluate_sync("window.__log").await.unwrap();
    browser.close().await.unwrap();

    let down = log
        .iter()
        .position(|e| e["t"] == "mousedown")
        .expect("mousedown");
    let up = log
        .iter()
        .rposition(|e| e["t"] == "mouseup")
        .expect("mouseup");
    assert!(down < up, "mousedown must precede mouseup");
    assert_eq!(
        log[down]["b"].as_u64(),
        Some(1),
        "mousedown must report left held"
    );
    assert_eq!(
        log[up]["b"].as_u64(),
        Some(0),
        "mouseup must report no buttons held"
    );

    let drag_moves: Vec<&Value> = log[down + 1..up]
        .iter()
        .filter(|e| e["t"] == "mousemove")
        .collect();
    assert!(
        drag_moves.len() >= 10,
        "expected a rich move stream, got {}",
        drag_moves.len()
    );

    for m in &drag_moves {
        assert_eq!(
            m["b"].as_u64(),
            Some(1),
            "move without left button held: {m}"
        );
    }

    let start_x = log[down]["x"].as_f64().unwrap();
    let end_x = log[up]["x"].as_f64().unwrap();
    let max_x = drag_moves
        .iter()
        .map(|m| m["x"].as_f64().unwrap())
        .fold(f64::NEG_INFINITY, f64::max);

    assert!(
        (end_x - (start_x + dx)).abs() <= 3.0,
        "release x {end_x} should be ~{}",
        start_x + dx
    );
    assert!(
        max_x > end_x + 2.0,
        "expected overshoot past {end_x}, max was {max_x}"
    );
}

#[tokio::test]
#[ignore = "requires headed Chrome and DISPLAY"]
async fn native_pointer_event_semantics() {
    let browser = Browser::launch_with(|config| {
        config.headless = false;
        config.live_session = true;
    })
    .await
    .unwrap();
    let page = fixture(&browser).await;
    page.human()
        .drag_horizontal_by(120.0, 120.0, 150.0)
        .await
        .unwrap();
    let events: Vec<Value> = page.evaluate_sync("window.__pointerLog").await.unwrap();
    browser.close().await.unwrap();
    assert!(events
        .iter()
        .all(|e| e["trusted"] == true && e["visibility"] == "visible" && e["focus"] == true));
    assert!(events
        .iter()
        .all(|e| e["pressure"] == if e["buttons"] == 0 { 0.0 } else { 0.5 }));
    let moves: Vec<_> = events
        .iter()
        .filter(|e| e["t"] == "pointermove" && e["buttons"] == 1)
        .collect();
    assert!(!moves.is_empty());
    assert!(moves.iter().all(|e| e["button"] == -1));
    assert!(moves.iter().all(|e| e["pressure"] == 0.5));
    assert!(moves
        .iter()
        .any(|e| e["mx"].as_f64().is_some_and(|x| x > 0.0)));
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn horizontal_drag_stays_on_track_without_reversal() {
    let browser = Browser::launch().await.unwrap();
    let page = fixture(&browser).await;
    for (start, dx) in [(120.0, 150.0), (300.0, -150.0)] {
        page.execute_sync("window.__log=[]").await.unwrap();
        page.human()
            .drag_horizontal_by(start, 120.0, dx)
            .await
            .unwrap();
        let log: Vec<Value> = page.evaluate_sync("window.__log").await.unwrap();
        let down = log.iter().position(|e| e["t"] == "mousedown").unwrap();
        let up = log.iter().position(|e| e["t"] == "mouseup").unwrap();
        let mut last_x = start;
        for event in &log[down + 1..up] {
            let x = event["x"].as_f64().unwrap();
            assert!((x - last_x) * dx.signum() >= 0.0);
            assert!((x - start).abs() <= dx.abs());
            assert_eq!(event["y"].as_f64(), Some(120.0));
            assert_eq!(event["b"].as_u64(), Some(1));
            last_x = x;
        }
        assert!((log[up]["x"].as_f64().unwrap() - (start + dx)).abs() < 1.0);
    }
    browser.close().await.unwrap();
}
