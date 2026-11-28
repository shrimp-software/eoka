use super::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// A mock Chrome that answers commands through `answer` and reports them.
/// Events are injected through the returned sender.
async fn mock(
    answer: impl Fn(&Value) -> std::result::Result<Value, Value> + Send + 'static,
) -> (
    Connection,
    mpsc::UnboundedReceiver<Value>,
    mpsc::UnboundedSender<Value>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let (seen, commands) = mpsc::unbounded_channel();
    let (inject, mut injected) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut peer = tokio_tungstenite::accept_async(socket).await.unwrap();
        loop {
            tokio::select! {
                message = peer.next() => {
                    let Some(Ok(Message::Text(text))) = message else { break };
                    let command: Value = serde_json::from_str(&text).unwrap();
                    let reply = match answer(&command) {
                        Ok(result) => json!({"id":command["id"],"result":result}),
                        Err(error) => json!({"id":command["id"],"error":error}),
                    };
                    let _ = seen.send(command);
                    if peer.send(Message::Text(reply.to_string().into())).await.is_err() { break }
                }
                event = injected.recv() => {
                    let Some(event) = event else { break };
                    if peer.send(Message::Text(event.to_string().into())).await.is_err() { break }
                }
            }
        }
    });
    let transport = Transport::connect(&url, 5).await.unwrap();
    (Connection::new(transport), commands, inject)
}

fn attach_answer(command: &Value) -> std::result::Result<Value, Value> {
    match command["method"].as_str().unwrap() {
        "Target.attachToTarget" => Ok(json!({
            "sessionId": format!("session-{}", command["params"]["targetId"].as_str().unwrap())
        })),
        "Target.getTargetInfo" if command["params"]["targetId"] == "alive" => {
            Ok(json!({"targetInfo":{"targetId":"alive"}}))
        }
        "Target.getTargetInfo" => {
            Err(json!({"code":-32602,"message":"No target with given id found"}))
        }
        "Page.getFrameTree" => Ok(json!({"frameTree":{"frame":{"id":"main","loaderId":"loader"}}})),
        _ => Ok(json!({})),
    }
}

fn known(connection: &Connection, target: &str) -> bool {
    connection.targets.lock().unwrap().contains_key(target)
}

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("condition must hold before the deadline");
}

#[tokio::test]
async fn destroyed_and_confirmed_closed_targets_release_their_state() {
    let (connection, mut commands, inject) = mock(attach_answer).await;
    for target in ["destroyed", "closed", "alive"] {
        connection.attach_to_target(target).await.unwrap();
        assert!(known(&connection, target));
    }
    while commands.try_recv().is_ok() {}

    inject
        .send(json!({"method":"Target.targetDestroyed","params":{"targetId":"destroyed"}}))
        .unwrap();
    until(|| !known(&connection, "destroyed")).await;

    // A detached session of a live target keeps its state.
    inject
        .send(json!({"method":"Target.detachedFromTarget","params":{"sessionId":"helper","targetId":"alive"}}))
        .unwrap();
    let probe = tokio::time::timeout(Duration::from_secs(2), commands.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(probe["method"], "Target.getTargetInfo");
    assert_eq!(probe["params"]["targetId"], "alive");

    // A site-closed tab: the session detaches and the target no longer exists.
    inject
        .send(json!({"method":"Target.detachedFromTarget","params":{"sessionId":"session-closed","targetId":"closed"}}))
        .unwrap();
    until(|| !known(&connection, "closed")).await;
    assert!(known(&connection, "alive"));

    // Unknown targets are not probed.
    while commands.try_recv().is_ok() {}
    inject
        .send(json!({"method":"Target.detachedFromTarget","params":{"sessionId":"x","targetId":"unknown"}}))
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), commands.recv())
            .await
            .is_err()
    );
    connection.disconnect().await.unwrap();
}

