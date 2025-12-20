//! GitHub Actions Workflow Schema Types
//!
//! Defines the data structures for GitHub Actions workflow YAML generation.
//! See: <https://docs.github.com/en/actions/using-workflows/workflow-syntax-for-github-actions>

use indexmap::IndexMap;
use serde::Serialize;
use std::collections::HashMap;

/// A GitHub Actions workflow definition.
///
/// Represents the complete structure of a workflow file that can be committed
/// to `.github/workflows/`.
#[derive(Debug, Clone, Serialize)]
pub struct Workflow {
    /// Workflow name displayed in GitHub UI
    pub name: String,

    /// Trigger configuration
    #[serde(rename = "on")]
    pub on: WorkflowTriggers,

    /// Concurrency settings to prevent duplicate runs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<Concurrency>,

    /// Default permissions for `GITHUB_TOKEN`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Permissions>,

    /// Environment variables available to all jobs
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,

    /// Job definitions (order preserved via `IndexMap`)
    pub jobs: IndexMap<String, Job>,
}

/// Workflow trigger configuration.
///
/// Defines when the workflow should run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WorkflowTriggers {
    /// Trigger on push events
    #[serde(skip_serializing_if = "Option::is_none")]
    pub push: Option<PushTrigger>,

    /// Trigger on pull request events
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<PullRequestTrigger>,

    /// Trigger on release events
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<ReleaseTrigger>,

    /// Manual trigger with optional inputs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_dispatch: Option<WorkflowDispatchTrigger>,

    /// Scheduled trigger (cron expressions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Vec<ScheduleTrigger>>,
}

/// Push event trigger configuration.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct PushTrigger {
    /// Branch patterns to trigger on
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<String>,

    /// Tag patterns to trigger on
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Path patterns that must be matched to trigger
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,

    /// Path patterns to ignore
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths_ignore: Vec<String>,
}

/// Pull request event trigger configuration.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct PullRequestTrigger {
    /// Branch patterns to trigger on (target branches)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<String>,

    /// Activity types to trigger on (e.g., "opened", "synchronize")
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,

    /// Path patterns that must be matched to trigger
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,

    /// Path patterns to ignore
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths_ignore: Vec<String>,
}

/// Release event trigger configuration.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ReleaseTrigger {
    /// Activity types to trigger on (e.g., "published", "created")
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,
}

/// Manual workflow dispatch trigger configuration.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WorkflowDispatchTrigger {
    /// Input parameters for manual trigger
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub inputs: HashMap<String, WorkflowInput>,
}

/// Input definition for `workflow_dispatch` triggers.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowInput {
    /// Human-readable description of the input
    pub description: String,

    /// Whether the input is required
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,

    /// Default value for the input
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,

    /// Input type (string, boolean, choice, environment)
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
}

/// Schedule trigger using cron expressions.
#[derive(Debug, Clone, Serialize)]
pub struct ScheduleTrigger {
    /// Cron expression (e.g., "0 0 * * *" for daily at midnight)
    pub cron: String,
}

/// Concurrency configuration to prevent duplicate workflow runs.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Concurrency {
    /// Concurrency group name (use expressions like `${{ github.workflow }}`)
    pub group: String,

    /// Whether to cancel in-progress runs when a new run is triggered
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_in_progress: Option<bool>,
}

/// `GITHUB_TOKEN` permissions configuration.
///
/// Controls what the workflow can access using the automatic `GITHUB_TOKEN`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Permissions {
    /// Repository contents permission
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contents: Option<PermissionLevel>,

    /// Check runs permission
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<PermissionLevel>,

    /// Pull requests permission
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pull_requests: Option<PermissionLevel>,

    /// Issues permission
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issues: Option<PermissionLevel>,

    /// GitHub Packages permission
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packages: Option<PermissionLevel>,

    /// OIDC token permission (for cloud authentication)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_token: Option<PermissionLevel>,

    /// GitHub Actions permission
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<PermissionLevel>,
}

/// Permission level for `GITHUB_TOKEN` scopes.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionLevel {
    /// Read-only access
    Read,
    /// Read and write access
    Write,
    /// No access
    None,
}

/// A job in a GitHub Actions workflow.
///
/// Jobs run in parallel by default unless `needs` dependencies are specified.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Job {
    /// Job display name (shown in GitHub UI)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Runner label(s) specifying where to run
    pub runs_on: RunsOn,

    /// Job dependencies (these jobs must complete first)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,

    /// Conditional execution expression
    #[serde(rename = "if", skip_serializing_if = "Option::is_none")]
    pub if_condition: Option<String>,

    /// Environment for deployment protection rules
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<Environment>,

    /// Job-level environment variables
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,

    /// Job concurrency settings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<Concurrency>,

    /// Continue workflow if this job fails
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_on_error: Option<bool>,

    /// Job timeout in minutes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_minutes: Option<u32>,

    /// Job steps (executed sequentially)
    pub steps: Vec<Step>,
}

