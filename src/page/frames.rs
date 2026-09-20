use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::{json, Value};

use super::{escape_js_string, FrameInfo, Page};
use crate::cdp::{Frame, FrameTree, RuntimeEvaluateResult, Session, TargetGetTargets};
use crate::error::{Error, Result};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Targets {
    target_infos: Vec<FrameTarget>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FrameTarget {
    target_id: String,
    r#type: String,
    url: String,
    parent_frame_id: Option<String>,
}

fn frame_discovery_raced(error: &Error) -> bool {
    matches!(error, Error::Cdp { message, .. } if [
        "No target with given id found", "Session with given id not found",
        "Not attached to an active page", "No frame with given id found",
    ].iter().any(|text| message.contains(text)))
}

struct AttachedFrame(Session);

impl Drop for AttachedFrame {
    fn drop(&mut self) {
        let session = self.0.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Err(error) = session.detach().await {
                    tracing::debug!("Frame session detach failed: {error}");
                }
            });
        }
    }
}

struct FrameQuadObjects {
    session: Session,
    group: String,
    active: bool,
}

impl FrameQuadObjects {
    fn new(session: Session) -> Self {
        static NEXT_GROUP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            session,
            group: format!(
                "eoka-frame-quad-{}",
                NEXT_GROUP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
            active: true,
        }
    }

    async fn release(mut self) -> Result<()> {
        release_quad_objects(&self.session, &self.group).await?;
        self.active = false;
        Ok(())
    }
}

async fn release_quad_objects(session: &Session, group: &str) -> Result<()> {
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        session.send::<_, Value>("Runtime.releaseObjectGroup", &json!({"objectGroup":group})),
    )
    .await
    .map_err(|_| Error::cdp_msg("Frame quad object cleanup timed out"))??;
    Ok(())
}

impl Drop for FrameQuadObjects {
    fn drop(&mut self) {
        if self.active {
            let session = self.session.clone();
            let group = self.group.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    if release_quad_objects(&session, &group).await.is_err() {
                        tracing::debug!("Frame quad object cleanup unconfirmed");
                    }
                });
            }
        }
    }
}

struct Entry {
    info: FrameInfo,
    parent: Option<String>,
    session: Session,
}

struct Snapshot {
    entries: HashMap<String, Entry>,
    order: Vec<String>,
    attachments: Vec<AttachedFrame>,
}

impl Snapshot {
    fn get(&self, id: &str) -> Result<&Entry> {
        self.entries
            .get(id)
            .ok_or_else(|| Error::cdp_msg(format!("Frame not in this page: {id}")))
    }

    async fn capture(page: &Page) -> Result<Self> {
        for attempt in 0..3 {
            match Self::capture_once(page).await {
                Err(error) if frame_discovery_raced(&error) && attempt < 2 => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                result => return result,
            }
        }
        unreachable!()
    }

    async fn capture_once(page: &Page) -> Result<Self> {
        let targets: Targets = page
            .session
            .transport()
            .send("Target.getTargets", &TargetGetTargets {})
            .await?;
        let targets: Vec<_> = targets
            .target_infos
            .into_iter()
            .filter(|t| t.r#type == "iframe")
            .collect();
        let iframe_targets: HashSet<_> = targets.iter().map(|t| t.target_id.as_str()).collect();
        let root = page.session.get_frame_tree().await?;
        let mut stack = vec![(root, None, page.session.clone())];
        let mut snapshot = Self {
            entries: HashMap::new(),
            order: Vec::new(),
            attachments: Vec::new(),
        };
        while let Some((mut tree, parent, mut session)) = stack.pop() {
            let id = tree.frame.id.clone();
            if id.is_empty() || snapshot.entries.contains_key(&id) {
                return Err(Error::cdp_msg("Invalid or duplicate frame in tree"));
            }
            if iframe_targets.contains(id.as_str()) && session.target_id() != id {
                session = page.session.attach_frame_target(&id).await?;
                snapshot.attachments.push(AttachedFrame(session.clone()));
                tree = session.get_frame_tree().await?;
                if tree.frame.id != id {
                    return Err(Error::cdp_msg("Frame target changed while attaching"));
                }
            }
            for target in targets
                .iter()
                .filter(|t| t.parent_frame_id.as_deref() == Some(id.as_str()))
            {
                if !tree
                    .child_frames
                    .iter()
                    .any(|child| child.frame.id == target.target_id)
                {
                    tree.child_frames.push(FrameTree {
                        frame: Frame {
                            id: target.target_id.clone(),
                            name: None,
                            url: target.url.clone(),
                        },
                        child_frames: Vec::new(),
                    });
                }
            }
            for child in tree.child_frames.into_iter().rev() {
                stack.push((child, Some(id.clone()), session.clone()));
            }
            snapshot.order.push(id.clone());
            snapshot.entries.insert(
                id.clone(),
                Entry {
                    info: FrameInfo {
                        id,
                        url: tree.frame.url,
                        name: tree.frame.name,
                    },
                    parent,
                    session,
                },
            );
        }
        Ok(snapshot)
    }
}

impl Page {
    /// Get this page's frame tree, including nested out-of-process iframe targets.
    pub async fn frames(&self) -> Result<Vec<FrameInfo>> {
        self.nested_frame_infos().await
    }

