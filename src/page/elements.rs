use super::{escape_js_string, Element, Page, TextMatch};
use crate::error::{Error, Result};
use crate::{Action, LocatorSpec};

impl Page {
    /// Drag horizontally from the selected element's center.
    /// For explicit cancellation cleanup, retain a [`crate::stealth::Human`] helper.
    pub async fn human_drag(&self, selector: &str, dx: f64) -> Result<()> {
        self.act(
            Action::Drag {
                target: LocatorSpec::css(selector),
                dx,
                dy: 0.0,
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }

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
        self.act(
            Action::Click {
                target: LocatorSpec::css(selector),
                human: false,
                double: false,
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }

    /// Type text into an element by selector
    pub async fn type_into(&self, selector: &str, text: &str) -> Result<()> {
        self.act(
            Action::Type {
                target: LocatorSpec::css(selector),
                text: text.into(),
                human: false,
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }

    /// Click an element by its text content
    pub async fn click_by_text(&self, text: &str) -> Result<()> {
        self.act(
            Action::Click {
                target: LocatorSpec::text(text),
                human: false,
                double: false,
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }

    /// Try to click an element, returning Ok(false) if not found or not clickable
    #[must_use = "returns true if clicked, false if not found/visible"]
    pub async fn try_click(&self, selector: &str) -> Result<bool> {
        self.try_action(Action::Click {
            target: LocatorSpec::css(selector),
            human: false,
            double: false,
        })
        .await
    }

    /// Try to click an element by text, returning Ok(false) if not found or not clickable
    #[must_use = "returns true if clicked, false if not found/visible"]
    pub async fn try_click_by_text(&self, text: &str) -> Result<bool> {
        self.try_action(Action::Click {
            target: LocatorSpec::text(text),
            human: false,
            double: false,
        })
        .await
    }

    async fn try_action(&self, action: Action) -> Result<bool> {
        let mut options = self.action_options.clone();
        options.wait_for_target = false;
        match self.act(action, options).await {
            Ok(_) => Ok(true),
            Err(Error::Action {
                receipt, source, ..
            }) if receipt.dispatch == crate::DispatchState::NotSent
                && matches!(
                    *source,
                    Error::ElementNotFound(_) | Error::ElementNotVisible { .. }
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Fill a form field: click, clear, type, and verify the resulting value.
    ///
    /// Range inputs are assigned through their DOM property rather than text
    /// insertion. The requested value must be finite and satisfy the range's
    /// min/max/step constraints; accepted range values dispatch bubbling
    /// `input` and `change` events.
    pub async fn fill(&self, selector: &str, value: &str) -> Result<()> {
        self.act(
            Action::Fill {
                target: LocatorSpec::css(selector),
                value: value.into(),
                human: false,
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }

    /// Human-like click on an element
    pub async fn human_click(&self, selector: &str) -> Result<()> {
        self.act(
            Action::Click {
                target: LocatorSpec::css(selector),
                human: true,
                double: false,
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }

    /// Human-like typing into an element
    pub async fn human_type(&self, selector: &str, text: &str) -> Result<()> {
        self.act(
            Action::Type {
                target: LocatorSpec::css(selector),
                text: text.into(),
                human: true,
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }

    /// Human-like click on an element found by text content
    pub async fn human_click_by_text(&self, text: &str) -> Result<()> {
        self.act(
            Action::Click {
                target: LocatorSpec::text(text),
                human: true,
                double: false,
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }

    /// Try to human-click an element, returning Ok(true) if clicked, Ok(false) if not found or not clickable
    #[must_use = "returns true if clicked, false if not found/visible"]
    pub async fn try_human_click(&self, selector: &str) -> Result<bool> {
        self.try_action(Action::Click {
            target: LocatorSpec::css(selector),
            human: true,
            double: false,
        })
        .await
    }

    /// Try to human-click an element by text, returning Ok(true) if clicked, Ok(false) if not found or not clickable
    #[must_use = "returns true if clicked, false if not found/visible"]
    pub async fn try_human_click_by_text(&self, text: &str) -> Result<bool> {
        self.try_action(Action::Click {
            target: LocatorSpec::text(text),
            human: true,
            double: false,
        })
        .await
    }

    /// Human-like form fill: click, clear, type with natural delays
    pub async fn human_fill(&self, selector: &str, value: &str) -> Result<()> {
        self.act(
            Action::Fill {
                target: LocatorSpec::css(selector),
                value: value.into(),
                human: true,
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
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
        self.act(
            Action::Upload {
                target: LocatorSpec::css(selector),
                files: paths.iter().map(|v| (*v).into()).collect(),
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }

    /// Select option by value
    pub async fn select(&self, selector: &str, value: &str) -> Result<()> {
        self.act(
            Action::Select {
                target: LocatorSpec::css(selector),
                values: vec![value.into()],
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }

    /// Select option by visible text
    pub async fn select_by_text(&self, selector: &str, text: &str) -> Result<()> {
        self.act(
            Action::SelectText {
                target: LocatorSpec::css(selector),
                text: text.into(),
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }

    /// Select multiple options by values
    pub async fn select_multiple(&self, selector: &str, values: &[&str]) -> Result<()> {
        self.act(
            Action::Select {
                target: LocatorSpec::css(selector),
                values: values.iter().map(|v| (*v).into()).collect(),
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }

    /// Hover over element (for revealing menus)
    pub async fn hover(&self, selector: &str) -> Result<()> {
        self.act(
            Action::Hover {
                target: LocatorSpec::css(selector),
                human: false,
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }

    /// Human-like hover with Bezier curve movement
    pub async fn human_hover(&self, selector: &str) -> Result<()> {
        self.act(
            Action::Hover {
                target: LocatorSpec::css(selector),
                human: true,
            },
            self.action_options.clone(),
        )
        .await
        .map(|_| ())
    }
}
