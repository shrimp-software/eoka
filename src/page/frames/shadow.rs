//! Clicking elements inside (closed) shadow roots, in the top frame or any
//! child frame.

#[allow(unused_imports)]
use super::*;

/// Default deadline for resolving a shadow-root target.
const SHADOW_CLICK_TIMEOUT: Duration = Duration::from_secs(10);
const SHADOW_POLL_INTERVAL: Duration = Duration::from_millis(200);
const SHADOW_SELECTOR_LIMIT: usize = 4096;
const SHADOW_CLICK_HOLD: Duration = Duration::from_millis(60);

struct ShadowFrame {
    frame_id: String,
    session: Session,
    /// Isolated world used to prove a search result belongs to this frame.
    context: i64,
    /// True when the session is the frame's own target. Box-model coordinates
    /// are frame-local only for OOPIFs; same-process frames report them in the
    /// owning (top) session's viewport.
    oopif: bool,
}

/// Best-effort release if the click future is cancelled between press and
/// release. Individual input coordination does not cover a dropped future.
struct HeldInputGuard {
    page: Option<Page>,
}

impl HeldInputGuard {
    fn new(page: Page) -> Self {
        Self { page: Some(page) }
    }

    fn disarm(mut self) {
        self.page = None;
    }
}

impl Drop for HeldInputGuard {
    fn drop(&mut self) {
        if let Some(page) = self.page.take() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = page.release_all_inputs().await;
                });
            }
        }
    }
}

/// Transient CDP failures during discovery, navigation or frame mapping are
/// retried until the shadow deadline; protocol errors such as an invalid
/// selector propagate immediately.
fn shadow_retryable(error: &Error) -> bool {
    if frame_discovery_raced(error) {
        return true;
    }
    match error {
        Error::Cdp { message, .. } => [
            "Cannot find context with specified id",
            "Execution context was destroyed",
            "Inspected target navigated or closed",
            "Node with given id does not belong to the document",
            "Frame point is outside a viewport in its ancestor chain",
            "Frame owner could not be resolved",
        ]
        .iter()
        .any(|text| message.contains(text)),
        _ => false,
    }
}

impl Page {
    /// Click an element inside any shadow root in the top frame.
    ///
    /// The selector is evaluated in the light DOM and in every shadow root in
    /// the document, including closed roots. The target is re-resolved until
    /// the deadline; the click uses human-like movement after mapping the
    /// node's center to top-level viewport coordinates. The click is serialized
    /// with the action gate and releases held input if cancelled.
    pub async fn click_shadow(&self, selector: &str) -> Result<ShadowClickReceipt> {
        self.click_shadow_with_timeout(None, selector, SHADOW_CLICK_TIMEOUT)
            .await
    }

    /// Click an element inside any shadow root in `frame_id`.
    ///
    /// Use IDs from [`Page::frames`]. The selector is evaluated in the frame's
    /// light DOM and in every shadow root in that document, including closed
    /// roots. Results that belong to another frame sharing the same CDP target
    /// are rejected.
    pub async fn click_shadow_in_frame(
        &self,
        frame_id: &str,
        selector: &str,
    ) -> Result<ShadowClickReceipt> {
        self.click_shadow_with_timeout(Some(frame_id), selector, SHADOW_CLICK_TIMEOUT)
            .await
    }

