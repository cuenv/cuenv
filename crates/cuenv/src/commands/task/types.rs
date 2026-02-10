//! Task execution request types
//!
//! Defines structured types for task execution parameters, replacing
//! the 16-parameter function signature with a single request struct.

use std::fmt;
use std::path::PathBuf;

use crate::commands::CommandExecutor;
use cuenv_core::{DryRun, OutputCapture};

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

    /// Skip executing task dependencies (for CI orchestrators that handle deps externally)
    pub skip_dependencies: bool,

    /// Dry run mode: export DAG as JSON without executing
    pub dry_run: DryRun,

    /// Executor for cached module evaluation (required - single CUE eval per process)
    pub executor: &'a CommandExecutor,
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
            .field("skip_dependencies", &self.skip_dependencies)
            .field("dry_run", &self.dry_run)
            .field("executor", &"<CommandExecutor>")
            .finish()
    }
}

/// How to select which task(s) to execute.
///
/// These variants are mutually exclusive - you can only use one
/// selection mode per execution request.
#[derive(Debug, Clone, Default)]
pub enum TaskSelection {
    /// Execute a specific named task with optional arguments.
    Named {
        /// The task name.
        name: String,
        /// Optional arguments to pass to the task.
        args: Vec<String>,
    },

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
    pub capture_output: OutputCapture,

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
            capture_output: OutputCapture::Capture,
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

impl<'a> TaskExecutionRequest<'a> {
    /// Create a new request for listing tasks.
    #[must_use]
    pub fn list(
        path: impl Into<String>,
        package: impl Into<String>,
        executor: &'a CommandExecutor,
    ) -> Self {
        Self {
            path: path.into(),
            package: package.into(),
            selection: TaskSelection::List,
            environment: None,
            output: OutputConfig::default(),
            execution_mode: ExecutionMode::default(),
            backend: None,
            skip_dependencies: false,
            dry_run: DryRun::No,
            executor,
        }
    }

    /// Create a new request for executing a named task.
    #[must_use]
    pub fn named(
        path: impl Into<String>,
        package: impl Into<String>,
        task_name: impl Into<String>,
        executor: &'a CommandExecutor,
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
            skip_dependencies: false,
            dry_run: DryRun::No,
            executor,
        }
    }

    /// Create a new request for executing tasks matching labels.
    #[must_use]
    pub fn labels(
        path: impl Into<String>,
        package: impl Into<String>,
        labels: Vec<String>,
        executor: &'a CommandExecutor,
    ) -> Self {
        Self {
            path: path.into(),
            package: package.into(),
            selection: TaskSelection::Labels(labels),
            environment: None,
            output: OutputConfig::default(),
            execution_mode: ExecutionMode::default(),
            backend: None,
            skip_dependencies: false,
            dry_run: DryRun::No,
            executor,
        }
    }

    /// Create a new request for interactive task selection.
    #[must_use]
    pub fn interactive(
        path: impl Into<String>,
        package: impl Into<String>,
        executor: &'a CommandExecutor,
    ) -> Self {
        Self {
            path: path.into(),
            package: package.into(),
            selection: TaskSelection::Interactive,
            environment: None,
            output: OutputConfig::default(),
            execution_mode: ExecutionMode::default(),
            backend: None,
            skip_dependencies: false,
            dry_run: DryRun::No,
            executor,
        }
    }

