//! CDP Connection/Session Management
//!
//! Manages browser and page sessions over the CDP transport.

use std::sync::Arc;

use super::transport::Transport;
use super::types::*;
use crate::error::Result;
use crate::page::HeldInputState;

/// A CDP connection to Chrome
pub struct Connection {
    transport: Arc<Transport>,
}

impl Connection {
    /// Create a new connection wrapping a transport
    pub(crate) fn new(transport: Transport) -> Self {
        Self {
            transport: Arc::new(transport),
        }
    }

    /// Get a reference to the transport
    pub fn transport(&self) -> &Arc<Transport> {
        &self.transport
    }

    /// Get browser version info
    pub(crate) async fn version(&self) -> Result<BrowserGetVersionResult> {
        self.transport
            .send("Browser.getVersion", &BrowserGetVersion {})
            .await
    }

    /// Create a new target (tab)
    pub(crate) async fn create_target(
        &self,
        url: &str,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<String> {
        let result: TargetCreateTargetResult = self
            .transport
            .send(
                "Target.createTarget",
                &TargetCreateTarget {
                    url: url.to_string(),
                    width,
                    height,
                },
            )
            .await?;
        Ok(result.target_id)
    }

    /// Attach to a target and get a session
    pub(crate) async fn attach_to_target(&self, target_id: &str) -> Result<Session> {
        let result: TargetAttachToTargetResult = self
            .transport
            .send(
                "Target.attachToTarget",
                &TargetAttachToTarget {
                    target_id: target_id.to_string(),
                    flatten: Some(true),
                },
            )
            .await?;

        Ok(Session {
            transport: Arc::clone(&self.transport),
            session_id: result.session_id,
            target_id: target_id.to_string(),
            held_input: Arc::new(tokio::sync::Mutex::new(HeldInputState::default())),
        })
    }

    /// Close a target
    pub(crate) async fn close_target(&self, target_id: &str) -> Result<bool> {
        let result: TargetCloseTargetResult = self
            .transport
            .send(
                "Target.closeTarget",
                &TargetCloseTarget {
                    target_id: target_id.to_string(),
                },
            )
            .await?;
        Ok(result.success)
    }

    /// Get all targets (tabs)
    pub(crate) async fn get_targets(&self) -> Result<Vec<TargetInfo>> {
        let result: TargetGetTargetsResult = self
            .transport
            .send("Target.getTargets", &TargetGetTargets {})
            .await?;
        Ok(result.target_infos)
    }

    /// Activate (focus) a target
    pub(crate) async fn activate_target(&self, target_id: &str) -> Result<()> {
        self.send_void(
            "Target.activateTarget",
            &TargetActivateTarget {
                target_id: target_id.to_string(),
            },
        )
        .await
    }

    /// Send a fire-and-forget command (discard the response)
    async fn send_void<C: serde::Serialize>(&self, method: &str, params: &C) -> Result<()> {
        self.transport
            .send::<_, serde_json::Value>(method, params)
            .await?;
        Ok(())
    }

    /// Close the browser
    pub(crate) async fn close(&self) -> Result<()> {
        if let Err(error) = self.send_void("Browser.close", &BrowserClose {}).await {
            // Chrome may close the socket before acknowledging Browser.close.
            // Transport::close still terminates and reaps managed Chrome.
            tracing::debug!("Browser.close command did not complete: {}", error);
        }
        self.transport.close().await
    }
}

/// A CDP session attached to a specific target. Cheap to clone (an `Arc` + two
/// small strings) — clones share the same underlying transport/target.
#[derive(Clone)]
pub struct Session {
    transport: Arc<Transport>,
    session_id: String,
    target_id: String,
    /// Per-target native input state shared by Page and Human helpers.
    held_input: Arc<tokio::sync::Mutex<HeldInputState>>,
}

impl Session {
    /// Get the session ID
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Get the target ID
    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    /// Get a reference to the underlying transport (for event polling)
    pub fn transport(&self) -> &Arc<Transport> {
        &self.transport
    }

    /// Return the per-target held-input coordinator state.
    pub(crate) fn held_input(&self) -> Arc<tokio::sync::Mutex<HeldInputState>> {
        Arc::clone(&self.held_input)
    }

    /// Send a command to this session
    pub async fn send<C, R>(&self, method: &str, params: &C) -> Result<R>
    where
        C: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        self.transport
            .send_to_session(&self.session_id, method, params)
            .await
    }

