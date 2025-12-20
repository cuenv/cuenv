//! IR v1.3 Schema Types
//!
//! JSON schema for the intermediate representation used by the CI pipeline compiler.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// IR version identifier
pub const IR_VERSION: &str = "1.3";

/// Root IR document
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IntermediateRepresentation {
    /// IR version (always "1.3")
    pub version: String,

    /// Pipeline metadata
    pub pipeline: PipelineMetadata,

    /// Runtime environment definitions
    #[serde(default)]
    pub runtimes: Vec<Runtime>,

    /// Task definitions
    pub tasks: Vec<Task>,
}

impl IntermediateRepresentation {
    /// Create a new IR document
    pub fn new(pipeline_name: impl Into<String>) -> Self {
        Self {
            version: IR_VERSION.to_string(),
            pipeline: PipelineMetadata {
                name: pipeline_name.into(),
                project_name: None,
                trigger: None,
            },
            runtimes: Vec::new(),
            tasks: Vec::new(),
        }
    }
}

/// Pipeline metadata and trigger configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PipelineMetadata {
    /// Pipeline name
    pub name: String,

    /// Project name (for monorepo prefixing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,

    /// Trigger conditions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<TriggerCondition>,
}

/// Trigger conditions for pipeline execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub struct TriggerCondition {
    /// Branch patterns to trigger on
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<String>,

    /// Enable pull request triggers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<bool>,

    /// Cron expressions for scheduled runs
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scheduled: Vec<String>,

    /// Release event types (e.g., `["published"]`)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub release: Vec<String>,

    /// Manual trigger configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual: Option<ManualTriggerConfig>,

    /// Path patterns derived from task inputs (triggers on these paths)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,

    /// Path patterns to ignore (from provider config)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths_ignore: Vec<String>,
}

/// Manual trigger (`workflow_dispatch`) configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ManualTriggerConfig {
    /// Whether manual trigger is enabled
    pub enabled: bool,

    /// Input definitions for `workflow_dispatch`
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub inputs: HashMap<String, WorkflowDispatchInputDef>,
}

/// Workflow dispatch input definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct WorkflowDispatchInputDef {
    /// Human-readable description
    pub description: String,

    /// Whether the input is required
    #[serde(default)]
    pub required: bool,

    /// Default value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,

    /// Input type (string, boolean, choice, environment)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,

    /// Options for choice-type inputs
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

/// Runtime environment definition (Nix flake-based)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Runtime {
    /// Unique runtime identifier
    pub id: String,

    /// Nix flake reference (e.g., "github:NixOS/nixpkgs/nixos-unstable")
    pub flake: String,

    /// Flake output path (e.g., "devShells.x86_64-linux.default")
    pub output: String,

    /// System architecture (e.g., "x86_64-linux", "aarch64-darwin")
    pub system: String,

    /// Runtime digest for caching (computed from flake.lock + output)
    pub digest: String,

    /// Purity enforcement mode
    #[serde(default)]
    pub purity: PurityMode,
}

/// Purity enforcement for Nix flakes
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PurityMode {
    /// Reject unlocked flakes (strict mode)
    Strict,

    /// Warn on unlocked flakes, inject UUID into digest (default)
    #[default]
    Warning,

    /// Allow manual input pinning at compile time
    Override,
}

/// Task definition in the IR
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Task {
    /// Unique task identifier
    pub id: String,

    /// Runtime environment ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,

    /// Command to execute (array form for direct execve)
    pub command: Vec<String>,

    /// Shell execution mode (false = direct execve, true = wrap in /bin/sh -c)
    #[serde(default)]
    pub shell: bool,

    /// Environment variables
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,

    /// Secret configurations
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub secrets: HashMap<String, SecretConfig>,

    /// Resource requirements (for scheduling)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceRequirements>,

    /// Concurrency group for serialized execution
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency_group: Option<String>,

    /// Input file globs (expanded at compile time)
    #[serde(default)]
    pub inputs: Vec<String>,

    /// Output declarations
    #[serde(default)]
    pub outputs: Vec<OutputDeclaration>,

    /// Task dependencies (must complete before this task runs)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Cache policy
    #[serde(default)]
    pub cache_policy: CachePolicy,

    /// Deployment flag (if true, `cache_policy` is forced to disabled)
    #[serde(default)]
    pub deployment: bool,

    /// Manual approval required before execution
    #[serde(default)]
    pub manual_approval: bool,
}

/// Secret configuration for a task
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretConfig {
    /// Source reference (e.g., CI variable name, 1Password reference)
    pub source: String,

    /// Include secret in cache key via salted HMAC
    #[serde(default)]
    pub cache_key: bool,
}

/// Resource requirements for task execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ResourceRequirements {
    /// CPU request/limit (e.g., "2", "1000m")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,

    /// Memory request/limit (e.g., "2Gi", "512Mi")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,

    /// Agent/runner tags for scheduling
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Output artifact declaration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutputDeclaration {
    /// Path to output file/directory
    pub path: String,

    /// Storage type
    #[serde(rename = "type")]
    pub output_type: OutputType,
}

/// Output storage type
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum OutputType {
    /// Store in Content Addressable Store (default)
    #[default]
    Cas,

    /// Upload via orchestrator (e.g., GitLab artifacts, Buildkite artifacts)
    Orchestrator,
}

