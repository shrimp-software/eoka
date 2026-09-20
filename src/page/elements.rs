use super::{
    escape_js_string, is_element_cdp_error, sleep_ms, Element, Page, TextMatch,
    INTERACTION_DELAY_MS, SETTLE_MS,
};
use crate::error::{Error, Result};

impl Page {
    /// Find an element by CSS selector
    pub async fn find(&self, selector: &str) -> Result<Element> {
        let node_id = self
            .with_document(|doc| self.session.query_selector(doc, selector))
            .await?;

        if node_id == 0 {
            return Err(Error::ElementNotFound(selector.to_string()));
        }

        Ok(Element {
            page: self.clone(),
            node_id,
        })
    }

    /// Find all elements matching a CSS selector
    pub async fn find_all(&self, selector: &str) -> Result<Vec<Element>> {
        let node_ids = self
            .with_document(|doc| self.session.query_selector_all(doc, selector))
            .await?;

        Ok(node_ids
            .into_iter()
            .filter(|&id| id != 0)
            .map(|node_id| Element {
                page: self.clone(),
                node_id,
            })
            .collect())
    }

    /// Check if an element exists
    #[must_use = "returns true if element exists"]
    pub async fn exists(&self, selector: &str) -> bool {
        self.find(selector).await.is_ok()
    }

    /// Find an element by its text content (case-insensitive contains)
    pub async fn find_by_text(&self, text: &str) -> Result<Element> {
        self.find_by_text_match(text, TextMatch::Contains).await
    }

    /// Find an element by text with specific matching strategy
    ///
    /// Prioritizes interactive elements (a, button, input) over static elements.
    /// Uses Runtime.callFunctionOn to avoid mutating the DOM (no marker attributes).
    pub async fn find_by_text_match(&self, text: &str, match_type: TextMatch) -> Result<Element> {
        let escaped_text = escape_js_string(text);
        let match_js = match match_type {
            TextMatch::Exact => format!("t.trim() === '{}'", escaped_text),
            TextMatch::Contains => format!(
                "t.toLowerCase().includes('{}')",
                escaped_text.to_lowercase()
            ),
            TextMatch::StartsWith => format!(
                "t.toLowerCase().startsWith('{}')",
                escaped_text.to_lowercase()
            ),
            TextMatch::EndsWith => format!(
                "t.toLowerCase().endsWith('{}')",
                escaped_text.to_lowercase()
            ),
        };

        let js = format!(
            r#"
            (() => {{
                const interactive = 'a, button, input[type="submit"], input[type="button"], [role="button"], [onclick]';
                for (const el of document.querySelectorAll(interactive)) {{
                    const t = el.innerText || el.textContent || el.value || '';
                    if ({match_js}) return el;
                }}
                const secondary = 'label, span, div, p, h1, h2, h3, h4, h5, h6, li, td, th';
                for (const el of document.querySelectorAll(secondary)) {{
                    const t = el.innerText || el.textContent || el.value || '';
                    if ({match_js}) return el;
                }}
                return null;
            }})()
            "#,
        );

        let result = self.session.evaluate_for_remote_object(&js).await?;
        let remote = self.check_js_result(result)?;

        if remote.subtype.as_deref() == Some("null") {
            return Err(Error::ElementNotFound(format!("text: {}", text)));
        }

        let object_id = remote
            .object_id
            .ok_or_else(|| Error::ElementNotFound(format!("text: {}", text)))?;

        // Prime the DOM node-id space (DOM.requestNode returns 0 unless
        // DOM.getDocument has populated it for the current document).
        self.document_node().await?;

        // Convert remote object to DOM node_id
        let node_id = self.session.request_node(&object_id).await?;

        if node_id == 0 {
            return Err(Error::ElementNotFound(format!("text: {}", text)));
        }

        Ok(Element {
            page: self.clone(),
            node_id,
        })
    }

    /// Find all elements matching the given text
    pub async fn find_all_by_text(&self, text: &str) -> Result<Vec<Element>> {
        let escaped_text = escape_js_string(text).to_lowercase();

        let js = format!(
            r#"
            (() => {{
                const selectors = 'a, button, input, label, span, div, p, h1, h2, h3, h4, h5, h6, li, td, th';
                const elements = document.querySelectorAll(selectors);
                const matches = [];
                for (const el of elements) {{
                    const t = (el.innerText || el.textContent || el.value || '').toLowerCase();
                    if (t.includes('{escaped_text}')) {{
                        matches.push(el);
                    }}
                }}
                return matches;
            }})()
            "#,
        );

        // Evaluate without returnByValue to get remote object references
        let result = self.session.evaluate_for_remote_object(&js).await?;

        let remote = self.check_js_result(result)?;

        let array_object_id = match &remote.object_id {
            Some(id) => id.clone(),
            None => return Ok(Vec::new()),
        };

        // Get all indexed properties of the array in one CDP call
        let properties = self.session.get_properties(&array_object_id).await?;

        // Prime the DOM node-id space (DOM.requestNode returns 0 otherwise).
        self.document_node().await?;

        let mut elements = Vec::new();
        for prop in &properties {
            // Array elements have numeric names; skip "length" and prototype props
            if prop.name.parse::<usize>().is_err() {
                continue;
            }
            if let Some(ref obj_id) = prop.value.as_ref().and_then(|v| v.object_id.clone()) {
                if let Ok(node_id) = self.session.request_node(obj_id).await {
                    if node_id != 0 {
                        elements.push(Element {
                            page: self.clone(),
                            node_id,
                        });
                    }
                }
            }
        }

        Ok(elements)
    }

