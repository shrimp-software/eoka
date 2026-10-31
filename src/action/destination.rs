use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::{cdp::transport::CdpMessage, Destination, Error, Expectation, Page, Result};

pub(super) struct DestinationWatch {
    origin: Page,
    scope: Destination,
    events: broadcast::Receiver<CdpMessage>,
    existing: HashSet<String>,
    candidates: HashMap<String, Option<Page>>,
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
        })
    }

    pub async fn check(&mut self, expect: &Expectation) -> Result<Option<String>> {
        loop {
            match self.events.try_recv() {
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
                                return Err(Error::cdp_msg(
                                    "ambiguous_destination: too many new child tabs",
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
                        self.candidates.remove(id);
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(error) => {
                    return Err(Error::cdp_msg(format!(
                        "Destination event stream lost: {error}"
                    )))
                }
            }
        }
        let mut matches = Vec::new();
        if self.scope != Destination::NewTab && self.origin.check_expectation(expect).await? {
            matches.push(self.origin.target_id().to_owned());
        }
        for (id, page) in &mut self.candidates {
            if page.is_none() {
                let session = self.origin.session().attach_frame_target(id).await?;
                *page = Some(Page::new(session, self.origin.config.clone()));
            }
            if let Some(page) = page {
                if page.check_expectation(expect).await? {
                    matches.push(id.clone());
                }
            }
        }
        if matches.len() > 1 {
            return Err(Error::cdp_msg(
                "ambiguous_destination: multiple pages match; select a tab explicitly",
            ));
        }
        Ok(matches.pop())
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
