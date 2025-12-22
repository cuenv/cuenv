//! Task execution request types
//!
//! Defines structured types for task execution parameters, replacing
//! the 16-parameter function signature with a single request struct.

use std::fmt;
use std::path::PathBuf;

use crate::commands::CommandExecutor;

/// Request to execute a task or set of tasks.
///
/// This struct groups all parameters needed for task execution,
/// replacing the 16-parameter `execute_task` signature with a
/// single, structured request.
#[derive(Clone)]
pub struct TaskExecutionRequest<'a> {
    /// Path to the directory containing the CUE configuration
    pub path: String,

    /// CUE package name to evaluate
    pub package: String,

    /// How to select which task(s) to run
    pub selection: TaskSelection,

    /// Environment name to use (if any)
    pub environment: Option<String>,

    /// Output configuration
    pub output: OutputConfig,

    /// Execution mode (TUI vs simple)
    pub execution_mode: ExecutionMode,

    /// Backend to use for task execution (e.g., "dagger")
    pub backend: Option<String>,

    /// Optional executor for cached module evaluation
    pub executor: Option<&'a CommandExecutor>,
}

impl fmt::Debug for TaskExecutionRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskExecutionRequest")
            .field("path", &self.path)
            .field("package", &self.package)
            .field("selection", &self.selection)
            .field("environment", &self.environment)
            .field("output", &self.output)
            .field("execution_mode", &self.execution_mode)
            .field("backend", &self.backend)
            .field(
                "executor",
                &self.executor.as_ref().map(|_| "<CommandExecutor>"),
            )
            .finish()
    }
}

/// How to select which task(s) to execute.
///
/// These variants are mutually exclusive - you can only use one
/// selection mode per execution request.
#[derive(Debug, Clone, Default)]
pub enum TaskSelection {
    /// Execute a specific named task with optional arguments
    Named { name: String, args: Vec<String> },

    /// Execute all tasks matching the given labels (AND semantics)
    Labels(Vec<String>),

    /// List all available tasks (no execution)
    #[default]
    List,

    /// Interactively pick a task to run
    Interactive,

    /// Discover and list tasks from all projects in workspace
    All,
}

/// Configuration for output formatting and capture.
#[derive(Debug, Clone)]
pub struct OutputConfig {
    /// Output format: "simple", "json", "text", etc.
    pub format: String,

    /// Whether to capture stdout/stderr instead of streaming
    pub capture_output: bool,

    /// Whether to show cache paths in output
    pub show_cache_path: bool,

    /// Path to materialize task outputs to
    pub materialize_outputs: Option<PathBuf>,

    /// Show help for the task instead of executing
    pub help: bool,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            format: "simple".to_string(),
            capture_output: false,
            show_cache_path: false,
            materialize_outputs: None,
            help: false,
        }
    }
}

/// Execution mode for task running.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Standard execution with simple output
    #[default]
    Simple,

    /// Rich TUI mode with interactive display
    Tui,
}

// These builder methods are part of the public API but not all callers have migrated yet.
// Suppress dead_code warnings until the migration is complete.
#[allow(dead_code)]
impl<'a> TaskExecutionRequest<'a> {
    /// Create a new request for listing tasks.
    #[must_use]
    pub fn list(path: impl Into<String>, package: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            package: package.into(),
            selection: TaskSelection::List,
            environment: None,
            output: OutputConfig::default(),
            execution_mode: ExecutionMode::default(),
            backend: None,
            executor: None,
        }
    }

    /// Create a new request for executing a named task.
    #[must_use]
    pub fn named(
        path: impl Into<String>,
        package: impl Into<String>,
        task_name: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            package: package.into(),
            selection: TaskSelection::Named {
                name: task_name.into(),
                args: Vec::new(),
            },
            environment: None,
            output: OutputConfig::default(),
            execution_mode: ExecutionMode::default(),
            backend: None,
            executor: None,
        }
    }

    /// Set the executor for cached module evaluation.
    #[must_use]
    pub fn with_executor(mut self, executor: &'a CommandExecutor) -> Self {
        self.executor = Some(executor);
        self
    }

    /// Set task arguments (for named task selection).
    #[must_use]
    pub fn with_args(mut self, args: Vec<String>) -> Self {
        if let TaskSelection::Named { name, .. } = self.selection {
            self.selection = TaskSelection::Named { name, args };
        }
        self
    }

    /// Set the environment name.
    #[must_use]
    pub fn with_environment(mut self, env: impl Into<String>) -> Self {
        self.environment = Some(env.into());
        self
    }

    /// Set the output format.
    #[must_use]
    pub fn with_format(mut self, format: impl Into<String>) -> Self {
        self.output.format = format.into();
        self
    }

    /// Enable output capture.
    #[must_use]
    pub fn with_capture(mut self) -> Self {
        self.output.capture_output = true;
        self
    }

    /// Enable TUI mode.
    #[must_use]
    pub fn with_tui(mut self) -> Self {
        self.execution_mode = ExecutionMode::Tui;
        self
    }

    /// Set the backend.
    #[must_use]
    pub fn with_backend(mut self, backend: impl Into<String>) -> Self {
        self.backend = Some(backend.into());
        self
    }

    /// Enable help mode.
    #[must_use]
    pub fn with_help(mut self) -> Self {
        self.output.help = true;
        self
    }

    /// Set materialize outputs path.
    #[must_use]
    pub fn with_materialize_outputs(mut self, path: impl Into<PathBuf>) -> Self {
        self.output.materialize_outputs = Some(path.into());
        self
    }

    /// Enable cache path display.
    #[must_use]
    pub fn with_show_cache_path(mut self) -> Self {
        self.output.show_cache_path = true;
        self
    }
}