    /// Send a fire-and-forget command (discard the response)
    async fn send_void<C: serde::Serialize>(&self, method: &str, params: &C) -> Result<()> {
        self.send::<_, serde_json::Value>(method, params).await?;
        Ok(())
    }

    /// Enable the Fetch domain, optionally handling auth challenges (proxy auth).
    /// Must be called per-session (per-tab).
    /// Enable page events
    pub(crate) async fn page_enable(&self) -> Result<()> {
        self.send_void("Page.enable", &PageEnable {}).await
    }

    /// Navigate to a URL, optionally with a custom Referer header
    pub(crate) async fn navigate(
        &self,
        url: &str,
        referrer: Option<&str>,
    ) -> Result<PageNavigateResult> {
        self.send(
            "Page.navigate",
            &PageNavigate {
                url: url.to_string(),
                referrer: referrer.map(String::from),
            },
        )
        .await
    }

    /// Set extra HTTP headers sent with every request on this session.
    /// Pass an empty map to clear previously set headers.
    pub(crate) async fn set_extra_headers(
        &self,
        headers: std::collections::HashMap<String, String>,
    ) -> Result<()> {
        self.send_void(
            "Network.setExtraHTTPHeaders",
            &NetworkSetExtraHTTPHeaders { headers },
        )
        .await
    }

    /// Remove all extra HTTP headers previously set via set_extra_headers.
    pub(crate) async fn clear_extra_headers(&self) -> Result<()> {
        self.set_extra_headers(std::collections::HashMap::new())
            .await
    }

    /// Clear all browser cookies for this context.
    pub(crate) async fn clear_all_cookies(&self) -> Result<()> {
        self.send_void(
            "Network.clearBrowserCookies",
            &NetworkClearBrowserCookies {},
        )
        .await
    }

    /// Bulk-set multiple cookies at once.
    pub(crate) async fn set_cookies(&self, cookies: Vec<NetworkSetCookie>) -> Result<()> {
        self.send_void("Network.setCookies", &NetworkSetCookies { cookies })
            .await
    }

    /// Bypass CSP enforcement for the current page.
    /// Must be called before navigation to take effect.
    pub(crate) async fn set_bypass_csp(&self, enabled: bool) -> Result<()> {
        self.send_void("Page.setBypassCSP", &PageSetBypassCSP { enabled })
            .await
    }

    /// Switch JS execution off/on for the current page.
    /// Must be called before navigation to take effect.
    pub(crate) async fn set_script_execution_disabled(&self, disabled: bool) -> Result<()> {
        self.send_void(
            "Emulation.setScriptExecutionDisabled",
            &EmulationSetScriptExecutionDisabled { value: disabled },
        )
        .await
    }

    /// Override the User-Agent string (and optionally Accept-Language).
    pub(crate) async fn set_user_agent(
        &self,
        user_agent: &str,
        accept_language: Option<&str>,
    ) -> Result<()> {
        self.send_void(
            "Emulation.setUserAgentOverride",
            &EmulationSetUserAgentOverride {
                user_agent: user_agent.to_string(),
                accept_language: accept_language.map(String::from),
                platform: None,
                user_agent_metadata: None,
            },
        )
        .await
    }

    /// Override the User-Agent together with its client-hint metadata.
    pub(crate) async fn set_user_agent_full(
        &self,
        override_opts: EmulationSetUserAgentOverride,
    ) -> Result<()> {
        self.send_void("Emulation.setUserAgentOverride", &override_opts)
            .await
    }

    /// Set the browser-native ICU timezone for this session.
    pub(crate) async fn set_timezone_override(&self, timezone_id: &str) -> Result<()> {
        self.send_void(
            "Emulation.setTimezoneOverride",
            &EmulationSetTimezoneOverride {
                timezone_id: timezone_id.to_string(),
            },
        )
        .await
    }

    /// Ignore TLS certificate errors for this session.
    pub(crate) async fn set_ignore_cert_errors(&self, ignore: bool) -> Result<()> {
        self.send_void(
            "Security.setIgnoreCertificateErrors",
            &SecuritySetIgnoreCertificateErrors { ignore },
        )
        .await
    }