/// Cache policy for task execution
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CachePolicy {
    /// Read from cache, write on miss (default)
    #[default]
    Normal,

    /// Read from cache only, never write (fork PRs)
    Readonly,

    /// Always execute, write results (cache warming)
    Writeonly,

    /// No cache interaction (deployments)
    Disabled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ir_version() {
        let ir = IntermediateRepresentation::new("test-pipeline");
        assert_eq!(ir.version, "1.3");
        assert_eq!(ir.pipeline.name, "test-pipeline");
        assert!(ir.runtimes.is_empty());
        assert!(ir.tasks.is_empty());
    }

    #[test]
    fn test_purity_mode_serialization() {
        let strict = PurityMode::Strict;
        let json = serde_json::to_string(&strict).unwrap();
        assert_eq!(json, r#""strict""#);

        let warning = PurityMode::Warning;
        let json = serde_json::to_string(&warning).unwrap();
        assert_eq!(json, r#""warning""#);

        let override_mode = PurityMode::Override;
        let json = serde_json::to_string(&override_mode).unwrap();
        assert_eq!(json, r#""override""#);
    }

    #[test]
    fn test_cache_policy_serialization() {
        let normal = CachePolicy::Normal;
        assert_eq!(serde_json::to_string(&normal).unwrap(), r#""normal""#);

        let readonly = CachePolicy::Readonly;
        assert_eq!(serde_json::to_string(&readonly).unwrap(), r#""readonly""#);

        let writeonly = CachePolicy::Writeonly;
        assert_eq!(serde_json::to_string(&writeonly).unwrap(), r#""writeonly""#);

        let disabled = CachePolicy::Disabled;
        assert_eq!(serde_json::to_string(&disabled).unwrap(), r#""disabled""#);
    }

    #[test]
    fn test_output_type_serialization() {
        let cas = OutputType::Cas;
        assert_eq!(serde_json::to_string(&cas).unwrap(), r#""cas""#);

        let orchestrator = OutputType::Orchestrator;
        assert_eq!(
            serde_json::to_string(&orchestrator).unwrap(),
            r#""orchestrator""#
        );
    }

    #[test]
    fn test_task_minimal() {
        let task = Task {
            id: "test-task".to_string(),
            runtime: None,
            command: vec!["echo".to_string(), "hello".to_string()],
            shell: false,
            env: HashMap::new(),
            secrets: HashMap::new(),
            resources: None,
            concurrency_group: None,
            inputs: vec![],
            outputs: vec![],
            depends_on: vec![],
            cache_policy: CachePolicy::Normal,
            deployment: false,
            manual_approval: false,
        };

        let json = serde_json::to_value(&task).unwrap();
        assert_eq!(json["id"], "test-task");
        assert_eq!(json["command"], serde_json::json!(["echo", "hello"]));
        assert_eq!(json["shell"], false);
    }

    #[test]
    fn test_task_with_deployment() {
        let task = Task {
            id: "deploy-prod".to_string(),
            runtime: None,
            command: vec!["deploy".to_string()],
            shell: false,
            env: HashMap::new(),
            secrets: HashMap::new(),
            resources: None,
            concurrency_group: Some("production".to_string()),
            inputs: vec![],
            outputs: vec![],
            depends_on: vec!["build".to_string()],
            cache_policy: CachePolicy::Disabled,
            deployment: true,
            manual_approval: true,
        };

        let json = serde_json::to_value(&task).unwrap();
        assert_eq!(json["deployment"], true);
        assert_eq!(json["manual_approval"], true);
        assert_eq!(json["cache_policy"], "disabled");
        assert_eq!(json["concurrency_group"], "production");
    }

    #[test]
    fn test_secret_config() {
        let secret = SecretConfig {
            source: "CI_API_KEY".to_string(),
            cache_key: true,
        };

        let json = serde_json::to_value(&secret).unwrap();
        assert_eq!(json["source"], "CI_API_KEY");
        assert_eq!(json["cache_key"], true);
    }

    #[test]
    fn test_runtime() {
        let runtime = Runtime {
            id: "nix-rust".to_string(),
            flake: "github:NixOS/nixpkgs/nixos-unstable".to_string(),
            output: "devShells.x86_64-linux.default".to_string(),
            system: "x86_64-linux".to_string(),
            digest: "sha256:abc123".to_string(),
            purity: PurityMode::Strict,
        };

        let json = serde_json::to_value(&runtime).unwrap();
        assert_eq!(json["id"], "nix-rust");
        assert_eq!(json["purity"], "strict");
    }

    #[test]
    fn test_full_ir_serialization() {
        let mut ir = IntermediateRepresentation::new("my-pipeline");
        ir.pipeline.trigger = Some(TriggerCondition {
            branches: vec!["main".to_string()],
            ..Default::default()
        });

        ir.runtimes.push(Runtime {
            id: "default".to_string(),
            flake: "github:NixOS/nixpkgs/nixos-unstable".to_string(),
            output: "devShells.x86_64-linux.default".to_string(),
            system: "x86_64-linux".to_string(),
            digest: "sha256:def456".to_string(),
            purity: PurityMode::Warning,
        });

        ir.tasks.push(Task {
            id: "build".to_string(),
            runtime: Some("default".to_string()),
            command: vec!["cargo".to_string(), "build".to_string()],
            shell: false,
            env: HashMap::new(),
            secrets: HashMap::new(),
            resources: Some(ResourceRequirements {
                cpu: Some("2".to_string()),
                memory: Some("4Gi".to_string()),
                tags: vec!["rust".to_string()],
            }),
            concurrency_group: None,
            inputs: vec!["src/**/*.rs".to_string(), "Cargo.toml".to_string()],
            outputs: vec![OutputDeclaration {
                path: "target/release/binary".to_string(),
                output_type: OutputType::Cas,
            }],
            depends_on: vec![],
            cache_policy: CachePolicy::Normal,
            deployment: false,
            manual_approval: false,
        });

        let json = serde_json::to_string_pretty(&ir).unwrap();
        assert!(json.contains(r#""version": "1.3""#));
        assert!(json.contains(r#""name": "my-pipeline""#));
        assert!(json.contains(r#""id": "build""#));
    }
}
