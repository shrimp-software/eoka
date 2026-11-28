use std::collections::{HashMap, HashSet};

use futures_util::future::{select, Either};
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::{cdp::transport::CdpMessage, Destination, Error, Expectation, Page, Result};

pub(super) struct DestinationWatch {
    origin: Page,
    scope: Destination,
    events: broadcast::Receiver<std::sync::Arc<CdpMessage>>,
    existing: HashSet<String>,
    candidates: HashMap<String, Option<Page>>,
    origin_closed: bool,
}

impl DestinationWatch {
    pub async fn begin(origin: &Page, scope: Destination) -> Result<Self> {
        let events = origin.session().transport().subscribe();
        origin
            .session()
            .transport()
            .ensure_target_discovery()
            .await?;
        let targets: Value = origin
            .session()
            .transport()
            .send("Target.getTargets", &json!({}))
            .await?;
        let existing = targets["targetInfos"]
            .as_array()
            .ok_or_else(|| Error::cdp_msg("Target list unavailable"))?
            .iter()
            .filter_map(|info| info["targetId"].as_str().map(str::to_owned))
            .collect();
        Ok(Self {
            origin: origin.clone(),
            scope,
            events,
            existing,
            candidates: HashMap::new(),
            origin_closed: false,
        })
    }

    /// Rebuilds candidates and the origin's liveness from `Target.getTargets`.
    async fn rescan(&mut self) -> Result<()> {
        let targets: Value = self
            .origin
            .session()
            .transport()
            .send("Target.getTargets", &json!({}))
            .await?;
        let infos = targets["targetInfos"]
            .as_array()
            .ok_or_else(|| Error::cdp_msg("Target list unavailable"))?;
        let mut live = HashSet::new();
        for info in infos {
            let Some(id) = info["targetId"].as_str() else {
                continue;
            };
            live.insert(id.to_owned());
            if info["type"] == "page"
                && info["openerId"] == self.origin.target_id()
                && !self.existing.contains(id)
                && !self.candidates.contains_key(id)
            {
                if self.candidates.len() >= 32 {
                    return Err(Error::coded(
                        crate::ErrorCode::AmbiguousDestination,
                        "too many new child tabs",
                    ));
                }
                self.candidates.insert(id.to_owned(), None);
            }
        }
        self.candidates.retain(|id, _| live.contains(id));
        if !live.contains(self.origin.target_id()) {
            self.origin_closed = true;
        }
        Ok(())
    }

