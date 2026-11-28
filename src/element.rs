//! Element Abstraction
//!
//! DOM element wrapper and its bounding box type. Split out of `page.rs`.

use crate::action::ActionCall;
use crate::error::{Error, Result};
use crate::page::Page;
use crate::{Action, LocatorQuery, LocatorSpec};
use serde_json::{json, Value};

/// Bounding box of an element
#[derive(Debug, Clone, Copy)]
pub struct BoundingBox {
    /// X coordinate of the top-left corner, in CSS pixels.
    pub x: f64,
    /// Y coordinate of the top-left corner, in CSS pixels.
    pub y: f64,
    /// Width in CSS pixels.
    pub width: f64,
    /// Height in CSS pixels.
    pub height: f64,
}

impl BoundingBox {
    /// Center point of the box as `(x, y)`.
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

/// A handle to a DOM element. Owns a cheap clone of its `Page`, so it is
/// `'static` — you can store it, return it, and pass it around freely.
///
/// Pinned to Chrome's backend node id, which survives document re-requests.
/// Removing the node or navigating makes the handle stale: it then errors on
/// use and is never retargeted to a replacement.
pub struct Element {
    pub(crate) page: Page,
    pub(crate) backend_node_id: i64,
}

impl Element {
    /// Pins a frontend node id from the current document to its backend id.
    pub(crate) async fn from_node_id(page: &Page, node_id: i32) -> Result<Self> {
        let described: Value = page
            .session
            .send("DOM.describeNode", &json!({ "nodeId": node_id }))
            .await?;
        let backend_node_id = described["node"]["backendNodeId"]
            .as_i64()
            .ok_or_else(|| Error::cdp("DOM.describeNode", -1, "No backendNodeId returned"))?;
        Ok(Self {
            page: page.clone(),
            backend_node_id,
        })
    }

    fn action_target(&self) -> LocatorSpec {
        LocatorSpec {
            query: LocatorQuery::BackendNode {
                id: self.backend_node_id,
            },
            frame_id: None,
            index: None,
        }
    }

    async fn box_model(&self) -> Result<crate::cdp::types::BoxModel> {
        let result: crate::cdp::types::DOMGetBoxModelResult = self
            .page
            .session
            .send(
                "DOM.getBoxModel",
                &json!({ "backendNodeId": self.backend_node_id }),
            )
            .await?;
        Ok(result.model)
    }

    /// Get the element's center coordinates
    pub async fn center(&self) -> Result<(f64, f64)> {
        let model = self.box_model().await?;
        model.try_center().ok_or_else(|| Error::ElementNotVisible {
            selector: format!("backend node {}", self.backend_node_id),
        })
    }

    /// Click this element. Awaiting proves dispatch only; see [`ActionCall`].
    pub fn click(&self) -> ActionCall {
        ActionCall::new(
            &self.page,
            Action::Click {
                target: self.action_target(),
                human: false,
                double: false,
            },
        )
    }

    /// Human-like click
    pub fn human_click(&self) -> ActionCall {
        ActionCall::new(
            &self.page,
            Action::Click {
                target: self.action_target(),
                human: true,
                double: false,
            },
        )
    }

    /// Drag horizontally from this element's center using human-like motion.
    /// See [`crate::stealth::Human::drag_by`] for ownership and cleanup semantics.
    pub fn human_drag_by(&self, dx: f64) -> ActionCall {
        ActionCall::new(
            &self.page,
            Action::Drag {
                target: self.action_target(),
                dx,
                dy: 0.0,
            },
        )
    }

    /// Get outer HTML
    pub async fn outer_html(&self) -> Result<String> {
        let result: Value = self
            .page
            .session
            .send(
                "DOM.getOuterHTML",
                &json!({ "backendNodeId": self.backend_node_id }),
            )
            .await?;
        Ok(result["outerHTML"].as_str().unwrap_or_default().to_owned())
    }

    /// Get the element's `textContent` (empty when it has none).
    pub async fn text(&self) -> Result<String> {
        self.eval_str("function(){return this.textContent || ''}", &[])
            .await
    }

    /// Calls `function` with this element as `this`, returning the result by
    /// value. The remote object is released so repeated calls don't pin nodes.
    async fn call_on_element(&self, function: &str, args: &[Value]) -> Result<Value> {
        let resolved: Value = self
            .page
            .session
            .send(
                "DOM.resolveNode",
                &json!({ "backendNodeId": self.backend_node_id }),
            )
            .await?;
        let object_id = resolved["object"]["objectId"]
            .as_str()
            .ok_or_else(|| Error::cdp("DOM.resolveNode", -1, "No object_id returned"))?
            .to_owned();
        let arguments: Vec<Value> = args.iter().map(|value| json!({ "value": value })).collect();
        let result: Result<Value> = self
            .page
            .session
            .send(
                "Runtime.callFunctionOn",
                &json!({
                    "objectId": object_id,
                    "functionDeclaration": function,
                    "arguments": arguments,
                    "returnByValue": true,
                }),
            )
            .await;
        let _ = self
            .page
            .session
            .send::<_, Value>("Runtime.releaseObject", &json!({ "objectId": object_id }))
            .await;
        let result = result?;
        if let Some(details) = result.get("exceptionDetails") {
            return Err(Error::javascript_exception(details));
        }
        Ok(result["result"]["value"].clone())
    }