    /// Click a shadow-root element with an explicit deadline.
    ///
    /// `frame_id` defaults to the top frame. The selector is re-evaluated every
    /// 200 ms until it resolves and maps to a visible viewport point. Closed
    /// shadow roots are traversed through CDP's DOM search; no JavaScript
    /// shadow-root access is required. Coordinates are mapped through the frame
    /// chain only for OOPIFs: same-process frames report box-model coordinates
    /// in the owning session's viewport. The deadline covers discovery, DOM
    /// initialization, mapping and dispatch. Coordinates are mapped without an
    /// overlay hit-test: closed shadow roots hide their internals from
    /// `elementFromPoint`, so the caller owns occlusion checks.
    pub async fn click_shadow_with_timeout(
        &self,
        frame_id: Option<&str>,
        selector: &str,
        timeout: Duration,
    ) -> Result<ShadowClickReceipt> {
        let selector = selector.trim();
        if selector.is_empty() || selector.len() > SHADOW_SELECTOR_LIMIT {
            return Err(Error::cdp_msg(
                "Shadow selector must be 1..=4096 characters",
            ));
        }
        if timeout.is_zero() {
            return Err(Error::Timeout(
                "Shadow click deadline must be positive".into(),
            ));
        }
        let deadline = tokio::time::Instant::now() + timeout;
        let operation = async {
            // Serialize with the action engine and other shadow clicks.
            let state = self.session.action_state();
            let _gate = state.gate.clone().lock_owned().await;
            if state.cleanup_failed.load(Ordering::Acquire) {
                return Err(Error::InputState(
                    "Previous action input cleanup failed; release_all_inputs is required".into(),
                ));
            }
            // Keep the snapshot alive: its AttachedFrame guards detach OOPIF sessions.
            let snapshot = Snapshot::capture(self).await?;
            let requested = frame_id
                .map(str::to_owned)
                .or_else(|| snapshot.order.first().cloned())
                .ok_or_else(|| Error::cdp_msg("Page has no frames"))?;
            let entry = snapshot.get(&requested)?;
            let oopif = entry.parent.is_some() && entry.session.target_id() == requested;
            let session = entry.session.clone();
            session.send::<_, Value>("DOM.enable", &json!({})).await?;
            // Prime the DOM agent's document tree; performSearch needs it.
            session
                .send::<_, Value>("DOM.getDocument", &json!({"depth": -1, "pierce": true}))
                .await?;
            let context = session.create_isolated_world(&requested).await?;
            let frame = ShadowFrame {
                frame_id: requested,
                session,
                context,
                oopif,
            };
            let mut last = String::from("selector not found in any shadow root");
            loop {
                match self.shadow_click_point(&frame, selector).await {
                    Ok(Some((x, y))) => {
                        // Movement is button-free; the guard covers the press.
                        Human::new(&self.session).move_to(x, y).await?;
                        let guard = HeldInputGuard::new(self.clone());
                        self.mouse_down(x, y, MouseButton::Left).await?;
                        tokio::time::sleep(SHADOW_CLICK_HOLD).await;
                        self.mouse_up(x, y, MouseButton::Left).await?;
                        guard.disarm();
                        return Ok(ShadowClickReceipt {
                            frame_id: frame.frame_id.clone(),
                            selector: selector.to_string(),
                            x,
                            y,
                        });
                    }
                    Ok(None) => {}
                    Err(error) if shadow_retryable(&error) => last = error.to_string(),
                    Err(error) => return Err(error),
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(Error::Timeout(format!(
                        "Shadow selector {selector:?} was not clickable in frame {} within {timeout:?}: {last}",
                        frame.frame_id
                    )));
                }
                tokio::time::sleep(SHADOW_POLL_INTERVAL).await;
            }
        };
        match tokio::time::timeout_at(deadline, operation).await {
            Ok(result) => result,
            Err(_) => Err(Error::Timeout(format!(
                "Shadow click did not complete within {timeout:?}"
            ))),
        }
    }

    async fn shadow_click_point(
        &self,
        frame: &ShadowFrame,
        selector: &str,
    ) -> Result<Option<(f64, f64)>> {
        let Some(node_id) = Self::find_shadow_node(frame, selector).await? else {
            return Ok(None);
        };
        let (x, y) = Self::shadow_node_center(&frame.session, node_id).await?;
        if frame.oopif {
            Ok(Some(
                self.frame_point_to_viewport(&frame.frame_id, x, y).await?,
            ))
        } else {
            // Same-process frames and the top frame report box-model
            // coordinates in the owning session's viewport.
            Ok(Some((x, y)))
        }
    }

