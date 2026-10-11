//! Native browser download events. File placement and job policy belong to callers.

use crate::{
    cdp::{transport::CdpMessage, Transport},
    Error, Result,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};
use tokio::sync::{broadcast, OwnedSemaphorePermit, Semaphore};

/// A native download beginning in a browser frame.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadBeginning {
    /// Initiating frame.
    pub frame_id: String,
    /// Chrome's download identifier and staging filename.
    pub guid: String,
    /// Resource URL; may contain secrets and must not be logged verbatim.
    pub url: String,
    /// Untrusted display name, never a safe filesystem path.
    pub suggested_filename: String,
}

/// Native transfer progress. Completion still requires local file verification.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    /// Chrome's download identifier.
    pub guid: String,
    /// Bytes received, as reported by Chrome.
    pub received_bytes: f64,
    /// Expected bytes; zero can mean unknown.
    pub total_bytes: f64,
    /// Native transfer state.
    pub state: NativeDownloadState,
}

/// Chrome's transfer state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeDownloadState {
    /// Transfer continues.
    InProgress,
    /// Chrome finished; the caller must check the file.
    Completed,
    /// Transfer was interrupted or canceled.
    Canceled,
}

/// A browser-level download event.
#[derive(Clone)]
pub enum DownloadEvent {
    /// A new download has begun.
    Beginning(DownloadBeginning),
    /// An existing download has changed state.
    Progress(DownloadProgress),
}

/// Exclusive ownership of native download configuration for one browser.
/// Subscribe before triggering input. Explicitly stop before changing directories.
/// Dropping schedules bounded restoration; a failed restore blocks future watchers.
pub struct DownloadWatch {
    transport: Arc<Transport>,
    events: broadcast::Receiver<CdpMessage>,
    permit: Option<OwnedSemaphorePermit>,
    openers: HashMap<String, String>,
}

impl DownloadWatch {
    pub(crate) async fn begin(
        transport: Arc<Transport>,
        slot: Arc<Semaphore>,
        directory: &Path,
    ) -> Result<Self> {
        let permit = slot
            .try_acquire_owned()
            .map_err(|_| Error::cdp_msg("download_busy: download configuration unavailable"))?;
        let directory = std::fs::canonicalize(directory)?;
        if !directory.is_dir() {
            return Err(Error::cdp_msg("Download directory is not a directory"));
        }
        let watch = Self {
            events: transport.subscribe(),
            transport,
            permit: Some(permit),
            openers: HashMap::new(),
        };
        watch
            .transport
            .send::<_, Value>("Target.setDiscoverTargets", &json!({"discover": true}))
            .await?;
        watch
            .transport
            .send::<_, Value>(
                "Browser.setDownloadBehavior",
                &json!({
                    "behavior": "allowAndName", "downloadPath": directory, "eventsEnabled": true,
                }),
            )
            .await?;
        Ok(watch)
    }

    /// Read the next native event. Event loss and disconnection are errors.
    pub async fn next_event(&mut self) -> Result<DownloadEvent> {
        loop {
            if self.transport.is_closed() {
                return Err(Error::transport("Download event transport disconnected"));
            }
            let message =
                match tokio::time::timeout(Duration::from_secs(1), self.events.recv()).await {
                    Err(_) => continue,
                    Ok(Err(error)) => {
                        return Err(Error::transport(format!("Download event loss: {error}")))
                    }
                    Ok(Ok(message)) => message,
                };
            if let CdpMessage::Event { method, params, .. } = message {
                match method.as_str() {
                    "Target.targetCreated" | "Target.targetInfoChanged" => {
                        if let (Some(target), Some(opener)) = (
                            params["targetInfo"]["targetId"].as_str(),
                            params["targetInfo"]["openerId"].as_str(),
                        ) {
                            if self.openers.len() >= 256 && !self.openers.contains_key(target) {
                                return Err(Error::transport(
                                    "Download target ancestry limit exceeded",
                                ));
                            }
                            self.openers.insert(target.into(), opener.into());
                        }
                    }
                    "Browser.downloadWillBegin" => {
                        return Ok(DownloadEvent::Beginning(serde_json::from_value(params)?))
                    }
                    "Browser.downloadProgress" => {
                        return Ok(DownloadEvent::Progress(serde_json::from_value(params)?))
                    }
                    _ => {}
                }
            }
        }
    }

    /// Whether observed target creation linked this popup to an ancestor target.
    /// Retains ancestry after popup closure; unrelated same-process frames are not targets.
    pub fn has_opener_ancestor(&self, target_id: &str, ancestor_id: &str) -> bool {
        let mut target = target_id;
        for _ in 0..16 {
            let Some(opener) = self.openers.get(target) else {
                return false;
            };
            if opener == ancestor_id {
                return true;
            }
            target = opener;
        }
        false
    }

