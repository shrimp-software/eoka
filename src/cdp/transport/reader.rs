//! WebSocket framing, command replies and event dispatch.

use super::interception::{EventAction, FetchInterceptor};
use super::*;

impl Transport {
    /// Deliver command replies, route private monitoring and Fetch events, then
    /// broadcast remaining events. A failed write closes every pending command.
    pub(super) async fn reader_loop(
        mut source: WsSource,
        pending: Arc<PendingMap>,
        event_tx: broadcast::Sender<CdpMessage>,
        writer: SharedSink,
        idle_routes: IdleRoutes,
        interception: FetchInterceptor,
    ) {
        let exit_reason;
        // Separate command ID space for auth/interception auto-responses (won't
        // collide with main IDs for the first ~2 billion commands). Must stay
        // within int32: DevTools silently drops commands with ids outside it.
        let mut auth_cmd_id: u64 = 1_000_000_000;

        loop {
            let text = match source.next().await {
                Some(Ok(Message::Text(t))) => t,
                Some(Ok(Message::Binary(b))) => match String::from_utf8(b.to_vec()) {
                    Ok(s) => s.into(),
                    Err(_) => continue,
                },
                Some(Ok(Message::Ping(payload))) => {
                    // Split streams don't auto-pong; reply via the shared sink.
                    let mut w = writer.lock().await;
                    if let Err(error) = w.send(Message::Pong(payload)).await {
                        exit_reason = format!("WebSocket pong write failed: {}", error);
                        tracing::debug!("{}", exit_reason);
                        break;
                    }
                    continue;
                }
                Some(Ok(Message::Pong(_))) | Some(Ok(Message::Frame(_))) => continue,
                Some(Ok(Message::Close(_))) => {
                    exit_reason = "WebSocket closed by server".to_string();
                    tracing::debug!("{}", exit_reason);
                    break;
                }
                Some(Err(e)) => {
                    exit_reason = format!("WebSocket read error: {}", e);
                    tracing::debug!("{}", exit_reason);
                    break;
                }
                None => {
                    exit_reason = "WebSocket stream ended".to_string();
                    tracing::debug!("{}", exit_reason);
                    break;
                }
            };

            let msg: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Failed to parse CDP message: {} - {}", e, text);
                    continue;
                }
            };

            // Check if response or event
            if let Some(id) = msg.get("id").and_then(|v| v.as_u64()) {
                let result = if let Some(error) = msg.get("error") {
                    Err(Error::cdp(
                        msg.get("method")
                            .and_then(|m| m.as_str())
                            .unwrap_or("unknown"),
                        error.get("code").and_then(|c| c.as_i64()).unwrap_or(-1),
                        error
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("unknown"),
                    ))
                } else {
                    Ok(msg.get("result").cloned().unwrap_or(json!({})))
                };

                let mut pending_guard = lock_pending(&pending);
                if let Some(sender) = pending_guard.remove(&id) {
                    drop(sender.send(result));
                } else {
                    tracing::trace!("Response for unknown id: {}", id);
                }
            } else if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                let params = msg.get("params").cloned().unwrap_or(json!({}));
                let session_id = msg
                    .get("sessionId")
                    .and_then(|s| s.as_str())
                    .map(String::from);

                if network_idle::route_event(&idle_routes, session_id.as_deref(), method, &params) {
                    continue;
                }

                match interception.on_event(method, session_id.as_deref(), &params) {
                    EventAction::Consumed => continue,
                    EventAction::Forward => {}
                    EventAction::Reply { method, params } => {
                        auth_cmd_id += 1;
                        let mut response =
                            json!({"id":auth_cmd_id, "method":method, "params":params});
                        if let Some(ref sid) = session_id {
                            response["sessionId"] = json!(sid);
                        }
                        if let Err(error) = writer
                            .lock()
                            .await
                            .send(Message::Text(response.to_string().into()))
                            .await
                        {
                            exit_reason = format!("Automatic {method} write failed: {error}");
                            break;
                        }
                        continue;
                    }
                }

                // Publish event; ignore "no active receivers" errors.
                drop(event_tx.send(CdpMessage::Event {
                    method: method.to_string(),
                    params,
                    session_id,
                }));
            }
        }

        // Drain all pending requests so callers fail immediately instead of
        // hanging until the per-command timeout fires.
        network_idle::disconnect(&idle_routes);
        let context = format!("WebSocket connection lost: {}", exit_reason);
        let n = fail_pending(&pending, &context);
        if n > 0 {
            tracing::error!(
                "Reader loop exiting ({}), failing {} pending request(s)",
                exit_reason,
                n
            );
        }
        tracing::debug!("CDP reader loop ended ({})", exit_reason);
    }
}
