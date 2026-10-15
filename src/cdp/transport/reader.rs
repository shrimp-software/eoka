//! WebSocket framing, command replies and event dispatch.

use super::interception::{EventAction, FetchInterceptor};
use super::*;

impl Transport {
    /// Deliver command replies, route private monitoring and Fetch events, then
    /// broadcast remaining events. A failed write closes every pending command.
    pub(super) async fn reader_loop(
        mut source: WsSource,
        pending: Arc<PendingMap>,
        completion_state: ReaderCompletionState,
        event_tx: broadcast::Sender<CdpMessage>,
        writer: SharedSink,
        idle_routes: IdleRoutes,
        interception: FetchInterceptor,
    ) {
        let ReaderCompletionState {
            closed,
            routes: completion_routes,
        } = completion_state;
        let exit_reason;
        // Separate command ID space for auth/interception auto-responses (won't
        // collide with main IDs for the first ~2 billion commands). Must stay
        // within int32: DevTools silently drops commands with ids outside it.
        let mut auth_cmd_id: u64 = 1_000_000_000;

        loop {
            let message = match source.next().await {
                Some(Ok(message @ (Message::Text(_) | Message::Binary(_)))) => message,
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
            let text = match message.to_text() {
                Ok(text) => text,
                Err(_) => continue,
            };

            let mut msg: Value = match serde_json::from_str(text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Failed to parse CDP message: {} - {}", e, text);
                    continue;
                }
            };

            // Check if response or event
            if let Some(id) = msg.get("id").and_then(|v| v.as_u64()) {
                let Some(sender) = lock_pending(&pending).remove(&id) else {
                    tracing::trace!("Response for unknown id: {}", id);
                    continue;
                };
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
                    Ok(msg
                        .get_mut("result")
                        .map(Value::take)
                        .unwrap_or_else(|| json!({})))
                };

                drop(sender.send(result));
            } else if let Some(Value::String(method)) = msg.get_mut("method").map(Value::take) {
                let mut params = msg
                    .get_mut("params")
                    .map(Value::take)
                    .unwrap_or_else(|| json!({}));
                let session_id = match msg.get_mut("sessionId").map(Value::take) {
                    Some(Value::String(session)) => Some(session),
                    _ => None,
                };

                if method == "Target.detachedFromTarget" {
                    if let Some(session) = params.get("sessionId").and_then(Value::as_str) {
                        let id = lock_completions(&completion_routes).remove(session);
                        if let Some(id) = id {
                            let sender = lock_pending(&pending).remove(&id);
                            if let Some(sender) = sender {
                                drop(sender.send(Err(Error::transport(
                                    "Native command session detached before completion",
                                ))));
                            }
                        }
                    }
                }

                if network_idle::route_event(&idle_routes, session_id.as_deref(), &method, &params)
                {
                    continue;
                }

                match interception.on_event(&method, session_id.as_deref(), &mut params) {
                    EventAction::Consumed => continue,
                    EventAction::Forward => {}
                    EventAction::Reply { method, params } => {
                        auth_cmd_id += 1;
                        let response = serde_json::to_string(&CdpCommand {
                            id: auth_cmd_id,
                            method,
                            params: &params,
                            session_id: session_id.as_deref(),
                        })
                        .expect("CDP automatic reply contains only serializable JSON values");
                        if let Err(error) = writer
                            .lock()
                            .await
                            .send(Message::Text(response.into()))
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
                    method,
                    params,
                    session_id,
                }));
            }
        }
        closed.store(true, Ordering::Release);

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
