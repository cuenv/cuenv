//! Exec hook definition DTOs deserialized from CUE evaluation output.
//!
//! These are the `onEnter`/`onExit`/`prePush` command hooks. They are distinct
//! from the task-dependency hook steps (`HookItem`) in
//! `crate::manifest::hooks`.
//!
//! These types live here, rather than in `cuenv-hooks`, so the manifest crate
//! does not depend on hook execution code. `cuenv-hooks` re-exports them.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Default order for hooks (100)
fn default_order() -> i32 {
    100
}

/// A hook represents a command that can be executed when entering or exiting environments
/// Based on schema/hooks.cue #ExecHook definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hook {
    /// Execution order within a single env.cue (lower runs first, default 100)
    #[serde(default = "default_order")]
    pub order: i32,
    /// Whether this hook propagates to child directories (default false)
    #[serde(default)]
    pub propagate: bool,
    /// The command to execute
    pub command: String,
    /// Arguments to pass to the command
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory for command execution (defaults to ".")
    #[serde(default)]
    pub dir: Option<String>,
    /// Input files that trigger re-execution when changed
    #[serde(default)]
    pub inputs: Vec<String>,
    /// Whether to source the command output as shell script to capture environment changes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<bool>,
}

/// Collection of hooks that can be executed
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Hooks {
    /// Named hooks to execute when entering an environment (map of name -> hook)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "onEnter")]
    pub on_enter: Option<HashMap<String, Hook>>,

    /// Named hooks to execute when exiting an environment (map of name -> hook)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "onExit")]
    pub on_exit: Option<HashMap<String, Hook>>,

    /// Named hooks to execute before git push (map of name -> hook)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "prePush")]
    pub pre_push: Option<HashMap<String, Hook>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_serialization() {
        let hook = Hook {
            order: 50,
            propagate: false,
            command: "npm".to_string(),
            args: vec!["install".to_string()],
            dir: Some("/tmp".to_string()),
            inputs: vec![],
            source: Some(false),
        };

        let json = serde_json::to_string(&hook).unwrap();
        let deserialized: Hook = serde_json::from_str(&json).unwrap();

        assert_eq!(hook, deserialized);
    }

    #[test]
    fn test_hook_defaults() {
        let json = r#"{"command": "echo", "args": ["hello"]}"#;
        let hook: Hook = serde_json::from_str(json).unwrap();

        assert_eq!(hook.order, 100); // default order
        assert_eq!(hook.command, "echo");
        assert_eq!(hook.args, vec!["hello"]);
        assert_eq!(hook.dir, None);
        assert!(hook.inputs.is_empty());
        assert_eq!(hook.source, None); // default
    }
}