    pub async fn check(&mut self, expect: &Expectation) -> Result<Option<String>> {
        loop {
            match self.events.try_recv().as_deref() {
                Ok(CdpMessage::Event { method, params, .. })
                    if method == "Target.targetCreated" || method == "Target.targetInfoChanged" =>
                {
                    let info = &params["targetInfo"];
                    if info["type"] == "page" && info["openerId"] == self.origin.target_id() {
                        if let Some(id) = info["targetId"]
                            .as_str()
                            .filter(|id| !self.existing.contains(*id))
                        {
                            if self.candidates.len() >= 32 && !self.candidates.contains_key(id) {
                                return Err(Error::coded(
                                    crate::ErrorCode::AmbiguousDestination,
                                    "too many new child tabs",
                                ));
                            }
                            self.candidates.entry(id.into()).or_insert(None);
                        }
                    }
                }
                Ok(CdpMessage::Event { method, params, .. })
                    if method == "Target.targetDestroyed" =>
                {
                    if let Some(id) = params["targetId"].as_str() {
                        if id == self.origin.target_id() {
                            self.origin_closed = true;
                        }
                        self.candidates.remove(id);
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::TryRecvError::Empty) => break,
                // Busy browsers can outrun the shared event buffer. The
                // dropped events may have announced a child tab, so rebuild
                // the candidate set from Chrome's target list.
                Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                    tracing::debug!(skipped, "destination events lagged; rescanning targets");
                    self.rescan().await?;
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    return Err(Error::transport_io(
                        "Destination event stream lost",
                        std::io::Error::other(broadcast::error::TryRecvError::Closed),
                    ))
                }
            }
        }
        let mut matches = Vec::new();
        if self.scope != Destination::NewTab && !self.origin_closed {
            match observe(&self.origin, expect).await? {
                Observation::Matches => matches.push(self.origin.target_id().to_owned()),
                Observation::Closed => self.origin_closed = true,
                Observation::Pending => {}
            }
        }
        let mut closed = Vec::new();
        for (id, page) in &mut self.candidates {
            if page.is_none() {
                let session = match self.origin.session().attach_frame_target(id).await {
                    Ok(session) => session,
                    Err(error) if matches!(lifecycle_error(&error), Some(Observation::Closed)) => {
                        closed.push(id.clone());
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                *page = Some(Page::new(session, self.origin.config.clone()));
            }
            if let Some(page) = page {
                match observe(page, expect).await? {
                    Observation::Matches => matches.push(id.clone()),
                    Observation::Closed => closed.push(id.clone()),
                    Observation::Pending => {}
                }
            }
        }
        for id in closed {
            self.candidates.remove(&id);
        }
        if matches.len() > 1 {
            return Err(Error::coded(
                crate::ErrorCode::AmbiguousDestination,
                "multiple pages match; select a tab explicitly",
            ));
        }
        Ok(matches.pop())
    }
}

enum Observation {
    Matches,
    Pending,
    Closed,
}

async fn observe(page: &Page, expect: &Expectation) -> Result<Observation> {
    let mut events = page.session().transport().subscribe();
    let destroyed = async {
        loop {
            match events.recv().await.as_deref() {
                Ok(CdpMessage::Event { method, params, .. })
                    if method == "Target.targetDestroyed"
                        && params["targetId"] == page.target_id() =>
                {
                    return Ok(Observation::Closed);
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(Error::transport_io(
                        "Destination event stream lost",
                        std::io::Error::other(broadcast::error::RecvError::Closed),
                    ))
                }
            }
        }
    };
    let observation = std::pin::pin!(page.check_expectation(expect));
    let destroyed = std::pin::pin!(destroyed);
    let result = select(observation, destroyed).await;
    match result {
        Either::Left((result, _)) => match result {
            Ok(true) => Ok(Observation::Matches),
            Ok(false) => Ok(Observation::Pending),
            Err(error) => lifecycle_error(&error).ok_or(error),
        },
        Either::Right((result, _)) => result,
    }
}

fn lifecycle_error(error: &Error) -> Option<Observation> {
    use crate::error::classify::{cdp_failure, CdpFailure};
    match cdp_failure(error)? {
        CdpFailure::SessionGone => Some(Observation::Closed),
        CdpFailure::ContextLost => Some(Observation::Pending),
        CdpFailure::NodeGone | CdpFailure::NotRendered | CdpFailure::NotElement => None,
    }
}

impl Drop for DestinationWatch {
    fn drop(&mut self) {
        let sessions: Vec<_> = self
            .candidates
            .values()
            .filter_map(|p| p.as_ref().map(|p| p.session().clone()))
            .collect();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                for session in sessions {
                    let _ =
                        tokio::time::timeout(std::time::Duration::from_secs(2), session.detach())
                            .await;
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdp::{Connection, Transport};
    use futures_util::{SinkExt, StreamExt};
    use std::{sync::Arc, time::Duration};
    use tokio_tungstenite::tungstenite::Message;

    #[tokio::test]
    async fn target_destruction_interrupts_an_unanswered_observation() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let peer = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let command: Value = serde_json::from_str(&text).unwrap();
                let response = match command["method"].as_str().unwrap() {
                    "Target.attachToTarget" => {
                        json!({"id":command["id"],"result":{"sessionId":"origin-session"}})
                    }
                    "Runtime.evaluate" => {
                        json!({"method":"Target.targetDestroyed","params":{"targetId":"origin"}})
                    }
                    method => panic!("unexpected command: {method}"),
                };
                socket
                    .send(Message::Text(response.to_string().into()))
                    .await
                    .unwrap();
            }
        });
        let connection = Connection::new(Transport::connect(&url, 30).await.unwrap());
        let page = Page::new(
            connection.attach_to_target("origin").await.unwrap(),
            Arc::default(),
        );
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            observe(&page, &Expectation::DocumentReady),
        )
        .await;
        connection.transport().close().await.unwrap();
        peer.await.unwrap();
        assert!(matches!(result, Ok(Ok(Observation::Closed))));
    }
}
