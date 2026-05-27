use super::model::{
    IMAGE_REF_PREFIX, OUTPUT_REF_PREFIX, OutputRefDep, PASSTHROUGH_PREFIX, TaskOutputField,
    TaskOutputRef,
};
use std::collections::HashMap;

/// Process task output references in raw JSON before deserialization.
///
/// Walks the `tasks` subtree of a CUE-evaluated JSON value and:
/// 1. Replaces `#TaskOutputRef` objects in `args` and `env` with placeholder strings
/// 2. Strips computed `stdout`/`stderr`/`exitCode` fields from task objects
/// 3. Returns dependency pairs for auto-dependency inference
///
/// This must be called on raw `serde_json::Value` BEFORE deserializing into
/// `Task` structs, because ref objects would fail `Vec<String>` deserialization.
pub fn process_output_refs(value: &mut serde_json::Value) -> Vec<OutputRefDep> {
    let mut deps = Vec::new();

    if let Some(tasks) = value.get_mut("tasks") {
        process_task_node(tasks, "", &mut deps);
    }

    deps
}

/// Recursively process a task node (Task, Group, or Sequence).
fn process_task_node(
    value: &mut serde_json::Value,
    current_task: &str,
    deps: &mut Vec<OutputRefDep>,
) {
    match value {
        serde_json::Value::Object(obj) => {
            // Check if this is a Task (has command or script)
            let is_task = obj.contains_key("command") || obj.contains_key("script");

            if is_task {
                // Strip computed output ref fields (they're schema artifacts)
                obj.remove("stdout");
                obj.remove("stderr");
                obj.remove("exitCode");

                // Process args array
                if let Some(serde_json::Value::Array(args)) = obj.get_mut("args") {
                    for arg in args.iter_mut() {
                        if let Some(placeholder) = try_extract_output_ref(arg) {
                            if let Some(parsed) = TaskOutputRef::parse(&placeholder) {
                                deps.push((current_task.to_string(), parsed.task.clone()));
                            }
                            *arg = serde_json::Value::String(placeholder);
                        }
                    }
                }

                // Process env map — replace output refs and passthrough objects with placeholders
                if let Some(serde_json::Value::Object(env)) = obj.get_mut("env") {
                    let keys: Vec<String> = env.keys().cloned().collect();
                    for key in keys {
                        let Some(env_val) = env.get_mut(&key) else {
                            continue;
                        };
                        if let Some(placeholder) = try_extract_output_ref(env_val) {
                            if let Some(parsed) = TaskOutputRef::parse(&placeholder) {
                                deps.push((current_task.to_string(), parsed.task.clone()));
                            }
                            *env_val = serde_json::Value::String(placeholder);
                        } else if let Some(placeholder) = try_extract_passthrough(env_val, &key) {
                            *env_val = serde_json::Value::String(placeholder);
                        }
                    }
                }

                return;
            }

            // Check if this is a TaskGroup (type: "group")
            let is_group = obj
                .get("type")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s == "group");

            if is_group {
                // Process group children
                let child_keys: Vec<String> = obj
                    .keys()
                    .filter(|k| {
                        !matches!(
                            k.as_str(),
                            "type" | "dependsOn" | "maxConcurrency" | "description"
                        )
                    })
                    .cloned()
                    .collect();

                for key in child_keys {
                    let child_task = if current_task.is_empty() {
                        key.clone()
                    } else {
                        format!("{current_task}.{key}")
                    };
                    if let Some(child) = obj.get_mut(&key) {
                        process_task_node(child, &child_task, deps);
                    }
                }
                return;
            }

            // Named task children (top-level tasks map or nested struct)
            let keys: Vec<String> = obj.keys().cloned().collect();
            for key in keys {
                let child_task = if current_task.is_empty() {
                    key.clone()
                } else {
                    format!("{current_task}.{key}")
                };
                if let Some(child) = obj.get_mut(&key) {
                    process_task_node(child, &child_task, deps);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            // Sequence: process each element
            for (i, element) in arr.iter_mut().enumerate() {
                let child_task = format!("{current_task}[{i}]");
                process_task_node(element, &child_task, deps);
            }
        }
        _ => {}
    }
}

/// Try to extract an `#EnvPassthrough` from a JSON value.
/// Returns the passthrough placeholder string if the value is a passthrough object.
/// `env_key` is the map key, used as fallback when `name` is absent.
pub(crate) fn try_extract_passthrough(value: &serde_json::Value, env_key: &str) -> Option<String> {
    let obj = value.as_object()?;

    let is_passthrough = obj
        .get("cuenvPassthrough")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !is_passthrough {
        return None;
    }

    let var_name = obj.get("name").and_then(|v| v.as_str()).unwrap_or(env_key);

    Some(format!("{PASSTHROUGH_PREFIX}{var_name}"))
}

/// Parse a passthrough placeholder string, returning the host var name.
#[must_use]
pub fn parse_passthrough(s: &str) -> Option<&str> {
    s.strip_prefix(PASSTHROUGH_PREFIX)
}

/// Try to extract a `#TaskOutputRef` or `#ImageOutputRef` from a JSON value.
/// Returns the placeholder string if the value is a ref object, None otherwise.
pub(crate) fn try_extract_output_ref(value: &serde_json::Value) -> Option<String> {
    let obj = value.as_object()?;

    // Check discriminator field
    let is_ref = obj
        .get("cuenvOutputRef")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !is_ref {
        return None;
    }

    // Task output ref: { cuenvOutputRef: true, cuenvTask: "...", cuenvOutput: "stdout"|"stderr"|"exitCode" }
    if let Some(task) = obj.get("cuenvTask").and_then(|v| v.as_str()) {
        let output = obj.get("cuenvOutput")?.as_str()?;
        let output_field = match output {
            "stdout" => TaskOutputField::Stdout,
            "stderr" => TaskOutputField::Stderr,
            "exitCode" => TaskOutputField::ExitCode,
            _ => return None,
        };
        let r = TaskOutputRef {
            task: task.to_string(),
            output: output_field,
        };
        return Some(r.to_placeholder());
    }

    // Image output ref: { cuenvOutputRef: true, cuenvImage: "...", cuenvOutput: "ref"|"digest" }
    if let Some(image) = obj.get("cuenvImage").and_then(|v| v.as_str()) {
        let output = obj.get("cuenvOutput")?.as_str()?;
        if output != "ref" && output != "digest" {
            return None;
        }
        return Some(format!("{IMAGE_REF_PREFIX}{image}:{output}"));
    }

    None
}

/// Returns `true` if any string in `args` or `env` contains an output ref placeholder.
///
/// Use this as a fast check to avoid cloning tasks that have no refs to resolve.
#[must_use]
pub fn has_output_refs(args: &[String], env: &HashMap<String, serde_json::Value>) -> bool {
    let has_ref = |s: &str| s.starts_with(OUTPUT_REF_PREFIX) || s.starts_with(IMAGE_REF_PREFIX);
    args.iter().any(|a| has_ref(a)) || env.values().any(|v| v.as_str().is_some_and(has_ref))
}
