//! Root Cuenv configuration type
//!
//! Based on schema/cuenv.cue

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::config::Config;
use crate::environment::Env;
use crate::hooks::Hook;
use crate::tasks::TaskDefinition;

/// Workspace configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceConfig {
    /// Enable or disable the workspace
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Optional: manually specify the root of the workspace relative to env.cue
    pub root: Option<String>,

    /// Optional: manually specify the package manager
    pub package_manager: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Collection of hooks that can be executed
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
pub struct Hooks {
    /// Hooks to execute when entering an environment
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "onEnter")]
    pub on_enter: Option<HookList>,

    /// Hooks to execute when exiting an environment
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "onExit")]
    pub on_exit: Option<HookList>,
}

/// Hook list can be a single hook or an array of hooks
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum HookList {
    Single(Hook),
    Multiple(Vec<Hook>),
}

impl HookList {
    /// Convert to a vector of hooks
    pub fn to_vec(&self) -> Vec<Hook> {
        match self {
            HookList::Single(hook) => vec![hook.clone()],
            HookList::Multiple(hooks) => hooks.clone(),
        }
    }
}

/// Root Cuenv configuration structure
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
pub struct Cuenv {
    /// Configuration settings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Config>,

    /// Environment variables configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<Env>,

    /// Hooks configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Hooks>,

    /// Workspaces configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspaces: Option<HashMap<String, WorkspaceConfig>>,

    /// Tasks configuration
    #[serde(default)]
    pub tasks: HashMap<String, TaskDefinition>,
}

impl Cuenv {
    /// Create a new empty Cuenv configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Get hooks to execute when entering environment
    pub fn on_enter_hooks(&self) -> Vec<Hook> {
        self.hooks
            .as_ref()
            .and_then(|h| h.on_enter.as_ref())
            .map(|h| h.to_vec())
            .unwrap_or_default()
    }

    /// Get hooks to execute when exiting environment
    pub fn on_exit_hooks(&self) -> Vec<Hook> {
        self.hooks
            .as_ref()
            .and_then(|h| h.on_exit.as_ref())
            .map(|h| h.to_vec())
            .unwrap_or_default()
    }
}
