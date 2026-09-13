//! Element Abstraction
//!
//! DOM element wrapper and its bounding box type. Split out of `page.rs`.

use crate::error::{Error, Result};
use crate::page::{escape_js_string, sleep_ms, Page, INTERACTION_DELAY_MS};

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
/// `'static` — you can store it, return it, and pass it around freely. The
/// `node_id` can still go stale if the DOM mutates or the page navigates; a
/// stale handle surfaces as an `ElementNotFound`/`Cdp` error on use.
pub struct Element {
    pub(crate) page: Page,
    pub(crate) node_id: i32,
}

impl Element {
    /// Get the element's center coordinates
    pub async fn center(&self) -> Result<(f64, f64)> {
        let model = self.page.session.get_box_model(self.node_id).await?;
        model.try_center().ok_or_else(|| Error::ElementNotVisible {
            selector: format!("node {}", self.node_id),
        })
    }

    /// Click this element
    pub async fn click(&self) -> Result<()> {
        let (x, y) = self.center().await?;
        self.page.click_at(x, y).await
    }

    /// Human-like click
    pub async fn human_click(&self) -> Result<()> {
        let (x, y) = self.center().await?;
        self.page.human().move_and_click(x, y).await
    }

    /// Get outer HTML
    pub async fn outer_html(&self) -> Result<String> {
        self.page.session.get_outer_html(self.node_id).await
    }

    /// Get inner text
    ///
    /// Extracts text content from the element's outerHTML without using focus.
    pub async fn text(&self) -> Result<String> {
        self.eval_str("this.textContent || ''").await
    }

    /// Evaluate a JavaScript expression on this element via Runtime.callFunctionOn.
    ///
    /// The expression should use `this` to refer to the element.
    /// Example: `"this.textContent || ''"`, `"this.tagName.toLowerCase()"`
    async fn eval_on_element(&self, js_body: &str) -> Result<serde_json::Value> {
        let object_id = self.page.session.resolve_node(self.node_id).await?;
        let func = format!("function() {{ return {}; }}", js_body);
        let result = self
            .page
            .session
            .call_function_on(&object_id, &func)
            .await?;
        Ok(result.result.value.unwrap_or(serde_json::Value::Null))
    }

    /// Evaluate JS on element, return as String (empty string on null/non-string)
    async fn eval_str(&self, js_body: &str) -> Result<String> {
        let value = self.eval_on_element(js_body).await?;
        Ok(value.as_str().unwrap_or("").to_string())
    }

    /// Evaluate JS on element, return as bool with a default
    async fn eval_bool(&self, js_body: &str, default: bool) -> Result<bool> {
        let value = self.eval_on_element(js_body).await?;
        Ok(value.as_bool().unwrap_or(default))
    }

    /// Type text into this element
    pub async fn type_text(&self, text: &str) -> Result<()> {
        self.click().await?;
        sleep_ms(INTERACTION_DELAY_MS).await;
        self.page.session.insert_text(text).await
    }

    /// Focus this element
    pub async fn focus(&self) -> Result<()> {
        self.page.session.focus(self.node_id).await
    }
    /// Check if the element is visible (has a computable box model)
    pub async fn is_visible(&self) -> Result<bool> {
        match self.page.session.get_box_model(self.node_id).await {
            Ok(_) => Ok(true),
            Err(Error::Cdp { message, .. }) if message.contains("box model") => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Get the element's bounding box
    ///
    /// Returns None if the element is not visible/rendered.
    pub async fn bounding_box(&self) -> Option<BoundingBox> {
        match self.page.session.get_box_model(self.node_id).await {
            Ok(model) => {
                let content = &model.content;
                if content.len() >= 8 {
                    // content is [x1,y1, x2,y2, x3,y3, x4,y4] for a quad
                    // Handle rotated/transformed elements by finding actual bounds
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
                } else {
                    None
                }
            }
            Err(_) => None,
        }
    }

    /// Get an attribute value
    pub async fn get_attribute(&self, name: &str) -> Result<Option<String>> {
        let escaped_name = escape_js_string(name);
        let value = self
            .eval_on_element(&format!("this.getAttribute('{}')", escaped_name))
            .await?;

        if value.is_null() {
            return Ok(None);
        }
        if let Some(s) = value.as_str() {
            return Ok(Some(s.to_string()));
        }
        Ok(None)
    }

    /// Get the tag name of the element (e.g., "div", "input", "a")
    pub async fn tag_name(&self) -> Result<String> {
        self.eval_str("this.tagName.toLowerCase()").await
    }

    /// Check if the element is enabled (not disabled)
    pub async fn is_enabled(&self) -> Result<bool> {
        self.eval_bool("!this.disabled", true).await
    }

    /// Check if a checkbox/radio is checked
    pub async fn is_checked(&self) -> Result<bool> {
        self.eval_bool("this.checked === true", false).await
    }

    /// Get the value of an input element
    pub async fn value(&self) -> Result<String> {
        self.eval_str("this.value || ''").await
    }

    /// Return this element's effective input type, if it is an input.
    pub(crate) async fn input_type(&self) -> Result<Option<String>> {
        let value = self
            .eval_on_element("this instanceof HTMLInputElement ? this.type : null")
            .await?;
        Ok(value.as_str().map(str::to_owned))
    }

    /// Set a range input through its property and notify application listeners.
    ///
    /// The assignment is first checked on a detached clone so unsupported,
    /// out-of-range, or step-invalid requests do not mutate the live control.
    /// Returns the browser-normalized value when it is accepted.
    pub(crate) async fn set_range_value(&self, value: &str) -> Result<Option<String>> {
        let object_id = self.page.session.resolve_node(self.node_id).await?;
        let requested = serde_json::to_string(value)?;
        let function = format!(
            r#"function() {{
                const requested = {requested};
                const requestedNumber = Number(requested);
                if (!Number.isFinite(requestedNumber)) return null;
                const probe = this.cloneNode(false);
                probe.value = requested;
                const normalized = probe.value;
                if (!probe.validity.valid || Number(normalized) !== requestedNumber) return null;
                this.value = requested;
                // Capture before listeners run: fill must reject handlers that
                // overwrite the requested range value during input/change.
                const assigned = this.value;
                this.dispatchEvent(new Event('input', {{ bubbles: true }}));
                this.dispatchEvent(new Event('change', {{ bubbles: true }}));
                return assigned;
            }}"#
        );
        let result = self
            .page
            .session
            .call_function_on(&object_id, &function)
            .await?;
        if let Some(exception) = result.exception_details {
            return Err(Error::cdp_msg(format!(
                "JavaScript error: {} at {}:{}",
                exception.text, exception.line_number, exception.column_number
            )));
        }
        Ok(result
            .result
            .value
            .and_then(|value| value.as_str().map(str::to_owned)))
    }

    /// Get computed CSS property value
    pub async fn css(&self, property: &str) -> Result<String> {
        let escaped = escape_js_string(property);
        self.eval_str(&format!(
            "getComputedStyle(this).getPropertyValue('{}')",
            escaped
        ))
        .await
    }

    /// Scroll this element into view
    pub async fn scroll_into_view(&self) -> Result<()> {
        let object_id = self.page.session.resolve_node(self.node_id).await?;
        self.page
            .session
            .call_function_on(
                &object_id,
                "function() { this.scrollIntoView({ behavior: 'smooth', block: 'center' }); }",
            )
            .await?;
        Ok(())
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
