use super::{Element, Page, TextMatch};
use crate::action::{ActionCall, TryActionCall};
use crate::error::{Error, Result};
use crate::{Action, InputMode, LocatorSpec};

/// A JavaScript expression evaluating to the elements `text` matches, in
/// priority order. Arguments are JSON literals, never spliced as source.
fn text_candidates_expression(text: &str, match_type: TextMatch, all: bool) -> Result<String> {
    let mode = match match_type {
        TextMatch::Exact => "exact",
        TextMatch::Contains => "contains",
        TextMatch::StartsWith => "starts_with",
        TextMatch::EndsWith => "ends_with",
    };
    Ok(format!(
        "({})({}, '{mode}', {all})",
        include_str!("text_candidates.js"),
        serde_json::to_string(text)?
    ))
}

fn click_action(target: LocatorSpec, human: bool) -> Action {
    Action::Click {
        target,
        human,
        double: false,
    }
}

fn hover_action(selector: &str, human: bool) -> Action {
    Action::Hover {
        target: LocatorSpec::css(selector),
        human,
    }
}

fn type_action(selector: &str, text: &str, input_mode: InputMode) -> Action {
    Action::Type {
        target: LocatorSpec::css(selector),
        text: text.into(),
        input_mode,
    }
}

fn fill_action(selector: &str, value: &str, input_mode: InputMode) -> Action {
    Action::Fill {
        target: LocatorSpec::css(selector),
        value: value.into(),
        input_mode,
    }
}

impl Page {
    /// Run `action` with this page's [`crate::ActionDefaults`], discarding the receipt.
    async fn act_default(&self, action: Action) -> Result<()> {
        self.act(action, self.action_defaults.options())
            .await
            .map(|_| ())
    }

    /// Drag horizontally from the selected element's center.
    /// For explicit cancellation cleanup, retain a [`crate::stealth::Human`] helper.
    pub fn human_drag(&self, selector: &str, dx: f64) -> ActionCall {
        ActionCall::new(
            self,
            Action::Drag {
                target: LocatorSpec::css(selector),
                dx,
                dy: 0.0,
            },
        )
    }

    /// Find an element by CSS selector
    pub async fn find(&self, selector: &str) -> Result<Element> {
        let node_id = self
            .with_document(|doc| self.session.query_selector(doc, selector))
            .await?;

        if node_id == 0 {
            return Err(Error::ElementNotFound(selector.to_string()));
        }

        Element::from_node_id(self, node_id).await
    }

