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
    /// Call a function once with JSON arguments in a fresh lexical scope.
    pub async fn evaluate_function<T: serde::de::DeserializeOwned>(
        &self,
        function: &str,
        arguments: &[serde_json::Value],
    ) -> Result<T> {
        let value: serde_json::Value = self
            .evaluate(&Self::function_expression(function, arguments)?)
            .await?;
        if serde_json::to_vec(&value)?.len() > 1_048_576 {
            return Err(Error::cdp_msg("JavaScript result exceeds 1 MiB"));
        }
        Ok(serde_json::from_value(value)?)
    }

    /// Call a function once and discard its result while retaining exceptions.
    pub async fn execute_function(
        &self,
        function: &str,
        arguments: &[serde_json::Value],
    ) -> Result<()> {
        self.execute(&Self::function_expression(function, arguments)?)
            .await
    }

    /// Build an explicit function invocation; arguments never become executable source.
    pub fn function_expression(function: &str, arguments: &[serde_json::Value]) -> Result<String> {
        fn validate(value: &serde_json::Value, depth: usize) -> Result<()> {
            if depth > 64 {
                return Err(Error::cdp_msg("JavaScript argument nesting exceeds 64"));
            }
            match value {
                serde_json::Value::Number(n)
                    if n.as_f64().is_none_or(|n| {
                        !n.is_finite() || (n.fract() == 0.0 && n.abs() > 9_007_199_254_740_991.0)
                    }) =>
                {
                    return Err(Error::cdp_msg(
                        "JavaScript argument contains an unsafe number",
                    ))
                }
                serde_json::Value::Array(values) => {
                    for value in values {
                        validate(value, depth + 1)?;
                    }
                }
                serde_json::Value::Object(values) => {
                    for value in values.values() {
                        validate(value, depth + 1)?;
                    }
                }
                _ => {}
            }
            Ok(())
        }
        for value in arguments {
            validate(value, 0)?;
        }
        let args = serde_json::to_string(arguments)?;
        if args.len() > 1_048_576 || function.len() > 1_048_576 {
            return Err(Error::cdp_msg(
                "JavaScript function or arguments exceed 1 MiB",
            ));
        }
        let encoded = serde_json::to_string(&args)?;
        Ok(format!("({function})(...JSON.parse({encoded}))"))
    }

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
            return Err(Error::cdp_msg(exception.message()));
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
