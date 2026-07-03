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
//! 4. `OutputRefResolver::resolve` (in `cuenv-task-exec`) replaces placeholder strings with actual values before execution

mod extraction;
mod model;

#[cfg(test)]
use extraction::try_extract_output_ref;
pub use extraction::{
    has_output_refs, parse_passthrough, process_output_refs, try_extract_passthrough,
};
pub use model::{OutputRefDep, TaskOutputField, TaskOutputRef};

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
}
