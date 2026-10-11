//! Serializes temporary header scopes and retains cleanup across cancellation.

use super::Transport;
use crate::{Error, Result};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{watch, OwnedMutexGuard};

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
type Outcome = Option<std::result::Result<(), String>>;

struct Cleanup {
    sessions: Vec<String>,
    result: watch::Receiver<Outcome>,
}

#[derive(Default)]
pub(crate) struct HeaderState {
    serial: Arc<tokio::sync::Mutex<()>>,
    cleanup: Mutex<Option<Cleanup>>,
}

impl HeaderState {
    pub(crate) async fn wait(&self) -> Result<()> {
        loop {
            let receiver = self
                .cleanup
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|cleanup| cleanup.result.clone());
            let Some(mut receiver) = receiver else {
                return Ok(());
            };
            let outcome = match receiver.wait_for(Option::is_some).await {
                Ok(result) => result.clone(),
                Err(_) => Some(Err("restoration stopped before completion".into())),
            };
            // A completed restoration may have been followed by another cancelled
            // scope while this waiter was suspended. Fence the latest one too.
            let current = self
                .cleanup
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(|cleanup| cleanup.result.same_channel(&receiver));
            if !current {
                continue;
            }
            return match outcome {
                Some(Ok(())) => Ok(()),
                Some(Err(error)) => Err(Error::transport(format!(
                    "Temporary header cleanup failed: {error}; call clear_extra_headers to recover"
                ))),
                None => Err(Error::transport(
                    "Temporary header cleanup did not report completion",
                )),
            };
        }
    }

    pub(crate) async fn lock(&self) -> Result<OwnedMutexGuard<()>> {
        self.wait().await?;
        let lock = self.serial.clone().lock_owned().await;
        self.wait().await?;
        Ok(lock)
    }

    pub(crate) async fn temporary(
        self: &Arc<Self>,
        transport: Arc<Transport>,
        session: &str,
    ) -> Result<TemporaryHeaders> {
        let lock = self.lock().await?;
        Ok(TemporaryHeaders {
            state: self.clone(),
            transport,
            sessions: vec![session.into()],
            lock: Some(lock),
        })
    }

    pub(crate) async fn clear(
        self: &Arc<Self>,
        transport: Arc<Transport>,
        session: &str,
    ) -> Result<()> {
        // Recovery deliberately acquires the lock without rejecting a prior failure.
        let lock = self.serial.clone().lock_owned().await;
        let mut sessions = if self.wait().await.is_err() {
            self.cleanup
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|c| c.sessions.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if !sessions.iter().any(|id| id == session) {
            sessions.push(session.into());
        }
        TemporaryHeaders {
            state: self.clone(),
            transport,
            sessions,
            lock: Some(lock),
        }
        .finish()
        .await
    }

    fn schedule(
        &self,
        transport: Arc<Transport>,
        sessions: Vec<String>,
        lock: OwnedMutexGuard<()>,
    ) {
        let (finished, result) = watch::channel(None);
        *self.cleanup.lock().unwrap_or_else(|e| e.into_inner()) = Some(Cleanup {
            sessions: sessions.clone(),
            result,
        });
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            // The completion channel supervises failure, panic and runtime shutdown.
            // Dropping any waiter cannot abort this work or release its serial lock.
            runtime.spawn(async move {
                let result = tokio::time::timeout(CLEANUP_TIMEOUT, async {
                    for session in sessions {
                        set_headers(&transport, &session, HashMap::new()).await?;
                    }
                    Ok::<_, Error>(())
                })
                .await
                .map_err(|_| "restoration timed out".to_string())
                .and_then(|result| result.map_err(|error| error.to_string()));
                finished.send_replace(Some(result));
                drop(lock);
            });
        } else {
            finished.send_replace(Some(Err("restoration requires an active runtime".into())));
        }
    }
}

pub(crate) async fn set_headers(
    transport: &Transport,
    session: &str,
    headers: HashMap<String, String>,
) -> Result<()> {
    transport
        .send_to_session::<_, Value>(session, "Network.enable", &json!({"maxPostDataSize":65536}))
        .await?;
    transport
        .send_to_session::<_, Value>(
            session,
            "Network.setExtraHTTPHeaders",
            &json!({"headers":headers}),
        )
        .await?;
    Ok(())
}

pub(crate) struct TemporaryHeaders {
    state: Arc<HeaderState>,
    transport: Arc<Transport>,
    sessions: Vec<String>,
    lock: Option<OwnedMutexGuard<()>>,
}

impl TemporaryHeaders {
    pub(crate) async fn finish(mut self) -> Result<()> {
        self.schedule();
        self.state.wait().await
    }

