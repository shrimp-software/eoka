use super::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::{tungstenite::Message, WebSocketStream};

async fn command(peer: &mut WebSocketStream<TcpStream>, method: &str) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(2), peer.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let command: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
    assert_eq!(command["method"], method);
    command
}

async fn reply(peer: &mut WebSocketStream<TcpStream>, command: &Value, result: Value) {
    peer.send(Message::Text(
        json!({"id":command["id"],"result":result})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
}

async fn reject(peer: &mut WebSocketStream<TcpStream>, command: &Value, code: i64, message: &str) {
    peer.send(Message::Text(
        json!({"id":command["id"],"error":{"code":code,"message":message}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
}

type Fixture = (
    Arc<crate::cdp::Connection>,
    Page,
    WebSocketStream<TcpStream>,
);

async fn fixture() -> Fixture {
    shared_fixtures(1).await.pop().unwrap()
}

async fn shared_fixtures(count: usize) -> Vec<Fixture> {
    let listener = Arc::new(tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap());
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let mut fixtures = Vec::new();
    for index in 0..count {
        let listener = Arc::clone(&listener);
        let accepted = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(socket).await.unwrap()
        });
        let connection = Arc::new(crate::cdp::Connection::new(
            Transport::connect_with_options(&url, 1, false, false)
                .await
                .unwrap(),
        ));
        let mut peer = accepted.await.unwrap();
        let connecting = Arc::clone(&connection);
        let attached =
            tokio::spawn(async move { connecting.attach_to_target("recorded-target").await });
        let request = command(&mut peer, "Target.attachToTarget").await;
        let session_id = if index == 0 {
            "recording-session"
        } else {
            "other-session"
        };
        reply(&mut peer, &request, json!({"sessionId":session_id})).await;
        let page = Page::new(
            attached.await.unwrap().unwrap(),
            Arc::new(crate::StealthConfig::default()),
        );
        fixtures.push((connection, page, peer));
    }
    fixtures
}

async fn start(page: &Page, peer: &mut WebSocketStream<TcpStream>) -> NativeRecording {
    let page = page.clone();
    let task = tokio::spawn(async move {
        page.start_recording(NativeRecordingOptions {
            fps: Some(60.0),
            max_width: Some(1280),
            max_height: Some(720),
        })
        .await
    });
    let ready = command(peer, "Runtime.evaluate").await;
    reply(
        peer,
        &ready,
        json!({"result":{"type":"object","value":{"ready":true,"top":true}}}),
    )
    .await;
    let request = command(peer, "Page.startScreenRecording").await;
    assert_eq!(
        request["params"],
        json!({"audio":false,"maxWidth":1280,"maxHeight":720,"frameRate":60})
    );
    reply(peer, &request, json!({"stream":"start-stream"})).await;
    task.await.unwrap().unwrap()
}

#[test]
fn native_integer_options_and_chunk_boundaries() {
    for fps in [0.0, -1.0, 0.5, 30.5, 121.0, f64::INFINITY, f64::NAN] {
        assert!(matches!(
            NativeRecordingOptions {
                fps: Some(fps),
                ..Default::default()
            }
            .params(),
            Err(RecordingError::InvalidOptions(_))
        ));
    }
    for dimension in [0, i32::MAX as u32 + 1] {
        assert!(NativeRecordingOptions {
            max_width: Some(dimension),
            ..Default::default()
        }
        .params()
        .is_err());
        assert!(NativeRecordingOptions {
            max_height: Some(dimension),
            ..Default::default()
        }
        .params()
        .is_err());
    }
    for size in [0, CHUNK_BYTES - 1, CHUNK_BYTES] {
        let data = vec![0xa5; size];
        let chunk = decode_chunk(IoReadResult {
            data: base64::engine::general_purpose::STANDARD.encode(&data),
            base64_encoded: true,
            eof: true,
        })
        .unwrap();
        assert_eq!(chunk, RecordingChunk { data, eof: true });
    }
    for encoded in [false, true] {
        let data = "x".repeat(CHUNK_BYTES + 1);
        let data = if encoded {
            base64::engine::general_purpose::STANDARD.encode(data)
        } else {
            data
        };
        assert!(decode_chunk(IoReadResult {
            data,
            base64_encoded: encoded,
            eof: false
        })
        .is_err());
    }
    assert!(decode_chunk(IoReadResult {
        data: "not-base64".into(),
        base64_encoded: true,
        eof: false
    })
    .is_err());
}

#[tokio::test]
async fn stop_once_read_bounded_eof_and_close_both_native_handles() {
    let (connection, page, mut peer) = fixture().await;
    let recording = Arc::new(start(&page, &mut peer).await);
    assert!(matches!(
        page.start_recording(Default::default()).await,
        Err(RecordingError::AlreadyActive)
    ));
    assert!(matches!(
        recording.read_chunk().await,
        Err(RecordingError::InvalidState(_))
    ));
    let stopping = Arc::clone(&recording);
    let task = tokio::spawn(async move { stopping.stop().await });
    let request = command(&mut peer, "Page.stopScreenRecording").await;
    reply(&mut peer, &request, json!({"stream":"stop-stream"})).await;
    task.await.unwrap().unwrap();
    recording.stop().await.unwrap();
    let reading = Arc::clone(&recording);
    let task = tokio::spawn(async move { reading.read_chunk().await });
    let request = command(&mut peer, "IO.read").await;
    assert_eq!(
        request["params"],
        json!({"handle":"stop-stream","size":65536})
    );
    reply(
        &mut peer,
        &request,
        json!({"data":"AAEC/w==","base64Encoded":true,"eof":true}),
    )
    .await;
    assert_eq!(
        task.await.unwrap().unwrap(),
        RecordingChunk {
            data: vec![0, 1, 2, 255],
            eof: true
        }
    );
    assert_eq!(
        recording.read_chunk().await.unwrap(),
        RecordingChunk {
            data: Vec::new(),
            eof: true
        }
    );
    let closing = Arc::clone(&recording);
    let task = tokio::spawn(async move { closing.close().await });
    for stream in ["start-stream", "stop-stream"] {
        let request = command(&mut peer, "IO.close").await;
        assert_eq!(request["params"]["handle"], stream);
        reply(&mut peer, &request, json!({})).await;
    }
    task.await.unwrap().unwrap();
    recording.close().await.unwrap();
    let replacement = Arc::new(start(&page, &mut peer).await);
    // Old handles must not accidentally close the target's replacement recording.
    recording.close().await.unwrap();
    drop(recording);
    let cancelling = Arc::clone(&replacement);
    let task = tokio::spawn(async move { cancelling.cancel().await });
    let request = command(&mut peer, "Page.stopScreenRecording").await;
    reply(&mut peer, &request, json!({"stream":"start-stream"})).await;
    let request = command(&mut peer, "IO.close").await;
    reply(&mut peer, &request, json!({})).await;
    task.await.unwrap().unwrap();
    connection.disconnect().await.unwrap();
}

#[tokio::test]
async fn cancelled_start_retains_target_until_owned_stop_and_close() {
    let (connection, page, mut peer) = fixture().await;
    let starting = page.clone();
    let task = tokio::spawn(async move { starting.start_recording(Default::default()).await });
    let ready = command(&mut peer, "Runtime.evaluate").await;
    reply(
        &mut peer,
        &ready,
        json!({"result":{"value":{"ready":true,"top":true}}}),
    )
    .await;
    let request = command(&mut peer, "Page.startScreenRecording").await;
    task.abort();
    let _ = task.await;
    assert!(matches!(
        page.start_recording(Default::default()).await,
        Err(RecordingError::AlreadyActive)
    ));
    reply(&mut peer, &request, json!({"stream":"abandoned-stream"})).await;
    let request = command(&mut peer, "Page.stopScreenRecording").await;
    reply(&mut peer, &request, json!({"stream":"abandoned-stream"})).await;
    let request = command(&mut peer, "IO.close").await;
    reply(&mut peer, &request, json!({})).await;
    // Await bounded teardown to avoid racing its target release acknowledgement.
    connection.cancel_recordings().await.unwrap();
    connection.disconnect().await.unwrap();
}

#[tokio::test]
async fn unsupported_and_not_ready_fail_without_fallback_or_stop() {
    let (connection, page, mut peer) = fixture().await;
    for ready in [false, true] {
        let starting = page.clone();
        let task = tokio::spawn(async move { starting.start_recording(Default::default()).await });
        let request = command(&mut peer, "Runtime.evaluate").await;
        reply(
            &mut peer,
            &request,
            json!({"result":{"value":{"ready":ready,"top":true}}}),
        )
        .await;
        if ready {
            let request = command(&mut peer, "Page.startScreenRecording").await;
            reject(
                &mut peer,
                &request,
                -32601,
                "Page.startScreenRecording wasn't found",
            )
            .await;
            assert!(matches!(
                task.await.unwrap(),
                Err(RecordingError::Unsupported(_))
            ));
        } else {
            assert!(matches!(
                task.await.unwrap(),
                Err(RecordingError::Precondition(_))
            ));
        }
    }
    connection.disconnect().await.unwrap();
}

#[tokio::test]
async fn unacknowledged_stop_is_never_repeated_and_retains_reservation() {
    let (connection, page, mut peer) = fixture().await;
    let recording = Arc::new(start(&page, &mut peer).await);
    let stopping = Arc::clone(&recording);
    let task = tokio::spawn(async move { stopping.stop().await });
    let request = command(&mut peer, "Page.stopScreenRecording").await;
    reject(&mut peer, &request, -32000, "native finalization failed").await;
    assert!(matches!(
        task.await.unwrap(),
        Err(RecordingError::CleanupUncertain(_))
    ));
    assert!(matches!(
        recording.stop().await,
        Err(RecordingError::CleanupUncertain(_))
    ));
    let closing = Arc::clone(&recording);
    let task = tokio::spawn(async move { closing.cancel().await });
    let request = command(&mut peer, "IO.close").await;
    reply(&mut peer, &request, json!({})).await;
    assert!(matches!(
        task.await.unwrap(),
        Err(RecordingError::CleanupUncertain(_))
    ));
    assert!(matches!(
        page.start_recording(Default::default()).await,
        Err(RecordingError::AlreadyActive)
    ));
    assert!(connection.disconnect().await.is_err());
    assert!(connection.transport().is_closed());
}

#[tokio::test]
async fn borrowed_disconnect_stops_owned_recording_without_browser_close() {
    let (connection, page, mut peer) = fixture().await;
    let recording = start(&page, &mut peer).await;
    let closing = Arc::clone(&connection);
    let task = tokio::spawn(async move { closing.disconnect().await });
    let request = command(&mut peer, "Page.stopScreenRecording").await;
    reply(&mut peer, &request, json!({"stream":"start-stream"})).await;
    let request = command(&mut peer, "IO.close").await;
    reply(&mut peer, &request, json!({})).await;
    task.await.unwrap().unwrap();
    assert!(connection.transport().is_closed());
    assert!(matches!(
        recording.stop().await,
        Err(RecordingError::Core(_))
    ));
}

#[tokio::test]
async fn failed_stream_close_retains_target_and_retry_closes_without_double_stop() {
    let (connection, page, mut peer) = fixture().await;
    let recording = Arc::new(start(&page, &mut peer).await);
    let closing = Arc::clone(&recording);
    let task = tokio::spawn(async move { closing.cancel().await });
    let request = command(&mut peer, "Page.stopScreenRecording").await;
    reply(&mut peer, &request, json!({"stream":"start-stream"})).await;
    let request = command(&mut peer, "IO.close").await;
    reject(&mut peer, &request, -32000, "stream cleanup failed").await;
    assert!(matches!(
        task.await.unwrap(),
        Err(RecordingError::CleanupUncertain(_))
    ));
    assert!(matches!(
        page.start_recording(Default::default()).await,
        Err(RecordingError::AlreadyActive)
    ));
    let closing = Arc::clone(&recording);
    let task = tokio::spawn(async move { closing.close().await });
    let request = command(&mut peer, "IO.close").await;
    reply(&mut peer, &request, json!({})).await;
    task.await.unwrap().unwrap();
    connection.disconnect().await.unwrap();
}

#[tokio::test]
async fn partial_stream_cleanup_retries_only_the_unacknowledged_handle() {
    for failed_stream in ["start-stream", "stop-stream"] {
        let (connection, page, mut peer) = fixture().await;
        let recording = Arc::new(start(&page, &mut peer).await);
        let closing = Arc::clone(&recording);
        let task = tokio::spawn(async move { closing.cancel().await });
        let request = command(&mut peer, "Page.stopScreenRecording").await;
        reply(&mut peer, &request, json!({"stream":"stop-stream"})).await;
        for stream in ["start-stream", "stop-stream"] {
            let request = command(&mut peer, "IO.close").await;
            assert_eq!(request["params"]["handle"], stream);
            if stream == failed_stream {
                reject(&mut peer, &request, -32000, "stream cleanup failed").await;
            } else {
                reply(&mut peer, &request, json!({})).await;
            }
        }
        assert!(matches!(
            task.await.unwrap(),
            Err(RecordingError::CleanupUncertain(_))
        ));
        assert!(matches!(
            page.start_recording(Default::default()).await,
            Err(RecordingError::AlreadyActive)
        ));
        let closing = Arc::clone(&recording);
        let task = tokio::spawn(async move { closing.close().await });
        let request = command(&mut peer, "IO.close").await;
        assert_eq!(request["params"]["handle"], failed_stream);
        reply(&mut peer, &request, json!({})).await;
        task.await.unwrap().unwrap();
        assert!(recording.state.finished.load(Ordering::Acquire));
        recording.close().await.unwrap();
        connection.disconnect().await.unwrap();
    }
}

#[tokio::test]
async fn read_error_closes_storage_and_never_reissues_stop() {
    let (connection, page, mut peer) = fixture().await;
    let recording = Arc::new(start(&page, &mut peer).await);
    let stopping = Arc::clone(&recording);
    let task = tokio::spawn(async move { stopping.stop().await });
    let request = command(&mut peer, "Page.stopScreenRecording").await;
    reply(&mut peer, &request, json!({"stream":"start-stream"})).await;
    task.await.unwrap().unwrap();
    let reading = Arc::clone(&recording);
    let task = tokio::spawn(async move { reading.read_chunk().await });
    let request = command(&mut peer, "IO.read").await;
    reject(&mut peer, &request, -32000, "backing storage read failed").await;
    assert!(matches!(task.await.unwrap(), Err(RecordingError::Core(_))));
    let request = command(&mut peer, "IO.close").await;
    reply(&mut peer, &request, json!({})).await;
    recording.close().await.unwrap();
    connection.disconnect().await.unwrap();
}

#[tokio::test]
async fn target_loss_health_signal_does_not_wait_for_native_command_completion() {
    let (connection, page, mut peer) = fixture().await;
    let recording = Arc::new(start(&page, &mut peer).await);
    let stopping = Arc::clone(&recording);
    let task = tokio::spawn(async move { stopping.stop().await });
    let request = command(&mut peer, "Page.stopScreenRecording").await;
    peer.send(Message::Text(
        json!({
            "method":"Inspector.targetCrashed", "sessionId":"recording-session", "params":{}
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(500), recording.wait_for_failure())
            .await
            .unwrap(),
        RecordingError::Precondition(_)
    ));
    reply(&mut peer, &request, json!({"stream":"start-stream"})).await;
    assert!(matches!(
        task.await.unwrap(),
        Err(RecordingError::Precondition(_))
    ));
    let closing = Arc::clone(&recording);
    let task = tokio::spawn(async move { closing.close().await });
    let request = command(&mut peer, "IO.close").await;
    reply(&mut peer, &request, json!({})).await;
    task.await.unwrap().unwrap();
    connection.disconnect().await.unwrap();
}

#[tokio::test]
async fn explicit_target_close_preserves_cleanup_error_but_still_closes_only_the_target() {
    let (connection, page, mut peer) = fixture().await;
    let recording = start(&page, &mut peer).await;
    let closing = Arc::clone(&connection);
    let task = tokio::spawn(async move { closing.close_target("recorded-target").await });
    let request = command(&mut peer, "Page.stopScreenRecording").await;
    reject(&mut peer, &request, -32000, "native finalization failed").await;
    let request = command(&mut peer, "IO.close").await;
    reply(&mut peer, &request, json!({})).await;
    let request = command(&mut peer, "Target.closeTarget").await;
    assert_eq!(request["params"]["targetId"], "recorded-target");
    reply(&mut peer, &request, json!({"success":true})).await;
    let error = task.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("native finalization failed"));
    assert!(!connection.transport().is_closed());
    drop(recording);
    connection.disconnect().await.unwrap();
}

#[tokio::test]
async fn cancelled_enqueued_read_cleans_up_instead_of_silently_losing_final_bytes() {
    let (connection, page, mut peer) = fixture().await;
    let recording = Arc::new(start(&page, &mut peer).await);
    let stopping = Arc::clone(&recording);
    let task = tokio::spawn(async move { stopping.stop().await });
    let request = command(&mut peer, "Page.stopScreenRecording").await;
    reply(&mut peer, &request, json!({"stream":"start-stream"})).await;
    task.await.unwrap().unwrap();

    let mut read = Box::pin(recording.read_chunk());
    assert!(
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(Future::poll(read.as_mut(), cx).is_pending())
        })
        .await
    );
    let request = command(&mut peer, "IO.read").await;
    reply(
        &mut peer,
        &request,
        json!({"data":"final bytes","eof":true}),
    )
    .await;
    // Wait until the worker has enqueued its result, without polling the caller's receiver.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if recording.state.lifecycle.lock().await.phase == Phase::Eof {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        recording.read_chunk().await,
        Err(RecordingError::InvalidState(message)) if message.contains("awaiting delivery")
    ));
    assert!(!recording.state.cleanup_requested.load(Ordering::Acquire));
    drop(read);
    assert!(recording.state.cleanup_requested.load(Ordering::Acquire));
    let request = command(&mut peer, "IO.close").await;
    reply(&mut peer, &request, json!({})).await;
    recording.close().await.unwrap();
    assert!(matches!(
        recording.read_chunk().await,
        Err(RecordingError::InvalidState(_))
    ));
    connection.disconnect().await.unwrap();
}

