//! Task output reference processing.
//!
//! Handles detection and resolution of `#TaskOutputRef` structs in task JSON.
//! These structs are produced by CUE when tasks reference another task's
//! `stdout`, `stderr`, or `exitCode` fields.
//!
//! # Processing Pipeline
//!
//! 1. CUE evaluates `tasks.tmpdir.stdout` → `{ cuenvOutputRef: true, cuenvTask: "tmpdir", cuenvOutput: "stdout" }`
//! 2. [`process_output_refs`] walks raw JSON, replaces ref objects with placeholder strings
//! 3. Task deserialization sees plain strings in `args`/`env` (`Vec<String>`)
//! 4. [`OutputRefResolver::resolve`] replaces placeholder strings with actual values before execution

use super::TaskResult;
use crate::{Error, Result};
use std::collections::HashMap;

/// Prefix for placeholder strings that represent task output references.
/// Format: `cuenv:ref:<task_name>:<output_field>`
const OUTPUT_REF_PREFIX: &str = "cuenv:ref:";

/// Which output field of a task is being referenced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOutputField {
    Stdout,
    Stderr,
    ExitCode,
}

/// A parsed task output reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOutputRef {
    /// Name of the referenced task (e.g., "tmpdir", "pipeline[0]")
    pub task: String,
    /// Which output field is referenced
    pub output: TaskOutputField,
}

impl TaskOutputRef {
    /// Parse a placeholder string like `"cuenv:ref:tmpdir:stdout"`.
    /// Returns `None` if the string is not a valid output ref placeholder.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let rest = s.strip_prefix(OUTPUT_REF_PREFIX)?;
        // Find the last ':' to split task name from output field.
        // Task names can contain dots and brackets but not colons.
        let last_colon = rest.rfind(':')?;
        let task = &rest[..last_colon];
        let output_str = &rest[last_colon + 1..];

        if task.is_empty() {
            return None;
        }

        let output = match output_str {
            "stdout" => TaskOutputField::Stdout,
            "stderr" => TaskOutputField::Stderr,
            "exitCode" => TaskOutputField::ExitCode,
            _ => return None,
        };

        Some(Self {
            task: task.to_string(),
            output,
        })
    }

    /// Convert to a placeholder string.
    #[must_use]
    pub fn to_placeholder(&self) -> String {
        let output_str = match self.output {
            TaskOutputField::Stdout => "stdout",
            TaskOutputField::Stderr => "stderr",
            TaskOutputField::ExitCode => "exitCode",
        };
        format!("{OUTPUT_REF_PREFIX}{}:{output_str}", self.task)
    }
}