    /// Check if an element with the given text exists
    #[must_use = "returns true if text exists on page"]
    pub async fn text_exists(&self, text: &str) -> bool {
        self.find_by_text(text).await.is_ok()
    }

    /// Click on an element by selector
    pub async fn click(&self, selector: &str) -> Result<()> {
        let element = self.find(selector).await?;
        element.click().await
    }

    /// Type text into an element by selector
    pub async fn type_into(&self, selector: &str, text: &str) -> Result<()> {
        let element = self.find(selector).await?;
        element.click().await?;
        sleep_ms(INTERACTION_DELAY_MS).await;
        self.session.insert_text(text).await
    }

    /// Click an element by its text content
    pub async fn click_by_text(&self, text: &str) -> Result<()> {
        let element = self.find_by_text(text).await?;
        element.click().await
    }

    /// Try to click an element, returning Ok(false) if not found or not clickable
    #[must_use = "returns true if clicked, false if not found/visible"]
    pub async fn try_click(&self, selector: &str) -> Result<bool> {
        self.try_click_impl(self.find(selector).await).await
    }

    /// Try to click an element by text, returning Ok(false) if not found or not clickable
    #[must_use = "returns true if clicked, false if not found/visible"]
    pub async fn try_click_by_text(&self, text: &str) -> Result<bool> {
        self.try_click_impl(self.find_by_text(text).await).await
    }

