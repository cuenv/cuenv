//! CLI renderer for cuenv events.
//!
//! Renders events to stdout/stderr for terminal display.
//! This module is allowed to use println!/eprintln! as it's the output layer.

#![allow(clippy::print_stdout, clippy::print_stderr, clippy::too_many_lines)]

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
    pub const fn with_config(config: CliRendererConfig) -> Self {
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