#[tokio::test]
async fn another_connection_shares_admission_but_cannot_cancel_the_recording_on_disconnect() {
    let mut fixtures = shared_fixtures(2).await;
    let (other, other_page, mut other_peer) = fixtures.pop().unwrap();
    let (owner, owner_page, mut owner_peer) = fixtures.pop().unwrap();
    let recording = Arc::new(start(&owner_page, &mut owner_peer).await);
    assert!(matches!(
        other_page.start_recording(Default::default()).await,
        Err(RecordingError::AlreadyActive)
    ));

    let session = other_page.session.clone();
    let detach = tokio::spawn(async move { session.detach().await });
    let request = command(&mut other_peer, "Target.detachFromTarget").await;
    assert_eq!(request["params"]["sessionId"], "other-session");
    reply(&mut other_peer, &request, json!({})).await;
    detach.await.unwrap().unwrap();
    other.disconnect().await.unwrap();
    assert!(!owner.transport().is_closed());
    assert!(!recording.state.cleanup_requested.load(Ordering::Acquire));

    let closing = Arc::clone(&recording);
    let task = tokio::spawn(async move { closing.cancel().await });
    let request = command(&mut owner_peer, "Page.stopScreenRecording").await;
    assert_eq!(request["sessionId"], "recording-session");
    reply(&mut owner_peer, &request, json!({"stream":"start-stream"})).await;
    let request = command(&mut owner_peer, "IO.close").await;
    reply(&mut owner_peer, &request, json!({})).await;
    task.await.unwrap().unwrap();
    owner.disconnect().await.unwrap();
}

#[tokio::test]
async fn cancelled_disconnect_waiter_still_admits_cleanup_and_closes_transport() {
    let (connection, page, mut peer) = fixture().await;
    let recording = start(&page, &mut peer).await;
    let mut disconnect = Box::pin(connection.disconnect());
    // Poll only once, then cancel before the owned teardown/JoinSet tasks can run.
    assert!(
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(Future::poll(disconnect.as_mut(), cx).is_pending())
        })
        .await
    );
    drop(disconnect);
    let request = command(&mut peer, "Page.stopScreenRecording").await;
    reply(&mut peer, &request, json!({"stream":"start-stream"})).await;
    let request = command(&mut peer, "IO.close").await;
    reply(&mut peer, &request, json!({})).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while !connection.transport().is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        recording.stop().await,
        Err(RecordingError::Core(_))
    ));
}
