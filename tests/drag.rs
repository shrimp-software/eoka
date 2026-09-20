//! Integration test for Human::drag_by trajectory shape.
//!
//! Requires Chrome and a display for native-event coverage.
//! Run with: xvfb-run -a cargo test --test drag -- --ignored
//!
//! Serves a mock slider page that records every mouse event, drags the
//! "handle", then asserts the trajectory looks human: press -> moves with
//! the left button held -> overshoot past the target -> settle back ->
//! release on the exact target.

use eoka::Browser;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
  document.addEventListener(t,e=>window.__pointerLog.push({t,x:e.clientX,y:e.clientY,sx:e.screenX,sy:e.screenY,mx:e.movementX,my:e.movementY,button:e.button,buttons:e.buttons,pressure:e.pressure,trusted:e.isTrusted,focus:document.hasFocus(),visibility:document.visibilityState}),true);
}
</script></body></html>"#;

async fn serve() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let body = PAGE;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });
    (format!("http://{addr}/"), server)
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn drag_by_produces_human_shaped_trajectory() {
    let (url, server) = serve().await;
    let browser = Browser::launch().await.expect("launch");
    let page = browser.new_page(&url).await.expect("page");

    // Handle center: track at (100,100), handle 40x40 => (120, 120).
    let dx = 150.0;
    page.human_drag("#handle", dx).await.expect("drag");

    let log: Value = page
        .evaluate_sync("JSON.stringify(window.__log)")
        .await
        .expect("log");
    let log: Vec<Value> = serde_json::from_str(log.as_str().unwrap()).unwrap();
    server.abort();
    browser.close().await.ok();

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

    // Every move during the drag must report the left button held.
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

    // Release lands on the exact target (start + dx), within a few px.
    assert!(
        (end_x - (start_x + dx)).abs() <= 3.0,
        "release x {end_x} should be ~{}",
        start_x + dx
    );
    // Overshoot: the path must have gone past the release point.
    assert!(
        max_x > end_x + 2.0,
        "expected overshoot past {end_x}, max was {max_x}"
    );
}

#[tokio::test]
#[ignore = "requires headed Chrome and DISPLAY"]
async fn native_pointer_event_semantics() {
    let (url, server) = serve().await;
    let browser = Browser::launch_with(|config| {
        config.headless = false;
        config.live_session = true;
        config.extra_args.extend([
            "--disable-gpu-compositing".into(),
            "--use-angle=vulkan".into(),
        ]);
    })
    .await
    .unwrap();
    let page = browser.new_page(&url).await.unwrap();
    page.human()
        .drag_horizontal_by(120.0, 120.0, 150.0)
        .await
        .unwrap();
    let events: Vec<Value> = page.evaluate_sync("window.__pointerLog").await.unwrap();
    let geometry:Value=page.evaluate_sync("({screenX,screenY,outerWidth,outerHeight,innerWidth,innerHeight,devicePixelRatio,focus:document.hasFocus()})").await.unwrap();
    println!("native geometry: {geometry}");
    for event in events
        .iter()
        .filter(|e| e["t"] != "pointermove" || e["mx"] != 0)
        .take(6)
    {
        println!("pointer: {event}");
    }
    browser.close().await.unwrap();
    server.abort();
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
    println!("held pointer: {}", moves[0]);
    assert!(moves.iter().all(|e| e["button"] == -1));
    assert!(moves.iter().all(|e| e["pressure"] == 0.5));
    assert!(moves
        .iter()
        .any(|e| e["mx"].as_f64().is_some_and(|x| x > 0.0)));
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn horizontal_drag_stays_on_track_without_reversal() {
    let (url, server) = serve().await;
    let browser = Browser::launch().await.unwrap();
    let page = browser.new_page(&url).await.unwrap();
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
    server.abort();
}