    /// Find all elements matching a CSS selector
    pub async fn find_all(&self, selector: &str) -> Result<Vec<Element>> {
        let node_ids = self
            .with_document(|doc| self.session.query_selector_all(doc, selector))
            .await?;

        futures_util::future::try_join_all(
            node_ids
                .into_iter()
                .filter(|&id| id != 0)
                .map(|node_id| Element::from_node_id(self, node_id)),
        )
        .await
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
    /// Interactive elements (a, button, input) win over static ones; among
    /// static elements the innermost match is returned, never a container
    /// that merely includes the text. Matching is the same as
    /// [`Page::locator_text`]. The DOM is not mutated.
    pub async fn find_by_text_match(&self, text: &str, match_type: TextMatch) -> Result<Element> {
        let js = format!(
            "({})[0] ?? null",
            text_candidates_expression(text, match_type, false)?
        );
        let (result, _objects) = self.session.evaluate_for_remote_object(&js).await?;
        let remote = self.check_js_result(result)?;

        if remote.subtype.as_deref() == Some("null") {
            return Err(Error::ElementNotFound(format!("text: {}", text)));
        }

        let object_id = remote
            .object_id
            .ok_or_else(|| Error::ElementNotFound(format!("text: {}", text)))?;

        let node_id = self.request_nodes(std::slice::from_ref(&object_id)).await?[0];
        if node_id == 0 {
            return Err(Error::ElementNotFound(format!("text: {}", text)));
        }

        Element::from_node_id(self, node_id).await
    }

    /// Find all elements matching the given text
    pub async fn find_all_by_text(&self, text: &str) -> Result<Vec<Element>> {
        let js = text_candidates_expression(text, TextMatch::Contains, true)?;

        // Evaluate without returnByValue to get remote object references
        let (result, _objects) = self.session.evaluate_for_remote_object(&js).await?;

        let remote = self.check_js_result(result)?;

        let array_object_id = match &remote.object_id {
            Some(id) => id.clone(),
            None => return Ok(Vec::new()),
        };

        // Get all indexed properties of the array in one CDP call. Array
        // elements have numeric names; skip "length" and prototype props.
        let properties = self.session.get_properties(&array_object_id).await?;
        let object_ids: Vec<String> = properties
            .into_iter()
            .filter(|prop| prop.name.parse::<usize>().is_ok())
            .filter_map(|prop| prop.value?.object_id)
            .collect();

        let node_ids = self.request_nodes(&object_ids).await?;
        futures_util::future::try_join_all(
            node_ids
                .into_iter()
                .filter(|&node_id| node_id != 0)
                .map(|node_id| Element::from_node_id(self, node_id)),
        )
        .await
    }

    /// Convert remote element objects to DOM node ids. `DOM.requestNode` returns
    /// 0 unless `DOM.getDocument` has populated the node-id space for the current
    /// document, and the cached document node is stale after a navigation the
    /// page started itself. On a 0, refresh the cache and convert every object
    /// again, since ids from before the refresh are not valid after it.
    async fn request_nodes(&self, object_ids: &[String]) -> Result<Vec<i32>> {
        let mut refreshed = false;
        loop {
            self.document_node().await?;
            let mut node_ids = Vec::with_capacity(object_ids.len());
            for object_id in object_ids {
                node_ids.push(self.session.request_node(object_id).await?);
            }
            if refreshed || !node_ids.contains(&0) {
                return Ok(node_ids);
            }
            self.invalidate_document();
            refreshed = true;
        }
    }

    /// Check if an element with the given text exists
    #[must_use = "returns true if text exists on page"]
    pub async fn text_exists(&self, text: &str) -> bool {
        self.find_by_text(text).await.is_ok()
    }

    /// Click an element by selector. Awaiting proves dispatch only; add
    /// [`ActionCall::expecting`] to prove the page reacted.
    pub fn click(&self, selector: &str) -> ActionCall {
        ActionCall::new(self, click_action(LocatorSpec::css(selector), false))
    }

    /// Type text into an element by selector. Awaiting proves dispatch only; see [`ActionCall`].
    pub fn type_into(&self, selector: &str, text: &str) -> ActionCall {
        ActionCall::new(self, type_action(selector, text, InputMode::default()))
    }

    /// Click an element by its text content. Awaiting proves dispatch only; see [`ActionCall`].
    pub fn click_by_text(&self, text: &str) -> ActionCall {
        ActionCall::new(self, click_action(LocatorSpec::text(text), false))
    }

    /// Try to click an element, returning Ok(false) if not found or not clickable
    pub fn try_click(&self, selector: &str) -> TryActionCall {
        TryActionCall::new(self, click_action(LocatorSpec::css(selector), false))
    }

    /// Try to click an element by text, returning Ok(false) if not found or not clickable
    pub fn try_click_by_text(&self, text: &str) -> TryActionCall {
        TryActionCall::new(self, click_action(LocatorSpec::text(text), false))
    }

    /// Fill a form field: click, clear, type, and verify the resulting value.
    ///
    /// Range inputs are assigned through their DOM property rather than text
    /// insertion. The requested value must be finite and satisfy the range's
    /// min/max/step constraints; accepted range values dispatch bubbling
    /// `input` and `change` events.
    pub async fn fill(&self, selector: &str, value: &str) -> Result<()> {
        self.act_default(fill_action(selector, value, InputMode::default()))
            .await
    }

    /// Human-like click on an element
    pub fn human_click(&self, selector: &str) -> ActionCall {
        ActionCall::new(self, click_action(LocatorSpec::css(selector), true))
    }

    /// Human-like typing into an element: key events at irregular intervals
    /// ([`crate::InputMode::Human`]).
    pub fn human_type(&self, selector: &str, text: &str) -> ActionCall {
        ActionCall::new(self, type_action(selector, text, InputMode::Human))
    }

    /// Human-like click on an element found by text content
    pub fn human_click_by_text(&self, text: &str) -> ActionCall {
        ActionCall::new(self, click_action(LocatorSpec::text(text), true))
    }

    /// Try to human-click an element, returning Ok(true) if clicked, Ok(false) if not found or not clickable
    pub fn try_human_click(&self, selector: &str) -> TryActionCall {
        TryActionCall::new(self, click_action(LocatorSpec::css(selector), true))
    }

    /// Try to human-click an element by text, returning Ok(true) if clicked, Ok(false) if not found or not clickable
    pub fn try_human_click_by_text(&self, text: &str) -> TryActionCall {
        TryActionCall::new(self, click_action(LocatorSpec::text(text), true))
    }

    /// Human-like form fill: clear, then type with key events at irregular
    /// intervals ([`crate::InputMode::Human`]), and verify the value.
    pub async fn human_fill(&self, selector: &str, value: &str) -> Result<()> {
        self.act_default(fill_action(selector, value, InputMode::Human))
            .await
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
        self.act_default(Action::Upload {
            target: LocatorSpec::css(selector),
            files: paths.iter().map(|v| (*v).into()).collect(),
        })
        .await
    }

    /// Select option by value
    pub async fn select(&self, selector: &str, value: &str) -> Result<()> {
        self.act_default(Action::Select {
            target: LocatorSpec::css(selector),
            values: vec![value.into()],
        })
        .await
    }

    /// Select option by visible text
    pub async fn select_by_text(&self, selector: &str, text: &str) -> Result<()> {
        self.act_default(Action::SelectText {
            target: LocatorSpec::css(selector),
            text: text.into(),
        })
        .await
    }

    /// Select multiple options by values
    pub async fn select_multiple(&self, selector: &str, values: &[&str]) -> Result<()> {
        self.act_default(Action::Select {
            target: LocatorSpec::css(selector),
            values: values.iter().map(|v| (*v).into()).collect(),
        })
        .await
    }

    /// Hover over element (for revealing menus)
    pub async fn hover(&self, selector: &str) -> Result<()> {
        self.act_default(hover_action(selector, false)).await
    }

    /// Human-like hover with Bezier curve movement
    pub async fn human_hover(&self, selector: &str) -> Result<()> {
        self.act_default(hover_action(selector, true)).await
    }
}
