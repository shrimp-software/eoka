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
        event_tx: broadcast::Sender<Arc<CdpMessage>>,
        replies: WriteQueue,
        idle_routes: IdleRoutes,
        interception: FetchInterceptor,
    ) {
        let ReaderCompletionState {
            closed,
            reason,
            shutdown,
            lifecycle,
            writer_failed,
        } = completion_state;
        let mut exit_reason;
        // Automatic replies use their own id range, disjoint from commands and
        // within int32: DevTools silently drops commands with ids outside it.
        let mut auto_id: u64 = AUTO_ID_MAX;

        loop {
            // `next()` is cancel-safe: a partly read frame stays buffered.
            let next = match futures_util::future::select(
                std::pin::pin!(source.next()),
                std::pin::pin!(writer_failed.notified()),
            )
            .await
            {
                futures_util::future::Either::Left((next, _)) => next,
                futures_util::future::Either::Right(((), _)) => {
                    exit_reason = reason
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .unwrap_or_else(|| "CDP writer stopped".to_string());
                    break;
                }
            };
            let message = match next {
                Some(Ok(message @ (Message::Text(_) | Message::Binary(_)))) => message,
                Some(Ok(Message::Ping(payload))) => {
                    // Split streams don't auto-pong; the writer sends it first.
                    if queue_automatic(&replies, Message::Pong(payload)).is_err() {
                        exit_reason = "CDP writer stopped before a pong".to_string();
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
                Some(Err(tokio_tungstenite::tungstenite::Error::Capacity(e))) => {
                    exit_reason = format!(
                        "CDP message exceeded the {} MiB limit ({e})",
                        MAX_CDP_MESSAGE_BYTES / (1024 * 1024)
                    );
                    tracing::error!("{}", exit_reason);
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
                Err(_) => {
                    exit_reason = "Invalid UTF-8 in CDP message".to_string();
                    tracing::warn!(
                        bytes = message.len(),
                        "Invalid UTF-8 in CDP message; payload omitted"
                    );
                    break;
                }
            };

            let mut msg = match parse_message(text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        line = e.line(),
                        column = e.column(),
                        bytes = text.len(),
                        "Failed to parse CDP message; payload omitted"
                    );
                    // An unreadable frame may own a paused request or a native
                    // completion. Dropping it would silently lose that progress.
                    exit_reason = format!(
                        "Invalid CDP JSON at line {}, column {}",
                        e.line(),
                        e.column()
                    );
                    break;
                }
            };

            // Check if response or event
            if let Some(id) = msg.get("id").and_then(|v| v.as_u64()) {
                let Some(request) = lock_pending(&pending).remove(&id) else {
                    tracing::trace!("Response for unknown id: {}", id);
                    continue;
                };
                let sender = request.sender;
                let result = if let Some(error) = msg.get("error") {
                    // CDP replies omit the method; `CommandResponse::wait` adds it.
                    Err(Error::Cdp {
                        method: None,
                        code: Some(error.get("code").and_then(|c| c.as_i64()).unwrap_or(-1)),
                        message: error_message(error),
                    })
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

                // Before private routing, which consumes monitor-session events.
                session_lifecycle(
                    &pending,
                    &lifecycle,
                    &method,
                    session_id.as_deref(),
                    &params,
                );

                if network_idle::route_event(&idle_routes, session_id.as_deref(), &method, &params)
                {
                    continue;
                }

                match interception.on_event(&method, session_id.as_deref(), &mut params) {
                    EventAction::Consumed => continue,
                    EventAction::Forward => {}
                    EventAction::Reply { method, params } => {
                        auto_id = wrapping_next(auto_id, AUTO_ID_MIN, AUTO_ID_MAX);
                        let response = serde_json::to_string(&CdpCommand {
                            id: auto_id,
                            method,
                            params: &params,
                            session_id: session_id.as_deref(),
                        })
                        .expect("CDP automatic reply contains only serializable JSON values");
                        if queue_automatic(&replies, Message::Text(response.into())).is_err() {
                            exit_reason = format!("CDP writer stopped before automatic {method}");
                            break;
                        }
                        continue;
                    }
                }

                // Publish event; ignore "no active receivers" errors.
                drop(event_tx.send(Arc::new(CdpMessage::Event {
                    method,
                    params,
                    session_id,
                })));
            }
        }
        closed.store(true, Ordering::Release);
        // A stalled writer shuts the socket down; report its reason, not EOF.
        if let Some(writer_reason) = reason.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            exit_reason = writer_reason;
        }
        // Do not acquire the writer or flush frames on terminal failure: either
        // can be indefinitely blocked while the retained sink owns the socket.
        drop(shutdown);
        lifecycle.lock().unwrap_or_else(|e| e.into_inner()).take();
        interception.shutdown();

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

/// Queue an automatic reply on the writer's priority lane without waiting.
/// The writer's deadline detects a stalled socket; the reader keeps reading.
fn queue_automatic(replies: &WriteQueue, message: Message) -> std::result::Result<(), ()> {
    replies
        .send(WriteItem {
            message,
            claim: None,
            ack: None,
        })
        .map_err(|_| ())
}

/// CDP error text, keeping the optional `data` detail Chrome attaches.
fn error_message(error: &Value) -> String {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    match error.get("data") {
        None | Some(Value::Null) => message.to_owned(),
        Some(Value::String(data)) if data.is_empty() => message.to_owned(),
        Some(Value::String(data)) => format!("{message}: {data}"),
        Some(data) => format!("{message}: {data}"),
    }
}

/// Fail commands whose session can no longer answer, and tell the connection
/// about detached or destroyed targets.
fn session_lifecycle(
    pending: &PendingMap,
    lifecycle: &LifecycleSink,
    method: &str,
    session: Option<&str>,
    params: &Value,
) {
    let notify = |event: TargetLifecycle| {
        if let Some(sender) = lifecycle.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            drop(sender.send(event));
        }
    };
    match method {
        "Target.detachedFromTarget" => {
            if let Some(detached) = params["sessionId"].as_str() {
                fail_session_pending(pending, detached);
            }
            if let Some(target_id) = params["targetId"].as_str() {
                notify(TargetLifecycle::Detached {
                    target_id: target_id.to_owned(),
                });
            }
        }
        "Target.targetDestroyed" => {
            if let Some(target_id) = params["targetId"].as_str() {
                notify(TargetLifecycle::Destroyed {
                    target_id: target_id.to_owned(),
                });
            }
        }
        "Inspector.detached" => {
            if let Some(session) = session {
                fail_session_pending(pending, session);
            }
        }
        // Inspector.targetCrashed is deliberately not handled: the target and
        // session survive a crash, and browser-side commands still reply.
        _ => {}
    }
}

/// Chrome can serialize binary postData as ill-formed UTF-16 escapes. Repair
/// only after strict parsing fails; ordinary CDP traffic pays no extra scan.
fn parse_message(text: &str) -> serde_json::Result<Value> {
    match serde_json::from_str(text) {
        Ok(message) => Ok(message),
        Err(error) => match sanitize_unicode_escapes(text) {
            Some(repaired) => serde_json::from_str(&repaired),
            None => Err(error),
        },
    }
}

/// Replace invalid Unicode escapes inside JSON strings, without changing JSON
/// structure. No allocation unless an escape needs repair. The result is still
/// parsed strictly: never guess routing metadata from an unparseable message.
fn sanitize_unicode_escapes(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut repaired: Option<String> = None;
    let mut copied = 0;
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            in_string = !in_string;
            i += 1;
            continue;
        }
        if !in_string || bytes[i] != b'\\' {
            i += 1;
            continue;
        }
        if bytes.get(i + 1) != Some(&b'u') {
            // Skip escaped quotes and backslashes together. In particular,
            // a literal \\u is not a Unicode escape.
            i += 2;
            continue;
        }

        let start = i;
        let (unit, digits) = unicode_unit(&bytes[i + 2..]);
        i += 2 + digits;
        if digits == 4 {
            if (0xd800..=0xdbff).contains(&unit) {
                if bytes.get(i..i + 2) == Some(b"\\u") {
                    let (low, digits) = unicode_unit(&bytes[i + 2..]);
                    if digits == 4 && (0xdc00..=0xdfff).contains(&low) {
                        i += 6;
                        continue;
                    }
                }
            } else if !(0xdc00..=0xdfff).contains(&unit) {
                continue;
            }
        }

        let output = repaired.get_or_insert_with(|| String::with_capacity(text.len()));
        output.push_str(&text[copied..start]);
        output.push('\u{fffd}');
        copied = i;
    }
    if let Some(output) = &mut repaired {
        output.push_str(&text[copied..]);
    }
    repaired
}

