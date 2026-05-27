use serde::{Deserialize, Serialize};

/// Prefix for all contributor-injected tasks
pub const CONTRIBUTOR_TASK_PREFIX: &str = "cuenv:contributor:";

/// Activation condition for contributors
///
/// All specified conditions must be true (AND logic)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContributorActivation {
    /// Always active (no conditions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub always: Option<bool>,

    /// Workspace membership detection (active if project is member of these workspace types)
    /// Values: "npm", "bun", "pnpm", "yarn", "cargo", "deno"
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_member: Vec<String>,

    /// Command detection for auto-association (active if any task uses these commands)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,

    /// Service command detection (active if any service uses these commands)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_command: Vec<String>,

    /// Service presence (active if project has any services defined)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_service: Option<bool>,
}

/// Auto-association rules for contributors
///
/// Defines how user tasks are automatically connected to contributor tasks
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AutoAssociate {
    /// Commands that trigger auto-association (e.g., ["bun", "bunx"])
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,

    /// Task to inject as dependency (e.g., "cuenv:contributor:bun.workspace.setup")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inject_dependency: Option<String>,
}

/// A task contributed by a contributor
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContributorTask {
    /// Task identifier (will be prefixed with contributor namespace)
    pub id: String,

    /// Shell command to execute
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Command arguments
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// Multi-line script (alternative to command)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,

    /// Input files/patterns for caching
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,

    /// Output files/patterns for caching
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,

    /// Whether task requires hermetic execution
    #[serde(default)]
    pub hermetic: bool,

    /// Dependencies on other tasks (within contributor namespace)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Human-readable description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Contributor definition
///
/// Contributors inject tasks into the DAG based on activation conditions
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Contributor {
    /// Contributor identifier (e.g., "bun.workspace")
    pub id: String,

    /// Activation condition (defaults to always active)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<ContributorActivation>,

    /// Tasks to contribute when active
    pub tasks: Vec<ContributorTask>,

    /// Auto-association rules for user tasks
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_associate: Option<AutoAssociate>,
}

/// Result of applying contributors
#[derive(Debug, Clone, Default)]
pub struct ContributorResult {
    /// Number of tasks injected
    pub tasks_injected: usize,

    /// Contributors that were activated
    pub active_contributors: Vec<String>,
}
