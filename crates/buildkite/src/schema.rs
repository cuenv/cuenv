//! Buildkite Pipeline Schema Types
//!
//! Defines the data structures for Buildkite pipeline YAML generation.
//! See: <https://buildkite.com/docs/pipelines/configure/defining-steps>

use serde::Serialize;
use std::collections::HashMap;

/// A Buildkite pipeline definition
#[derive(Debug, Clone, Default, Serialize)]
pub struct Pipeline {
    /// Pipeline steps
    pub steps: Vec<Step>,

    /// Pipeline-level environment variables
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
}

/// A step in a Buildkite pipeline
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Step {
    /// A command step that runs commands
    Command(Box<CommandStep>),
    /// A block step for manual approval
    Block(BlockStep),
    /// A wait step to synchronize parallel steps
    Wait(WaitStep),
    /// A group of steps
    Group(GroupStep),
}

/// A command step that executes commands
#[derive(Debug, Clone, Default, Serialize)]
pub struct CommandStep {
    /// Display label for the step
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Unique key for the step (used for `depends_on`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,

    /// Commands to execute (can be single string or array)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandValue>,

    /// Environment variables for this step
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,

    /// Agent targeting rules
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents: Option<AgentRules>,

    /// Artifact paths to upload
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub artifact_paths: Vec<String>,

    /// Step dependencies
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<DependsOn>,

    /// Concurrency group name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency_group: Option<String>,

    /// Maximum concurrent jobs in the group
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<u32>,

    /// Retry configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfig>,

    /// Timeout in minutes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_in_minutes: Option<u32>,

    /// Soft fail configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub soft_fail: Option<bool>,
}

/// Command value can be a single string or an array
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum CommandValue {
    /// Single command string
    Single(String),
    /// Array of commands
    Array(Vec<String>),
}

/// A block step for manual approval
#[derive(Debug, Clone, Serialize)]
pub struct BlockStep {
    /// Block step marker
    pub block: String,

    /// Unique key for the step
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,

    /// Step dependencies
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<DependsOn>,

    /// Prompt message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,

    /// Fields for the block form
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<BlockField>,
}

impl BlockStep {
    /// Create a new block step with the given label
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            block: label.into(),
            key: None,
            depends_on: Vec::new(),
            prompt: None,
            fields: Vec::new(),
        }
    }
}

/// A field in a block step form
#[derive(Debug, Clone, Serialize)]
pub struct BlockField {
    /// Field type (text, select)
    #[serde(rename = "type")]
    pub field_type: String,

    /// Field key
    pub key: String,

    /// Field label
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,

    /// Whether the field is required
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

/// A wait step to synchronize parallel steps
#[derive(Debug, Clone, Serialize)]
pub struct WaitStep {
    /// Wait step marker
    pub wait: Option<String>,

    /// Continue on failure
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_on_failure: Option<bool>,
}

impl Default for WaitStep {
    fn default() -> Self {
        Self {
            wait: Some("~".to_string()),
            continue_on_failure: None,
        }
    }
}

/// A group of steps
#[derive(Debug, Clone, Serialize)]
pub struct GroupStep {
    /// Group label
    pub group: String,

    /// Unique key for the group
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,

    /// Steps within the group
    pub steps: Vec<Step>,

    /// Group dependencies
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<DependsOn>,
}

/// Agent targeting rules
#[derive(Debug, Clone, Serialize)]
pub struct AgentRules {
    /// Queue name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,

    /// Additional agent tags
    #[serde(flatten)]
    pub tags: HashMap<String, String>,
}

impl AgentRules {
    /// Create agent rules with a queue
    pub fn with_queue(queue: impl Into<String>) -> Self {
        Self {
            queue: Some(queue.into()),
            tags: HashMap::new(),
        }
    }

    /// Create agent rules from tags
    #[must_use]
    pub fn from_tags(tags: Vec<String>) -> Option<Self> {
        if tags.is_empty() {
            return None;
        }

        let mut rules = Self {
            queue: None,
            tags: HashMap::new(),
        };

        for tag in tags {
            if let Some((key, value)) = tag.split_once('=') {
                if key == "queue" {
                    rules.queue = Some(value.to_string());
                } else {
                    rules.tags.insert(key.to_string(), value.to_string());
                }
            } else {
                // Treat as queue if no key=value format
                rules.queue = Some(tag);
            }
        }

        Some(rules)
    }
}

/// Step dependency specification
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum DependsOn {
    /// Simple key reference
    Key(String),
    /// Detailed dependency with options
    Detailed(DetailedDependency),
}

/// Detailed dependency specification
#[derive(Debug, Clone, Serialize)]
pub struct DetailedDependency {
    /// Step key to depend on
    pub step: String,

    /// Allow dependency failure
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_failure: Option<bool>,
}

/// Retry configuration
#[derive(Debug, Clone, Serialize)]
pub struct RetryConfig {
    /// Automatic retry settings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic: Option<AutomaticRetry>,

    /// Manual retry allowed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual: Option<ManualRetry>,
}

/// Automatic retry configuration
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum AutomaticRetry {
    /// Simple boolean
    Enabled(bool),
    /// Detailed configuration
    Config(Vec<AutomaticRetryRule>),
}

/// Automatic retry rule
#[derive(Debug, Clone, Serialize)]
pub struct AutomaticRetryRule {
    /// Exit status to retry on
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<String>,

    /// Number of retries
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Manual retry configuration
#[derive(Debug, Clone, Serialize)]
pub struct ManualRetry {
    /// Allow manual retry
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed: Option<bool>,

    /// Permit retry on passed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permit_on_passed: Option<bool>,

    /// Reason required for retry
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_step_serialization() {
        let step = CommandStep {
            label: Some(":rust: Build".to_string()),
            key: Some("build".to_string()),
            command: Some(CommandValue::Array(vec![
                "cargo".to_string(),
                "build".to_string(),
            ])),
            env: HashMap::from([("RUST_BACKTRACE".to_string(), "1".to_string())]),
            ..Default::default()
        };

        let yaml = serde_yaml::to_string(&step).unwrap();
        assert!(yaml.contains("label:"));
        assert!(yaml.contains("key: build"));
        assert!(yaml.contains("RUST_BACKTRACE"));
    }

    #[test]
    fn test_block_step_serialization() {
        let step = BlockStep::new(":hand: Approve Deploy");

        let yaml = serde_yaml::to_string(&step).unwrap();
        assert!(yaml.contains("block:"));
        assert!(yaml.contains("Approve Deploy"));
    }

    #[test]
    fn test_agent_rules_from_tags() {
        let rules = AgentRules::from_tags(vec!["linux-x86".to_string()]);
        assert!(rules.is_some());
        assert_eq!(rules.unwrap().queue, Some("linux-x86".to_string()));

        let rules = AgentRules::from_tags(vec!["queue=deploy".to_string(), "os=linux".to_string()]);
        let rules = rules.unwrap();
        assert_eq!(rules.queue, Some("deploy".to_string()));
        assert_eq!(rules.tags.get("os"), Some(&"linux".to_string()));
    }

    #[test]
    fn test_pipeline_serialization() {
        let pipeline = Pipeline {
            steps: vec![Step::Command(Box::new(CommandStep {
                label: Some("Test".to_string()),
                key: Some("test".to_string()),
                command: Some(CommandValue::Single("echo hello".to_string())),
                ..Default::default()
            }))],
            env: HashMap::new(),
        };

        let yaml = serde_yaml::to_string(&pipeline).unwrap();
        assert!(yaml.contains("steps:"));
        assert!(yaml.contains("label: Test"));
    }
}