    /// Accept or dismiss a JavaScript dialog (alert / confirm / prompt).
    /// `prompt_text` is only used for prompt() dialogs.
    pub(crate) async fn handle_dialog(
        &self,
        accept: bool,
        prompt_text: Option<&str>,
    ) -> Result<()> {
        self.send_void(
            "Page.handleJavaScriptDialog",
            &PageHandleJavaScriptDialog {
                accept,
                prompt_text: prompt_text.map(String::from),
            },
        )
        .await
    }

    /// Enable Fetch domain request interception with URL patterns.
    /// `patterns` — list of URL/resource-type filters; empty matches everything.
    pub(crate) async fn fetch_enable_interception(
        &self,
        patterns: Vec<RequestPattern>,
        handle_auth: bool,
    ) -> Result<()> {
        self.send_void(
            "Fetch.enable",
            &FetchEnable {
                patterns: if patterns.is_empty() {
                    None
                } else {
                    Some(patterns)
                },
                handle_auth_requests: Some(handle_auth),
            },
        )
        .await
    }

    /// Reload the page
    pub(crate) async fn reload(&self, ignore_cache: bool) -> Result<()> {
        self.send_void(
            "Page.reload",
            &PageReload {
                ignore_cache: Some(ignore_cache),
                script_to_evaluate_on_load: None,
            },
        )
        .await
    }

    /// Navigate to a history entry by offset (-1 = back, +1 = forward)
    async fn navigate_history(&self, offset: i32) -> Result<()> {
        let history: PageGetNavigationHistoryResult = self
            .send("Page.getNavigationHistory", &PageGetNavigationHistory {})
            .await?;
        let target = history.current_index + offset;
        if target < 0 {
            return Ok(());
        }
        if let Some(entry) = history.entries.get(target as usize) {
            self.send_void(
                "Page.navigateToHistoryEntry",
                &PageNavigateToHistoryEntry { entry_id: entry.id },
            )
            .await?;
        }
        Ok(())
    }

    /// Go back in history
    pub(crate) async fn go_back(&self) -> Result<()> {
        self.navigate_history(-1).await
    }

    /// Go forward in history
    pub(crate) async fn go_forward(&self) -> Result<()> {
        self.navigate_history(1).await
    }

    /// Add a script to evaluate on every new document
    pub(crate) async fn add_script_to_evaluate_on_new_document(
        &self,
        source: &str,
    ) -> Result<String> {
        let result: PageAddScriptToEvaluateOnNewDocumentResult = self
            .send(
                "Page.addScriptToEvaluateOnNewDocument",
                &PageAddScriptToEvaluateOnNewDocument {
                    source: source.to_string(),
                    world_name: None,
                    include_command_line_api: None,
                },
            )
            .await?;
        Ok(result.identifier)
    }