    /// Shared impl for try_click and try_click_by_text
    async fn try_click_impl(&self, find_result: Result<Element>) -> Result<bool> {
        match find_result {
            Ok(element) => match element.click().await {
                Ok(()) => Ok(true),
                Err(e) if is_element_cdp_error(&e) => Ok(false),
                Err(e) => Err(e),
            },
            Err(e) if is_element_cdp_error(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Fill a form field: click, clear, type, and verify the resulting value.
    ///
    /// Range inputs are assigned through their DOM property rather than text
    /// insertion. The requested value must be finite and satisfy the range's
    /// min/max/step constraints; accepted range values dispatch bubbling
    /// `input` and `change` events.
    pub async fn fill(&self, selector: &str, value: &str) -> Result<()> {
        let element = self.find(selector).await?;

        if element.input_type().await?.as_deref() == Some("range") {
            // Clicking a range is itself a value-changing native action. Focus
            // it directly so this method emits only the assignment events.
            element.focus().await?;
            let expected = match element.set_range_value(value).await? {
                Some(value) => value,
                None => {
                    return Err(Error::ValueMismatch {
                        selector: selector.to_string(),
                        expected: value.to_string(),
                        actual: element.value().await?,
                    });
                }
            };
            return self
                .verify_filled_value(&element, selector, &expected)
                .await;
        }

        element.click().await?;
        sleep_ms(INTERACTION_DELAY_MS).await;

        // Focus + select via selector (don't rely on activeElement — popups can steal focus).
        let escaped = escape_js_string(selector);
        self.execute(&format!(
            "(() => {{ const el = document.querySelector('{}'); if (el) {{ el.focus(); el.select(); }} }})()",
            escaped
        )).await?;
        self.session.insert_text("").await?;
        self.session.insert_text(value).await?;
        self.verify_filled_value(&element, selector, value).await
    }

    async fn verify_filled_value(
        &self,
        element: &Element,
        selector: &str,
        expected: &str,
    ) -> Result<()> {
        let actual = element.value().await?;
        if actual == expected {
            Ok(())
        } else {
            Err(Error::ValueMismatch {
                selector: selector.to_string(),
                expected: expected.to_string(),
                actual,
            })
        }
    }

    /// Human-like click on an element
    pub async fn human_click(&self, selector: &str) -> Result<()> {
        let element = self.find(selector).await?;
        let (x, y) = element.center().await?;
        self.human_click_at_center_xy(x, y).await
    }

    /// Human-like typing into an element
    pub async fn human_type(&self, selector: &str, text: &str) -> Result<()> {
        self.human_click(selector).await?;
        sleep_ms(SETTLE_MS).await;
        self.human_type_text(text).await
    }

    /// Human-like click on an element found by text content
    pub async fn human_click_by_text(&self, text: &str) -> Result<()> {
        let element = self.find_by_text(text).await?;
        let (x, y) = element.center().await?;
        self.human_click_at_center_xy(x, y).await
    }

    /// Try to human-click an element, returning Ok(true) if clicked, Ok(false) if not found or not clickable
    #[must_use = "returns true if clicked, false if not found/visible"]
    pub async fn try_human_click(&self, selector: &str) -> Result<bool> {
        self.try_human_click_impl(self.find(selector).await).await
    }

    /// Try to human-click an element by text, returning Ok(true) if clicked, Ok(false) if not found or not clickable
    #[must_use = "returns true if clicked, false if not found/visible"]
    pub async fn try_human_click_by_text(&self, text: &str) -> Result<bool> {
        self.try_human_click_impl(self.find_by_text(text).await)
            .await
    }

    /// Human-like form fill: click, clear, type with natural delays
    pub async fn human_fill(&self, selector: &str, value: &str) -> Result<()> {
        self.human_click(selector).await?;
        sleep_ms(SETTLE_MS).await;
        let escaped = escape_js_string(selector);
        let select_js = format!(
            "(() => {{ const el = document.querySelector('{escaped}'); \
             if (el) {{ el.focus(); if (typeof el.select === 'function') el.select(); }} }})()"
        );
        self.execute(&select_js).await?;
        sleep_ms(INTERACTION_DELAY_MS).await;
        self.human_type_text(value).await
    }

    /// Type text, using human-like timing if configured
    async fn human_type_text(&self, text: &str) -> Result<()> {
        if self.config.human_typing {
            self.human().type_text(text).await
        } else {
            self.session.insert_text(text).await
        }
    }

    async fn human_click_at_center_xy(&self, x: f64, y: f64) -> Result<()> {
        if self.config.human_mouse {
            self.human().move_and_click(x, y).await
        } else {
            self.click_at(x, y).await
        }
    }

    /// Shared impl for try_human_click and try_human_click_by_text
    async fn try_human_click_impl(&self, find_result: Result<Element>) -> Result<bool> {
        match find_result {
            Ok(element) => match element.center().await {
                Ok((x, y)) => {
                    self.human_click_at_center_xy(x, y).await?;
                    Ok(true)
                }
                Err(e) if is_element_cdp_error(&e) => Ok(false),
                Err(e) => Err(e),
            },
            Err(e) if is_element_cdp_error(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Find the first element matching any of the given selectors
    pub async fn find_any(&self, selectors: &[&str]) -> Result<Element> {
        for selector in selectors {
            match self.find(selector).await {
                Ok(element) => return Ok(element),
                Err(Error::ElementNotFound(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Err(Error::ElementNotFound(format!(
            "None of selectors found: {:?}",
            selectors
        )))
    }

    /// Upload file(s) to a file input element
    pub async fn upload_file(&self, selector: &str, path: &str) -> Result<()> {
        self.upload_files(selector, &[path]).await
    }

    /// Upload multiple files to a file input element
    pub async fn upload_files(&self, selector: &str, paths: &[&str]) -> Result<()> {
        let element = self.find(selector).await?;
        self.session
            .set_file_input_files(
                element.node_id,
                paths.iter().map(|p| p.to_string()).collect(),
            )
            .await
    }

    /// Select option by value
    pub async fn select(&self, selector: &str, value: &str) -> Result<()> {
        let (sel, val) = (escape_js_string(selector), escape_js_string(value));
        self.execute(&format!(
            r#"(()=>{{const el=document.querySelector('{sel}');if(!el)throw new Error('Select not found');const opt=[...el.options].find(o=>o.value==='{val}');if(!opt)throw new Error('Option not found: {val}');el.value='{val}';el.dispatchEvent(new Event('change',{{bubbles:true}}))}})()"#
        )).await
    }

    /// Select option by visible text
    pub async fn select_by_text(&self, selector: &str, text: &str) -> Result<()> {
        let (sel, txt) = (escape_js_string(selector), escape_js_string(text));
        self.execute(&format!(
            r#"(()=>{{const el=document.querySelector('{sel}');if(!el)throw new Error('Select not found');const opt=[...el.options].find(o=>o.text.trim()==='{txt}');if(!opt)throw new Error('Option not found: {txt}');el.value=opt.value;el.dispatchEvent(new Event('change',{{bubbles:true}}))}})()"#
        )).await
    }

    /// Select multiple options by values
    pub async fn select_multiple(&self, selector: &str, values: &[&str]) -> Result<()> {
        let sel = escape_js_string(selector);
        let vals = serde_json::to_string(values).unwrap_or_else(|_| "[]".into());
        self.execute(&format!(
            r#"(()=>{{const el=document.querySelector('{sel}');if(!el)throw new Error('Select not found');const v={vals};for(const o of el.options)o.selected=v.includes(o.value);el.dispatchEvent(new Event('change',{{bubbles:true}}))}})()"#
        )).await
    }

    /// Hover over element (for revealing menus)
    pub async fn hover(&self, selector: &str) -> Result<()> {
        let (x, y) = self.find(selector).await?.center().await?;
        self.mouse_move(x, y).await
    }

    /// Human-like hover with Bezier curve movement
    pub async fn human_hover(&self, selector: &str) -> Result<()> {
        let element = self.find(selector).await?;
        element.scroll_into_view().await?;
        let (x, y) = element.center().await?;
        self.human().move_to(x, y).await?;
        sleep_ms(SETTLE_MS).await;
        Ok(())
    }
}