#[tokio::test]
async fn failed_network_monitor_is_recreated_on_the_next_setup() {
    let (connection, mut commands, inject) = mock(attach_answer).await;
    let session = connection.attach_to_target("page").await.unwrap();
    session.monitor_network(true).await.unwrap();
    let first = session.target.monitor.lock().await.clone().unwrap();
    assert!(!first.failed());
    // An existing healthy monitor is reused.
    session.monitor_network(true).await.unwrap();
    assert!(Arc::ptr_eq(
        &first,
        session.target.monitor.lock().await.as_ref().unwrap()
    ));
    while commands.try_recv().is_ok() {}
    inject
        .send(json!({"method":"Inspector.detached","sessionId":"session-page","params":{"reason":"test"}}))
        .unwrap();
    // The monitor attaches its own session to the same target.
    inject
        .send(json!({"method":"Inspector.targetCrashed","sessionId":"session-page","params":{}}))
        .unwrap();
    until(|| first.failed()).await;
    drop(first);
    session.monitor_network(true).await.unwrap();
    let second = session.target.monitor.lock().await.clone().unwrap();
    assert!(!second.failed());
    let mut methods = Vec::new();
    while let Ok(command) = commands.try_recv() {
        methods.push(command["method"].as_str().unwrap().to_owned());
    }
    assert!(
        methods.iter().any(|m| m == "Target.attachToTarget"),
        "{methods:?}"
    );
    connection.disconnect().await.unwrap();
}

async fn chrome() -> crate::Browser {
    crate::Browser::launch_with(|config| {
        config.chrome_path = std::env::var("EOKA_TEST_CHROME_PATH").ok();
        config.patch_binary = false;
        config.strip_x_client_data = false;
        config.cdp_timeout = 5;
    })
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn chrome_target_state_is_released_when_tabs_close_outside_eoka() {
    let browser = chrome().await;
    let page = browser.new_page("about:blank").await.unwrap();
    let session = page.session().clone();
    let registry = Arc::clone(&session.targets);
    let other = browser.new_page("about:blank").await.unwrap();
    let other_id = other.target_id().to_owned();
    assert!(registry.lock().unwrap().contains_key(&other_id));
    // Closed by the page itself would look the same: eoka never sees closeTarget.
    session
        .transport()
        .send::<_, Value>("Target.closeTarget", &json!({"targetId": other_id}))
        .await
        .unwrap();
    until(|| !registry.lock().unwrap().contains_key(&other_id)).await;
    assert!(registry.lock().unwrap().contains_key(page.target_id()));
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn chrome_object_groups_release_derived_property_objects() {
    let browser = chrome().await;
    let page = browser
        .new_page("data:text/html,<p>a</p><p>b</p>")
        .await
        .unwrap();
    let session = page.session().clone();
    let (result, group) = session
        .evaluate_for_remote_object("[...document.querySelectorAll('p')]")
        .await
        .unwrap();
    let array = result.result.object_id.unwrap();
    let element = session
        .get_properties(&array)
        .await
        .unwrap()
        .into_iter()
        .find(|property| property.name == "0")
        .and_then(|property| property.value)
        .and_then(|value| value.object_id)
        .unwrap();
    let call = |object: String| {
        let session = session.clone();
        async move {
            session
                .send::<_, Value>(
                    "Runtime.callFunctionOn",
                    &json!({"objectId":object,"functionDeclaration":"function(){return 1}","returnByValue":true}),
                )
                .await
        }
    };
    call(element.clone()).await.unwrap();
    drop(group);
    tokio::time::timeout(Duration::from_secs(2), async {
        while call(element.clone()).await.is_ok() || call(array.clone()).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("releasing the group must release the array and its property objects");
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn chrome_removed_out_of_process_frame_releases_its_target_state() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0; 4096];
                let n = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..n]);
                let body = if request.starts_with("GET /top") {
                    format!("<iframe id=f src='http://localhost:{port}/frame'></iframe>")
                } else {
                    "frame".to_owned()
                };
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            });
        }
    });
    let browser = chrome().await;
    let page = browser
        .new_page(&format!("http://127.0.0.1:{port}/top"))
        .await
        .unwrap();
    let session = page.session().clone();
    let frame = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let targets: Value = session
                .transport()
                .send("Target.getTargets", &json!({}))
                .await
                .unwrap();
            if let Some(id) = targets["targetInfos"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["type"] == "iframe" && t["parentId"] == page.target_id())
                .and_then(|t| t["targetId"].as_str())
            {
                break id.to_owned();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the cross-site frame must be out of process");
    session.attach_frame_target(&frame).await.unwrap();
    assert!(session.targets.lock().unwrap().contains_key(&frame));
    page.execute("document.getElementById('f').remove()")
        .await
        .unwrap();
    until(|| !session.targets.lock().unwrap().contains_key(&frame)).await;
    browser.close().await.unwrap();
}