    /// Capture a screenshot
    pub(crate) async fn capture_screenshot(
        &self,
        format: Option<&str>,
        quality: Option<u8>,
    ) -> Result<Vec<u8>> {
        // Chrome expects quality 0-100; clamp to valid range
        let quality = quality.map(|q| q.min(100));
        let result: PageCaptureScreenshotResult = self
            .send(
                "Page.captureScreenshot",
                &PageCaptureScreenshot {
                    format: format.map(String::from),
                    quality,
                },
            )
            .await?;

        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&result.data)
            .map_err(|e| crate::error::Error::Decode(e.to_string()))?;
        Ok(bytes)
    }

    /// Get the frame tree
    pub(crate) async fn get_frame_tree(&self) -> Result<FrameTree> {
        let result: PageGetFrameTreeResult =
            self.send("Page.getFrameTree", &PageGetFrameTree {}).await?;
        Ok(result.frame_tree)
    }

    /// Dispatch a raw mouse event with full control over all fields
    pub(crate) async fn dispatch_mouse_event_full(
        &self,
        event: InputDispatchMouseEvent,
    ) -> Result<()> {
        self.send_void("Input.dispatchMouseEvent", &event).await
    }

    /// Insert text at current cursor position
    pub(crate) async fn insert_text(&self, text: &str) -> Result<()> {
        self.send_void(
            "Input.insertText",
            &InputInsertText {
                text: text.to_string(),
            },
        )
        .await
    }

    /// Get the document root node
    pub(crate) async fn get_document(&self, depth: Option<i32>) -> Result<DOMNode> {
        let result: DOMGetDocumentResult = self
            .send(
                "DOM.getDocument",
                &DOMGetDocument {
                    depth,
                    pierce: Some(true),
                },
            )
            .await?;
        Ok(result.root)
    }

    /// Query for a single element
    pub(crate) async fn query_selector(&self, node_id: i32, selector: &str) -> Result<i32> {
        let result: DOMQuerySelectorResult = self
            .send(
                "DOM.querySelector",
                &DOMQuerySelector {
                    node_id,
                    selector: selector.to_string(),
                },
            )
            .await?;
        Ok(result.node_id)
    }

    /// Query for all matching elements
    pub(crate) async fn query_selector_all(
        &self,
        node_id: i32,
        selector: &str,
    ) -> Result<Vec<i32>> {
        let result: DOMQuerySelectorAllResult = self
            .send(
                "DOM.querySelectorAll",
                &DOMQuerySelectorAll {
                    node_id,
                    selector: selector.to_string(),
                },
            )
            .await?;
        Ok(result.node_ids)
    }

    /// Get the box model for an element
    pub(crate) async fn get_box_model(&self, node_id: i32) -> Result<BoxModel> {
        let result: DOMGetBoxModelResult = self
            .send(
                "DOM.getBoxModel",
                &DOMGetBoxModel {
                    node_id: Some(node_id),
                },
            )
            .await?;
        Ok(result.model)
    }

    /// Get outer HTML of an element
    pub(crate) async fn get_outer_html(&self, node_id: i32) -> Result<String> {
        let result: DOMGetOuterHTMLResult = self
            .send(
                "DOM.getOuterHTML",
                &DOMGetOuterHTML {
                    node_id: Some(node_id),
                },
            )
            .await?;
        Ok(result.outer_html)
    }

    /// Resolve a DOM node to a Runtime remote object ID
    pub(crate) async fn resolve_node(&self, node_id: i32) -> Result<String> {
        let result: DOMResolveNodeResult = self
            .send(
                "DOM.resolveNode",
                &DOMResolveNode {
                    node_id: Some(node_id),
                    object_group: Some("eoka".to_string()),
                },
            )
            .await?;
        result
            .object
            .object_id
            .ok_or_else(|| crate::error::Error::cdp("DOM.resolveNode", -1, "No object_id returned"))
    }

    /// Call a function on a remote object and return the result by value
    pub(crate) async fn call_function_on(
        &self,
        object_id: &str,
        function_declaration: &str,
    ) -> Result<RuntimeEvaluateResult> {
        self.call_function_on_impl(object_id, function_declaration, true)
            .await
    }

    /// Focus an element
    pub(crate) async fn focus(&self, node_id: i32) -> Result<()> {
        self.send_void(
            "DOM.focus",
            &DOMFocus {
                node_id: Some(node_id),
            },
        )
        .await
    }

    /// Get all cookies
    pub(crate) async fn get_cookies(&self, urls: Option<Vec<String>>) -> Result<Vec<Cookie>> {
        let result: NetworkGetCookiesResult = self
            .send("Network.getCookies", &NetworkGetCookies { urls })
            .await?;
        Ok(result.cookies)
    }

    /// Set a cookie
    pub(crate) async fn set_cookie(
        &self,
        name: &str,
        value: &str,
        url: Option<&str>,
        domain: Option<&str>,
        path: Option<&str>,
    ) -> Result<bool> {
        let result: NetworkSetCookieResult = self
            .send(
                "Network.setCookie",
                &NetworkSetCookie {
                    name: name.to_string(),
                    value: value.to_string(),
                    url: url.map(String::from),
                    domain: domain.map(String::from),
                    path: path.map(String::from),
                    ..Default::default()
                },
            )
            .await?;
        Ok(result.success)
    }

    /// Delete cookies
    pub(crate) async fn delete_cookies(
        &self,
        name: &str,
        url: Option<&str>,
        domain: Option<&str>,
    ) -> Result<()> {
        self.send_void(
            "Network.deleteCookies",
            &NetworkDeleteCookies {
                name: name.to_string(),
                url: url.map(String::from),
                domain: domain.map(String::from),
                ..Default::default()
            },
        )
        .await
    }

    /// Enable network events (request/response capture)
    /// NOTE: This enables Network.enable which may be slightly detectable
    pub(crate) async fn network_enable(&self) -> Result<()> {
        self.send_void(
            "Network.enable",
            &NetworkEnable {
                max_post_data_size: Some(65536), // Capture POST data up to 64KB
            },
        )
        .await
    }

    /// Disable network events
    pub(crate) async fn network_disable(&self) -> Result<()> {
        self.send_void("Network.disable", &NetworkDisable {}).await
    }

    /// Get response body for a request
    pub(crate) async fn get_response_body(&self, request_id: &str) -> Result<(String, bool)> {
        let result: NetworkGetResponseBodyResult = self
            .send(
                "Network.getResponseBody",
                &NetworkGetResponseBody {
                    request_id: request_id.to_string(),
                },
            )
            .await?;
        Ok((result.body, result.base64_encoded))
    }

    /// Evaluate JavaScript and return a remote object reference (not by value).
    pub(crate) async fn evaluate_for_remote_object(
        &self,
        expression: &str,
    ) -> Result<RuntimeEvaluateResult> {
        self.evaluate_impl(expression, false, Some("eoka"), true)
            .await
    }

    /// Convert a remote object ID to a DOM node_id via DOM.requestNode
    pub(crate) async fn request_node(&self, object_id: &str) -> Result<i32> {
        let result: DOMRequestNodeResult = self
            .send(
                "DOM.requestNode",
                &DOMRequestNode {
                    object_id: object_id.to_string(),
                },
            )
            .await?;
        Ok(result.node_id)
    }

    /// Get all own properties of a remote object (used for array element enumeration)
    pub(crate) async fn get_properties(
        &self,
        object_id: &str,
    ) -> Result<Vec<crate::cdp::types::PropertyDescriptor>> {
        let result: crate::cdp::types::RuntimeGetPropertiesResult = self
            .send(
                "Runtime.getProperties",
                &crate::cdp::types::RuntimeGetProperties {
                    object_id: object_id.to_string(),
                    own_properties: Some(true),
                },
            )
            .await?;
        Ok(result.result)
    }

    async fn call_function_on_impl(
        &self,
        object_id: &str,
        function_declaration: &str,
        return_by_value: bool,
    ) -> Result<RuntimeEvaluateResult> {
        let result: RuntimeCallFunctionOnResult = self
            .send(
                "Runtime.callFunctionOn",
                &RuntimeCallFunctionOn {
                    function_declaration: function_declaration.to_string(),
                    object_id: Some(object_id.to_string()),
                    arguments: None,
                    silent: None,
                    return_by_value: Some(return_by_value),
                    await_promise: Some(true),
                },
            )
            .await?;
        Ok(RuntimeEvaluateResult {
            result: result.result,
            exception_details: result.exception_details,
        })
    }

    /// Evaluate JavaScript expression and return the result by value
    pub(crate) async fn evaluate(&self, expression: &str) -> Result<RuntimeEvaluateResult> {
        self.evaluate_impl(expression, true, None, true).await
    }

    /// Evaluate JavaScript synchronously (don't await promises).
    /// Use this when the page may have unresolved promises that would block.
    pub(crate) async fn evaluate_sync(&self, expression: &str) -> Result<RuntimeEvaluateResult> {
        self.evaluate_impl(expression, true, None, false).await
    }

    async fn evaluate_impl(
        &self,
        expression: &str,
        return_by_value: bool,
        object_group: Option<&str>,
        await_promise: bool,
    ) -> Result<RuntimeEvaluateResult> {
        self.send(
            "Runtime.evaluate",
            &RuntimeEvaluate {
                expression: expression.to_string(),
                object_group: object_group.map(String::from),
                return_by_value: Some(return_by_value),
                await_promise: Some(await_promise),
            },
        )
        .await
    }

    /// Set files for a file input element
    pub(crate) async fn set_file_input_files(
        &self,
        node_id: i32,
        files: Vec<String>,
    ) -> Result<()> {
        self.send_void(
            "DOM.setFileInputFiles",
            &DOMSetFileInputFiles {
                files,
                node_id: Some(node_id),
                backend_node_id: None,
                object_id: None,
            },
        )
        .await
    }

    /// Dispatch a key event with full modifier support
    pub(crate) async fn dispatch_key_event_full(
        &self,
        event: InputDispatchKeyEventFull,
    ) -> Result<()> {
        self.send_void("Input.dispatchKeyEvent", &event).await
    }
}
