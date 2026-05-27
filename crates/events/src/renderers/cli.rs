//! CLI renderer for cuenv events.
//!
//! Renders events to stdout/stderr for terminal display.

use crate::bus::EventReceiver;
use crate::event::{CuenvEvent, EventCategory, SystemEvent};
#[cfg(feature = "spinner")]
use crate::renderers::SpinnerRenderer;
use std::fmt;
use std::io::{self, IsTerminal, Write};
#[cfg(feature = "spinner")]
use std::sync::Mutex;

mod ci;
mod command;
mod interactive;
mod output;
mod service;
mod system;
mod task;

/// CLI renderer configuration.
#[derive(Debug, Clone)]
pub struct CliRendererConfig {
    /// Whether to use ANSI colors.
    pub colors: bool,
    /// Whether to show verbose output.
    pub verbose: bool,
    /// Spinner rendering options.
    pub spinner: CliSpinnerConfig,
}

/// Spinner-specific CLI renderer options.
#[derive(Debug, Clone, Copy)]
pub struct CliSpinnerConfig {
    /// Whether to render an indicatif spinner UI on a TTY. When `false`
    /// (or when stdout isn't a TTY) the renderer falls back to plain
    /// eprintln output so CI logs stay grep-able.
    pub enabled: bool,
    /// Mirror the latest output line under each task spinner. Off by
    /// default to keep terminals quiet.
    pub output_tail: bool,
}

impl Default for CliRendererConfig {
    fn default() -> Self {
        let tty = io::stdout().is_terminal();
        Self {
            colors: tty,
            verbose: false,
            spinner: CliSpinnerConfig {
                enabled: tty,
                output_tail: false,
            },
        }
    }
}

/// CLI renderer that outputs events to stdout/stderr.
pub struct CliRenderer {
    config: CliRendererConfig,
    #[cfg(feature = "spinner")]
    spinner: Option<Mutex<SpinnerRenderer>>,
}

impl std::fmt::Debug for CliRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("CliRenderer");
        s.field("config", &self.config);
        #[cfg(feature = "spinner")]
        s.field("spinner", &self.spinner.is_some());
        s.finish()
    }
}

impl CliRenderer {
    /// Create a new CLI renderer with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(CliRendererConfig::default())
    }

    /// Create a new CLI renderer with the given configuration.
    #[must_use]
    pub fn with_config(config: CliRendererConfig) -> Self {
        #[cfg(feature = "spinner")]
        let spinner = if config.spinner.enabled {
            Some(Mutex::new(
                SpinnerRenderer::new().with_output_tail(config.spinner.output_tail),
            ))
        } else {
            None
        };
        Self {
            config,
            #[cfg(feature = "spinner")]
            spinner,
        }
    }

    /// Run the renderer, consuming events from the receiver.
    ///
    /// The renderer will exit gracefully when it receives a `SystemEvent::Shutdown` event,
    /// ensuring all pending events are processed before termination.
    pub async fn run(self, mut receiver: EventReceiver) {
        while let Some(event) = receiver.recv().await {
            self.render(&event);
            // Exit after rendering shutdown event
            if matches!(event.category, EventCategory::System(SystemEvent::Shutdown)) {
                break;
            }
        }
    }

    /// Render a single event.
    pub fn render(&self, event: &CuenvEvent) {
        match &event.category {
            EventCategory::Task(task_event) => self.render_task(task_event),
            EventCategory::Service(service_event) => Self::render_service(service_event),
            EventCategory::Ci(ci_event) => Self::render_ci(ci_event),
            EventCategory::Command(cmd_event) => self.render_command(cmd_event),
            EventCategory::Interactive(interactive_event) => {
                Self::render_interactive(interactive_event);
            }
            EventCategory::System(system_event) => self.render_system(system_event),
            EventCategory::Output(output_event) => Self::render_output(output_event),
        }
    }
}

impl Default for CliRenderer {
    fn default() -> Self {
        Self::new()
    }
}

pub(super) fn stdout_line(args: fmt::Arguments<'_>) {
    let stdout = io::stdout();
    write_line(stdout.lock(), args);
}

