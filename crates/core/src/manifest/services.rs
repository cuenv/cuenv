use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::Runtime;
use crate::environment::EnvValue;
use crate::tasks::{Input, ScriptShell, ShellOptions, Task, TaskDependency};

// ============================================================================
// Service Types
// ============================================================================

/// Structured command invocation: a program plus its arguments.
///
/// Shared base type for tasks and service entrypoints. Arguments may be
/// literal strings or runtime task output references.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Command {
    /// Program to execute.
    pub command: String,

    /// Arguments (may contain output refs).
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

/// Inline script invocation: a script body interpreted by a shell.
///
/// Shared base type for tasks and service entrypoints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Script {
    /// Script body.
    pub script: String,

    /// Shell interpreter (defaults to bash on the CUE side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_shell: Option<ScriptShell>,

    /// Shell options (errexit, nounset, pipefail, xtrace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_options: Option<ShellOptions>,
}

/// How a [`Service`] is executed.
///
/// Either:
/// - a full [`Task`] (lets a service reuse an existing task definition),
/// - an inline [`Script`], or
/// - an inline [`Command`].
///
/// Deserialized as an untagged enum, with the most specific variant
/// (`Task`) attempted first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Entrypoint {
    /// Full task reference (or inline task) reused as a service entrypoint.
    Task(Box<Task>),
    /// Inline script.
    Script(Script),
    /// Inline command.
    Command(Command),
}

impl Default for Entrypoint {
    fn default() -> Self {
        Entrypoint::Command(Command::default())
    }
}

/// Long-running supervised process definition.
///
/// Services live alongside tasks on a project but execute under different
/// rules: they must reach a readiness state, are kept alive across the
/// session, restart according to policy, and tear down on `cuenv down`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Service {
    /// Type discriminator — always `"service"`.
    #[serde(rename = "type", default = "default_service_type")]
    pub service_type: String,

    /// How the service process is launched.
    #[serde(default)]
    pub entrypoint: Entrypoint,

    /// Environment variables (same shape as Task).
    #[serde(default)]
    pub env: HashMap<String, EnvValue>,

    /// Working directory override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,

    /// Dependencies — may reference tasks OR services.
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<TaskDependency>,

    /// Labels for discovery via ServiceMatcher.
    #[serde(default)]
    pub labels: Vec<String>,

    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Runtime override for this service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Runtime>,

    /// Readiness probe (single probe per service).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<Readiness>,

    /// Restart policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<RestartPolicy>,

    /// File watcher for restart-on-change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<ServiceWatch>,

    /// Log handling configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<ServiceLogs>,

    /// Shutdown behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown: Option<Shutdown>,

    /// Hard kill if startup-to-ready exceeds this duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
}

impl Service {
    /// Return the primary program name for workspace-detection heuristics
    /// (bun, cargo, etc.). Scripts have no single program.
    #[must_use]
    pub fn primary_command(&self) -> Option<&str> {
        match &self.entrypoint {
            Entrypoint::Task(task) => {
                if task.command.is_empty() {
                    None
                } else {
                    Some(task.command.as_str())
                }
            }
            Entrypoint::Command(cmd) => Some(cmd.command.as_str()),
            Entrypoint::Script(_) => None,
        }
    }
}

fn default_service_type() -> String {
    "service".to_string()
}

// ============================================================================
// Container Image Types
// ============================================================================

/// Output reference for a container image (ref or digest).
///
/// Mirrors [`TaskOutputRef`] but for image build outputs. The executor
/// resolves these at runtime after the image is built.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageOutputRef {
    #[serde(rename = "cuenvOutputRef")]
    pub cuenv_output_ref: bool,
    #[serde(rename = "cuenvImage")]
    pub cuenv_image: String,
    #[serde(rename = "cuenvOutput")]
    pub cuenv_output: String,
}

/// Container image build definition.
///
/// Declares a container image as a first-class project artifact. Images
/// participate in the task DAG and produce output references (`.ref`,
/// `.digest`) that downstream tasks can consume.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContainerImage {
    /// Type discriminator — always `"image"`.
    #[serde(rename = "type", default = "default_image_type")]
    pub image_type: String,

    /// Image reference output — resolved at runtime after build.
    #[serde(rename = "ref")]
    pub ref_output: ImageOutputRef,

    /// Image digest output — resolved at runtime after build.
    pub digest: ImageOutputRef,

    /// Build context directory (required).
    pub context: String,

    /// Dockerfile path relative to context.
    #[serde(default = "default_dockerfile")]
    pub dockerfile: String,

    /// Build arguments (values may be literal strings or image output refs).
    #[serde(
        default,
        rename = "buildArgs",
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub build_args: HashMap<String, serde_json::Value>,

    /// Target stage for multi-stage builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,

    /// Image tags (e.g., `["latest", "v1.0.0"]`).
    #[serde(default)]
    pub tags: Vec<String>,

    /// Registry to push to (omit for local-only builds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,

    /// Repository name (defaults to image name if omitted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,

    /// Target platforms for multi-arch builds.
    #[serde(default)]
    pub platform: Vec<String>,

    /// Dependencies on tasks or other images.
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<TaskDependency>,

    /// Labels for discovery.
    #[serde(default)]
    pub labels: Vec<String>,

    /// Input files/patterns for cache key derivation.
    #[serde(default)]
    pub inputs: Vec<Input>,

    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn default_image_type() -> String {
    "image".to_string()
}

