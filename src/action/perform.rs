use super::{
    dispatch::Dispatch,
    locator::{resolve, Node},
    Action,
};
use crate::{page::ActionInput, Error, Page, Result};
use serde_json::{json, Value};

pub(super) enum Verification {
    Properties {
        field: &'static str,
        expected: Value,
    },
    Navigation {
        loader: Option<String>,
    },
    Explicit,
    Noop,
}

pub(super) async fn mutation(
    node: &Node,
    dispatch: &Dispatch,
    function: &str,
    args: Vec<Value>,
    preparation: bool,
) -> Result<Value> {
    let result = dispatch.send(&node.session, "Runtime.callFunctionOn", json!({
        "objectId":node.object,"functionDeclaration":function,"returnByValue":true,"awaitPromise":true,
        "arguments":args.into_iter().map(|value|json!({"value":value})).collect::<Vec<_>>(),
    }), preparation).await?;
    if let Some(exception) = result.get("exceptionDetails").filter(|v| !v.is_null()) {
        let exception: crate::cdp::types::ExceptionDetails =
            serde_json::from_value(exception.clone())?;
        return Err(Error::cdp_msg(exception.message()));
    }
    Ok(result["result"]["value"].clone())
}

pub(super) async fn prepare(
    page: &Page,
    action: &Action,
    dispatch: &Dispatch,
    wait: bool,
) -> Result<Option<Node>> {
    let Some(target) = action.target() else {
        return Ok(None);
    };
    loop {
        let Some(node) = resolve(page, target).await? else {
            dispatch.note("preparation: target not found");
            if !wait {
                return Err(Error::ElementNotFound("Action target".into()));
            }
            pause().await;
            continue;
        };
        let props = node.properties().await?;
        let upload = matches!(action, Action::Upload { .. });
        let editable = matches!(action, Action::Fill { .. } | Action::Type { .. });
        let pointer = matches!(
            action,
            Action::Click { .. } | Action::Hover { .. } | Action::Drag { .. }
        );
        if props["connected"] != true
            || (props["disabled"] == true
                && !matches!(action, Action::Hover { .. } | Action::ScrollIntoView { .. }))
            || (!upload && props["visible"] != true)
            || (editable && props["editable"] != true)
        {
            dispatch.note(if props["connected"] != true {
                "preparation: target disconnected"
            } else if props["disabled"] == true {
                "preparation: target disabled"
            } else if props["visible"] != true {
                "preparation: target hidden"
            } else {
                "preparation: target readonly or not editable"
            });
            if !wait {
                return Err(Error::ElementNotVisible {
                    selector: "Action target".into(),
                });
            }
            pause().await;
            continue;
        }
        if pointer || matches!(action, Action::ScrollIntoView { .. }) {
            if props["inViewport"] != true {
                mutation(&node,dispatch,"function(){this.scrollIntoView({block:'center',inline:'center',behavior:'instant'})}",vec![],true).await?;
            }
            let stable = node.call("function(){return new Promise(resolve=>requestAnimationFrame(()=>{const a=this.getBoundingClientRect();requestAnimationFrame(()=>{const b=this.getBoundingClientRect();resolve(this.isConnected && ['x','y','width','height'].every(k=>Math.abs(a[k]-b[k])<0.5))})}))}",vec![]).await?;
            let current = node.properties().await?;
            if stable != true || (pointer && current["receives"] != true) {
                dispatch.note(if stable != true {
                    "preparation: target moving"
                } else {
                    "preparation: target covered"
                });
                if !wait {
                    return Err(Error::ElementNotVisible {
                        selector: "Action target".into(),
                    });
                }
                pause().await;
                continue;
            }
        }
        dispatch.note("input dispatch");
        return Ok(Some(node));
    }
}

pub(super) async fn point(page: &Page, node: &Node, require_enabled: bool) -> Result<(f64, f64)> {
    let props = node.properties().await?;
    if props["receives"] != true || (require_enabled && props["disabled"] == true) {
        return Err(Error::cdp_msg(
            "actionability_changed: target no longer receives input",
        ));
    }
    let x = props["x"]
        .as_f64()
        .ok_or_else(|| Error::cdp_msg("Invalid input coordinate"))?;
    let y = props["y"]
        .as_f64()
        .ok_or_else(|| Error::cdp_msg("Invalid input coordinate"))?;
    if let Some(frame) = &node.frame_id {
        page.frame_point_for_input(frame, x, y)
            .await
            .map_err(|error| match error {
                Error::Cdp { ref message, .. }
                    if message == "Frame input point is covered by another element" =>
                {
                    Error::cdp_msg("actionability_changed: an iframe owner is covered")
                }
                other => other,
            })
    } else {
        Ok((x, y))
    }
}

