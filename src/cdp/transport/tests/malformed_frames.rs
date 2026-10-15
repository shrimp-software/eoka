use super::*;
use tracing_subscriber::{layer::SubscriberExt, Layer};

#[derive(Clone, Default)]
struct Diagnostics(Arc<Mutex<Vec<(tracing::Level, String)>>>);

impl<S: tracing::Subscriber> Layer<S> for Diagnostics {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, _: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write;
                write!(&mut self.0, "{value:?}").unwrap();
            }
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.0
            .try_lock()
            .expect("diagnostics are only read after the reader exits")
            .push((*event.metadata().level(), fields.0));
    }
}

#[tokio::test]
async fn unreadable_frames_fail_pending_commands_and_native_completions_without_payload_logs() {
    const SECRET: &str = "private-body-token-do-not-log";
    let diagnostics = Diagnostics::default();
    let subscriber = tracing_subscriber::registry().with(diagnostics.clone());
    // This current-thread runtime keeps the reader task under the same subscriber.
    let _guard = tracing::subscriber::set_default(subscriber);
    let frames = [
        Message::Text(format!(r#"{{"method":"Fetch.requestPaused","params":{{"requestId":"paused","request":{{"postData":"{SECRET}\q"}}}}}}"#).into()),
        Message::Text(format!(r#"{{"id":1,"result":{{"value":"{SECRET}\uD800"}}"#).into()),
        Message::Binary([SECRET.as_bytes(), &[0xff, 0xfe]].concat().into()),
    ];
    for frame in frames {
        let (transport, mut peer) = pending_test_peer().await;
        let transport = Arc::new(transport);
        let session: Arc<str> = "native-owner".into();
        let dispatch = CommandDispatch::new();
        let (written, completion) = transport
            .send_to_session_for_completion(
                &session,
                "Page.captureScreenshot",
                &json!({}),
                &dispatch,
            )
            .await
            .unwrap();
        written.unwrap();
        peer.next().await.unwrap().unwrap();
        let command = tokio::spawn({
            let transport = transport.clone();
            async move {
                transport
                    .send::<_, Value>("Browser.getVersion", &json!({}))
                    .await
            }
        });
        peer.next().await.unwrap().unwrap();
        let writer = transport.writer.lock().await;
        peer.send(frame).await.unwrap();

        let native_result = tokio::time::timeout(Duration::from_secs(1), completion.wait()).await;
        let ordinary_result = tokio::time::timeout(Duration::from_secs(1), command).await;
        let closed = transport.is_closed();
        assert_peer_terminated(&mut peer).await;
        drop(writer);
        // Explicit cleanup must not be what makes the peer observe termination.
        transport.close().await.unwrap();
        assert!(
            closed,
            "unreadable protocol frames must invalidate the transport"
        );
        assert!(
            native_result.is_ok(),
            "native completion hung after an unreadable frame"
        );
        assert!(
            ordinary_result.is_ok(),
            "ordinary command waited for its timeout"
        );
        for result in [native_result.unwrap(), ordinary_result.unwrap().unwrap()] {
            assert!(matches!(result, Err(Error::Transport { .. })), "{result:?}");
            assert!(!result.unwrap_err().to_string().contains(SECRET));
        }
        assert!(
            dispatch.may_have_been_sent(),
            "failure must not make replay safe"
        );
        assert!(lock_pending(&transport.pending).is_empty());
        assert!(lock_completions(&transport.completion_routes).is_empty());
    }
    let events = diagnostics.0.lock().await;
    assert!(events
        .iter()
        .any(|(level, _)| *level == tracing::Level::WARN));
    assert!(events.iter().all(|(_, fields)| !fields.contains(SECRET)));
}

async fn network_monitor(
    transport: &Arc<Transport>,
    peer: &mut WebSocketStream<TcpStream>,
) -> crate::cdp::network_idle::NetworkMonitor {
    let attach = crate::cdp::network_idle::NetworkMonitor::start(transport, "target", true);
    let replies = async {
        for _ in 0..5 {
            let message = peer.next().await.unwrap().unwrap();
            let command: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            let result = match command["method"].as_str().unwrap() {
                "Target.attachToTarget" => json!({"sessionId":"monitor"}),
                "Page.getFrameTree" => {
                    json!({"frameTree":{"frame":{"id":"main","loaderId":"loader"}}})
                }
                "Page.enable" | "Page.setLifecycleEventsEnabled" | "Network.enable" => json!({}),
                method => panic!("unexpected monitor setup command: {method}"),
            };
            peer.send(Message::Text(
                json!({"id":command["id"],"result":result})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        }
    };
    let (monitor, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(attach, replies)
    })
    .await
    .unwrap();
    monitor.unwrap()
}

async fn reader_barrier(
    peer: &mut WebSocketStream<TcpStream>,
    events: &mut broadcast::Receiver<CdpMessage>,
) {
    peer.send(Message::Text(
        r#"{"method":"Fixture.barrier","params":{}}"#.into(),
    ))
    .await
    .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event, CdpMessage::Event { method, .. } if method == "Fixture.barrier"));
}

#[tokio::test]
async fn malformed_network_metadata_cannot_report_false_idle_or_lose_request_completion() {
    let (transport, mut peer) = pending_test_peer().await;
    let transport = Arc::new(transport);
    let monitor = network_monitor(&transport, &mut peer).await;
    let mut events = transport.subscribe();
    let request = r#"{"method":"Network.requestWillBeSent","sessionId":"monitor","params":{"requestId":"binary","loaderId":"loader","frameId":"main","type":"Fetch","request":{"postData":"\uD800"}}}"#;
    peer.send(Message::Text(request.into())).await.unwrap();
    reader_barrier(&mut peer, &mut events).await;
    assert!(matches!(monitor.wait(0, 0).await, Err(Error::Timeout(_))));

    peer.send(Message::Text(r#"{"method":"Network.loadingFailed","sessionId":"monitor","params":{"requestId":"binary","errorText":"\uDC00"}}"#.into()))
        .await
        .unwrap();
    reader_barrier(&mut peer, &mut events).await;
    monitor.wait(0, 0).await.unwrap();

    // An unrepairable event must invalidate accounting, not be mistaken for a
    // quiet network or leave the active request waiting until the idle deadline.
    peer.send(Message::Text(request.into())).await.unwrap();
    reader_barrier(&mut peer, &mut events).await;
    peer.send(Message::Text("{".into())).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), monitor.wait(0, 30_000))
        .await
        .expect("network idle must be notified of protocol failure");
    assert!(matches!(result, Err(Error::Transport { .. })), "{result:?}");
    transport.close().await.unwrap();
}
