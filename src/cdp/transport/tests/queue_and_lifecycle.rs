use super::*;

async fn next_command(peer: &mut WebSocketStream<TcpStream>) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(2), peer.next())
        .await
        .expect("command must reach the peer")
        .unwrap()
        .unwrap();
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}

async fn filtered_peer(filter_cdp: bool) -> (Transport, WebSocketStream<TcpStream>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(socket).await.unwrap()
    });
    let stream = Transport::ws_connect(&url, Duration::from_secs(30))
        .await
        .unwrap();
    (
        Transport::build(None, stream, None, 30, filter_cdp, false).unwrap(),
        server.await.unwrap(),
    )
}

#[tokio::test]
async fn reader_keeps_delivering_events_and_replies_while_the_writer_is_stalled() {
    let (transport, mut peer) = test_peer_with_proxy_credentials().await;
    let mut events = transport.subscribe();
    let reply = {
        let params = json!({});
        let send = transport.send::<_, Value>("Fixture.before", &params);
        tokio::pin!(send);
        let command = tokio::select! {
            _ = &mut send => panic!("reply was withheld"),
            command = next_command(&mut peer) => command,
        };
        let _stalled = transport.writer.lock().await;
        // Each of these used to make the reader wait for the writer.
        peer.send(Message::Ping(vec![1].into())).await.unwrap();
        peer.send(Message::Text(
            json!({"method":"Fetch.authRequired","sessionId":"s","params":{"requestId":"r","authChallenge":{"source":"Proxy"}}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        peer.send(Message::Text(
            json!({"method":"Fixture.after","params":{}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        peer.send(Message::Text(
            json!({"id":command["id"],"result":{"ok":true}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        // Well inside the 2 s write deadline: the reader did not block.
        let event = tokio::time::timeout(Duration::from_millis(500), events.recv())
            .await
            .expect("events must be delivered while the writer is stalled")
            .unwrap();
        assert!(matches!(&*event, CdpMessage::Event { method, .. } if method == "Fixture.after"));
        tokio::time::timeout(Duration::from_millis(500), send)
            .await
            .expect("replies must be delivered while the writer is stalled")
    };
    assert_eq!(reply.unwrap()["ok"], true);
    assert!(!transport.is_closed());
    // Once the writer is released, the queued automatic replies go out in order.
    let auth = loop {
        let message = tokio::time::timeout(Duration::from_secs(2), peer.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(text) = message {
            break serde_json::from_str::<Value>(&text).unwrap();
        }
    };
    assert_eq!(auth["method"], "Fetch.continueWithAuth");
    let id = auth["id"].as_u64().unwrap();
    assert!((AUTO_ID_MIN..=AUTO_ID_MAX).contains(&id), "{id}");
    transport.close().await.unwrap();
}

#[tokio::test]
async fn commands_on_a_detached_session_fail_without_waiting_for_their_deadline() {
    for (detach, session) in [
        (
            json!({"method":"Target.detachedFromTarget","params":{"sessionId":"gone","targetId":"t"}}),
            "gone",
        ),
        (
            json!({"method":"Inspector.detached","sessionId":"gone","params":{"reason":"Render process gone."}}),
            "gone",
        ),
    ] {
        let (transport, mut peer) = pending_test_peer().await;
        let mut lifecycle = transport.watch_target_lifecycle();
        let params = json!({});
        let detached = transport.send_to_session::<_, Value>(session, "Fixture.detached", &params);
        let survivor = transport.send_to_session::<_, Value>("alive", "Fixture.alive", &params);
        tokio::pin!(detached, survivor);
        let mut sent = Vec::new();
        for _ in 0..2 {
            tokio::select! {
                _ = &mut detached => panic!("no reply yet"),
                _ = &mut survivor => panic!("no reply yet"),
                command = next_command(&mut peer) => sent.push(command),
            }
        }
        peer.send(Message::Text(detach.to_string().into()))
            .await
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(1), &mut detached)
            .await
            .expect("a detached session must fail its commands at once")
            .unwrap_err();
        // The same error Chrome returns for a command to a missing session.
        assert!(
            matches!(&error, Error::Cdp { method: Some(method), code: Some(-32001), message }
                if method == "Fixture.detached" && message == "Session with given id not found."),
            "{error:?}"
        );
        let alive = sent.iter().find(|c| c["sessionId"] == "alive").unwrap();
        peer.send(Message::Text(
            json!({"id":alive["id"],"result":{"ok":true}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        assert_eq!(survivor.await.unwrap()["ok"], true);
        if detach["method"] == "Target.detachedFromTarget" {
            assert_eq!(
                lifecycle.recv().await,
                Some(TargetLifecycle::Detached {
                    target_id: "t".into()
                })
            );
        }
        transport.close().await.unwrap();
    }
}

#[tokio::test]
async fn destroyed_targets_reach_the_lifecycle_channel_and_close_it_on_shutdown() {
    let (transport, mut peer) = pending_test_peer().await;
    let mut lifecycle = transport.watch_target_lifecycle();
    peer.send(Message::Text(
        json!({"method":"Target.targetDestroyed","params":{"targetId":"t"}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    assert_eq!(
        lifecycle.recv().await,
        Some(TargetLifecycle::Destroyed {
            target_id: "t".into()
        })
    );
    drop(peer);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), lifecycle.recv())
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn blocked_commands_fail_clearly_and_honour_filter_cdp() {
    let (transport, mut peer) = filtered_peer(true).await;
    let dispatch = CommandDispatch::new();
    for result in [
        transport
            .send::<_, Value>("Runtime.enable", &json!({}))
            .await,
        transport
            .send_to_session_with_dispatch::<_, Value>("s", "Console.enable", &json!({}), &dispatch)
            .await,
    ] {
        let error = result.unwrap_err();
        assert!(error.to_string().contains("filter_cdp"), "{error}");
    }
    assert!(!dispatch.may_have_been_sent());
    let completion = CommandDispatch::new();
    assert!(transport
        .send_to_session_for_completion(&"s".into(), "Debugger.enable", &json!({}), &completion)
        .await
        .is_err());
    assert!(tokio::time::timeout(Duration::from_millis(20), peer.next())
        .await
        .is_err());
    transport.close().await.unwrap();

    let (transport, mut peer) = filtered_peer(false).await;
    let params = json!({});
    let reply = async {
        let command = next_command(&mut peer).await;
        assert_eq!(command["method"], "Runtime.enable");
        peer.send(Message::Text(
            json!({"id":command["id"],"result":{}}).to_string().into(),
        ))
        .await
        .unwrap();
    };
    let (result, ()) = tokio::join!(transport.send::<_, Value>("Runtime.enable", &params), reply);
    result.unwrap();
    transport.close().await.unwrap();
}

#[tokio::test]
async fn cdp_errors_keep_their_data_and_timeouts_name_method_and_id() {
    let (mut transport, mut peer) = pending_test_peer().await;
    let params = json!({});
    let reply = async {
        let command = next_command(&mut peer).await;
        peer.send(Message::Text(
            json!({"id":command["id"],"error":{"code":-32602,"message":"Invalid parameters","data":"Failed to deserialize params.url"}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    };
    let (result, ()) = tokio::join!(transport.send::<_, Value>("Page.navigate", &params), reply);
    match result.unwrap_err() {
        Error::Cdp {
            method,
            code,
            message,
        } => {
            assert_eq!(method.as_deref(), Some("Page.navigate"));
            assert_eq!(code, Some(-32602));
            assert_eq!(
                message,
                "Invalid parameters: Failed to deserialize params.url"
            );
        }
        other => panic!("{other:?}"),
    }
    transport.cmd_timeout = Duration::from_millis(20);
    let error = transport
        .send::<_, Value>("Fixture.silent", &params)
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Transport { .. }), "{error:?}");
    let text = error.to_string();
    assert!(
        text.contains("Fixture.silent") && text.contains("id="),
        "{text}"
    );
    assert!(lock_pending(&transport.pending).is_empty());
}

#[tokio::test]
async fn command_ids_wrap_within_their_range_and_skip_pending_ids() {
    assert_eq!(wrapping_next(1, 1, 3), 2);
    assert_eq!(wrapping_next(3, 1, 3), 1);
    assert_eq!(wrapping_next(0, 1, 3), 1);
    assert_eq!(
        wrapping_next(AUTO_ID_MAX, AUTO_ID_MIN, AUTO_ID_MAX),
        AUTO_ID_MIN
    );
    const { assert!(COMMAND_ID_MAX < AUTO_ID_MIN) };
    const { assert!(AUTO_ID_MAX <= i32::MAX as u64) };

    let (transport, _peer) = pending_test_peer().await;
    transport.next_id.store(COMMAND_ID_MAX, Ordering::Relaxed);
    let (_, last) = transport
        .prepare_command(None, "Fixture.last", &json!({}), false)
        .unwrap();
    assert_eq!(last.registration.id, COMMAND_ID_MAX);
    // COMMAND_ID_MIN is now free again, but keep it pending to force a skip.
    let (sender, _receiver) = oneshot::channel();
    lock_pending(&transport.pending).insert(
        COMMAND_ID_MIN,
        PendingRequest {
            sender,
            session: None,
        },
    );
    let (_, wrapped) = transport
        .prepare_command(None, "Fixture.wrapped", &json!({}), false)
        .unwrap();
    assert_eq!(wrapped.registration.id, COMMAND_ID_MIN + 1);
    transport.close().await.unwrap();
}

#[tokio::test]
async fn sustained_writer_stall_closes_the_transport_with_its_reason() {
    let (transport, mut peer) = test_peer_with_proxy_credentials().await;
    let session: Arc<str> = "stalled".into();
    let dispatch = CommandDispatch::new();
    let (written, pending) = transport
        .send_to_session_for_completion(&session, "Fixture.pending", &json!({}), &dispatch)
        .await
        .unwrap();
    written.unwrap();
    next_command(&mut peer).await;
    let stalled = transport.writer.lock().await;
    peer.send(Message::Ping(vec![1].into())).await.unwrap();
    let error = tokio::time::timeout(
        transport.cmd_timeout + Duration::from_secs(1),
        pending.wait(),
    )
    .await
    .expect("a stalled writer must fail pending commands")
    .unwrap_err();
    assert!(error.to_string().contains("stalled"), "{error}");
    assert!(transport.is_closed());
    drop(stalled);
    transport.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn close_reaps_the_owned_child_on_a_blocking_thread() {
    let (transport, _peer) = pending_test_peer().await;
    let child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    *transport.child.lock().unwrap() = Some(child);
    let runtime = RuntimeDirectory::create(None).unwrap();
    let path = runtime.path().to_owned();
    *transport.runtime.lock().unwrap() = Some(runtime);
    tokio::time::timeout(Duration::from_secs(5), transport.close())
        .await
        .unwrap()
        .unwrap();
    assert!(transport.child.lock().unwrap().is_none());
    assert!(transport.runtime.lock().unwrap().is_none());
    assert!(!path.exists());
    assert!(transport.wait_for_owned_exit(Duration::ZERO).await.unwrap());
}