fn default_dockerfile() -> String {
    "Dockerfile".to_string()
}

/// Readiness probe — discriminated by `kind` field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind")]
pub enum Readiness {
    /// TCP port connectivity check.
    #[serde(rename = "port")]
    Port(ReadinessPort),
    /// HTTP endpoint check.
    #[serde(rename = "http")]
    Http(ReadinessHttp),
    /// Regex match on service output.
    #[serde(rename = "log")]
    Log(ReadinessLog),
    /// External command check (exit 0 = ready).
    #[serde(rename = "command")]
    Command(ReadinessCommand),
    /// Simple delay before considering ready.
    #[serde(rename = "delay")]
    Delay(ReadinessDelay),
}

/// Common readiness probe fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ReadinessCommon {
    /// Time between probe attempts (e.g., "500ms").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    /// Max time to reach ready (e.g., "60s").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    /// Initial delay before first probe (e.g., "0s").
    #[serde(
        default,
        rename = "initialDelay",
        skip_serializing_if = "Option::is_none"
    )]
    pub initial_delay: Option<String>,
}

/// TCP port readiness probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadinessPort {
    /// Common probe settings.
    #[serde(flatten)]
    pub common: ReadinessCommon,
    /// TCP port on localhost.
    pub port: u16,
    /// Host to connect to (default: 127.0.0.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// HTTP readiness probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadinessHttp {
    /// Common probe settings.
    #[serde(flatten)]
    pub common: ReadinessCommon,
    /// URL to check.
    pub url: String,
    /// Expected status codes (default: 2xx).
    #[serde(
        default,
        rename = "expectStatus",
        skip_serializing_if = "Option::is_none"
    )]
    pub expect_status: Option<Vec<u16>>,
    /// HTTP method (default: GET).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
}

/// Log pattern readiness probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadinessLog {
    /// Common probe settings.
    #[serde(flatten)]
    pub common: ReadinessCommon,
    /// Regex pattern — first match declares ready.
    pub pattern: String,
    /// Which stream to watch (default: "either").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// External command readiness probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadinessCommand {
    /// Common probe settings.
    #[serde(flatten)]
    pub common: ReadinessCommon,
    /// Command to run (exit 0 = ready).
    pub command: String,
    /// Command arguments.
    #[serde(default)]
    pub args: Vec<String>,
}

/// Simple delay readiness probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadinessDelay {
    /// Duration to wait before considering ready.
    pub delay: String,
}

impl Readiness {
    /// Access the common probe fields shared by all readiness types.
    ///
    /// Returns `None` for `Delay`, which has no common fields.
    #[must_use]
    pub fn common_fields(&self) -> Option<&ReadinessCommon> {
        match self {
            Self::Port(p) => Some(&p.common),
            Self::Http(h) => Some(&h.common),
            Self::Log(l) => Some(&l.common),
            Self::Command(c) => Some(&c.common),
            Self::Delay(_) => None,
        }
    }
}

/// Restart policy for services.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RestartPolicy {
    /// Restart mode (default: "onFailure").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Exponential backoff between restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff: Option<BackoffConfig>,
    /// Max restarts within the sliding window (default: 5).
    #[serde(
        default,
        rename = "maxRestarts",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_restarts: Option<u32>,
    /// Sliding window for restart counting (default: "60s").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
}

/// Exponential backoff configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackoffConfig {
    /// Initial delay (default: "1s").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial: Option<String>,
    /// Maximum delay (default: "30s").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<String>,
    /// Backoff multiplier (default: 2.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub factor: Option<f64>,
}

/// File watcher configuration for services.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceWatch {
    /// Glob patterns relative to project root.
    pub paths: Vec<String>,
    /// Patterns to ignore (gitignore syntax).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore: Option<Vec<String>>,
    /// Debounce window (default: "200ms").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debounce: Option<String>,
    /// Action on change (default: "restart").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    /// Tasks to re-run before restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild: Option<Vec<TaskDependency>>,
}

/// Service log configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceLogs {
    /// Stream prefix shown in multiplexed output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// ANSI color hint for renderers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Persist to file (default: true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist: Option<bool>,
}

/// Shutdown behavior for services.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Shutdown {
    /// Signal to send (default: "SIGTERM").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// Grace period before SIGKILL (default: "10s").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
}
