use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

use super::{Action, ActionOptions, ActionReceipt, LocatorQuery, LocatorSpec};
use crate::{cdp::Session, Error, Page, Result};

/// A read-only observation of a resolved control, without its input value.
#[derive(Debug, Clone)]
pub struct ControlState {
    /// Whether the node is attached to the document.
    pub connected: bool,
    /// Whether the node is rendered and visible.
    pub visible: bool,
    /// Whether the node is not disabled.
    pub enabled: bool,
}

/// A lazy query bound to a page; it holds no cached DOM node.
#[derive(Clone)]
pub struct Locator {
    pub(crate) page: Page,
    pub(crate) spec: LocatorSpec,
}

impl Locator {
    /// Inspect the current match without dispatching input or generating a selector.
    pub async fn state(&self) -> Result<Option<ControlState>> {
        let Some(node) = resolve(&self.page, &self.spec).await? else {
            return Ok(None);
        };
        let properties = node.properties().await?;
        Ok(Some(ControlState {
            connected: properties["connected"] == true,
            visible: properties["visible"] == true,
            enabled: properties["disabled"] != true,
        }))
    }

    /// Explicitly select the first matching element.
    pub fn first(self) -> Self {
        self.nth(0)
    }
    /// Explicitly select a zero-based match.
    pub fn nth(mut self, index: usize) -> Self {
        self.spec.index = Some(index);
        self
    }
    /// Restrict resolution to a pinned frame identifier.
    pub fn in_frame(mut self, frame_id: impl Into<String>) -> Self {
        self.spec.frame_id = Some(frame_id.into());
        self
    }
    /// Return the serializable query.
    pub fn specification(&self) -> LocatorSpec {
        self.spec.clone()
    }

    /// Click once and verify the supplied completion condition.
    pub async fn click(&self, options: ActionOptions) -> Result<ActionReceipt> {
        self.page
            .act(
                Action::Click {
                    target: self.spec.clone(),
                    human: false,
                    double: false,
                },
                options,
            )
            .await
    }
    /// Click with paced input and verify the supplied condition.
    pub async fn human_click(&self, options: ActionOptions) -> Result<ActionReceipt> {
        self.page
            .act(
                Action::Click {
                    target: self.spec.clone(),
                    human: true,
                    double: false,
                },
                options,
            )
            .await
    }
    /// Replace and verify the target value.
    pub async fn fill(&self, value: &str, options: ActionOptions) -> Result<ActionReceipt> {
        self.page
            .act(
                Action::Fill {
                    target: self.spec.clone(),
                    value: value.into(),
                    input_mode: Default::default(),
                },
                options,
            )
            .await
    }
    /// Set and verify selected option values.
    pub async fn select(
        &self,
        values: Vec<String>,
        options: ActionOptions,
    ) -> Result<ActionReceipt> {
        self.page
            .act(
                Action::Select {
                    target: self.spec.clone(),
                    values,
                },
                options,
            )
            .await
    }
    /// Set and verify the uploaded files.
    pub async fn upload(
        &self,
        files: Vec<String>,
        options: ActionOptions,
    ) -> Result<ActionReceipt> {
        self.page
            .act(
                Action::Upload {
                    target: self.spec.clone(),
                    files,
                },
                options,
            )
            .await
    }
}

impl Page {
    /// Create a lazy CSS locator requiring a unique match.
    pub fn locator(&self, selector: &str) -> Locator {
        self.locator_spec(LocatorSpec::css(selector))
    }
    /// Create a lazy, case-insensitive text locator.
    pub fn locator_text(&self, text: &str) -> Locator {
        self.locator_spec(LocatorSpec::text(text))
    }
    /// Bind a serializable locator to this page.
    pub fn locator_spec(&self, spec: LocatorSpec) -> Locator {
        Locator {
            page: self.clone(),
            spec,
        }
    }
}

pub(super) struct Node {
    pub session: Session,
    pub object: String,
    pub frame_id: Option<String>,
    group: String,
    attached: bool,
    hover_observer: Option<String>,
}

impl Node {
    pub async fn call(&self, body: &str, args: Vec<Value>) -> Result<Value> {
        let result: Value = self.session.send("Runtime.callFunctionOn", &json!({
            "objectId":self.object, "functionDeclaration":body,
            "arguments":args.into_iter().map(|value| json!({"value":value})).collect::<Vec<_>>(),
            "returnByValue":true, "awaitPromise":true,
        })).await?;
        if let Some(exception) = result.get("exceptionDetails").filter(|v| !v.is_null()) {
            let exception: crate::cdp::types::ExceptionDetails =
                serde_json::from_value(exception.clone())?;
            return Err(Error::cdp_msg(exception.message()));
        }
        Ok(result["result"]["value"].clone())
    }

    pub async fn properties(&self) -> Result<Value> {
        let mut properties = self.call(include_str!("properties.js"), vec![]).await?;
        if let Some(observer) = &self.hover_observer {
            let result: Value = self.session.send("Runtime.callFunctionOn", &json!({"objectId":observer,"functionDeclaration":"function(){return this.seen}","returnByValue":true})).await?;
            properties["hovered"] = result["result"]["value"].clone();
        }
        Ok(properties)
    }

