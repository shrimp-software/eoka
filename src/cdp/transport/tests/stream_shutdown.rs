use super::*;
use std::future::Future;
use std::task::Poll;
use tokio::io::AsyncWriteExt;

#[derive(Clone, Copy)]
enum Shutdown {
    RemoteClose,
    Eof,
    ProtocolError,
    ExplicitClose,
}

async fn shuts_down_streams(shutdown: Shutdown) {
    let (transport, mut peer) = pending_test_peer().await;
    let mut events = transport.subscribe();
    let mut barrier = transport.subscribe();
    let (request_tx, mut requests) = mpsc::channel(1);
    let (response_tx, mut responses) = mpsc::channel(1);
    transport
        .install_request_route("owned", request_tx, Arc::new(AtomicU64::new(0)))
        .unwrap();
    transport.install_response_route("owned", response_tx, Arc::new(AtomicU64::new(0)));

    for params in [
        json!({"requestId":"request", "request":{"headers":{}}}),
        json!({"requestId":"response", "responseStatusCode":200}),
    ] {
        peer.send(Message::Text(
            json!({"method":"Fetch.requestPaused", "sessionId":"owned", "params":params})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    }
    peer.send(Message::Text(
        json!({"method":"Test.queued", "params":{}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), barrier.recv())
        .await
        .expect("reader must queue every preceding pause")
        .unwrap();

    match shutdown {
        Shutdown::RemoteClose => peer.send(Message::Close(None)).await.unwrap(),
        Shutdown::Eof => drop(peer),
        Shutdown::ProtocolError => peer.get_mut().write_all(&[0x83, 0]).await.unwrap(),
        Shutdown::ExplicitClose => transport.close().await.unwrap(),
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        while !transport.reader_handle.as_ref().unwrap().is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("reader must terminate after shutdown");
    assert!(transport.is_closed());

    assert!(
        matches!(events.recv().await, Ok(CdpMessage::Event { method, .. }) if method == "Test.queued")
    );
    assert!(
        matches!(transport.recv_event().await, Some(CdpMessage::Event { method, .. }) if method == "Test.queued")
    );
    assert_eq!(
        requests.recv().await.unwrap().params["requestId"],
        "request"
    );
    assert_eq!(responses.recv().await.unwrap()["requestId"], "response");

    tokio::time::timeout(Duration::from_secs(1), async {
        assert!(matches!(
            events.recv().await,
            Err(broadcast::error::RecvError::Closed)
        ));
        assert!(transport.recv_event().await.is_none());
        assert!(requests.recv().await.is_none());
        assert!(responses.recv().await.is_none());
        assert!(matches!(
            transport.subscribe().recv().await,
            Err(broadcast::error::RecvError::Closed)
        ));
    })
    .await
    .expect("queued events must drain, then every stream must close while Transport remains alive");

    let (request_tx, mut late_requests) = mpsc::channel(1);
    assert!(transport
        .install_request_route("late", request_tx, Arc::new(AtomicU64::new(0)))
        .is_err());
    let (response_tx, mut late_responses) = mpsc::channel(1);
    transport.install_response_route("late", response_tx, Arc::new(AtomicU64::new(0)));
    tokio::time::timeout(Duration::from_secs(1), async {
        assert!(late_requests.recv().await.is_none());
        assert!(late_responses.recv().await.is_none());
    })
    .await
    .expect("routes installed after shutdown must not retain senders");
}

#[tokio::test]
async fn remote_close_drains_and_closes_event_and_route_streams() {
    shuts_down_streams(Shutdown::RemoteClose).await;
}

#[tokio::test]
async fn eof_drains_and_closes_event_and_route_streams() {
    shuts_down_streams(Shutdown::Eof).await;
}

#[tokio::test]
async fn websocket_protocol_error_drains_and_closes_event_and_route_streams() {
    shuts_down_streams(Shutdown::ProtocolError).await;
}

#[tokio::test]
async fn explicit_close_drains_and_closes_event_and_route_streams() {
    shuts_down_streams(Shutdown::ExplicitClose).await;
}

#[tokio::test]
async fn reader_abort_shuts_down_socket_before_first_poll_with_writer_retained() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(socket).await.unwrap()
    });
    let stream = Transport::ws_connect(&url, Duration::from_secs(2))
        .await
        .unwrap();
    let mut peer = server.await.unwrap();
    let transport = Transport::build(None, stream, None, 1, false, false).unwrap();
    // No await between build and abort: this current-thread runtime cannot poll
    // the spawned reader before its abort request. Keep both socket owners.
    transport.reader_handle.as_ref().unwrap().abort();
    let _writer = transport.writer.lock().await;
    assert_peer_terminated(&mut peer).await;
    assert!(transport.is_closed());
}

#[tokio::test]
async fn explicit_close_shuts_down_socket_while_writer_mutex_is_held() {
    let (mut transport, mut peer) = pending_test_peer().await;
    transport.cmd_timeout = Duration::from_millis(50);
    let _writer = transport.writer.lock().await;
    tokio::time::timeout(Duration::from_secs(1), transport.close())
        .await
        .expect("explicit close must not wait indefinitely for the writer")
        .unwrap();
    assert_peer_terminated(&mut peer).await;
    assert!(transport.is_closed());
}

#[tokio::test]
async fn cancelled_explicit_close_still_shuts_down_socket_with_writer_held() {
    let (transport, mut peer) = pending_test_peer().await;
    let _writer = transport.writer.lock().await;
    let mut closing = Box::pin(transport.close());
    poll_fn(|cx| {
        assert!(matches!(closing.as_mut().poll(cx), Poll::Pending));
        Poll::Ready(())
    })
    .await;
    drop(closing);
    assert_peer_terminated(&mut peer).await;
    assert!(transport.is_closed());
}

#[tokio::test]
async fn transport_drop_shuts_down_socket_with_writer_retained() {
    let (transport, mut peer) = pending_test_peer().await;
    let writer = Arc::clone(&transport.writer);
    let _writer = writer.lock().await;
    drop(transport);
    assert_peer_terminated(&mut peer).await;
}

#[tokio::test]
async fn try_recv_event_does_not_wait_for_a_blocked_recv_event() {
    let (transport, _peer) = pending_test_peer().await;
    let mut waiting = Box::pin(transport.recv_event());
    poll_fn(|cx| {
        assert!(matches!(waiting.as_mut().poll(cx), Poll::Pending));
        Poll::Ready(())
    })
    .await;
    assert!(
        tokio::time::timeout(Duration::from_secs(1), transport.try_recv_event())
            .await
            .expect("nonblocking receive must not wait for the persistent receiver mutex")
            .is_none()
    );
    drop(waiting);
    transport.close().await.unwrap();
}