/// Runner specification for where a job runs.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum RunsOn {
    /// Single runner label (e.g., "ubuntu-latest")
    Label(String),
    /// Multiple runner labels (job runs on runner matching all labels)
    Labels(Vec<String>),
}

/// Environment for deployment protection rules.
///
/// Environments can require manual approval or have other protection rules.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Environment {
    /// Simple environment name
    Name(String),
    /// Environment with deployment URL
    WithUrl {
        /// Environment name
        name: String,
        /// URL to the deployed environment
        url: String,
    },
}

/// A step in a job.
///
/// Steps can either `uses` an action or `run` a shell command.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Step {
    /// Step display name (shown in GitHub UI)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Unique identifier for referencing step outputs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Conditional execution expression
    #[serde(rename = "if", skip_serializing_if = "Option::is_none")]
    pub if_condition: Option<String>,

    /// Action to use (e.g., "actions/checkout@v4")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uses: Option<String>,

    /// Shell command(s) to run
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,

    /// Working directory for run commands
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,

    /// Shell to use for run commands (e.g., "bash", "pwsh")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,

    /// Action inputs (for `uses` steps)
    #[serde(rename = "with", skip_serializing_if = "HashMap::is_empty")]
    pub with_inputs: HashMap<String, serde_yaml::Value>,

    /// Step environment variables
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,

    /// Continue on error (don't fail the job)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_on_error: Option<bool>,

    /// Step timeout in minutes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_minutes: Option<u32>,
}

impl Step {
    /// Create a step that uses an action
    pub fn uses(action: impl Into<String>) -> Self {
        Self {
            uses: Some(action.into()),
            ..Default::default()
        }
    }

    /// Create a step that runs a shell command
    pub fn run(command: impl Into<String>) -> Self {
        Self {
            run: Some(command.into()),
            ..Default::default()
        }
    }

    /// Set the step name
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the step ID
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Add a with input
    #[must_use]
    pub fn with_input(mut self, key: impl Into<String>, value: impl Into<serde_yaml::Value>) -> Self {
        self.with_inputs.insert(key.into(), value.into());
        self
    }

    /// Add an environment variable
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Set a condition
    #[must_use]
    pub fn with_if(mut self, condition: impl Into<String>) -> Self {
        self.if_condition = Some(condition.into());
        self
    }

    /// Set working directory
    #[must_use]
    pub fn with_working_directory(mut self, dir: impl Into<String>) -> Self {
        self.working_directory = Some(dir.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_builder() {
        let step = Step::uses("actions/checkout@v4")
            .with_name("Checkout")
            .with_input("fetch-depth", serde_yaml::Value::Number(2.into()));

        assert_eq!(step.name, Some("Checkout".to_string()));
        assert_eq!(step.uses, Some("actions/checkout@v4".to_string()));
        assert!(step.with_inputs.contains_key("fetch-depth"));
    }

    #[test]
    fn test_workflow_serialization() {
        let workflow = Workflow {
            name: "CI".to_string(),
            on: WorkflowTriggers {
                push: Some(PushTrigger {
                    branches: vec!["main".to_string()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            concurrency: Some(Concurrency {
                group: "${{ github.workflow }}-${{ github.ref }}".to_string(),
                cancel_in_progress: Some(true),
            }),
            permissions: Some(Permissions {
                contents: Some(PermissionLevel::Read),
                ..Default::default()
            }),
            env: HashMap::new(),
            jobs: IndexMap::new(),
        };

        let yaml = serde_yaml::to_string(&workflow).unwrap();
        assert!(yaml.contains("name: CI"));
        assert!(yaml.contains("push:"));
        assert!(yaml.contains("branches:"));
        assert!(yaml.contains("- main"));
    }

    #[test]
    fn test_job_with_needs() {
        let job = Job {
            name: Some("Test".to_string()),
            runs_on: RunsOn::Label("ubuntu-latest".to_string()),
            needs: vec!["build".to_string()],
            if_condition: None,
            environment: None,
            env: HashMap::new(),
            concurrency: None,
            continue_on_error: None,
            timeout_minutes: None,
            steps: vec![],
        };

        let yaml = serde_yaml::to_string(&job).unwrap();
        assert!(yaml.contains("name: Test"));
        assert!(yaml.contains("runs-on: ubuntu-latest"));
        assert!(yaml.contains("needs:"));
        assert!(yaml.contains("- build"));
    }

    #[test]
    fn test_environment_serialization() {
        let env_simple = Environment::Name("production".to_string());
        let yaml = serde_yaml::to_string(&env_simple).unwrap();
        assert!(yaml.contains("production"));

        let env_with_url = Environment::WithUrl {
            name: "production".to_string(),
            url: "https://example.com".to_string(),
        };
        let yaml = serde_yaml::to_string(&env_with_url).unwrap();
        assert!(yaml.contains("name: production"));
        assert!(yaml.contains("url:"));
    }
}