    /// Execute JavaScript inside an iframe.
    ///
    /// # Safety
    ///
    /// `expression` is evaluated as **code** (via the `Function` constructor),
    /// not as a string literal. Do not pass untrusted user input as the
    /// expression — it will be executed in the iframe's JS context.
    pub async fn evaluate_in_frame<T: serde::de::DeserializeOwned>(
        &self,
        frame_selector: &str,
        expression: &str,
    ) -> Result<T> {
        let escaped_frame = escape_js_string(frame_selector);
        let escaped_expr = escape_js_string(expression);

        // Use Function constructor instead of eval (less likely to be blocked by CSP)
        let js = format!(
            r#"
            (() => {{
                const iframe = document.querySelector('{escaped_frame}');
                if (!iframe || !iframe.contentWindow) throw new Error('Frame not found: {escaped_frame}');
                const _exec = new iframe.contentWindow.Function('return (' + '{escaped_expr}' + ')');
                return _exec.call(iframe.contentWindow);
            }})()
            "#,
        );

        self.evaluate(&js).await
    }

    /// Return snapshot-time ancestor IDs, from the immediate parent to this page's root.
    ///
    /// The queried frame is excluded; the root has no ancestors. Stale or unrelated
    /// IDs are rejected. IDs describe this snapshot, not durable navigation identity.
    pub async fn frame_ancestor_ids(&self, frame_id: &str) -> Result<Vec<String>> {
        let snapshot = Snapshot::capture(self).await?;
        let mut entry = snapshot.get(frame_id)?;
        let mut seen = HashSet::from([frame_id.to_string()]);
        let mut ancestors = Vec::new();
        while let Some(parent) = &entry.parent {
            if !seen.insert(parent.clone()) {
                return Err(Error::cdp_msg("Cycle in frame ancestry"));
            }
            ancestors.push(parent.clone());
            entry = snapshot.get(parent)?;
        }
        Ok(ancestors)
    }

    /// Return the ordered rendered content quad of one frame-local element.
    ///
    /// Selects the zero-based `index` among at most 64 CSS selector matches in
    /// this page's frame, including OOPIFs. Index must be below 64 and selectors
    /// at most 4096 bytes. Missing, detached, fragmented or degenerate boxes fail.
    /// The vertices retain content corner order under transforms, including
    /// open/closed shadow-slot ancestry; they are not a normalized bounding box.
    /// Coordinates belong to the owning CDP session's viewport, not necessarily
    /// the selected frame or root page. This is snapshot geometry, not durable
    /// element identity or a visibility/hit-testing guarantee.
    /// Temporary objects are group-released on completion; errors/cancellation
    /// schedule bounded best-effort release, including a lost evaluation reply.
    pub async fn frame_element_content_quad(
        &self,
        frame_id: &str,
        selector: &str,
        index: usize,
    ) -> Result<[f64; 8]> {
        if index >= 64 || selector.len() > 4096 {
            return Err(Error::cdp_msg("Frame quad selection limit exceeded"));
        }
        let snapshot = Snapshot::capture(self).await?;
        let entry = snapshot.get(frame_id)?;
        let context = entry.session.create_isolated_world(frame_id).await?;
        let objects = FrameQuadObjects::new(entry.session.clone());
        let selector = serde_json::to_string(selector)?;
        let expression = format!("(() => {{ const matches=document.querySelectorAll({selector}); if(matches.length>64 || !matches[{index}] || matches[{index}].getClientRects().length!==1) throw new Error('Unsupported frame quad selection'); return matches[{index}]; }})()");
        let remote: RuntimeEvaluateResult = entry
            .session
            .send(
                "Runtime.evaluate",
                &json!({
                    "expression":expression, "contextId":context, "objectGroup":objects.group,
                    "returnByValue":false, "awaitPromise":false,
                }),
            )
            .await?;
        let object_id = self
            .check_js_result(remote)?
            .object_id
            .ok_or_else(|| Error::cdp_msg("Frame quad element unavailable"))?;
        let model: Value = entry
            .session
            .send("DOM.getBoxModel", &json!({"objectId":object_id}))
            .await?;
        let quad: [f64; 8] = serde_json::from_value(model["model"]["content"].clone())?;
        let area = (0..4)
            .map(|i| {
                let next = (i + 1) % 4;
                quad[i * 2] * quad[next * 2 + 1] - quad[next * 2] * quad[i * 2 + 1]
            })
            .sum::<f64>();
        if quad.iter().any(|v| !v.is_finite()) || !area.is_finite() || area.abs() < 1e-8 {
            return Err(Error::cdp_msg("Degenerate frame content quad"));
        }
        objects.release().await?;
        Ok(quad)
    }