    /// Create a new request for listing all workspace tasks.
    #[must_use]
    pub fn all(
        path: impl Into<String>,
        package: impl Into<String>,
        executor: &'a CommandExecutor,
    ) -> Self {
        Self {
            path: path.into(),
            package: package.into(),
            selection: TaskSelection::All,
            environment: None,
            output: OutputConfig::default(),
            execution_mode: ExecutionMode::default(),
            backend: None,
            skip_dependencies: false,
            dry_run: DryRun::No,
            executor,
        }
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
    pub const fn with_capture(mut self) -> Self {
        self.output.capture_output = OutputCapture::Capture;
        self
    }

    /// Enable TUI mode.
    #[must_use]
    pub const fn with_tui(mut self) -> Self {
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
    pub const fn with_help(mut self) -> Self {
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
    pub const fn with_show_cache_path(mut self) -> Self {
        self.output.show_cache_path = true;
        self
    }

    /// Skip executing task dependencies.
    #[must_use]
    pub const fn with_skip_dependencies(mut self) -> Self {
        self.skip_dependencies = true;
        self
    }

    /// Enable dry run mode (export DAG as JSON without executing).
    #[must_use]
    pub const fn with_dry_run(mut self) -> Self {
        self.dry_run = DryRun::Yes;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// Create a test executor for unit tests.
    fn create_test_executor() -> CommandExecutor {
        let (sender, _receiver) = mpsc::unbounded_channel();
        CommandExecutor::new(sender, "cuenv".to_string())
    }

    #[test]
    fn test_request_list() {
        let executor = create_test_executor();
        let req = TaskExecutionRequest::list("./", "cuenv", &executor);
        assert_eq!(req.path, "./");
        assert_eq!(req.package, "cuenv");
        assert!(matches!(req.selection, TaskSelection::List));
    }

    #[test]
    fn test_request_named() {
        let executor = create_test_executor();
        let req = TaskExecutionRequest::named("./", "cuenv", "build", &executor);
        assert!(matches!(
            req.selection,
            TaskSelection::Named { ref name, .. } if name == "build"
        ));
    }

    #[test]
    fn test_request_builder_methods() {
        let executor = create_test_executor();
        let req = TaskExecutionRequest::named("./", "cuenv", "test", &executor)
            .with_args(vec!["--verbose".to_string()])
            .with_environment("dev")
            .with_format("json")
            .with_capture()
            .with_tui();

        assert_eq!(req.environment, Some("dev".to_string()));
        assert_eq!(req.output.format, "json");
        assert!(req.output.capture_output.should_capture());
        assert_eq!(req.execution_mode, ExecutionMode::Tui);

        if let TaskSelection::Named { args, .. } = &req.selection {
            assert_eq!(args, &vec!["--verbose".to_string()]);
        } else {
            panic!("Expected Named selection");
        }
    }

    #[test]
    fn test_named_task_with_args() {
        let executor = create_test_executor();
        let req = TaskExecutionRequest::named("./", "cuenv", "build", &executor)
            .with_args(vec!["--release".to_string()])
            .with_environment("prod")
            .with_format("json")
            .with_capture();

        assert!(matches!(
            req.selection,
            TaskSelection::Named { ref name, ref args }
                if name == "build" && args == &vec!["--release".to_string()]
        ));
        assert_eq!(req.environment, Some("prod".to_string()));
        assert!(req.output.capture_output.should_capture());
    }

    #[test]
    fn test_tui_mode() {
        let executor = create_test_executor();
        let req = TaskExecutionRequest::named("./", "cuenv", "build", &executor).with_tui();

        assert_eq!(req.execution_mode, ExecutionMode::Tui);
    }

    #[test]
    fn test_skip_dependencies() {
        let executor = create_test_executor();
        let req =
            TaskExecutionRequest::named("./", "cuenv", "build", &executor).with_skip_dependencies();

        assert!(req.skip_dependencies);
    }

    #[test]
    fn test_request_labels() {
        let executor = create_test_executor();
        let req = TaskExecutionRequest::labels(
            "./",
            "cuenv",
            vec!["ci".to_string(), "fast".to_string()],
            &executor,
        );
        assert!(matches!(
            req.selection,
            TaskSelection::Labels(ref labels) if labels == &vec!["ci".to_string(), "fast".to_string()]
        ));
    }

    #[test]
    fn test_request_interactive() {
        let executor = create_test_executor();
        let req = TaskExecutionRequest::interactive("./", "cuenv", &executor);
        assert!(matches!(req.selection, TaskSelection::Interactive));
    }

    #[test]
    fn test_request_all() {
        let executor = create_test_executor();
        let req = TaskExecutionRequest::all("./", "cuenv", &executor);
        assert!(matches!(req.selection, TaskSelection::All));
    }

    #[test]
    fn test_with_backend() {
        let executor = create_test_executor();
        let req =
            TaskExecutionRequest::named("./", "cuenv", "build", &executor).with_backend("dagger");
        assert_eq!(req.backend, Some("dagger".to_string()));
    }

    #[test]
    fn test_with_help() {
        let executor = create_test_executor();
        let req = TaskExecutionRequest::named("./", "cuenv", "build", &executor).with_help();
        assert!(req.output.help);
    }

    #[test]
    fn test_with_materialize_outputs() {
        let executor = create_test_executor();
        let req = TaskExecutionRequest::named("./", "cuenv", "build", &executor)
            .with_materialize_outputs("/tmp/outputs");
        assert_eq!(
            req.output.materialize_outputs,
            Some(PathBuf::from("/tmp/outputs"))
        );
    }

    #[test]
    fn test_with_show_cache_path() {
        let executor = create_test_executor();
        let req =
            TaskExecutionRequest::named("./", "cuenv", "build", &executor).with_show_cache_path();
        assert!(req.output.show_cache_path);
    }

    #[test]
    fn test_output_config_default() {
        let config = OutputConfig::default();
        assert_eq!(config.format, "simple");
        assert!(config.capture_output.should_capture());
        assert!(!config.show_cache_path);
        assert!(config.materialize_outputs.is_none());
        assert!(!config.help);
    }

    #[test]
    fn test_execution_mode_default() {
        let mode = ExecutionMode::default();
        assert_eq!(mode, ExecutionMode::Simple);
    }

    #[test]
    fn test_task_selection_default() {
        let selection = TaskSelection::default();
        assert!(matches!(selection, TaskSelection::List));
    }

    #[test]
    fn test_request_debug() {
        let executor = create_test_executor();
        let req = TaskExecutionRequest::list("./", "cuenv", &executor);
        let debug = format!("{req:?}");
        assert!(debug.contains("TaskExecutionRequest"));
        assert!(debug.contains("path"));
        assert!(debug.contains("package"));
    }

    #[test]
    fn test_request_clone() {
        let executor = create_test_executor();
        let req = TaskExecutionRequest::named("./", "cuenv", "build", &executor)
            .with_environment("dev")
            .with_format("json");
        let cloned = req.clone();
        assert_eq!(cloned.path, "./");
        assert_eq!(cloned.package, "cuenv");
        assert_eq!(cloned.environment, Some("dev".to_string()));
        assert_eq!(cloned.output.format, "json");
    }

    #[test]
    fn test_output_config_clone() {
        let config = OutputConfig {
            format: "json".to_string(),
            capture_output: OutputCapture::Capture,
            show_cache_path: true,
            materialize_outputs: Some(PathBuf::from("/tmp")),
            help: true,
        };
        let cloned = config.clone();
        assert_eq!(cloned.format, "json");
        assert!(cloned.capture_output.should_capture());
        assert!(cloned.show_cache_path);
    }

    #[test]
    fn test_execution_mode_clone() {
        let mode = ExecutionMode::Tui;
        let cloned = mode.clone();
        assert_eq!(cloned, ExecutionMode::Tui);
    }

    #[test]
    fn test_task_selection_clone() {
        let selection = TaskSelection::Named {
            name: "test".to_string(),
            args: vec!["arg1".to_string()],
        };
        let cloned = selection.clone();
        assert!(matches!(
            cloned,
            TaskSelection::Named { ref name, ref args }
                if name == "test" && args == &vec!["arg1".to_string()]
        ));
    }

    #[test]
    fn test_with_args_on_non_named_selection() {
        // with_args should be a no-op for non-Named selections
        let executor = create_test_executor();
        let req =
            TaskExecutionRequest::list("./", "cuenv", &executor).with_args(vec!["arg".to_string()]);
        assert!(matches!(req.selection, TaskSelection::List));
    }
}