    /// Call a function on the element, returning a String (empty on null/non-string)
    async fn eval_str(&self, function: &str, args: &[Value]) -> Result<String> {
        let value = self.call_on_element(function, args).await?;
        Ok(value.as_str().unwrap_or("").to_string())
    }

    /// Call a function on the element, returning a bool with a default
    async fn eval_bool(&self, function: &str, default: bool) -> Result<bool> {
        let value = self.call_on_element(function, &[]).await?;
        Ok(value.as_bool().unwrap_or(default))
    }

    /// Type text into this element. Awaiting proves dispatch only; see [`ActionCall`].
    pub fn type_text(&self, text: &str) -> ActionCall {
        ActionCall::new(
            &self.page,
            Action::Type {
                target: self.action_target(),
                text: text.into(),
                input_mode: Default::default(),
            },
        )
    }

    /// Focus this element
    pub async fn focus(&self) -> Result<()> {
        self.page
            .act(
                Action::Focus {
                    target: self.action_target(),
                },
                self.page.action_defaults.options(),
            )
            .await
            .map(|_| ())
    }
    /// Check for a rendered, nonzero box with visible CSS visibility.
    /// Offscreen, transparent and disabled elements can still be visible.
    pub async fn is_visible(&self) -> Result<bool> {
        crate::action::is_visible(&self.page, &self.action_target()).await
    }

    /// Get the element's bounding box
    ///
    /// Returns None if the element is not visible/rendered.
    pub async fn bounding_box(&self) -> Option<BoundingBox> {
        let model = self.box_model().await.ok()?;
        let content = &model.content;
        if content.len() < 8 {
            return None;
        }
        // content is [x1,y1, x2,y2, x3,y3, x4,y4] for a quad; take the
        // bounds so rotated/transformed elements are covered.
        let xs = [content[0], content[2], content[4], content[6]];
        let ys = [content[1], content[3], content[5], content[7]];
        let min_x = xs.iter().copied().fold(f64::INFINITY, f64::min);
        let max_x = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let min_y = ys.iter().copied().fold(f64::INFINITY, f64::min);
        let max_y = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        Some(BoundingBox {
            x: min_x,
            y: min_y,
            width: max_x - min_x,
            height: max_y - min_y,
        })
    }

    /// Get an attribute value
    pub async fn get_attribute(&self, name: &str) -> Result<Option<String>> {
        let value = self
            .call_on_element(
                "function(name){return this.getAttribute(name)}",
                &[json!(name)],
            )
            .await?;
        Ok(value.as_str().map(str::to_owned))
    }

    /// Get the tag name of the element (e.g., "div", "input", "a")
    pub async fn tag_name(&self) -> Result<String> {
        self.eval_str("function(){return this.tagName.toLowerCase()}", &[])
            .await
    }

    /// Check if the element is enabled (not disabled)
    pub async fn is_enabled(&self) -> Result<bool> {
        self.eval_bool("function(){return !this.disabled}", true)
            .await
    }

    /// Check if a checkbox/radio is checked
    pub async fn is_checked(&self) -> Result<bool> {
        self.eval_bool("function(){return this.checked === true}", false)
            .await
    }

    /// Get the value of an input element
    pub async fn value(&self) -> Result<String> {
        self.eval_str("function(){return this.value || ''}", &[])
            .await
    }

    /// Get computed CSS property value
    pub async fn css(&self, property: &str) -> Result<String> {
        self.eval_str(
            "function(property){return getComputedStyle(this).getPropertyValue(property)}",
            &[json!(property)],
        )
        .await
    }

    /// Scroll this element into view
    pub async fn scroll_into_view(&self) -> Result<()> {
        self.page
            .act(
                Action::ScrollIntoView {
                    target: self.action_target(),
                },
                self.page.action_defaults.options(),
            )
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::Element;

    // Element owns its Page, so it must be Send + 'static — storable, returnable,
    // and movable across await points and tasks. This is the reusability contract.
    #[test]
    fn element_is_send_and_static() {
        fn assert_send_static<T: Send + 'static>() {}
        assert_send_static::<Element>();
    }
}
