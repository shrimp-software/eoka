use super::Page;
use crate::error::{Error, Result};

fn evaluated_value(remote: crate::cdp::types::RemoteObject) -> Result<serde_json::Value> {
    match (
        remote.value,
        remote.r#type.as_str(),
        remote.subtype.as_deref(),
    ) {
        (Some(value), _, _) => Ok(value),
        (None, "object", Some("null")) => Ok(serde_json::Value::Null),
        _ => Err(Error::cdp_msg("No value returned from evaluate")),
    }
}

impl Page {
    /// Evaluate JavaScript and return the result
    pub async fn evaluate<T: serde::de::DeserializeOwned>(&self, expression: &str) -> Result<T> {
        self.eval_impl(self.session.evaluate(expression).await?)
    }

    /// Evaluate JavaScript synchronously (don't await promises).
    /// Use when the page may have unresolved promises that block normal evaluate.
    pub async fn evaluate_sync<T: serde::de::DeserializeOwned>(
        &self,
        expression: &str,
    ) -> Result<T> {
        self.eval_impl(self.session.evaluate_sync(expression).await?)
    }

    /// Shared impl: check for exceptions and extract the value
    pub(super) fn eval_impl<T: serde::de::DeserializeOwned>(
        &self,
        result: crate::cdp::types::RuntimeEvaluateResult,
    ) -> Result<T> {
        let remote = self.check_js_result(result)?;
        Ok(serde_json::from_value(evaluated_value(remote)?)?)
    }

    /// Execute JavaScript without expecting a return value
    pub async fn execute(&self, expression: &str) -> Result<()> {
        self.check_js_result(self.session.evaluate(expression).await?)?;
        Ok(())
    }

    /// Execute JavaScript synchronously (don't await promises)
    pub async fn execute_sync(&self, expression: &str) -> Result<()> {
        self.check_js_result(self.session.evaluate_sync(expression).await?)?;
        Ok(())
    }

    /// Check a JS evaluation result for exceptions
    pub(super) fn check_js_result(
        &self,
        result: crate::cdp::types::RuntimeEvaluateResult,
    ) -> Result<crate::cdp::types::RemoteObject> {
        if let Some(exception) = result.exception_details {
            return Err(Error::cdp_msg(format!(
                "JavaScript error: {} at {}:{}",
                exception.text, exception.line_number, exception.column_number
            )));
        }
        Ok(result.result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_null_is_a_value_but_undefined_is_not() {
        let remote = serde_json::from_value(
            serde_json::json!({"type":"object", "subtype":"null", "value":null}),
        )
        .unwrap();
        let decoded: Option<String> =
            serde_json::from_value(evaluated_value(remote).unwrap()).unwrap();
        assert_eq!(decoded, None);
        let remote = serde_json::from_value(serde_json::json!({"type":"undefined"})).unwrap();
        assert!(evaluated_value(remote).is_err());
        for value in [
            serde_json::json!(0),
            serde_json::json!(false),
            serde_json::json!(""),
            serde_json::json!({"a":1}),
        ] {
            let remote = serde_json::from_value(serde_json::json!({"value":value})).unwrap();
            assert_eq!(evaluated_value(remote).unwrap(), value);
        }
    }
}