/// A dependency pair: (task_that_references, task_being_referenced).
pub type OutputRefDep = (String, String);

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

                // Process env map
                if let Some(serde_json::Value::Object(env)) = obj.get_mut("env") {
                    for env_val in env.values_mut() {
                        if let Some(placeholder) = try_extract_output_ref(env_val) {
                            if let Some(parsed) = TaskOutputRef::parse(&placeholder) {
                                deps.push((current_task.to_string(), parsed.task.clone()));
                            }
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

/// Try to extract a `#TaskOutputRef` from a JSON value.
/// Returns the placeholder string if the value is a ref object, None otherwise.
fn try_extract_output_ref(value: &serde_json::Value) -> Option<String> {
    let obj = value.as_object()?;

    // Check discriminator field
    let is_ref = obj
        .get("cuenvOutputRef")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !is_ref {
        return None;
    }

    let task = obj.get("cuenvTask")?.as_str()?;
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
    Some(r.to_placeholder())
}

/// Context for resolving task output reference placeholders at runtime.
pub struct OutputRefResolver<'a> {
    /// Name of the task being resolved (for error messages)
    pub task_name: &'a str,
    /// Completed upstream task results to resolve references against
    pub results: &'a HashMap<String, TaskResult>,
}

impl<'a> OutputRefResolver<'a> {
    /// Resolve all output ref placeholder strings in a task's args and env.
    ///
    /// Called just before task execution. Replaces placeholder strings with
    /// actual values from completed upstream tasks.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - A referenced task has not completed (missing from results)
    /// - A referenced task failed (non-zero exit code)
    /// - An `exitCode` ref is used in a string context (exitCode is int-only)
    pub fn resolve(
        &self,
        args: &mut [String],
        env: &mut HashMap<String, serde_json::Value>,
    ) -> Result<()> {
        // Resolve args
        for arg in args.iter_mut() {
            if let Some(resolved) = resolve_single_ref(self.task_name, arg, self.results)? {
                *arg = resolved;
            }
        }

        // Resolve env values
        for (_env_key, env_val) in env.iter_mut() {
            if let Some(s) = env_val.as_str()
                && let Some(resolved) = resolve_single_ref(self.task_name, s, self.results)?
            {
                *env_val = serde_json::Value::String(resolved);
            }
        }

        Ok(())
    }
}

/// Resolve a single placeholder string, returning the resolved value.
/// Returns Ok(None) if the string is not a placeholder.
fn resolve_single_ref(
    task_name: &str,
    value: &str,
    results: &HashMap<String, TaskResult>,
) -> Result<Option<String>> {
    let Some(output_ref) = TaskOutputRef::parse(value) else {
        return Ok(None);
    };

    // exitCode cannot be used in string context (args/env)
    if output_ref.output == TaskOutputField::ExitCode {
        return Err(Error::configuration(format!(
            "Task '{}': cannot use exitCode of '{}' in args/env (exitCode is an integer, not a string)",
            task_name, output_ref.task
        )));
    }

    let result = results.get(&output_ref.task).ok_or_else(|| {
        Error::configuration(format!(
            "Task '{}': references output of '{}', but that task has not completed",
            task_name, output_ref.task
        ))
    })?;

    if !result.success {
        return Err(Error::task_failed(
            &output_ref.task,
            result.exit_code.unwrap_or(-1),
            &result.stdout,
            &result.stderr,
        ));
    }

    let resolved = match output_ref.output {
        TaskOutputField::Stdout => result.stdout.trim().to_string(),
        TaskOutputField::Stderr => result.stderr.trim().to_string(),
        TaskOutputField::ExitCode => unreachable!("handled above"),
    };

    Ok(Some(resolved))
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // TaskOutputRef::parse tests
    // =========================================================================

    #[test]
    fn parse_valid_stdout_ref() {
        let r = TaskOutputRef::parse("cuenv:ref:tmpdir:stdout").unwrap();
        assert_eq!(r.task, "tmpdir");
        assert_eq!(r.output, TaskOutputField::Stdout);
    }

    #[test]
    fn parse_valid_stderr_ref() {
        let r = TaskOutputRef::parse("cuenv:ref:build:stderr").unwrap();
        assert_eq!(r.task, "build");
        assert_eq!(r.output, TaskOutputField::Stderr);
    }

    #[test]
    fn parse_valid_exit_code_ref() {
        let r = TaskOutputRef::parse("cuenv:ref:check:exitCode").unwrap();
        assert_eq!(r.task, "check");
        assert_eq!(r.output, TaskOutputField::ExitCode);
    }

    #[test]
    fn parse_dotted_task_name() {
        let r = TaskOutputRef::parse("cuenv:ref:check.lint:stdout").unwrap();
        assert_eq!(r.task, "check.lint");
        assert_eq!(r.output, TaskOutputField::Stdout);
    }

    #[test]
    fn parse_bracketed_task_name() {
        let r = TaskOutputRef::parse("cuenv:ref:pipeline[0]:stdout").unwrap();
        assert_eq!(r.task, "pipeline[0]");
        assert_eq!(r.output, TaskOutputField::Stdout);
    }

    #[test]
    fn parse_non_ref_string() {
        assert!(TaskOutputRef::parse("hello world").is_none());
        assert!(TaskOutputRef::parse("").is_none());
        assert!(TaskOutputRef::parse("cuenv:ref:").is_none());
        assert!(TaskOutputRef::parse("cuenv:ref::stdout").is_none());
    }

    #[test]
    fn parse_fqdn_task_name() {
        // FQDN format: task:project_id:task_name — contains colons
        let r = TaskOutputRef::parse("cuenv:ref:task:myproject:build:stdout").unwrap();
        assert_eq!(r.task, "task:myproject:build");
        assert_eq!(r.output, TaskOutputField::Stdout);
    }

    #[test]
    fn roundtrip_fqdn_placeholder() {
        let r = TaskOutputRef {
            task: "task:myproject:build".to_string(),
            output: TaskOutputField::Stderr,
        };
        let placeholder = r.to_placeholder();
        assert_eq!(placeholder, "cuenv:ref:task:myproject:build:stderr");
        let parsed = TaskOutputRef::parse(&placeholder).unwrap();
        assert_eq!(parsed, r);
    }

    #[test]
    fn parse_invalid_output_field() {
        assert!(TaskOutputRef::parse("cuenv:ref:task:invalid").is_none());
    }

    #[test]
    fn roundtrip_placeholder() {
        let r = TaskOutputRef {
            task: "tmpdir".to_string(),
            output: TaskOutputField::Stdout,
        };
        let placeholder = r.to_placeholder();
        assert_eq!(placeholder, "cuenv:ref:tmpdir:stdout");
        let parsed = TaskOutputRef::parse(&placeholder).unwrap();
        assert_eq!(parsed, r);
    }

    // =========================================================================
    // try_extract_output_ref tests
    // =========================================================================

    #[test]
    fn extract_valid_ref_object() {
        let val = serde_json::json!({
            "cuenvOutputRef": true,
            "cuenvTask": "tmpdir",
            "cuenvOutput": "stdout"
        });
        let result = try_extract_output_ref(&val).unwrap();
        assert_eq!(result, "cuenv:ref:tmpdir:stdout");
    }

    #[test]
    fn extract_non_ref_object() {
        let val = serde_json::json!({ "command": "echo" });
        assert!(try_extract_output_ref(&val).is_none());
    }

    #[test]
    fn extract_ref_false() {
        let val = serde_json::json!({
            "cuenvOutputRef": false,
            "cuenvTask": "tmpdir",
            "cuenvOutput": "stdout"
        });
        assert!(try_extract_output_ref(&val).is_none());
    }

    #[test]
    fn extract_string_value() {
        let val = serde_json::json!("just a string");
        assert!(try_extract_output_ref(&val).is_none());
    }

    // =========================================================================
    // process_output_refs tests
    // =========================================================================

    #[test]
    fn process_replaces_args_refs() {
        let mut value = serde_json::json!({
            "tasks": {
                "tmpdir": {
                    "command": "mktemp",
                    "args": ["-d"],
                    "stdout": { "cuenvOutputRef": true, "cuenvTask": "tmpdir", "cuenvOutput": "stdout" },
                    "stderr": { "cuenvOutputRef": true, "cuenvTask": "tmpdir", "cuenvOutput": "stderr" },
                    "exitCode": { "cuenvOutputRef": true, "cuenvTask": "tmpdir", "cuenvOutput": "exitCode" }
                },
                "work": {
                    "command": "echo",
                    "args": [
                        { "cuenvOutputRef": true, "cuenvTask": "tmpdir", "cuenvOutput": "stdout" }
                    ]
                }
            }
        });

        let deps = process_output_refs(&mut value);

        // Args should be replaced with placeholder strings
        let work_args = value["tasks"]["work"]["args"].as_array().unwrap();
        assert_eq!(work_args[0].as_str().unwrap(), "cuenv:ref:tmpdir:stdout");

        // stdout/stderr/exitCode should be stripped from task objects
        assert!(value["tasks"]["tmpdir"].get("stdout").is_none());
        assert!(value["tasks"]["tmpdir"].get("stderr").is_none());
        assert!(value["tasks"]["tmpdir"].get("exitCode").is_none());

        // Dependencies should be collected
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0], ("work".to_string(), "tmpdir".to_string()));
    }

    #[test]
    fn process_replaces_env_refs() {
        let mut value = serde_json::json!({
            "tasks": {
                "tmpdir": {
                    "command": "mktemp",
                    "args": ["-d"]
                },
                "work": {
                    "command": "ls",
                    "env": {
                        "TEMP_DIR": { "cuenvOutputRef": true, "cuenvTask": "tmpdir", "cuenvOutput": "stdout" }
                    }
                }
            }
        });

        let deps = process_output_refs(&mut value);

        let env_val = value["tasks"]["work"]["env"]["TEMP_DIR"].as_str().unwrap();
        assert_eq!(env_val, "cuenv:ref:tmpdir:stdout");
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0], ("work".to_string(), "tmpdir".to_string()));
    }

    #[test]
    fn process_handles_sequences() {
        let mut value = serde_json::json!({
            "tasks": {
                "pipeline": [
                    { "command": "mktemp", "args": ["-d"] },
                    {
                        "command": "echo",
                        "args": [
                            { "cuenvOutputRef": true, "cuenvTask": "pipeline[0]", "cuenvOutput": "stdout" }
                        ]
                    }
                ]
            }
        });

        let deps = process_output_refs(&mut value);

        let step1_args = value["tasks"]["pipeline"][1]["args"].as_array().unwrap();
        assert_eq!(
            step1_args[0].as_str().unwrap(),
            "cuenv:ref:pipeline[0]:stdout"
        );
        assert_eq!(deps.len(), 1);
        assert_eq!(
            deps[0],
            ("pipeline[1]".to_string(), "pipeline[0]".to_string())
        );
    }

    #[test]
    fn process_handles_groups() {
        let mut value = serde_json::json!({
            "tasks": {
                "check": {
                    "type": "group",
                    "lint": {
                        "command": "cargo",
                        "args": ["clippy"]
                    },
                    "test": {
                        "command": "cargo",
                        "args": ["test"]
                    }
                }
            }
        });

        let deps = process_output_refs(&mut value);
        assert!(deps.is_empty());
        // Verify stdout was stripped from group children
        assert!(value["tasks"]["check"]["lint"].get("stdout").is_none());
    }

    #[test]
    fn process_multiple_refs_in_args() {
        let mut value = serde_json::json!({
            "tasks": {
                "a": { "command": "echo", "args": ["hello"] },
                "b": { "command": "echo", "args": ["world"] },
                "c": {
                    "command": "echo",
                    "args": [
                        { "cuenvOutputRef": true, "cuenvTask": "a", "cuenvOutput": "stdout" },
                        { "cuenvOutputRef": true, "cuenvTask": "b", "cuenvOutput": "stdout" }
                    ]
                }
            }
        });

        let deps = process_output_refs(&mut value);
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&("c".to_string(), "a".to_string())));
        assert!(deps.contains(&("c".to_string(), "b".to_string())));
    }

    #[test]
    fn process_refs_in_both_args_and_env() {
        let mut value = serde_json::json!({
            "tasks": {
                "src": { "command": "echo", "args": ["data"] },
                "dst": {
                    "command": "echo",
                    "args": [
                        { "cuenvOutputRef": true, "cuenvTask": "src", "cuenvOutput": "stdout" }
                    ],
                    "env": {
                        "DATA": { "cuenvOutputRef": true, "cuenvTask": "src", "cuenvOutput": "stderr" }
                    }
                }
            }
        });

        let deps = process_output_refs(&mut value);
        // Both references should produce deps (deduplication is caller's concern)
        assert_eq!(deps.len(), 2);
    }

    // =========================================================================
    // OutputRefResolver tests
    // =========================================================================

    fn make_result(name: &str, stdout: &str, stderr: &str, exit_code: i32) -> TaskResult {
        TaskResult {
            name: name.to_string(),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit_code: Some(exit_code),
            success: exit_code == 0,
        }
    }

    fn resolver(results: &HashMap<String, TaskResult>) -> OutputRefResolver<'_> {
        OutputRefResolver {
            task_name: "work",
            results,
        }
    }

    #[test]
    fn resolve_stdout_in_args() {
        let mut args = vec!["cuenv:ref:tmpdir:stdout".to_string()];
        let mut env = HashMap::new();
        let mut results = HashMap::new();
        results.insert(
            "tmpdir".to_string(),
            make_result("tmpdir", "/tmp/abc\n", "", 0),
        );

        resolver(&results).resolve(&mut args, &mut env).unwrap();
        assert_eq!(args[0], "/tmp/abc"); // trimmed
    }

    #[test]
    fn resolve_stderr_in_env() {
        let mut args = Vec::new();
        let mut env = HashMap::new();
        env.insert(
            "ERR".to_string(),
            serde_json::Value::String("cuenv:ref:check:stderr".to_string()),
        );
        let mut results = HashMap::new();
        results.insert(
            "check".to_string(),
            make_result("check", "", "  warning  \n", 0),
        );

        resolver(&results).resolve(&mut args, &mut env).unwrap();
        assert_eq!(env["ERR"].as_str().unwrap(), "warning");
    }

    #[test]
    fn resolve_non_ref_strings_unchanged() {
        let mut args = vec!["hello".to_string(), "--flag".to_string()];
        let mut env = HashMap::new();
        env.insert(
            "FOO".to_string(),
            serde_json::Value::String("bar".to_string()),
        );
        let results = HashMap::new();

        resolver(&results).resolve(&mut args, &mut env).unwrap();
        assert_eq!(args, vec!["hello", "--flag"]);
        assert_eq!(env["FOO"].as_str().unwrap(), "bar");
    }

    #[test]
    fn resolve_missing_task_errors() {
        let mut args = vec!["cuenv:ref:nonexistent:stdout".to_string()];
        let mut env = HashMap::new();
        let results = HashMap::new();

        let err = resolver(&results).resolve(&mut args, &mut env).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nonexistent"));
        assert!(msg.contains("not completed"));
    }

    #[test]
    fn resolve_failed_task_errors() {
        let mut args = vec!["cuenv:ref:failing:stdout".to_string()];
        let mut env = HashMap::new();
        let mut results = HashMap::new();
        results.insert(
            "failing".to_string(),
            make_result("failing", "", "error!", 1),
        );

        let err = resolver(&results).resolve(&mut args, &mut env).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("failing") || msg.contains("failed"));
    }

    #[test]
    fn resolve_exit_code_in_args_errors() {
        let mut args = vec!["cuenv:ref:check:exitCode".to_string()];
        let mut env = HashMap::new();
        let mut results = HashMap::new();
        results.insert("check".to_string(), make_result("check", "", "", 0));

        let err = resolver(&results).resolve(&mut args, &mut env).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("exitCode"));
        assert!(msg.contains("integer"));
    }

    #[test]
    fn resolve_empty_stdout() {
        let mut args = vec!["cuenv:ref:quiet:stdout".to_string()];
        let mut env = HashMap::new();
        let mut results = HashMap::new();
        results.insert("quiet".to_string(), make_result("quiet", "", "", 0));

        resolver(&results).resolve(&mut args, &mut env).unwrap();
        assert_eq!(args[0], ""); // empty after trim
    }

    #[test]
    fn resolve_trimming_behavior() {
        let mut args = vec!["cuenv:ref:padded:stdout".to_string()];
        let mut env = HashMap::new();
        let mut results = HashMap::new();
        results.insert(
            "padded".to_string(),
            make_result("padded", "  hello world  \n\n", "", 0),
        );

        resolver(&results).resolve(&mut args, &mut env).unwrap();
        assert_eq!(args[0], "hello world");
    }
}
