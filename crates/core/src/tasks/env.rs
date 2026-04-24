//! Task-level environment variable handling.

use crate::environment::{EnvValue, Environment};
use crate::{Error, Result};
use serde_json::Value;
use std::collections::HashMap;

/// Resolve task-level environment values into process environment variables.
///
/// Task env values arrive as JSON because they may include runtime-only objects
/// such as output references, host passthrough markers, or secret refs.
pub(crate) async fn resolve_task_env(
    task_name: &str,
    task_env: &HashMap<String, Value>,
) -> Result<(HashMap<String, String>, Vec<String>)> {
    let mut resolved = HashMap::new();
    let mut deferred = HashMap::new();

    for (key, value) in task_env {
        if let Some(string_value) = value.as_str() {
            if let Some(host_var) = super::output_refs::parse_passthrough(string_value) {
                if let Ok(host_value) = std::env::var(host_var) {
                    resolved.insert(key.clone(), host_value);
                }
            } else if !string_value.starts_with("cuenv:ref:") {
                resolved.insert(key.clone(), string_value.to_string());
            }
        } else if let Some(number) = value.as_i64() {
            resolved.insert(key.clone(), number.to_string());
        } else if let Some(boolean) = value.as_bool() {
            resolved.insert(key.clone(), boolean.to_string());
        } else if let Some(host_value) = passthrough_object_value(value, key) {
            resolved.insert(key.clone(), host_value);
        } else {
            let env_value: EnvValue = serde_json::from_value(value.clone()).map_err(|e| {
                Error::configuration(format!(
                    "Invalid environment value for task '{task_name}' variable '{key}': {e}"
                ))
            })?;
            deferred.insert(key.clone(), env_value);
        }
    }

    if deferred.is_empty() {
        return Ok((resolved, Vec::new()));
    }

    let (deferred_resolved, secrets) =
        Environment::resolve_for_task_with_secrets(task_name, &deferred).await?;
    resolved.extend(deferred_resolved);

    Ok((resolved, secrets))
}

fn passthrough_object_value(value: &Value, env_key: &str) -> Option<String> {
    let placeholder = super::output_refs::try_extract_passthrough(value, env_key)?;
    let host_var = super::output_refs::parse_passthrough(&placeholder)?;
    std::env::var(host_var).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn resolve_task_env_supports_passthrough_and_secret_values() {
        let home = std::env::var("HOME").unwrap();

        let task_env = HashMap::from([
            ("PLAIN".to_string(), json!("literal")),
            (
                "FROM_HOST".to_string(),
                json!({
                    "cuenvPassthrough": true,
                    "name": "HOME"
                }),
            ),
            (
                "SECRET".to_string(),
                json!({
                    "resolver": "exec",
                    "command": "echo",
                    "args": ["resolved-secret"]
                }),
            ),
        ]);

        let (resolved, secrets) = resolve_task_env("publish", &task_env).await.unwrap();
        assert_eq!(resolved.get("PLAIN"), Some(&"literal".to_string()));
        assert_eq!(resolved.get("FROM_HOST"), Some(&home));
        assert_eq!(resolved.get("SECRET"), Some(&"resolved-secret".to_string()));
        assert_eq!(secrets, vec!["resolved-secret".to_string()]);
    }
}
