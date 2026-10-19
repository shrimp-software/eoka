use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use super::{FrameInfo, Page, ShadowClickReceipt};
use crate::cdp::{Frame, FrameTree, RuntimeEvaluateResult, Session, TargetGetTargets};
use crate::error::{Error, Result};
use crate::stealth::human::Human;
use crate::MouseButton;

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

struct FrameObjects {
    session: Session,
    group: String,
    active: bool,
}

impl FrameObjects {
    fn new(session: Session) -> Self {
        static NEXT_GROUP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            session,
            group: format!(
                "eoka-frame-objects-{}",
                NEXT_GROUP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
            active: true,
        }
    }

    async fn release(mut self) -> Result<()> {
        release_frame_objects(&self.session, &self.group).await?;
        self.active = false;
        Ok(())
    }
}

async fn release_frame_objects(session: &Session, group: &str) -> Result<()> {
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        session.send::<_, Value>("Runtime.releaseObjectGroup", &json!({"objectGroup":group})),
    )
    .await
    .map_err(|_| Error::cdp_msg("Frame object cleanup timed out"))??;
    Ok(())
}

impl Drop for FrameObjects {
    fn drop(&mut self) {
        if self.active {
            let session = self.session.clone();
            let group = self.group.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    if release_frame_objects(&session, &group).await.is_err() {
                        tracing::debug!("Frame object cleanup unconfirmed");
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Owner {
    backend_node_id: i64,
}

#[derive(Deserialize)]
struct Geometry {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale_x: f64,
    scale_y: f64,
    unsupported: bool,
}

const OWNER_GEOMETRY: &str = r#"function() {
    let unsupported = typeof this.checkVisibility !== 'function' || !this.checkVisibility({checkOpacity:true,checkVisibilityCSS:true,contentVisibilityAuto:true});
    let depth = 0;
    for (let e = this; e; e = e.assignedSlot || e.parentElement || e.getRootNode().host) {
        if (++depth > 64) { unsupported = true; break; }
        const s = getComputedStyle(e);
        if (s.transform !== 'none') {
            const m = new DOMMatrixReadOnly(s.transform);
            if (!m.is2D || Math.abs(m.b) > 1e-8 || Math.abs(m.c) > 1e-8 || m.a <= 0 || m.d <= 0) unsupported = true;
        }
        if (s.scale && s.scale !== 'none' && s.scale.split(/\s+/).some(v => !(parseFloat(v) > 0))) unsupported = true;
        if (s.perspective !== 'none' || (s.rotate && s.rotate !== 'none' && s.rotate !== '0deg') ||
            s.visibility !== 'visible' || s.display === 'none' || Number(s.opacity) === 0 ||
            s.contentVisibility === 'hidden') unsupported = true;
    }
    const s = getComputedStyle(this), r = this.getBoundingClientRect();
    const n = v => parseFloat(v) || 0;
    const pl=n(s.paddingLeft), pr=n(s.paddingRight), pt=n(s.paddingTop), pb=n(s.paddingBottom);
    const bl=n(s.borderLeftWidth), br=n(s.borderRightWidth), bt=n(s.borderTopWidth), bb=n(s.borderBottomWidth);
    const bw=n(s.width)+(s.boxSizing==='border-box'?0:pl+pr+bl+br);
    const bh=n(s.height)+(s.boxSizing==='border-box'?0:pt+pb+bt+bb);
    const sx=bw>0?r.width/bw:0, sy=bh>0?r.height/bh:0;
    return {x:r.left+(bl+pl)*sx, y:r.top+(bt+pt)*sy,
        width:bw-pl-pr-bl-br, height:bh-pt-pb-bt-bb, scale_x:sx, scale_y:sy,
        unsupported:unsupported || !this.getClientRects().length};
}"#;

const OWNER_HIT_TEST: &str = r#"function(x, y) {
    let hit = this.ownerDocument.elementFromPoint(x, y);
    for (let depth = 0; hit?.shadowRoot && depth < 64; depth++) {
        const inner = hit.shadowRoot.elementFromPoint(x, y);
        if (inner === hit) break;
        hit = inner;
    }
    return hit === this;
}"#;

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

    pub(crate) async fn frame_target_id(&self, frame_id: &str) -> Result<String> {
        let snapshot = Snapshot::capture(self).await?;
        Ok(snapshot.get(frame_id)?.session.target_id().to_owned())
    }

    /// Convert frame-local CSS coordinates to top-level viewport coordinates.
    /// Accounts for borders, padding, scrolling, positive axis-aligned scale
    /// and translation. Unsupported owner transforms and hidden owners fail,
    /// including reflection through closed shadow slots. Points outside any
    /// ancestor viewport are rejected. This is not an overlay hit-test guarantee.
    pub async fn frame_point_to_viewport(
        &self,
        frame_id: &str,
        x: f64,
        y: f64,
    ) -> Result<(f64, f64)> {
        self.map_frame_point(frame_id, x, y, false).await
    }

    /// Map a frame-local input point to the root viewport, rejecting covered iframe owners.
    /// Each parent document must hit its owning iframe at the mapped point.
    /// Includes the geometry checks of [`Page::frame_point_to_viewport`].
    /// Does not hit-test a leaf element or prevent mutations after this snapshot;
    /// callers must validate their target and dispatch input without changing the point.
    /// Inaccessible shadow-root hit paths fail closed.
    pub async fn frame_point_for_input(
        &self,
        frame_id: &str,
        x: f64,
        y: f64,
    ) -> Result<(f64, f64)> {
        self.map_frame_point(frame_id, x, y, true).await
    }

    async fn map_frame_point(
        &self,
        frame_id: &str,
        x: f64,
        y: f64,
        hit_test: bool,
    ) -> Result<(f64, f64)> {
        if !x.is_finite() || !y.is_finite() {
            return Err(Error::cdp_msg("Frame coordinates must be finite"));
        }
        let snapshot = Snapshot::capture(self).await?;
        let mut id = frame_id;
        let (mut x, mut y) = (x, y);
        loop {
            let entry = snapshot.get(id)?;
            let context = entry.session.create_isolated_world(id).await?;
            let size: [f64; 2] = self.eval_impl(
                entry
                    .session
                    .evaluate_in_context("[innerWidth, innerHeight]", context)
                    .await?,
            )?;
            if x < 0.0 || y < 0.0 || x >= size[0] || y >= size[1] {
                return Err(Error::cdp_msg(
                    "Frame point is outside a viewport in its ancestor chain",
                ));
            }
            let Some(parent_id) = entry.parent.as_deref() else {
                return Ok((x, y));
            };
            let parent = snapshot.get(parent_id)?;
            let owner: Owner = parent
                .session
                .send("DOM.getFrameOwner", &json!({"frameId": id}))
                .await?;
            let parent_context = parent.session.create_isolated_world(parent_id).await?;
            let objects = FrameObjects::new(parent.session.clone());
            let resolved: Value = parent.session.send("DOM.resolveNode", &json!({
                "backendNodeId": owner.backend_node_id, "executionContextId": parent_context,
                "objectGroup":objects.group,
            })).await?;
            let object_id = resolved["object"]["objectId"]
                .as_str()
                .ok_or_else(|| Error::cdp_msg("Frame owner could not be resolved"))?;
            let model: Value = parent
                .session
                .send("DOM.getBoxModel", &json!({"objectId":object_id}))
                .await?;
            let quad: [f64; 8] = serde_json::from_value(model["model"]["content"].clone())?;
            if quad.iter().any(|v| !v.is_finite())
                || quad[2] <= quad[0]
                || quad[5] <= quad[3]
                || (quad[1] - quad[3]).abs() > 1e-6
                || (quad[2] - quad[4]).abs() > 1e-6
                || (quad[5] - quad[7]).abs() > 1e-6
                || (quad[6] - quad[0]).abs() > 1e-6
            {
                return Err(Error::cdp_msg("Unsupported rendered frame owner transform"));
            }
            let result: Result<RuntimeEvaluateResult> = parent
                .session
                .send(
                    "Runtime.callFunctionOn",
                    &json!({
                        "objectId": object_id, "functionDeclaration": OWNER_GEOMETRY,
                        "returnByValue": true,
                    }),
                )
                .await;
            let geometry: Geometry = self.eval_impl(result?)?;
            if geometry.unsupported
                || geometry.width <= 0.0
                || geometry.height <= 0.0
                || !geometry.scale_x.is_finite()
                || !geometry.scale_y.is_finite()
                || geometry.scale_x <= 0.0
                || geometry.scale_y <= 0.0
            {
                return Err(Error::cdp_msg(
                    "Unsupported transformed or hidden frame owner",
                ));
            }
            if (geometry.width - size[0]).abs() > 1.0 || (geometry.height - size[1]).abs() > 1.0 {
                return Err(Error::cdp_msg("Frame owner and viewport dimensions differ"));
            }
            x = geometry.x + x * geometry.scale_x;
            y = geometry.y + y * geometry.scale_y;
            if hit_test {
                let result: RuntimeEvaluateResult = parent
                    .session
                    .send(
                        "Runtime.callFunctionOn",
                        &json!({
                            "objectId": object_id,
                            "functionDeclaration": OWNER_HIT_TEST,
                            "arguments":[{"value":x},{"value":y}],
                            "returnByValue":true,
                        }),
                    )
                    .await?;
                if !self.eval_impl::<bool>(result)? {
                    return Err(Error::cdp_msg(
                        "Frame input point is covered by another element",
                    ));
                }
            }
            objects.release().await?;
            id = parent_id;
        }
    }

    /// Get this page's frame tree, including nested out-of-process iframe targets.
    pub async fn frames(&self) -> Result<Vec<FrameInfo>> {
        self.nested_frame_infos().await
    }

    /// Evaluate JavaScript in an iframe selected by CSS in this page's top document.
    ///
    /// The selector must match exactly one `iframe` or `frame` element. Frame URLs
    /// and numeric indices are not selectors; use [`Page::evaluate_in_frame_id`]
    /// with IDs from [`Page::frames`] for nested frames or explicit selection.
    /// Cross-origin/OOPIF frames are routed through CDP, in an isolated world:
    /// DOM access works, but page-owned JavaScript globals are not available.
    /// No parent-to-child `contentWindow` access or `Function` constructor is used.
    ///
    /// `expression` is executed as code. Do not supply untrusted code.
    pub async fn evaluate_in_frame<T: serde::de::DeserializeOwned>(
        &self,
        frame_selector: &str,
        expression: &str,
    ) -> Result<T> {
        if frame_selector.len() > 4096 {
            return Err(Error::cdp_msg("Frame selector exceeds 4096 bytes"));
        }
        let root = self.session.get_frame_tree().await?.frame.id;
        let context = self.session.create_isolated_world(&root).await?;
        let objects = FrameObjects::new(self.session.clone());
        let selector = serde_json::to_string(frame_selector)?;
        let selection = format!("(() => {{ const matches=document.querySelectorAll({selector}); if(matches.length!==1 || !['iframe','frame'].includes(matches[0].localName)) throw new Error('Frame selector must match exactly one iframe or frame element'); return matches[0]; }})()");
        let remote: RuntimeEvaluateResult = self
            .session
            .send(
                "Runtime.evaluate",
                &json!({
                    "expression":selection, "contextId":context, "objectGroup":objects.group,
                    "returnByValue":false, "awaitPromise":false,
                }),
            )
            .await?;
        let object_id = self
            .check_js_result(remote)?
            .object_id
            .ok_or_else(|| Error::cdp_msg("Frame element unavailable"))?;
        let described: Value = self
            .session
            .send("DOM.describeNode", &json!({"objectId":object_id}))
            .await?;
        let frame_id = described["node"]["frameId"]
            .as_str()
            .ok_or_else(|| Error::cdp_msg("Selected frame has no attached document"))?;
        objects.release().await?;
        self.evaluate_in_frame_id(frame_id, expression).await
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
        let objects = FrameObjects::new(entry.session.clone());
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
    async fn frame_objects_release_on_success_error_and_cancelled_replies() {
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
