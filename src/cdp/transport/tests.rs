use super::*;

async fn pending_test_peer() -> (Transport, WebSocketStream<TcpStream>) {
    test_peer_with_header_policy(false).await
}

#[tokio::test]
async fn proxy_credentials_only_answer_explicit_proxy_challenges() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(socket).await.unwrap()
    });
    let stream = Transport::ws_connect(&url).await.unwrap();
    let transport = Transport::build(
        None,
        stream,
        Some(("fixture-user".into(), "fixture-password".into())),
        2,
        false,
        false,
    );
    let mut peer = server.await.unwrap();
    for source in [Some("Server"), None, Some("Unknown"), Some("Proxy")] {
        let mut challenge =
            json!({"origin":"http://fixture.invalid", "scheme":"basic", "realm":"fixture"});
        if let Some(source) = source {
            challenge["source"] = json!(source);
        }
        peer.send(Message::Text(
            json!({
                "method":"Fetch.authRequired", "sessionId":"challenged-session",
                "params":{"requestId":"challenged-request", "authChallenge":challenge}
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(2), peer.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let reply: Value = serde_json::from_str(reply.to_text().unwrap()).unwrap();
        assert_eq!(reply["sessionId"], "challenged-session");
        assert_eq!(reply["method"], "Fetch.continueWithAuth");
        assert_eq!(reply["params"]["requestId"], "challenged-request");
        let expected = if source == Some("Proxy") {
            json!({"response":"ProvideCredentials", "username":"fixture-user", "password":"fixture-password"})
        } else {
            json!({"response":"CancelAuth"})
        };
        assert_eq!(reply["params"]["authChallengeResponse"], expected);
    }
    transport.close().await.unwrap();
}

#[tokio::test]
async fn auth_without_proxy_credentials_is_left_to_the_caller() {
    let (transport, mut peer) = pending_test_peer().await;
    let mut events = transport.subscribe();
    peer.send(Message::Text(
        json!({
            "method":"Fetch.authRequired", "sessionId":"caller",
            "params":{"requestId":"request", "authChallenge":{"source":"Server"}}
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event, CdpMessage::Event { method, .. } if method == "Fetch.authRequired"));
    transport.close().await.unwrap();
}

async fn test_peer_with_header_policy(strip: bool) -> (Transport, WebSocketStream<TcpStream>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(socket).await.unwrap()
    });
    let stream = Transport::ws_connect(&url).await.unwrap();
    (
        Transport::build(None, stream, None, 30, false, strip),
        server.await.unwrap(),
    )
}

#[tokio::test]
async fn owned_request_pauses_resolve_once_and_fallback_preserves_header_policy() {
    async fn pause(peer: &mut WebSocketStream<TcpStream>, id: &str) {
        peer.send(Message::Text(json!({
            "method":"Fetch.requestPaused", "sessionId":"owned",
            "params":{"requestId":id,"request":{"headers":{"X-Client-Data":"remove","Other":"keep"}}},
        }).to_string().into())).await.unwrap();
    }
    async fn continuation(peer: &mut WebSocketStream<TcpStream>, id: &str) -> Value {
        let message = tokio::time::timeout(Duration::from_secs(2), peer.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let command: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        assert_eq!(command["method"], "Fetch.continueRequest");
        assert_eq!(command["sessionId"], "owned");
        assert_eq!(
            command["params"],
            json!({"requestId":id,"headers":[{"name":"Other","value":"keep"}]})
        );
        command
    }
    let (transport, mut peer) = test_peer_with_header_policy(true).await;
    let (sender, mut receiver) = mpsc::channel(1);
    let dropped = Arc::new(AtomicU64::new(0));
    transport
        .install_request_route("owned", sender.clone(), dropped.clone())
        .unwrap();
    assert!(transport
        .install_request_route("owned", sender, dropped.clone())
        .is_err());
    pause(&mut peer, "queued").await;
    pause(&mut peer, "overflow").await;
    continuation(&mut peer, "overflow").await;
    assert_eq!(dropped.load(Ordering::Relaxed), 1);
    let delivered = receiver.recv().await.unwrap();
    assert_eq!(delivered.params["requestId"], "queued");
    transport.remove_request_route("owned");
    let dispatch = CommandDispatch::new();
    let reply = async {
        let command = continuation(&mut peer, "queued").await;
        peer.send(Message::Text(
            json!({"id":command["id"],"result":{}}).to_string().into(),
        ))
        .await
        .unwrap();
    };
    let (result, ()) = tokio::join!(
        delivered.continue_request_with_dispatch(&transport, &dispatch),
        reply
    );
    result.unwrap();
    assert!(dispatch.may_have_been_sent());
    pause(&mut peer, "unowned").await;
    continuation(&mut peer, "unowned").await;
    assert_eq!(dropped.load(Ordering::Relaxed), 1);
    let (sender, receiver) = mpsc::channel(1);
    drop(receiver);
    transport
        .install_request_route("owned", sender, dropped.clone())
        .unwrap();
    pause(&mut peer, "closed").await;
    continuation(&mut peer, "closed").await;
    assert_eq!(dropped.load(Ordering::Relaxed), 2);
    assert!(tokio::time::timeout(Duration::from_millis(10), peer.next())
        .await
        .is_err());
}

#[tokio::test]
async fn pending_registration_cleans_cancelled_writer_and_reply_waits() {
    let (transport, mut peer) = pending_test_peer().await;
    let writer = transport.writer.lock().await;
    let (unrelated, _receiver) = oneshot::channel();
    lock_pending(&transport.pending).insert(9999, unrelated);
    for _ in 0..8 {
        assert!(tokio::time::timeout(
            Duration::from_millis(10),
            transport.send::<_, Value>("Fixture.wait", &json!({}))
        )
        .await
        .is_err());
        assert_eq!(lock_pending(&transport.pending).len(), 1);
        assert!(lock_pending(&transport.pending).contains_key(&9999));
    }
    drop(writer);
    for _ in 0..8 {
        assert!(tokio::time::timeout(
            Duration::from_millis(10),
            transport.send::<_, Value>("Fixture.wait", &json!({}))
        )
        .await
        .is_err());
        assert_eq!(lock_pending(&transport.pending).len(), 1);
        assert!(peer.next().await.unwrap().is_ok());
    }
    assert!(!transport.reader_handle.as_ref().unwrap().is_finished());
    let reply = async {
        let message = peer.next().await.unwrap().unwrap();
        let command: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        peer.send(Message::Text(
            json!({"id":command["id"], "result":{"ok":true}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    };
    let params = json!({});
    let (response, ()) = tokio::join!(transport.send::<_, Value>("Fixture.normal", &params), reply);
    assert_eq!(response.unwrap()["ok"], true);
    assert_eq!(lock_pending(&transport.pending).len(), 1);
}

#[tokio::test]
async fn pending_registration_cleans_internal_timeout_and_write_failure() {
    let (mut transport, _peer) = pending_test_peer().await;
    transport.cmd_timeout = Duration::from_millis(10);
    assert!(transport
        .send::<_, Value>("Fixture.wait", &json!({}))
        .await
        .is_err());
    assert!(lock_pending(&transport.pending).is_empty());
    transport.writer.lock().await.close().await.unwrap();
    assert!(transport
        .send::<_, Value>("Fixture.write", &json!({}))
        .await
        .is_err());
    assert!(lock_pending(&transport.pending).is_empty());
}

#[tokio::test]
async fn command_dispatch_is_one_shot_and_distinguishes_cancelled_writer_wait() {
    let (transport, mut peer) = pending_test_peer().await;
    let writer = transport.writer.lock().await;
    let dispatch = CommandDispatch::new();
    let params = json!({"requestId":"pause"});
    {
        let send = transport.send_to_session_with_dispatch::<_, Value>(
            "session",
            "Fetch.fulfillRequest",
            &params,
            &dispatch,
        );
        tokio::pin!(send);
        assert!(tokio::time::timeout(Duration::from_millis(10), &mut send)
            .await
            .is_err());
        assert!(!dispatch.may_have_been_sent());
        let error = transport
            .send_to_session_with_dispatch::<_, Value>(
                "session",
                "Fetch.continueRequest",
                &params,
                &dispatch,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("already used"));
    }
    assert!(lock_pending(&transport.pending).is_empty());
    assert!(!dispatch.may_have_been_sent());
    drop(writer);
    let fallback = CommandDispatch::new();
    let response = async {
        let text = peer.next().await.unwrap().unwrap();
        let command: Value = serde_json::from_str(text.to_text().unwrap()).unwrap();
        assert_eq!(command["method"], "Fetch.continueRequest");
        peer.send(Message::Text(
            json!({"id":command["id"],"result":{}}).to_string().into(),
        ))
        .await
        .unwrap();
    };
    let (result, ()) = tokio::join!(
        transport.send_to_session_with_dispatch::<_, Value>(
            "session",
            "Fetch.continueRequest",
            &params,
            &fallback
        ),
        response
    );
    result.unwrap();
    assert!(fallback.may_have_been_sent());
    assert!(transport
        .send_to_session_with_dispatch::<_, Value>(
            "session",
            "Fetch.fulfillRequest",
            &params,
            &fallback
        )
        .await
        .is_err());
    assert!(tokio::time::timeout(Duration::from_millis(10), peer.next())
        .await
        .is_err());
}

#[tokio::test]
async fn command_dispatch_stays_ambiguous_after_lost_reply_error_and_timeout() {
    let (mut transport, mut peer) = pending_test_peer().await;
    let params = json!({"requestId":"pause"});
    let cancelled = CommandDispatch::new();
    {
        let send = transport.send_to_session_with_dispatch::<_, Value>(
            "session",
            "Fetch.fulfillRequest",
            &params,
            &cancelled,
        );
        tokio::pin!(send);
        tokio::select! {
            _ = &mut send => panic!("reply was withheld"),
            message = peer.next() => { assert!(message.unwrap().is_ok()); }
        }
    }
    assert!(cancelled.may_have_been_sent());
    assert!(lock_pending(&transport.pending).is_empty());
    transport.cmd_timeout = Duration::from_millis(10);
    let timed_out = CommandDispatch::new();
    assert!(transport
        .send_to_session_with_dispatch::<_, Value>(
            "session",
            "Fetch.fulfillRequest",
            &params,
            &timed_out
        )
        .await
        .is_err());
    assert!(timed_out.may_have_been_sent());
    peer.next().await.unwrap().unwrap();
    let failed = CommandDispatch::new();
    let response = async {
        let message = peer.next().await.unwrap().unwrap();
        let command: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        peer.send(Message::Text(
            json!({"id":command["id"],"error":{"code":-32000,"message":"unknown result"}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    };
    let (result, ()) = tokio::join!(
        transport.send_to_session_with_dispatch::<_, Value>(
            "session",
            "Fetch.fulfillRequest",
            &params,
            &failed
        ),
        response
    );
    assert!(result.is_err());
    assert!(failed.may_have_been_sent());
    assert!(lock_pending(&transport.pending).is_empty());
    transport.writer.lock().await.close().await.unwrap();
    let write_failure = CommandDispatch::new();
    assert!(transport
        .send_to_session_with_dispatch::<_, Value>(
            "session",
            "Fetch.continueRequest",
            &params,
            &write_failure
        )
        .await
        .is_err());
    assert!(lock_pending(&transport.pending).is_empty());
}

#[tokio::test]
async fn completion_receipt_preserves_late_reply_after_caller_deadline() {
    let (transport, mut peer) = pending_test_peer().await;
    let session: Arc<str> = "capture-session".into();
    let dispatch = CommandDispatch::new();
    let (written, response) = transport
        .send_to_session_for_completion(&session, "Page.captureScreenshot", &json!({}), &dispatch)
        .await
        .unwrap();
    written.unwrap();
    assert!(dispatch.may_have_been_sent());
    let command = peer.next().await.unwrap().unwrap();
    let command: Value = serde_json::from_str(command.to_text().unwrap()).unwrap();
    let id = command["id"].as_u64().unwrap();
    let mut owner = tokio::spawn(response.wait());
    assert!(tokio::time::timeout(Duration::from_millis(10), &mut owner)
        .await
        .is_err());
    assert!(lock_pending(&transport.pending).contains_key(&id));
    assert_eq!(
        lock_completions(&transport.completion_routes).get(session.as_ref()),
        Some(&id)
    );
    peer.send(Message::Text(
        json!({"id":id,"result":{"data":"native-completion"}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    assert_eq!(
        owner.await.unwrap().unwrap(),
        json!({"data":"native-completion"})
    );
    assert!(lock_pending(&transport.pending).is_empty());
    assert!(lock_completions(&transport.completion_routes).is_empty());
    transport.close().await.unwrap();
}

#[tokio::test]
async fn completion_receipt_is_released_by_target_session_detachment() {
    let (transport, mut peer) = pending_test_peer().await;
    let session: Arc<str> = "detached-capture-session".into();
    let dispatch = CommandDispatch::new();
    let (written, response) = transport
        .send_to_session_for_completion(&session, "Page.captureScreenshot", &json!({}), &dispatch)
        .await
        .unwrap();
    written.unwrap();
    peer.next().await.unwrap().unwrap();
    peer.send(Message::Text(
        json!({
            "method":"Target.detachedFromTarget",
            "params":{"sessionId":session.as_ref(),"targetId":"closed-tab"}
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), response.wait())
        .await
        .unwrap();
    assert!(matches!(result, Err(Error::Transport { .. })));
    assert!(lock_pending(&transport.pending).is_empty());
    assert!(lock_completions(&transport.completion_routes).is_empty());
    transport.close().await.unwrap();
}

#[tokio::test]
async fn pending_completion_owner_does_not_retain_browser_transport() {
    let (transport, mut peer) = pending_test_peer().await;
    let transport = Arc::new(transport);
    let weak = Arc::downgrade(&transport);
    let session: Arc<str> = "owned-completion-session".into();
    let dispatch = CommandDispatch::new();
    let (written, response) = transport
        .send_to_session_for_completion(&session, "Page.captureScreenshot", &json!({}), &dispatch)
        .await
        .unwrap();
    written.unwrap();
    peer.next().await.unwrap().unwrap();
    let owner = tokio::spawn(response.wait());
    drop(transport);
    assert!(weak.upgrade().is_none());
    let result = tokio::time::timeout(Duration::from_secs(2), owner)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(Error::Transport { .. })));
}

#[tokio::test]
async fn closing_transport_is_visible_before_writer_and_reader_finish() {
    let (transport, _peer) = pending_test_peer().await;
    let writer = transport.writer.lock().await;
    let mut closing = Box::pin(transport.close());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut closing)
            .await
            .is_err()
    );
    assert!(transport.is_closed());
    assert!(transport
        .send::<_, Value>("Fixture.afterClose", &json!({}))
        .await
        .is_err());
    drop(writer);
    closing.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn closing_transport_times_out_writer_and_cleans_owned_runtime() {
    let (mut transport, _peer) = pending_test_peer().await;
    transport.cmd_timeout = Duration::from_secs(1);
    let runtime = RuntimeDirectory::create(None).unwrap();
    let runtime_path = runtime.path().to_owned();
    *transport.runtime.get_mut().unwrap() = Some(runtime);
    let (_, response) = transport
        .prepare_command(None, "Fixture.pendingAtClose", &json!({}), None)
        .unwrap();
    let writer = transport.writer.lock().await;

    tokio::time::timeout(
        transport.cmd_timeout + Duration::from_millis(10),
        transport.close(),
    )
    .await
    .expect("close must not wait indefinitely for the writer")
    .unwrap();

    let error = tokio::time::timeout(Duration::from_millis(10), response.wait())
        .await
        .expect("close must fail pending commands")
        .unwrap_err();
    assert!(error.to_string().contains("CDP transport closed"));
    assert!(lock_pending(&transport.pending).is_empty());
    assert!(transport.is_closed());
    assert!(!runtime_path.exists(), "close must clean the owned runtime");
    drop(writer);
}

#[tokio::test]
async fn binary_frames_preserve_event_payloads_and_command_replies() {
    let (transport, mut peer) = pending_test_peer().await;
    let mut events = transport.subscribe();
    peer.send(Message::Binary(vec![0xff, 0xfe].into()))
        .await
        .unwrap();
    let params = json!({"nested":{"text":"quoted \"Unicode: λ\"","values":[null,true,42]}});
    peer.send(Message::Binary(
        serde_json::to_vec(&json!({
            "method":"Fixture.binary",
            "sessionId":"binary-session",
            "params":params,
        }))
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    match event {
        CdpMessage::Event {
            method,
            params: received,
            session_id,
        } => {
            assert_eq!(method, "Fixture.binary");
            assert_eq!(received, params);
            assert_eq!(session_id.as_deref(), Some("binary-session"));
        }
        _ => panic!("expected binary event"),
    }
    let reply = async {
        let command = peer.next().await.unwrap().unwrap();
        let command: Value = serde_json::from_str(command.to_text().unwrap()).unwrap();
        peer.send(Message::Binary(
            serde_json::to_vec(&json!({"id":command["id"],"result":params}))
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
    };
    let command_params = json!({});
    let (result, ()) = tokio::join!(
        transport.send::<_, Value>("Fixture.binary", &command_params),
        reply
    );
    assert_eq!(result.unwrap(), params);
    transport.close().await.unwrap();
}
