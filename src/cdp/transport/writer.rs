//! The single WebSocket writer task.
//!
//! The reader never awaits the socket: pongs and automatic Fetch replies go to
//! a priority lane, user commands to a normal lane. A frame that cannot be
//! written within the write deadline means the connection is stuck; the writer
//! then shuts the socket down, which ends the reader and fails pending commands.

use super::*;
use std::task::Poll;

pub(super) struct WriterState {
    pub(super) closed: Arc<AtomicBool>,
    pub(super) reason: CloseReason,
    pub(super) write_timeout: Duration,
    pub(super) shutdown: SocketShutdownGuard,
    pub(super) failed: Arc<tokio::sync::Notify>,
}

pub(super) async fn writer_loop(
    sink: SharedSink,
    mut replies: mpsc::UnboundedReceiver<WriteItem>,
    mut commands: mpsc::UnboundedReceiver<WriteItem>,
    state: WriterState,
) {
    let WriterState {
        closed,
        reason,
        write_timeout,
        shutdown,
        failed,
    } = state;
    let (mut replies_open, mut commands_open) = (true, true);
    loop {
        // Automatic replies first: they unblock Chrome (paused requests, pings).
        let next = poll_fn(|cx| {
            if replies_open {
                match replies.poll_recv(cx) {
                    Poll::Ready(Some(item)) => return Poll::Ready(Some(item)),
                    Poll::Ready(None) => replies_open = false,
                    Poll::Pending => {}
                }
            }
            if commands_open {
                match commands.poll_recv(cx) {
                    Poll::Ready(Some(item)) => return Poll::Ready(Some(item)),
                    Poll::Ready(None) => commands_open = false,
                    Poll::Pending => {}
                }
            }
            if replies_open || commands_open {
                Poll::Pending
            } else {
                Poll::Ready(None)
            }
        })
        .await;
        let Some(item) = next else { break };
        // A sender that stopped waiting while this was queued withdrew it.
        if item
            .claim
            .as_ref()
            .is_some_and(|claim| claim.load(Ordering::Acquire) != DISPATCH_CLAIMED)
        {
            continue;
        }
        let written =
            tokio::time::timeout(write_timeout, write_frame(&sink, item.message, item.claim)).await;
        let failure = match written {
            Ok(Ok(())) => {
                if let Some(ack) = item.ack {
                    drop(ack.send(Ok(())));
                }
                continue;
            }
            Ok(Err(error)) => format!("WebSocket write failed: {error}"),
            Err(_) => format!(
                "WebSocket write exceeded its {:?} deadline; the connection is stalled",
                write_timeout
            ),
        };
        tracing::debug!("CDP writer stopping: {}", failure);
        record_close_reason(&reason, &failure);
        closed.store(true, Ordering::Release);
        if let Some(ack) = item.ack {
            drop(ack.send(Err(Error::transport(failure))));
        }
        // Stores a permit if the reader is not waiting yet.
        failed.notify_one();
        break;
    }
    // Shutting down the socket ends the reader, which fails every pending command.
    drop(shutdown);
}

/// Write one frame. A command is claimed only after the sink is ready, right
/// before insertion; a command withdrawn by then is skipped.
async fn write_frame(
    sink: &SharedSink,
    message: Message,
    claim: Option<Arc<AtomicU8>>,
) -> std::result::Result<(), tokio_tungstenite::tungstenite::Error> {
    let mut sink = sink.lock().await;
    poll_fn(|cx| sink.poll_ready_unpin(cx)).await?;
    if let Some(claim) = claim {
        if claim
            .compare_exchange(
                DISPATCH_CLAIMED,
                DISPATCH_SENT,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return Ok(());
        }
    }
    sink.start_send_unpin(message)?;
    sink.flush().await
}
