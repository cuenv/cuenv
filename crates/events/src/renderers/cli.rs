//! CLI renderer for cuenv events.
//!
//! Renders events to stdout/stderr for terminal display.
//! This module is allowed to use println!/eprintln! as it's the output layer.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use crate::bus::EventReceiver;
use crate::event::{
    CiEvent, CommandEvent, CuenvEvent, EventCategory, InteractiveEvent, OutputEvent, Stream,
    SystemEvent, TaskEvent,
};
use std::io::{self, IsTerminal, Write};

/// CLI renderer configuration.
#[derive(Debug, Clone)]
pub struct CliRendererConfig {
    /// Whether to use ANSI colors.
    pub colors: bool,
    /// Whether to show verbose output.
    pub verbose: bool,
}

impl Default for CliRendererConfig {
    fn default() -> Self {
        Self {
            colors: io::stdout().is_terminal(),
            verbose: false,
        }
    }
}

/// CLI renderer that outputs events to stdout/stderr.
#[derive(Debug)]
pub struct CliRenderer {
    config: CliRendererConfig,
}

impl CliRenderer {
    /// Create a new CLI renderer with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: CliRendererConfig::default(),
        }
    }

    /// Create a new CLI renderer with the given configuration.
    #[must_use]
    pub fn with_config(config: CliRendererConfig) -> Self {
        Self { config }
    }

    /// Run the renderer, consuming events from the receiver.
    pub async fn run(self, mut receiver: EventReceiver) {
        while let Some(event) = receiver.recv().await {
            self.render(&event);
        }
    }

    /// Render a single event.
    pub fn render(&self, event: &CuenvEvent) {
        match &event.category {
            EventCategory::Task(task_event) => self.render_task(task_event),
            EventCategory::Ci(ci_event) => self.render_ci(ci_event),
            EventCategory::Command(cmd_event) => self.render_command(cmd_event),
            EventCategory::Interactive(interactive_event) => {
                self.render_interactive(interactive_event);
            }
            EventCategory::System(system_event) => self.render_system(system_event),
            EventCategory::Output(output_event) => self.render_output(output_event),
        }
    }

    fn render_task(&self, event: &TaskEvent) {
        match event {
            TaskEvent::Started {
                name,
                command,
                hermetic,
            } => {
                let hermetic_indicator = if *hermetic { " (hermetic)" } else { "" };
                eprintln!("> [{name}] {command}{hermetic_indicator}");
            }
            TaskEvent::CacheHit { name, .. } => {
                eprintln!("> [{name}] (cached)");
            }
            TaskEvent::CacheMiss { name } => {
                if self.config.verbose {
                    eprintln!("> [{name}] cache miss, executing...");
                }
            }
            TaskEvent::Output {
                stream, content, ..
            } => match stream {
                Stream::Stdout => {
                    print!("{content}");
                    let _ = io::stdout().flush();
                }
                Stream::Stderr => {
                    eprint!("{content}");
                    let _ = io::stderr().flush();
                }
            },
            TaskEvent::Completed {
                name,
                success,
                duration_ms,
                ..
            } => {
                if self.config.verbose {
                    let status = if *success { "completed" } else { "failed" };
                    eprintln!("> [{name}] {status} in {duration_ms}ms");
                }
            }
            TaskEvent::GroupStarted {
                name,
                sequential,
                task_count,
            } => {
                let mode = if *sequential {
                    "sequential"
                } else {
                    "parallel"
                };
                eprintln!("> Running {mode} group: {name} ({task_count} tasks)");
            }
            TaskEvent::GroupCompleted {
                name,
                success,
                duration_ms,
            } => {
                if self.config.verbose {
                    let status = if *success { "completed" } else { "failed" };
                    eprintln!("> Group {name} {status} in {duration_ms}ms");
                }
            }
        }
    }

    fn render_ci(&self, event: &CiEvent) {
        let _ = &self.config; // Silence unused_self - config may be used for CI rendering options later
        match event {
            CiEvent::ContextDetected {
                provider,
                event_type,
                ref_name,
            } => {
                println!("Context: {provider} (event: {event_type}, ref: {ref_name})");
            }
            CiEvent::ChangedFilesFound { count } => {
                println!("Changed files: {count}");
            }
            CiEvent::ProjectsDiscovered { count } => {
                println!("Found {count} projects");
            }
            CiEvent::ProjectSkipped { path, reason } => {
                println!("Project {path}: {reason}");
            }
            CiEvent::TaskExecuting { task, .. } => {
                println!("  -> Executing {task}");
            }
            CiEvent::TaskResult {
                task,
                success,
                error,
                ..
            } => {
                if *success {
                    println!("  -> {task} passed");
                } else if let Some(err) = error {
                    println!("  -> {task} failed: {err}");
                } else {
                    println!("  -> {task} failed");
                }
            }
            CiEvent::ReportGenerated { path } => {
                println!("Report written to: {path}");
            }
        }
    }

    fn render_command(&self, event: &CommandEvent) {
        match event {
            CommandEvent::Started { command, .. } => {
                if self.config.verbose {
                    eprintln!("Starting command: {command}");
                }
            }
            CommandEvent::Progress {
                progress, message, ..
            } => {
                if self.config.verbose {
                    let pct = progress * 100.0;
                    eprintln!("[{pct:.0}%] {message}");
                }
            }
            CommandEvent::Completed {
                command,
                success,
                duration_ms,
            } => {
                if self.config.verbose {
                    let status = if *success { "completed" } else { "failed" };
                    eprintln!("Command {command} {status} in {duration_ms}ms");
                }
            }
        }
    }

    fn render_interactive(&self, event: &InteractiveEvent) {
        let _ = &self.config; // Silence unused_self - config may be used for interactive rendering options later
        match event {
            InteractiveEvent::PromptRequested {
                message, options, ..
            } => {
                println!("{message}");
                for (i, option) in options.iter().enumerate() {
                    println!("  [{i}] {option}");
                }
                print!("> ");
                let _ = io::stdout().flush();
            }
            InteractiveEvent::PromptResolved { .. } => {
                // Response handled elsewhere
            }
            InteractiveEvent::WaitProgress {
                target,
                elapsed_secs,
            } => {
                eprint!("\r\x1b[KWaiting for `{target}`... [{elapsed_secs}s]");
                let _ = io::stderr().flush();
            }
        }
    }

    fn render_system(&self, event: &SystemEvent) {
        match event {
            SystemEvent::SupervisorLog { tag, message } => {
                eprintln!("[{tag}] {message}");
            }
            SystemEvent::Shutdown => {
                if self.config.verbose {
                    eprintln!("System shutdown");
                }
            }
        }
    }

    fn render_output(&self, event: &OutputEvent) {
        let _ = &self.config; // Silence unused_self - config may be used for output rendering options later
        match event {
            OutputEvent::Stdout { content } => {
                println!("{content}");
            }
            OutputEvent::Stderr { content } => {
                eprintln!("{content}");
            }
        }
    }
}