    async fn find_shadow_node(frame: &ShadowFrame, selector: &str) -> Result<Option<i32>> {
        // CDP's DOM search traverses closed shadow roots and returns allocated
        // node IDs; DOM.querySelector cannot cross a shadow boundary. A
        // same-process child shares the parent target, so the search may return
        // parent or sibling nodes: every candidate is checked against the
        // requested frame's document before it is accepted.
        let search: Value = frame
            .session
            .send(
                "DOM.performSearch",
                &json!({"query": selector, "includeUserAgentShadowDOM": false}),
            )
            .await?;
        let search_id = search
            .get("searchId")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::cdp_msg("Shadow search returned no search id"))?;
        let count = search
            .get("resultCount")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let mut found = None;
        if count > 0 {
            let results: Value = frame
                .session
                .send(
                    "DOM.getSearchResults",
                    &json!({"searchId": search_id, "fromIndex": 0, "toIndex": count.min(64)}),
                )
                .await?;
            if let Some(node_ids) = results.get("nodeIds").and_then(Value::as_array) {
                for node_id in node_ids.iter().filter_map(Value::as_i64) {
                    if Self::node_in_frame(frame, node_id as i32).await? {
                        found = Some(node_id as i32);
                        break;
                    }
                }
            }
        }
        let _: Result<Value> = frame
            .session
            .send("DOM.discardSearchResults", &json!({"searchId": search_id}))
            .await;
        Ok(found)
    }

    /// Prove a search result belongs to the requested frame. The comparison
    /// runs in the frame's isolated world, so a same-process sibling or parent
    /// node cannot be mistaken for the requested frame's node.
    async fn node_in_frame(frame: &ShadowFrame, node_id: i32) -> Result<bool> {
        let objects = FrameObjects::new(frame.session.clone());
        let resolved = match frame
            .session
            .send::<_, Value>(
                "DOM.resolveNode",
                &json!({
                    "nodeId": node_id,
                    "executionContextId": frame.context,
                    "objectGroup": objects.group,
                }),
            )
            .await
        {
            Ok(value) => value,
            Err(error) if shadow_retryable(&error) => return Err(error),
            Err(_) => {
                objects.release().await.ok();
                return Ok(false);
            }
        };
        let Some(object_id) = resolved
            .get("object")
            .and_then(|object| object.get("objectId"))
            .and_then(Value::as_str)
        else {
            objects.release().await.ok();
            return Ok(false);
        };
        let owned = match frame
            .session
            .send::<_, Value>(
                "Runtime.callFunctionOn",
                &json!({
                    "objectId": object_id,
                    "functionDeclaration": "function(){ return this.ownerDocument === document; }",
                    "returnByValue": true,
                }),
            )
            .await
        {
            Ok(value) => value,
            Err(error) if shadow_retryable(&error) => return Err(error),
            Err(_) => {
                objects.release().await.ok();
                return Ok(false);
            }
        };
        objects.release().await.ok();
        Ok(owned
            .get("result")
            .and_then(|result| result.get("value"))
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    async fn shadow_node_center(session: &Session, node_id: i32) -> Result<(f64, f64)> {
        let model: Value = session
            .send("DOM.getBoxModel", &json!({"nodeId": node_id}))
            .await?;
        let quad = model
            .get("model")
            .and_then(|model| model.get("content"))
            .and_then(Value::as_array)
            .map(|quad| quad.iter().filter_map(Value::as_f64).collect::<Vec<_>>())
            .unwrap_or_default();
        if quad.len() < 8 || quad.iter().any(|value| !value.is_finite()) {
            return Err(Error::cdp_msg("Shadow node has no finite content quad"));
        }
        let area = (0..4)
            .map(|index| {
                let next = (index + 1) % 4;
                quad[index * 2] * quad[next * 2 + 1] - quad[next * 2] * quad[index * 2 + 1]
            })
            .sum::<f64>();
        if !area.is_finite() || area.abs() < 1e-8 {
            return Err(Error::cdp_msg("Shadow node content quad is degenerate"));
        }
        Ok((
            (quad[0] + quad[2] + quad[4] + quad[6]) / 4.0,
            (quad[1] + quad[3] + quad[5] + quad[7]) / 4.0,
        ))
    }
}