/// Convert from the old parameter-based API to the new request struct.
///
/// This is used during the transition period and for backward compatibility.
impl<'a> TaskExecutionRequest<'a> {
    /// Create a request from the legacy parameter list.
    #[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
    pub fn from_legacy(
        path: &str,
        package: &str,
        task_name: Option<&str>,
        labels: &[String],
        environment: Option<&str>,
        format: &str,
        capture_output: bool,
        materialize_outputs: Option<&str>,
        show_cache_path: bool,
        backend: Option<&str>,
        tui: bool,
        interactive: bool,
        help: bool,
        all: bool,
        task_args: &[String],
        executor: Option<&'a CommandExecutor>,
    ) -> Self {
        // Determine selection mode based on parameters
        let selection = if interactive {
            TaskSelection::Interactive
        } else if all {
            TaskSelection::All
        } else if !labels.is_empty() {
            TaskSelection::Labels(labels.to_vec())
        } else if let Some(name) = task_name {
            TaskSelection::Named {
                name: name.to_string(),
                args: task_args.to_vec(),
            }
        } else {
            TaskSelection::List
        };

        let execution_mode = if tui {
            ExecutionMode::Tui
        } else {
            ExecutionMode::Simple
        };

        Self {
            path: path.to_string(),
            package: package.to_string(),
            selection,
            environment: environment.map(ToString::to_string),
            output: OutputConfig {
                format: format.to_string(),
                capture_output,
                show_cache_path,
                materialize_outputs: materialize_outputs.map(PathBuf::from),
                help,
            },
            execution_mode,
            backend: backend.map(ToString::to_string),
            executor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_list() {
        let req = TaskExecutionRequest::list("./", "cuenv");
        assert_eq!(req.path, "./");
        assert_eq!(req.package, "cuenv");
        assert!(matches!(req.selection, TaskSelection::List));
    }

    #[test]
    fn test_request_named() {
        let req = TaskExecutionRequest::named("./", "cuenv", "build");
        assert!(matches!(
            req.selection,
            TaskSelection::Named { ref name, .. } if name == "build"
        ));
    }

    #[test]
    fn test_request_builder_methods() {
        let req = TaskExecutionRequest::named("./", "cuenv", "test")
            .with_args(vec!["--verbose".to_string()])
            .with_environment("dev")
            .with_format("json")
            .with_capture()
            .with_tui();

        assert_eq!(req.environment, Some("dev".to_string()));
        assert_eq!(req.output.format, "json");
        assert!(req.output.capture_output);
        assert_eq!(req.execution_mode, ExecutionMode::Tui);

        if let TaskSelection::Named { args, .. } = &req.selection {
            assert_eq!(args, &vec!["--verbose".to_string()]);
        } else {
            panic!("Expected Named selection");
        }
    }

    #[test]
    fn test_from_legacy_named_task() {
        let req = TaskExecutionRequest::from_legacy(
            "./",
            "cuenv",
            Some("build"),
            &[],
            Some("prod"),
            "json",
            true,
            None,
            false,
            None,
            false,
            false,
            false,
            false,
            &["--release".to_string()],
            None,
        );

        assert!(matches!(
            req.selection,
            TaskSelection::Named { ref name, ref args }
                if name == "build" && args == &vec!["--release".to_string()]
        ));
        assert_eq!(req.environment, Some("prod".to_string()));
        assert!(req.output.capture_output);
    }

    #[test]
    fn test_from_legacy_labels() {
        let req = TaskExecutionRequest::from_legacy(
            "./",
            "cuenv",
            None,
            &["test".to_string(), "unit".to_string()],
            None,
            "simple",
            false,
            None,
            false,
            None,
            false,
            false,
            false,
            false,
            &[],
            None,
        );

        assert!(matches!(
            req.selection,
            TaskSelection::Labels(ref labels) if labels == &vec!["test".to_string(), "unit".to_string()]
        ));
    }

    #[test]
    fn test_from_legacy_interactive() {
        let req = TaskExecutionRequest::from_legacy(
            "./",
            "cuenv",
            None,
            &[],
            None,
            "simple",
            false,
            None,
            false,
            None,
            false,
            true, // interactive
            false,
            false,
            &[],
            None,
        );

        assert!(matches!(req.selection, TaskSelection::Interactive));
    }

    #[test]
    fn test_from_legacy_tui_mode() {
        let req = TaskExecutionRequest::from_legacy(
            "./",
            "cuenv",
            Some("build"),
            &[],
            None,
            "simple",
            false,
            None,
            false,
            None,
            true, // tui
            false,
            false,
            false,
            &[],
            None,
        );

        assert_eq!(req.execution_mode, ExecutionMode::Tui);
    }
}