pub(super) fn stdout(args: fmt::Arguments<'_>) {
    let stdout = io::stdout();
    write(stdout.lock(), args);
}

pub(super) fn stderr_line(args: fmt::Arguments<'_>) {
    let stderr = io::stderr();
    write_line(stderr.lock(), args);
}

pub(super) fn stderr(args: fmt::Arguments<'_>) {
    let stderr = io::stderr();
    write(stderr.lock(), args);
}

pub(super) fn flush_stdout() {
    let stdout = io::stdout();
    flush(stdout.lock());
}

pub(super) fn flush_stderr() {
    let stderr = io::stderr();
    flush(stderr.lock());
}

fn write_line(mut writer: impl Write, args: fmt::Arguments<'_>) {
    if let Err(error) = writer.write_fmt(format_args!("{args}\n")) {
        log_cli_write_error(&error);
    }
}

fn write(mut writer: impl Write, args: fmt::Arguments<'_>) {
    if let Err(error) = writer.write_fmt(args) {
        log_cli_write_error(&error);
    }
}

fn flush(mut writer: impl Write) {
    if let Err(error) = writer.flush() {
        log_cli_write_error(&error);
    }
}

fn log_cli_write_error(error: &io::Error) {
    tracing::debug!(%error, "failed to write CLI event output");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{
        CiEvent, CommandEvent, EventCategory, EventSource, InteractiveEvent, OutputEvent, Stream,
        SystemEvent, TaskEvent,
    };
    use uuid::Uuid;

    fn make_event(category: EventCategory) -> CuenvEvent {
        CuenvEvent::new(Uuid::new_v4(), EventSource::new("test"), category)
    }

    #[test]
    fn test_cli_renderer_config_default() {
        let config = CliRendererConfig::default();
        // Verbose should default to false
        assert!(!config.verbose);
        // Colors depends on terminal, we just verify it doesn't panic
        let _ = config.colors;
    }

    #[test]
    fn test_cli_renderer_config_debug() {
        let config = CliRendererConfig {
            colors: true,
            verbose: true,
            spinner: CliSpinnerConfig {
                enabled: false,
                output_tail: false,
            },
        };
        let debug = format!("{config:?}");
        assert!(debug.contains("CliRendererConfig"));
        assert!(debug.contains("true"));
    }

    #[test]
    fn test_cli_renderer_config_clone() {
        let config = CliRendererConfig {
            colors: false,
            verbose: true,
            spinner: CliSpinnerConfig {
                enabled: false,
                output_tail: false,
            },
        };
        let cloned = config.clone();
        assert_eq!(config.colors, cloned.colors);
        assert_eq!(config.verbose, cloned.verbose);
    }

    #[test]
    fn test_cli_renderer_new() {
        let renderer = CliRenderer::new();
        assert!(!renderer.config.verbose);
    }

    #[test]
    fn test_cli_renderer_default() {
        let renderer = CliRenderer::default();
        assert!(!renderer.config.verbose);
    }

    #[test]
    fn test_cli_renderer_with_config() {
        let config = CliRendererConfig {
            colors: true,
            verbose: true,
            spinner: CliSpinnerConfig {
                enabled: false,
                output_tail: false,
            },
        };
        let renderer = CliRenderer::with_config(config);
        assert!(renderer.config.verbose);
        assert!(renderer.config.colors);
    }

    #[test]
    fn test_cli_renderer_debug() {
        let renderer = CliRenderer::new();
        let debug = format!("{renderer:?}");
        assert!(debug.contains("CliRenderer"));
    }

    #[test]
    fn test_render_task_started() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Task(TaskEvent::Started {
            name: "test-task".to_string(),
            command: "echo hello".to_string(),
            hermetic: false,
            parent_group: None,
            task_kind: crate::event::TaskKind::Task,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_started_hermetic() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Task(TaskEvent::Started {
            name: "test-task".to_string(),
            command: "echo hello".to_string(),
            hermetic: true,
            parent_group: None,
            task_kind: crate::event::TaskKind::Task,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_cache_hit() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Task(TaskEvent::CacheHit {
            name: "cached-task".to_string(),
            cache_key: "abc123".to_string(),
            parent_group: None,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_cache_miss_verbose() {
        let config = CliRendererConfig {
            colors: false,
            verbose: true,
            spinner: CliSpinnerConfig {
                enabled: false,
                output_tail: false,
            },
        };
        let renderer = CliRenderer::with_config(config);
        let event = make_event(EventCategory::Task(TaskEvent::CacheMiss {
            name: "uncached-task".to_string(),
            parent_group: None,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_cache_skipped_verbose() {
        let config = CliRendererConfig {
            colors: false,
            verbose: true,
            spinner: CliSpinnerConfig {
                enabled: false,
                output_tail: false,
            },
        };
        let renderer = CliRenderer::with_config(config);
        let event = make_event(EventCategory::Task(TaskEvent::CacheSkipped {
            name: "fmt".to_string(),
            parent_group: None,
            reason: crate::event::CacheSkipReason::EmptyInputs,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_skipped() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Task(TaskEvent::Skipped {
            name: "deploy".to_string(),
            parent_group: None,
            reason: crate::event::SkipReason::DependencyFailed {
                dep: "build".to_string(),
            },
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_queued_verbose() {
        let config = CliRendererConfig {
            colors: false,
            verbose: true,
            spinner: CliSpinnerConfig {
                enabled: false,
                output_tail: false,
            },
        };
        let renderer = CliRenderer::with_config(config);
        let event = make_event(EventCategory::Task(TaskEvent::Queued {
            name: "build".to_string(),
            parent_group: None,
            queue_position: 1,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_retrying() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Task(TaskEvent::Retrying {
            name: "flaky".to_string(),
            parent_group: None,
            attempt: 2,
            max_attempts: 3,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_output_stdout() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Task(TaskEvent::Output {
            name: "task".to_string(),
            stream: Stream::Stdout,
            content: "stdout content".to_string(),
            parent_group: None,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_output_stderr() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Task(TaskEvent::Output {
            name: "task".to_string(),
            stream: Stream::Stderr,
            content: "stderr content".to_string(),
            parent_group: None,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_completed_verbose() {
        let config = CliRendererConfig {
            colors: false,
            verbose: true,
            spinner: CliSpinnerConfig {
                enabled: false,
                output_tail: false,
            },
        };
        let renderer = CliRenderer::with_config(config);
        let event = make_event(EventCategory::Task(TaskEvent::Completed {
            name: "task".to_string(),
            success: true,
            exit_code: Some(0),
            duration_ms: 1000,
            parent_group: None,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_completed_failed_verbose() {
        let config = CliRendererConfig {
            colors: false,
            verbose: true,
            spinner: CliSpinnerConfig {
                enabled: false,
                output_tail: false,
            },
        };
        let renderer = CliRenderer::with_config(config);
        let event = make_event(EventCategory::Task(TaskEvent::Completed {
            name: "task".to_string(),
            success: false,
            exit_code: Some(1),
            duration_ms: 500,
            parent_group: None,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_group_started_sequential() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Task(TaskEvent::GroupStarted {
            name: "group".to_string(),
            sequential: true,
            task_count: 5,
            parent_group: None,
            max_concurrency: None,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_group_started_parallel() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Task(TaskEvent::GroupStarted {
            name: "group".to_string(),
            sequential: false,
            task_count: 3,
            parent_group: None,
            max_concurrency: Some(4),
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_group_completed_verbose() {
        let config = CliRendererConfig {
            colors: false,
            verbose: true,
            spinner: CliSpinnerConfig {
                enabled: false,
                output_tail: false,
            },
        };
        let renderer = CliRenderer::with_config(config);
        let event = make_event(EventCategory::Task(TaskEvent::GroupCompleted {
            name: "group".to_string(),
            success: true,
            duration_ms: 2000,
            parent_group: None,
            succeeded: 3,
            failed: 0,
            skipped: 0,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_ci_context_detected() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Ci(CiEvent::ContextDetected {
            provider: "github".to_string(),
            event_type: "push".to_string(),
            ref_name: "main".to_string(),
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_ci_changed_files_found() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Ci(CiEvent::ChangedFilesFound { count: 10 }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_ci_projects_discovered() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Ci(CiEvent::ProjectsDiscovered { count: 3 }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_ci_project_skipped() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Ci(CiEvent::ProjectSkipped {
            path: "path/to/project".to_string(),
            reason: "No affected tasks".to_string(),
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_ci_task_executing() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Ci(CiEvent::TaskExecuting {
            task: "build".to_string(),
            project: "myproject".to_string(),
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_ci_task_result_success() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Ci(CiEvent::TaskResult {
            task: "test".to_string(),
            project: "myproject".to_string(),
            success: true,
            error: None,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_ci_task_result_failed_with_error() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Ci(CiEvent::TaskResult {
            task: "test".to_string(),
            project: "myproject".to_string(),
            success: false,
            error: Some("assertion failed".to_string()),
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_ci_task_result_failed_no_error() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Ci(CiEvent::TaskResult {
            task: "test".to_string(),
            project: "myproject".to_string(),
            success: false,
            error: None,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_ci_report_generated() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Ci(CiEvent::ReportGenerated {
            path: "/path/to/report.json".to_string(),
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_command_started_verbose() {
        let config = CliRendererConfig {
            colors: false,
            verbose: true,
            spinner: CliSpinnerConfig {
                enabled: false,
                output_tail: false,
            },
        };
        let renderer = CliRenderer::with_config(config);
        let event = make_event(EventCategory::Command(CommandEvent::Started {
            command: "build".to_string(),
            args: vec!["--release".to_string()],
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_command_progress_verbose() {
        let config = CliRendererConfig {
            colors: false,
            verbose: true,
            spinner: CliSpinnerConfig {
                enabled: false,
                output_tail: false,
            },
        };
        let renderer = CliRenderer::with_config(config);
        let event = make_event(EventCategory::Command(CommandEvent::Progress {
            command: "build".to_string(),
            progress: 0.5,
            message: "Compiling...".to_string(),
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_command_completed_verbose() {
        let config = CliRendererConfig {
            colors: false,
            verbose: true,
            spinner: CliSpinnerConfig {
                enabled: false,
                output_tail: false,
            },
        };
        let renderer = CliRenderer::with_config(config);
        let event = make_event(EventCategory::Command(CommandEvent::Completed {
            command: "build".to_string(),
            success: true,
            duration_ms: 1000,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_interactive_prompt_requested() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Interactive(
            InteractiveEvent::PromptRequested {
                prompt_id: "test-prompt-1".to_string(),
                message: "Select an option:".to_string(),
                options: vec!["Option A".to_string(), "Option B".to_string()],
            },
        ));
        renderer.render(&event);
    }

    #[test]
    fn test_render_interactive_prompt_resolved() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Interactive(
            InteractiveEvent::PromptResolved {
                prompt_id: "test-prompt-1".to_string(),
                response: "Option A".to_string(),
            },
        ));
        renderer.render(&event);
    }

    #[test]
    fn test_render_interactive_wait_progress() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Interactive(InteractiveEvent::WaitProgress {
            target: "database".to_string(),
            elapsed_secs: 10,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_system_supervisor_log() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::System(SystemEvent::SupervisorLog {
            tag: "supervisor".to_string(),
            message: "Process started".to_string(),
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_system_shutdown_verbose() {
        let config = CliRendererConfig {
            colors: false,
            verbose: true,
            spinner: CliSpinnerConfig {
                enabled: false,
                output_tail: false,
            },
        };
        let renderer = CliRenderer::with_config(config);
        let event = make_event(EventCategory::System(SystemEvent::Shutdown));
        renderer.render(&event);
    }

    #[test]
    fn test_render_output_stdout() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Output(OutputEvent::Stdout {
            content: "Hello, world!".to_string(),
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_output_stderr() {
        let renderer = CliRenderer::new();
        let event = make_event(EventCategory::Output(OutputEvent::Stderr {
            content: "Error message".to_string(),
        }));
        renderer.render(&event);
    }
}
