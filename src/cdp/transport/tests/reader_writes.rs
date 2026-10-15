use super::*;

async fn blocked_reader_write_fails_native_completion(trigger: Message) {
    let (transport, mut peer) = test_peer_with_proxy_credentials().await;
    let session: Arc<str> = "blocked-reader".into();
    let dispatch = CommandDispatch::new();
    let (written, response) = transport
        .send_to_session_for_completion(&session, "Page.captureScreenshot", &json!({}), &dispatch)
        .await
        .unwrap();
    written.unwrap();
    peer.next().await.unwrap().unwrap();
    let writer = transport.writer.lock().await;
    peer.send(trigger).await.unwrap();
    let result = tokio::time::timeout(
        transport.cmd_timeout + Duration::from_secs(1),
        response.wait(),
    )
    .await;
    let closed = transport.is_closed();
    assert_peer_terminated(&mut peer).await;
    drop(writer);
    transport.close().await.unwrap();
    assert!(
        matches!(result, Ok(Err(Error::Transport { .. }))),
        "reader never failed its stalled write: {result:?}"
    );
    assert!(closed, "stalled reader must invalidate the connection");
    assert!(
        dispatch.may_have_been_sent(),
        "failure does not permit command replay"
    );
}

// Real time is required: a paused Tokio clock can expire the outer deadline
// before the kernel delivers the frame that starts the reader's write deadline.
#[tokio::test]
async fn pong_writer_contention_cannot_strand_native_completion() {
    blocked_reader_write_fails_native_completion(Message::Ping(vec![1, 2, 3].into())).await;
}

#[tokio::test]
async fn fetch_writer_contention_cannot_strand_native_completion() {
    blocked_reader_write_fails_native_completion(Message::Text(
        json!({"method":"Fetch.requestPaused","sessionId":"paused-session","params":{"requestId":"paused-request","request":{"headers":{}}}}).to_string().into(),
    )).await;
}

#[tokio::test]
async fn auth_writer_contention_cannot_strand_native_completion() {
    blocked_reader_write_fails_native_completion(Message::Text(
        json!({"method":"Fetch.authRequired","sessionId":"auth-session","params":{"requestId":"auth-request","authChallenge":{"source":"Proxy"}}}).to_string().into(),
    )).await;
}

#[tokio::test]
async fn silent_websocket_upgrade_obeys_connection_deadline() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let mut connection = tokio::spawn(async move { Transport::connect(&url, 1).await });
    let (mut socket, _) = listener.accept().await.unwrap();
    // TCP connects and the HTTP upgrade reaches the peer, which never replies.
    let mut first_byte = [0];
    socket.read_exact(&mut first_byte).await.unwrap();
    // Advance time only after the real socket has delivered the upgrade request.
    tokio::time::pause();
    let result = tokio::time::timeout(Duration::from_secs(2), &mut connection).await;
    connection.abort();
    assert!(
        matches!(result, Ok(Ok(Err(Error::Timeout(_))))),
        "connection waited beyond its deadline for the WebSocket upgrade"
    );
}

#[tokio::test]
async fn automatic_reply_socket_backpressure_cannot_strand_native_completion() {
    let listener = tokio::net::TcpSocket::new_v4().unwrap();
    // Bound the advertised receive window before the handshake. A small sender
    // buffer alone does not bound buffering on every platform.
    listener.set_recv_buffer_size(64 * 1024).unwrap();
    listener.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let listener = listener.listen(1).unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(socket).await.unwrap()
    });
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_send_buffer_size(1024).unwrap();
    let socket = socket.connect(address).await.unwrap();
    let (stream, _) =
        tokio_tungstenite::client_async(format!("ws://{address}"), MaybeTlsStream::Plain(socket))
            .await
            .unwrap();
    let transport = Transport::build(None, stream, None, 1, false, true).unwrap();
    let mut peer = server.await.unwrap();
    let session: Arc<str> = "backpressure".into();
    let dispatch = CommandDispatch::new();
    let (written, response) = transport
        .send_to_session_for_completion(&session, "Page.captureScreenshot", &json!({}), &dispatch)
        .await
        .unwrap();
    written.unwrap();
    peer.next().await.unwrap().unwrap();
    // The peer stops reading after this event. The rewritten header payload is
    // larger than the socket buffers, so the automatic continuation stalls in
    // flush rather than in writer-lock admission.
    peer.send(Message::Text(
        json!({
            "method":"Fetch.requestPaused", "sessionId":"backpressure",
            "params":{"requestId":"blocked", "request":{"headers":{
                "X-Client-Data":"remove", "Large":"x".repeat(1024 * 1024)
            }}}
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), response.wait()).await;
    let closed = transport.is_closed();
    // Check the write deadline before resuming reads, which would relieve
    // backpressure. This must be a terminal transport error, not our outer timeout.
    assert!(
        matches!(result, Ok(Err(Error::Transport { .. }))),
        "{result:?}"
    );
    assert!(closed);
    assert!(dispatch.may_have_been_sent());
    // Some platforms finish sending an already queued frame during shutdown.
    // Drain it and require EOF/error while Transport is still retained.
    assert_peer_terminated(&mut peer).await;
    transport.close().await.unwrap();
}
