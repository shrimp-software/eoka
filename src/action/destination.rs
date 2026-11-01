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
                        if id == self.origin.target_id() {
                            self.origin_closed = true;
                        }
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
            return Err(Error::cdp_msg(
                "ambiguous_destination: multiple pages match; select a tab explicitly",
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
    match page.check_expectation(expect).await {
        Ok(true) => Ok(Observation::Matches),
        Ok(false) => Ok(Observation::Pending),
        Err(error) => lifecycle_error(&error).ok_or(error),
    }
}

fn lifecycle_error(error: &Error) -> Option<Observation> {
    let Error::Cdp {
        code: Some(code),
        message,
        ..
    } = error
    else {
        return None;
    };
    if (*code == -32001 && message == "Session with given id not found.")
        || (*code == -32602 && message == "No target with given id found")
        || (*code == -32000 && message == "Not attached to an active page")
    {
        return Some(Observation::Closed);
    }
    if *code == -32000
        && [
            "Cannot find context",
            "Execution context was destroyed",
            "Could not find object",
            "Cannot find object",
        ]
        .iter()
        .any(|prefix| message.starts_with(prefix))
    {
        return Some(Observation::Pending);
    }
    None
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