    fn schedule(&mut self) {
        if let Some(lock) = self.lock.take() {
            self.state.schedule(
                self.transport.clone(),
                std::mem::take(&mut self.sessions),
                lock,
            );
        }
    }
}

impl Drop for TemporaryHeaders {
    fn drop(&mut self) {
        self.schedule();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{tungstenite::Message, WebSocketStream};
    type Peer = WebSocketStream<tokio::net::TcpStream>;

    async fn peer() -> (Arc<Transport>, Peer) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(socket).await.unwrap()
        });
        (
            Arc::new(Transport::connect(&address, 30).await.unwrap()),
            server.await.unwrap(),
        )
    }

    async fn next(peer: &mut Peer, expected: &str) -> Value {
        let message = tokio::time::timeout(Duration::from_secs(2), peer.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let command: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        assert_eq!(command["method"], expected);
        command
    }

    async fn reply(peer: &mut Peer, command: &Value, reject: bool) {
        let response = if reject {
            json!({"id":command["id"],"error":{"code":-32000,"message":"fixture rejection"}})
        } else {
            json!({"id":command["id"],"result":{}})
        };
        peer.send(Message::Text(response.to_string().into()))
            .await
            .unwrap();
    }

    async fn enable(peer: &mut Peer) {
        let command = next(peer, "Network.enable").await;
        reply(peer, &command, false).await;
    }

    #[tokio::test]
    async fn cancelled_dispatch_and_cleanup_waiters_retain_the_restoration_fence() {
        let (transport, mut peer) = peer().await;
        let state = Arc::new(HeaderState::default());
        let task = tokio::spawn({
            let (transport, state) = (transport.clone(), state.clone());
            async move {
                let guard = state
                    .temporary(transport.clone(), "original")
                    .await
                    .unwrap();
                set_headers(
                    &transport,
                    "original",
                    HashMap::from([("secret".into(), "fixture".into())]),
                )
                .await
                .unwrap();
                guard.finish().await.unwrap();
            }
        });
        enable(&mut peer).await;
        let set = next(&mut peer, "Network.setExtraHTTPHeaders").await;
        assert_eq!(set["params"]["headers"]["secret"], "fixture");
        // The command reached Chrome, but its reply is withheld when the caller aborts.
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        enable(&mut peer).await;
        let clear = next(&mut peer, "Network.setExtraHTTPHeaders").await;
        assert_eq!(clear["sessionId"], "original");
        assert_eq!(clear["params"]["headers"], json!({}));
        for _ in 0..2 {
            assert!(
                tokio::time::timeout(Duration::from_millis(20), state.wait())
                    .await
                    .is_err()
            );
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(20), state.lock())
                .await
                .is_err()
        );
        reply(&mut peer, &clear, false).await;
        let (a, b) = tokio::join!(state.wait(), state.wait());
        a.unwrap();
        b.unwrap();
        let _lock = state.lock().await.unwrap();
    }

    #[tokio::test]
    async fn cleanup_failure_blocks_commands_and_recovery_uses_the_original_session() {
        let (transport, mut peer) = peer().await;
        let state = Arc::new(HeaderState::default());
        let guard = state
            .temporary(transport.clone(), "original")
            .await
            .unwrap();
        let finish = tokio::spawn(async move { guard.finish().await });
        enable(&mut peer).await;
        let clear = next(&mut peer, "Network.setExtraHTTPHeaders").await;
        reply(&mut peer, &clear, true).await;
        assert!(finish.await.unwrap().is_err());
        assert!(state.wait().await.is_err());
        assert!(state.lock().await.is_err());
        let recovery = tokio::spawn({
            let (state, transport) = (state.clone(), transport.clone());
            async move { state.clear(transport, "other-attachment").await }
        });
        for expected in ["original", "other-attachment"] {
            enable(&mut peer).await;
            let clear = next(&mut peer, "Network.setExtraHTTPHeaders").await;
            assert_eq!(clear["sessionId"], expected);
            reply(&mut peer, &clear, false).await;
        }
        recovery.await.unwrap().unwrap();
        state.wait().await.unwrap();
    }

    #[tokio::test]
    async fn restoration_timeout_is_retained_until_explicit_recovery() {
        let (transport, mut peer) = peer().await;
        let state = Arc::new(HeaderState::default());
        drop(state.temporary(transport, "session").await.unwrap());
        // Withhold the first cleanup response past the five-second bound.
        next(&mut peer, "Network.enable").await;
        tokio::time::pause();
        tokio::time::advance(CLEANUP_TIMEOUT + Duration::from_millis(1)).await;
        let error = state.wait().await.unwrap_err().to_string();
        assert!(error.contains("timed out"), "{error}");
        assert!(state.lock().await.is_err());
    }
}
