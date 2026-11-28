//! Document commands on a CDP session.

use super::Session;
use crate::cdp::types::*;
use crate::Result;
use std::sync::atomic::{AtomicU64, Ordering};

/// A per-call Runtime object group, released (best effort) on drop so remote
/// objects do not accumulate in the page for the document's lifetime.
pub(crate) struct ObjectGroup {
    session: Session,
    pub(crate) name: String,
    active: bool,
}

impl ObjectGroup {
    pub(crate) fn new(session: &Session) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            session: session.clone(),
            name: format!("eoka-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
            active: true,
        }
    }

    /// Release now and report the outcome; dropping releases in the background.
    pub(crate) async fn release(mut self) -> Result<()> {
        release_group(&self.session, &self.name).await?;
        self.active = false;
        Ok(())
    }
}

async fn release_group(session: &Session, name: &str) -> Result<()> {
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        session.send::<_, serde_json::Value>(
            "Runtime.releaseObjectGroup",
            &serde_json::json!({ "objectGroup": name }),
        ),
    )
    .await
    .map_err(|_| crate::Error::cdp_msg("Object group cleanup timed out"))??;
    Ok(())
}

impl Drop for ObjectGroup {
    fn drop(&mut self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if !self.active {
            return;
        }
        let session = self.session.clone();
        let name = std::mem::take(&mut self.name);
        // A navigated or closed page has already discarded the group.
        runtime.spawn(async move { drop(release_group(&session, &name).await) });
    }
}

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

    /// Evaluate JavaScript and return a remote object reference (not by value).
    /// The result, and objects derived from it such as `Runtime.getProperties`
    /// entries, live in a per-call group released when the guard drops.
    pub(crate) async fn evaluate_for_remote_object(
        &self,
        expression: &str,
    ) -> Result<(RuntimeEvaluateResult, ObjectGroup)> {
        let group = ObjectGroup::new(self);
        let result = self
            .evaluate_impl(expression, false, Some(&group.name), true)
            .await?;
        Ok((result, group))
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