    pub async fn arm_hover(&mut self) -> Result<()> {
        let result: Value = self.session.send("Runtime.callFunctionOn", &json!({"objectId":self.object,"objectGroup":self.group,"returnByValue":false,
            "functionDeclaration":"function(){const target=this;const state={seen:false};const doc=this.ownerDocument;const handler=e=>{if(e.isTrusted && (e.target===target || target.contains(e.target))){state.seen=true;state.dispose()}};const timer=setTimeout(()=>state.dispose(),30000);state.dispose=()=>{clearTimeout(timer);doc.removeEventListener('mousemove',handler,true);doc.removeEventListener('mouseover',handler,true)};doc.addEventListener('mousemove',handler,true);doc.addEventListener('mouseover',handler,true);return state}"})).await?;
        self.hover_observer = Some(
            result["result"]["objectId"]
                .as_str()
                .ok_or_else(|| Error::cdp_msg("Could not arm hover observation"))?
                .into(),
        );
        Ok(())
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let session = self.session.clone();
        let group = std::mem::take(&mut self.group);
        let attached = self.attached;
        let observer = self.hover_observer.take();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    if let Some(observer)=observer {
                        let _=session.send::<_,Value>("Runtime.callFunctionOn",&json!({"objectId":observer,"functionDeclaration":"function(){this.dispose()}"})).await;
                    }
                    let _ = session
                        .send::<_, Value>(
                            "Runtime.releaseObjectGroup",
                            &json!({"objectGroup":group}),
                        )
                        .await;
                    if attached {
                        let _ = session.detach().await;
                    }
                })
                .await;
            });
        }
    }
}

pub(super) async fn resolve(page: &Page, spec: &LocatorSpec) -> Result<Option<Node>> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let mut node = Node {
        session: page.session().clone(),
        object: String::new(),
        frame_id: spec.frame_id.clone(),
        group: format!("eoka-action-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
        attached: false,
        hover_observer: None,
    };
    let mut context = None;
    if let Some(frame_id) = &spec.frame_id {
        let target = page.frame_target_id(frame_id).await?;
        if target != page.target_id() {
            node.session = page.session().attach_frame_target(&target).await?;
            node.attached = true;
        }
        context = Some(node.session.create_isolated_world(frame_id).await?);
    }
    let pinned = match spec.query {
        LocatorQuery::BackendNode { id } => Some(json!({"backendNodeId": id})),
        LocatorQuery::Node { id } => Some(json!({"nodeId": id})),
        LocatorQuery::Css { .. }
        | LocatorQuery::Text { .. }
        | LocatorQuery::Placeholder { .. }
        | LocatorQuery::Role { .. } => None,
    };
    if let Some(mut params) = pinned {
        params["objectGroup"] = json!(node.group);
        let result: Value = node.session.send("DOM.resolveNode", &params).await?;
        node.object = result["object"]["objectId"]
            .as_str()
            .ok_or_else(|| Error::cdp_msg("Pinned node is stale"))?
            .into();
        if !node
            .call("function(){return this.isConnected === true}", vec![])
            .await?
            .as_bool()
            .unwrap_or(false)
        {
            return Err(Error::cdp_msg("Pinned node is stale"));
        }
        return Ok(Some(node));
    }
    let query = serde_json::to_string(&spec.query)?;
    let expression = format!("({})({query})", include_str!("resolve.js"));
    let mut params =
        json!({"expression":expression,"objectGroup":node.group,"returnByValue":false});
    if let Some(context) = context {
        params["contextId"] = json!(context);
    }
    let result: Value = node.session.send("Runtime.evaluate", &params).await?;
    if result.get("exceptionDetails").is_some() {
        return Err(Error::cdp_msg("Invalid locator query"));
    }
    node.object = result["result"]["objectId"]
        .as_str()
        .ok_or_else(|| Error::cdp_msg("Locator did not return an array"))?
        .into();
    let count = node
        .call("function(){return this.length}", vec![])
        .await?
        .as_u64()
        .ok_or_else(|| Error::cdp_msg("Invalid locator count"))? as usize;
    let index = spec.index.unwrap_or(0);
    if count == 0 || index >= count {
        return Ok(None);
    }
    if count > 1 && spec.index.is_none() {
        return Err(Error::cdp_msg(
            "ambiguous_target: locator matched multiple elements; select first/nth explicitly",
        ));
    }
    let result: Value = node
        .session
        .send(
            "Runtime.callFunctionOn",
            &json!({
                "objectId":node.object,"functionDeclaration":"function(i){return this[i]}",
                "arguments":[{"value":index}],"returnByValue":false,"objectGroup":node.group,
            }),
        )
        .await?;
    node.object = result["result"]["objectId"]
        .as_str()
        .ok_or_else(|| Error::cdp_msg("Locator element disappeared"))?
        .into();
    Ok(Some(node))
}
