//! Task-level environment variable handling.

use crate::environment::EnvValue;
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
    let mut secrets = Vec::new();

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
        } else if let Some(host_var) = passthrough_object_host_var(value, key) {
            if let Ok(host_value) = std::env::var(host_var) {
                resolved.insert(key.clone(), host_value);
            }
        } else {
            let env_value: EnvValue = serde_json::from_value(value.clone()).map_err(|e| {
                Error::configuration(format!(
                    "Invalid environment value for task '{task_name}' variable '{key}': {e}"
                ))
            })?;

            if env_value.is_accessible_by_task(task_name) {
                let (value, mut resolved_secrets) = env_value.resolve_with_secrets().await?;
                resolved.insert(key.clone(), value);
                secrets.append(&mut resolved_secrets);
            }
        }
    }

    Ok((resolved, secrets))
}

fn passthrough_object_host_var<'a>(value: &'a Value, env_key: &'a str) -> Option<&'a str> {
    let obj = value.as_object()?;
    let is_passthrough = obj
        .get("cuenvPassthrough")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if !is_passthrough {
        return None;
    }

    Some(obj.get("name").and_then(Value::as_str).unwrap_or(env_key))
}
