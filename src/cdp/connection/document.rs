//! Document commands on a CDP session.

use super::Session;
use crate::cdp::types::*;
use crate::Result;

impl Session {
    pub(crate) async fn create_isolated_world(&self, frame_id: &str) -> Result<i64> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct World {
            execution_context_id: i64,
        }

        let result: World = self
            .send(
                "Page.createIsolatedWorld",
                &serde_json::json!({
                    "frameId":frame_id, "worldName":"eoka", "grantUniveralAccess":false,
                }),
            )
            .await?;
        Ok(result.execution_context_id)
    }

    pub(crate) async fn evaluate_in_context(
        &self,
        expression: &str,
        context_id: i64,
    ) -> Result<RuntimeEvaluateResult> {
        self.send(
            "Runtime.evaluate",
            &serde_json::json!({
                "expression":expression, "contextId":context_id,
                "returnByValue":true, "awaitPromise":true,
            }),
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
}