    pub(super) async fn nested_frame_infos(&self) -> Result<Vec<FrameInfo>> {
        let snapshot = Snapshot::capture(self).await?;
        Ok(snapshot
            .order
            .iter()
            .map(|id| snapshot.entries[id].info.clone())
            .collect())
    }

    /// Evaluate in a frame belonging to this page, including nested OOPIFs.
    /// Frame IDs come from `frames()`. Detached/unrelated IDs are rejected.
    /// Uses an isolated world, not the page's main-world JavaScript globals.
    /// No Runtime.enable or cross-origin DOM access is required.
    pub async fn evaluate_in_frame_id<T: serde::de::DeserializeOwned>(
        &self,
        frame_id: &str,
        expression: &str,
    ) -> Result<T> {
        let snapshot = Snapshot::capture(self).await?;
        let entry = snapshot.get(frame_id)?;
        let context = entry.session.create_isolated_world(frame_id).await?;
        self.eval_impl(
            entry
                .session
                .evaluate_in_context(expression, context)
                .await?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_quad_objects_release_on_success_error_and_cancelled_replies() {
        use futures_util::{SinkExt, StreamExt};
        use std::sync::{Arc, Mutex};
        use tokio_tungstenite::tungstenite::Message;
        for mode in ["normal", "error", "cancel-evaluate", "cancel-box"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let observed = Arc::new(Mutex::new((String::new(), Vec::new())));
            let released = Arc::new(tokio::sync::Notify::new());
            let stalled = Arc::new(tokio::sync::Notify::new());
            let (seen, done, paused) = (observed.clone(), released.clone(), stalled.clone());
            let peer = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
                while let Some(Ok(Message::Text(text))) = ws.next().await {
                    let command: Value = serde_json::from_str(&text).unwrap();
                    let method = command["method"].as_str().unwrap();
                    let result = match method {
                        "Target.attachToTarget" => json!({"sessionId":"session"}),
                        "Target.getTargets" => json!({"targetInfos":[]}),
                        "Page.getFrameTree" => {
                            json!({"frameTree":{"frame":{"id":"root","url":"http://fixture.test"}}})
                        }
                        "Page.createIsolatedWorld" => json!({"executionContextId":1}),
                        "Runtime.evaluate" => {
                            seen.lock().unwrap().0 = command["params"]["objectGroup"]
                                .as_str()
                                .unwrap()
                                .to_string();
                            if mode == "cancel-evaluate" {
                                paused.notify_one();
                                continue;
                            }
                            json!({"result":{"type":"object","objectId":"fixture-element"}})
                        }
                        "DOM.getBoxModel" => {
                            if mode == "cancel-box" {
                                paused.notify_one();
                                continue;
                            }
                            if mode == "error" {
                                ws.send(Message::Text(json!({"id":command["id"],"error":{"code":-32000,"message":"detached"}}).to_string().into())).await.unwrap();
                                continue;
                            }
                            json!({"model":{"content":[0,0,20,0,20,10,0,10]}})
                        }
                        "Runtime.releaseObjectGroup" => {
                            seen.lock().unwrap().1.push(
                                command["params"]["objectGroup"]
                                    .as_str()
                                    .unwrap()
                                    .to_string(),
                            );
                            done.notify_one();
                            json!({})
                        }
                        _ => panic!("unexpected command: {method}"),
                    };
                    ws.send(Message::Text(
                        json!({"id":command["id"],"result":result})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                }
            });
            let transport = crate::cdp::Transport::connect(&url, 30).await.unwrap();
            let connection = crate::cdp::Connection::new(transport);
            let session = connection.attach_to_target("root").await.unwrap();
            let page = Page::new(session, Arc::new(crate::StealthConfig::default()));
            if mode.starts_with("cancel") {
                let operation = async {
                    let quad = page.frame_element_content_quad("root", "img", 0);
                    tokio::pin!(quad);
                    tokio::select! {
                        _ = &mut quad => panic!("fixture should stall"),
                        _ = stalled.notified() => {},
                    }
                };
                tokio::time::timeout(std::time::Duration::from_secs(2), operation)
                    .await
                    .unwrap();
            } else {
                let result = page.frame_element_content_quad("root", "img", 0).await;
                assert_eq!(result.is_ok(), mode == "normal");
            }
            tokio::time::timeout(std::time::Duration::from_secs(2), released.notified())
                .await
                .unwrap();
            let observed = observed.lock().unwrap();
            assert!(!observed.0.is_empty());
            assert_eq!(observed.1, vec![observed.0.clone()]);
            peer.abort();
        }
    }

    #[test]
    fn only_known_discovery_races_are_retried() {
        assert!(frame_discovery_raced(&Error::cdp_msg(
            "No target with given id found"
        )));
        assert!(frame_discovery_raced(&Error::cdp_msg(
            "Session with given id not found"
        )));
        assert!(!frame_discovery_raced(&Error::cdp_msg(
            "Invalid parameters"
        )));
        assert!(!frame_discovery_raced(&Error::Decode(
            "No target with given id found".into()
        )));
    }
}