/// Read at most four hex digits, leaving a truncating quote/backslash untouched.
fn unicode_unit(bytes: &[u8]) -> (u16, usize) {
    let mut unit = 0;
    let mut digits = 0;
    for &byte in bytes.iter().take(4) {
        let nibble = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => break,
        };
        unit = (unit << 4) | u16::from(nibble);
        digits += 1;
    }
    (unit, digits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_unicode_and_literal_escapes_need_no_repair() {
        for text in [
            r#"{"text":"λ 🦀 \u0000 \uFFFF \uD83E\uDD80"}"#,
            r#"{"text":"\\uD800 \\\\u00 \"quoted\""}"#,
        ] {
            assert!(sanitize_unicode_escapes(text).is_none());
            assert_eq!(
                parse_message(text).unwrap(),
                serde_json::from_str::<Value>(text).unwrap()
            );
        }
    }

    #[test]
    fn malformed_unicode_preserves_string_and_object_boundaries() {
        for (escaped, expected) in [
            (r"\uD800", "\u{fffd}"),
            (r"\uDC00", "\u{fffd}"),
            (r"\uD800\u0041", "\u{fffd}A"),
            (r"\uD800\uD83E\uDD80", "\u{fffd}🦀"),
            (r"\uDC00\uD800", "\u{fffd}\u{fffd}"),
            (r"\u", "\u{fffd}"),
            (r"\u0", "\u{fffd}"),
            (r"\u00", "\u{fffd}"),
            (r"\u00F", "\u{fffd}"),
            (r"\u00\u0041", "\u{fffd}A"),
            (r"\u00λ", "\u{fffd}λ"),
            (r#"\"\\uD800\uDC00"#, "\"\\uD800\u{fffd}"),
        ] {
            let text = format!(r#"{{"body":"λ {escaped}","sessionId":"original"}}"#);
            assert_eq!(
                parse_message(&text).unwrap(),
                json!({"body": format!("λ {expected}"), "sessionId": "original"}),
                "{text}"
            );
        }
    }

    #[test]
    fn unicode_repair_does_not_accept_broken_json_structure() {
        for text in [
            r#"{"body":"\uD800""sessionId":"other"}"#,
            r#"{"body":"\u00"#,
            r#"{"body":"\q"}"#,
            r#"{"body":\u00}"#,
        ] {
            assert!(parse_message(text).is_err(), "{text}");
        }
    }
}