pub(super) fn retryable_preparation(action: &Action, error: &Error) -> bool {
    let Some(target) = action.target() else {
        return false;
    };
    if matches!(
        target.query,
        super::LocatorQuery::Node { .. } | super::LocatorQuery::BackendNode { .. }
    ) {
        return false;
    }
    match error {
        Error::Cdp { message, .. } if message.starts_with("actionability_changed:") => true,
        Error::Cdp {
            method: Some(method),
            code: Some(-32000),
            message,
        } if matches!(
            method.as_str(),
            "Runtime.evaluate" | "Runtime.callFunctionOn" | "DOM.resolveNode"
        ) =>
        {
            [
                "Cannot find context",
                "Execution context was destroyed",
                "Could not find object",
                "Cannot find object",
                "No node with given id",
            ]
            .iter()
            .any(|text| message.contains(text))
        }
        _ => false,
    }
}

async fn require_focused_editable(node: &Node) -> Result<()> {
    let properties = node.properties().await?;
    if properties["connected"] != true
        || properties["focused"] != true
        || properties["editable"] != true
    {
        return Err(Error::cdp_msg(
            "actionability_changed: input target detached, lost focus or became readonly",
        ));
    }
    Ok(())
}

async fn type_into(
    node: &Node,
    input: &mut ActionInput,
    dispatch: &Dispatch,
    text: &str,
    mode: crate::InputMode,
) -> Result<()> {
    if mode == crate::InputMode::Sequential && !text.is_empty() {
        for character in text.chars() {
            require_focused_editable(node).await?;
            input
                .text(dispatch, &character.to_string(), crate::InputMode::Bulk)
                .await?;
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
        Ok(())
    } else {
        require_focused_editable(node).await?;
        input.text(dispatch, text, crate::InputMode::Bulk).await
    }
}

pub(super) async fn perform(
    page: &Page,
    action: &Action,
    node: Option<&Node>,
    input: &mut ActionInput,
    dispatch: &Dispatch,
) -> Result<Verification> {
    let required = || node.ok_or_else(|| Error::cdp_msg("Action target missing"));
    let property = |field, expected| Verification::Properties { field, expected };
    let _navigation_guard = if matches!(
        action,
        Action::Navigate { .. }
            | Action::Reload
            | Action::Back
            | Action::Forward
            | Action::History { .. }
    ) {
        Some(page.session().navigation_guard().await?)
    } else {
        None
    };
    match action {
        Action::InsertText { text } => {
            input
                .text(dispatch, text, crate::InputMode::default())
                .await?;
            Ok(Verification::Explicit)
        }
        Action::Drag { dx, dy, .. } => {
            if !dx.is_finite() || !dy.is_finite() {
                return Err(Error::cdp_msg("Invalid drag offset"));
            }
            let (x, y) = point(page, required()?, true).await?;
            input.drag(dispatch, x, y, *dx, *dy).await?;
            Ok(Verification::Explicit)
        }
        Action::SelectText { text, .. } | Action::SelectOption { value: text, .. } => {
            let node = required()?;
            let values = node.call("function(t,byValue){const options=Array.from(this.options || []).filter(o=>!o.disabled&&!o.closest('optgroup[disabled]'));const values=byValue?options.filter(o=>o.value===t):[];return (values.length?values:options.filter(o=>o.text.trim()===t)).map(o=>o.value)}", vec![json!(text),json!(matches!(action,Action::SelectOption { .. }))]).await?;
            let values = values.as_array().filter(|v| v.len() == 1).ok_or_else(|| {
                Error::cdp_msg("Select label must match exactly one enabled option")
            })?;
            let expected = json!([values[0]]);
            if node.properties().await?["selected"] == expected {
                return Ok(Verification::Noop);
            }
            mutation(node,dispatch,"function(v){this.value=v;this.dispatchEvent(new Event('input',{bubbles:true}));this.dispatchEvent(new Event('change',{bubbles:true}))}",vec![values[0].clone()],false).await?;
            Ok(property("selected", expected))
        }
        Action::Click { human, double, .. } => {
            let node = required()?;
            let (x, y) = point(page, node, true).await?;
            if *human {
                input.human_move(dispatch, x, y).await?;
            } else {
                input.move_to(dispatch, x, y).await?;
            }
            if *human {
                tokio::time::sleep(std::time::Duration::from_millis(75)).await;
            }
            let (checked_x, checked_y) = point(page, node, true).await?;
            if (x - checked_x).abs() > 0.5 || (y - checked_y).abs() > 0.5 {
                return Err(Error::cdp_msg(
                    "actionability_changed: target moved before input",
                ));
            }
            input.click(dispatch, x, y, 1, *human).await?;
            if *double {
                input.click(dispatch, x, y, 2, *human).await?;
            }
            Ok(Verification::Explicit)
        }
        Action::Fill {
            value, input_mode, ..
        } => {
            let node = required()?;
            let props = node.properties().await?;
            let expected = if props["type"] == "range" {
                let normalized = node.call("function(v){const p=this.cloneNode(false);p.value=v;return Number.isFinite(Number(v)) && p.validity.valid && Number(p.value)===Number(v) ? p.value : null}",vec![json!(value)]).await?;
                if normalized.is_null() {
                    return Err(Error::cdp_msg("Invalid range value"));
                }
                normalized
            } else {
                json!(value)
            };
            if props["value"] == expected {
                return Ok(Verification::Noop);
            }
            if props["type"] == "range" {
                mutation(node,dispatch,"function(v){this.value=v;this.dispatchEvent(new Event('input',{bubbles:true}));this.dispatchEvent(new Event('change',{bubbles:true}))}",vec![expected.clone()],false).await?;
            } else {
                mutation(node,dispatch,"function(){this.focus();if(this.isContentEditable){const r=this.ownerDocument.createRange();r.selectNodeContents(this);const s=this.ownerDocument.getSelection();s.removeAllRanges();s.addRange(r)}else{this.select()}}",vec![],true).await?;
                type_into(node, input, dispatch, value, *input_mode).await?;
            }
            Ok(property("value", expected))
        }
        Action::Type {
            text, input_mode, ..
        } => {
            mutation(
                required()?,
                dispatch,
                "function(){this.focus()}",
                vec![],
                true,
            )
            .await?;
            type_into(required()?, input, dispatch, text, *input_mode).await?;
            Ok(Verification::Explicit)
        }
        Action::Select { values, .. } => {
            let node = required()?;
            let valid = node.call("function(v){return this instanceof HTMLSelectElement && (this.multiple || v.length<=1) && v.every(x=>Array.from(this.options).some(o=>o.value===x&&!o.disabled&&!o.closest('optgroup[disabled]')))}",vec![json!(values)]).await?;
            if valid != true {
                return Err(Error::cdp_msg("Requested select options are unavailable"));
            }
            let mut expected = values.clone();
            expected.sort();
            expected.dedup();
            let mut existing: Vec<String> =
                serde_json::from_value(node.properties().await?["selected"].clone())?;
            existing.sort();
            if existing == expected {
                return Ok(Verification::Noop);
            }
            mutation(node,dispatch,"function(v){for(const o of this.options)o.selected=v.includes(o.value);this.dispatchEvent(new Event('input',{bubbles:true}));this.dispatchEvent(new Event('change',{bubbles:true}))}",vec![json!(values)],false).await?;
            Ok(property("selected", json!(expected)))
        }
        Action::Upload { files, .. } => {
            let node = required()?;
            let valid = node.call("function(n){return this instanceof HTMLInputElement && this.type==='file' && (this.multiple || n<=1)}",vec![json!(files.len())]).await?;
            if valid != true {
                return Err(Error::cdp_msg("Target does not accept these files"));
            }
            let paths = files.clone();
            let (files, expected) =
                tokio::task::spawn_blocking(move || -> Result<(Vec<String>, Value)> {
                    let mut paths_out = Vec::new();
                    let mut evidence = Vec::new();
                    for path in paths {
                        let path = std::fs::canonicalize(path)?;
                        let metadata = path.metadata()?;
                        if !metadata.is_file() {
                            return Err(Error::cdp_msg("Upload requires regular files"));
                        }
                        let name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .ok_or_else(|| Error::cdp_msg("Upload path is not UTF-8"))?;
                        evidence.push(json!({"name":name,"size":metadata.len()}));
                        paths_out.push(
                            path.to_str()
                                .ok_or_else(|| Error::cdp_msg("Upload path is not UTF-8"))?
                                .into(),
                        );
                    }
                    Ok((paths_out, json!(evidence)))
                })
                .await
                .map_err(|error| {
                    Error::transport_io(
                        "Upload inspection task failed",
                        std::io::Error::other(error),
                    )
                })??;
            dispatch
                .send(
                    &node.session,
                    "DOM.setFileInputFiles",
                    json!({"objectId":node.object,"files":files}),
                    false,
                )
                .await?;
            Ok(property("files", expected))
        }
        Action::PressKey { key } => {
            input.key(dispatch, key).await?;
            Ok(Verification::Explicit)
        }
        Action::Submit { .. } => {
            mutation(
                required()?,
                dispatch,
                "function(){this.focus()}",
                vec![],
                true,
            )
            .await?;
            let properties = required()?.properties().await?;
            if properties["connected"] != true
                || properties["focused"] != true
                || properties["disabled"] == true
            {
                return Err(Error::cdp_msg(
                    "actionability_changed: submit target did not retain focus",
                ));
            }
            input.key(dispatch, "Enter").await?;
            Ok(Verification::Explicit)
        }
        Action::Focus { .. } => {
            mutation(
                required()?,
                dispatch,
                "function(){this.focus()}",
                vec![],
                false,
            )
            .await?;
            Ok(property("focused", json!(true)))
        }
        Action::Hover { human, .. } => {
            let (x, y) = point(page, required()?, false).await?;
            if *human {
                input.human_move(dispatch, x, y).await?;
            }
            dispatch
                .send(
                    page.session(),
                    "Input.dispatchMouseEvent",
                    json!({"type":"mouseMoved","x":x,"y":y,"buttons":0}),
                    false,
                )
                .await?;
            Ok(property("hovered", json!(true)))
        }
        Action::ScrollIntoView { .. } => {
            mutation(required()?,dispatch,"function(){this.scrollIntoView({block:'center',inline:'center',behavior:'instant'})}",vec![],false).await?;
            Ok(property("inViewport", json!(true)))
        }
        Action::NavigateWithHeaders { url, headers } => {
            let guard = page.session().temporary_headers().await?;
            let result = async {
                dispatch
                    .send(
                        page.session(),
                        "Network.enable",
                        json!({"maxPostDataSize":65536}),
                        true,
                    )
                    .await?;
                dispatch
                    .send(
                        page.session(),
                        "Network.setExtraHTTPHeaders",
                        json!({"headers":headers}),
                        true,
                    )
                    .await?;
                navigate(page, dispatch, json!({"url":url})).await
            }
            .await;
            let cleanup = guard.finish().await;
            match (result, cleanup) {
                (Ok(result), Ok(())) => Ok(result),
                (Err(error), _) | (_, Err(error)) => Err(error),
            }
        }
        Action::Navigate { url, referrer } => {
            let mut params = json!({"url":url});
            if let Some(referrer) = referrer {
                params["referrer"] = json!(referrer);
            }
            navigate(page, dispatch, params).await
        }
        Action::Reload | Action::History { delta: 0 } => {
            dispatch
                .send(page.session(), "Page.reload", json!({}), false)
                .await?;
            Ok(Verification::Navigation { loader: None })
        }
        Action::Back => navigate_history(page, dispatch, -1).await,
        Action::Forward => navigate_history(page, dispatch, 1).await,
        Action::History { delta } => navigate_history(page, dispatch, i64::from(*delta)).await,
    }
}

pub(super) async fn pause() {
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
}

async fn navigate(page: &Page, dispatch: &Dispatch, params: Value) -> Result<Verification> {
    let result = dispatch
        .send(page.session(), "Page.navigate", params, false)
        .await?;
    if result["errorText"]
        .as_str()
        .is_some_and(|e| e != "net::ERR_HTTP_RESPONSE_CODE_FAILURE" && e != "net::ERR_ABORTED")
    {
        return Err(Error::Navigation(
            result["errorText"]
                .as_str()
                .unwrap_or("Navigation failed")
                .into(),
        ));
    }
    Ok(Verification::Navigation {
        loader: result["loaderId"].as_str().map(str::to_owned),
    })
}

async fn navigate_history(page: &Page, dispatch: &Dispatch, offset: i64) -> Result<Verification> {
    let history: Value = page
        .session()
        .send("Page.getNavigationHistory", &json!({}))
        .await?;
    let index = history["currentIndex"]
        .as_i64()
        .ok_or_else(|| Error::cdp_msg("Invalid history index"))?
        .checked_add(offset)
        .ok_or_else(|| Error::cdp_msg("History index overflow"))?;
    let entry = usize::try_from(index)
        .ok()
        .and_then(|i| history["entries"].get(i));
    let Some(entry) = entry else {
        return Ok(Verification::Noop);
    };
    dispatch
        .send(
            page.session(),
            "Page.navigateToHistoryEntry",
            json!({"entryId":entry["id"]}),
            false,
        )
        .await?;
    Ok(Verification::Navigation { loader: None })
}