impl Default for CliRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{CuenvEvent, EventSource};
    use uuid::Uuid;

    fn create_test_event(category: EventCategory) -> CuenvEvent {
        CuenvEvent {
            id: Uuid::new_v4(),
            correlation_id: Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
            source: EventSource::new("cuenv::test"),
            category,
        }
    }

    #[test]
    fn test_cli_renderer_config_default() {
        let config = CliRendererConfig::default();
        // In test environment, stdout is usually not a terminal
        assert!(!config.verbose);
    }

    #[test]
    fn test_cli_renderer_config_custom() {
        let config = CliRendererConfig {
            colors: true,
            verbose: true,
        };
        assert!(config.colors);
        assert!(config.verbose);
    }

    #[test]
    fn test_cli_renderer_new() {
        let renderer = CliRenderer::new();
        assert!(!renderer.config.verbose);
    }

    #[test]
    fn test_cli_renderer_with_config() {
        let config = CliRendererConfig {
            colors: false,
            verbose: true,
        };
        let renderer = CliRenderer::with_config(config);
        assert!(!renderer.config.colors);
        assert!(renderer.config.verbose);
    }

    #[test]
    fn test_cli_renderer_default_impl() {
        let renderer = CliRenderer::default();
        assert!(!renderer.config.verbose);
    }

    #[test]
    fn test_render_task_started() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::Task(TaskEvent::Started {
            name: "build".to_string(),
            command: "cargo build".to_string(),
            hermetic: true,
        }));
        // This test just verifies it doesn't panic
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_started_non_hermetic() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::Task(TaskEvent::Started {
            name: "build".to_string(),
            command: "cargo build".to_string(),
            hermetic: false,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_cache_hit() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::Task(TaskEvent::CacheHit {
            name: "build".to_string(),
            cache_key: "abc123".to_string(),
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_cache_miss_non_verbose() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::Task(TaskEvent::CacheMiss {
            name: "build".to_string(),
        }));
        // Should not output in non-verbose mode
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_cache_miss_verbose() {
        let config = CliRendererConfig {
            colors: false,
            verbose: true,
        };
        let renderer = CliRenderer::with_config(config);
        let event = create_test_event(EventCategory::Task(TaskEvent::CacheMiss {
            name: "build".to_string(),
        }));
        // Should output in verbose mode
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_output_stdout() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::Task(TaskEvent::Output {
            name: "build".to_string(),
            stream: Stream::Stdout,
            content: "Hello, world!".to_string(),
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_output_stderr() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::Task(TaskEvent::Output {
            name: "build".to_string(),
            stream: Stream::Stderr,
            content: "Warning: deprecated".to_string(),
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_completed_verbose() {
        let config = CliRendererConfig {
            colors: false,
            verbose: true,
        };
        let renderer = CliRenderer::with_config(config);
        let event = create_test_event(EventCategory::Task(TaskEvent::Completed {
            name: "build".to_string(),
            success: true,
            exit_code: Some(0),
            duration_ms: 1500,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_completed_failed() {
        let config = CliRendererConfig {
            colors: false,
            verbose: true,
        };
        let renderer = CliRenderer::with_config(config);
        let event = create_test_event(EventCategory::Task(TaskEvent::Completed {
            name: "build".to_string(),
            success: false,
            exit_code: Some(1),
            duration_ms: 500,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_group_started_sequential() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::Task(TaskEvent::GroupStarted {
            name: "ci".to_string(),
            sequential: true,
            task_count: 3,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_task_group_started_parallel() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::Task(TaskEvent::GroupStarted {
            name: "ci".to_string(),
            sequential: false,
            task_count: 5,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_ci_context_detected() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::Ci(CiEvent::ContextDetected {
            provider: "github".to_string(),
            event_type: "push".to_string(),
            ref_name: "main".to_string(),
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_ci_changed_files() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::Ci(CiEvent::ChangedFilesFound { count: 42 }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_ci_task_result_success() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::Ci(CiEvent::TaskResult {
            project: "my-project".to_string(),
            task: "build".to_string(),
            success: true,
            error: None,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_ci_task_result_failure() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::Ci(CiEvent::TaskResult {
            project: "my-project".to_string(),
            task: "test".to_string(),
            success: false,
            error: Some("assertion failed".to_string()),
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_ci_task_result_failure_no_error() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::Ci(CiEvent::TaskResult {
            project: "my-project".to_string(),
            task: "test".to_string(),
            success: false,
            error: None,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_command_events_non_verbose() {
        let renderer = CliRenderer::new();

        // Started - should be silent in non-verbose
        let event = create_test_event(EventCategory::Command(CommandEvent::Started {
            command: "build".to_string(),
            args: vec![],
        }));
        renderer.render(&event);

        // Progress - should be silent in non-verbose
        let event = create_test_event(EventCategory::Command(CommandEvent::Progress {
            command: "build".to_string(),
            progress: 0.5,
            message: "Halfway there".to_string(),
        }));
        renderer.render(&event);

        // Completed - should be silent in non-verbose
        let event = create_test_event(EventCategory::Command(CommandEvent::Completed {
            command: "build".to_string(),
            success: true,
            duration_ms: 1000,
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_interactive_prompt() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::Interactive(
            InteractiveEvent::PromptRequested {
                prompt_id: "choice-1".to_string(),
                message: "Choose an option:".to_string(),
                options: vec!["Option A".to_string(), "Option B".to_string()],
            },
        ));
        renderer.render(&event);
    }

    #[test]
    fn test_render_system_supervisor_log() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::System(SystemEvent::SupervisorLog {
            tag: "INFO".to_string(),
            message: "Starting up".to_string(),
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_system_shutdown_verbose() {
        let config = CliRendererConfig {
            colors: false,
            verbose: true,
        };
        let renderer = CliRenderer::with_config(config);
        let event = create_test_event(EventCategory::System(SystemEvent::Shutdown));
        renderer.render(&event);
    }

    #[test]
    fn test_render_output_stdout() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::Output(OutputEvent::Stdout {
            content: "Output line".to_string(),
        }));
        renderer.render(&event);
    }

    #[test]
    fn test_render_output_stderr() {
        let renderer = CliRenderer::new();
        let event = create_test_event(EventCategory::Output(OutputEvent::Stderr {
            content: "Error line".to_string(),
        }));
        renderer.render(&event);
    }
}