    /// Cancel a GUID observed through this watcher. Does not delete published files.
    pub async fn cancel(&self, guid: &str) -> Result<()> {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.transport
                .send::<_, Value>("Browser.cancelDownload", &json!({"guid": guid})),
        )
        .await
        .map_err(|_| Error::Timeout("Download cancellation timed out".into()))??;
        Ok(())
    }

    /// Restore default browser download behavior and release exclusive ownership.
    pub async fn stop(mut self) -> Result<()> {
        let result = restore(&self.transport).await;
        if let Some(permit) = self.permit.take() {
            if result.is_err() {
                permit.semaphore().close();
            }
        }
        result
    }
}

async fn restore(transport: &Transport) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let behavior = transport
            .send::<_, Value>(
                "Browser.setDownloadBehavior",
                &json!({"behavior": "default", "eventsEnabled": false}),
            )
            .await;
        let discovery = transport
            .send::<_, Value>("Target.setDiscoverTargets", &json!({"discover": false}))
            .await;
        behavior?;
        discovery?;
        Ok(())
    })
    .await
    .map_err(|_| Error::Timeout("Download configuration restoration timed out".into()))?
}

impl Drop for DownloadWatch {
    fn drop(&mut self) {
        let Some(permit) = self.permit.take() else {
            return;
        };
        let transport = self.transport.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if restore(&transport).await.is_err() {
                    permit.semaphore().close();
                }
                drop(permit);
            });
        } else {
            permit.semaphore().close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    async fn peer(reject_restore: bool) -> (Arc<Transport>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("ws://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut stream = tokio_tungstenite::accept_async(socket).await.unwrap();
            while let Some(Ok(Message::Text(text))) = stream.next().await {
                let command: Value = serde_json::from_str(&text).unwrap();
                if command["params"]["behavior"] == "allowAndName" {
                    stream.send(Message::Text(json!({"method":"Browser.downloadWillBegin","params":{"frameId":"frame","guid":"guid","url":"https://example.test/file","suggestedFilename":"../../unsafe"}}).to_string().into())).await.unwrap();
                }
                if command["params"]["guid"] == "flood" {
                    for _ in 0..2048 {
                        stream.send(Message::Text(json!({"method":"Browser.downloadProgress","params":{"guid":"guid","receivedBytes":1,"totalBytes":2,"state":"inProgress"}}).to_string().into())).await.unwrap();
                    }
                }
                let response = if reject_restore && command["params"]["behavior"] == "default" {
                    json!({"id":command["id"],"error":{"code":-32000,"message":"restore failed"}})
                } else {
                    json!({"id":command["id"],"result":{}})
                };
                stream
                    .send(Message::Text(response.to_string().into()))
                    .await
                    .unwrap();
            }
        });
        (
            Arc::new(Transport::connect(&address, 2).await.unwrap()),
            task,
        )
    }

    #[tokio::test]
    async fn subscribes_before_configuration_and_excludes_competing_watchers() {
        let (transport, task) = peer(false).await;
        let slot = Arc::new(Semaphore::new(1));
        let mut watch =
            DownloadWatch::begin(transport.clone(), slot.clone(), &std::env::temp_dir())
                .await
                .unwrap();
        let event = watch.next_event().await.unwrap();
        assert!(matches!(event, DownloadEvent::Beginning(begin) if begin.guid == "guid"));
        assert!(
            DownloadWatch::begin(transport.clone(), slot.clone(), &std::env::temp_dir())
                .await
                .is_err()
        );
        watch.cancel("flood").await.unwrap();
        assert!(watch.next_event().await.is_err());
        watch.stop().await.unwrap();
        let watch = DownloadWatch::begin(transport, slot.clone(), &std::env::temp_dir())
            .await
            .unwrap();
        drop(watch);
        let _permit = tokio::time::timeout(Duration::from_secs(2), slot.acquire())
            .await
            .unwrap()
            .unwrap();
        task.abort();
    }

    #[tokio::test]
    async fn restoration_failure_closes_slot_and_disconnect_is_reported() {
        let (transport, task) = peer(true).await;
        let slot = Arc::new(Semaphore::new(1));
        let mut watch =
            DownloadWatch::begin(transport.clone(), slot.clone(), &std::env::temp_dir())
                .await
                .unwrap();
        watch.next_event().await.unwrap();
        assert!(watch.stop().await.is_err());
        assert!(slot.is_closed());
        let slot = Arc::new(Semaphore::new(1));
        let mut watch = DownloadWatch::begin(transport, slot, &std::env::temp_dir())
            .await
            .unwrap();
        watch.next_event().await.unwrap();
        task.abort();
        assert!(
            tokio::time::timeout(Duration::from_secs(3), watch.next_event())
                .await
                .unwrap()
                .is_err()
        );
    }
}
