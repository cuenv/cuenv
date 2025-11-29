//! `CUEnv` CLI Application

//! cuenv CLI Application - Production-grade CUE environment toolchain
//!
//! This binary provides command-line interface for CUE package evaluation,
//! environment variable management, and task orchestration.

// CLI binary needs to output to stdout/stderr - this is intentional
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod cli;
mod commands;
mod coordinator;
mod events;
mod performance;
mod tracing;
mod tui;

use crate::cli::{CliError, EXIT_OK, OkEnvelope, exit_code_for, parse, render_error};
use crate::commands::Command;
use crate::tracing::{Level, TracingConfig, TracingFormat};
use cuenv_core::hooks::execute_hooks;
use cuenv_core::hooks::state::StateManager;
use cuenv_core::hooks::{ExecutionStatus, Hook, HookExecutionConfig};
use cuenv_events::renderers::{CliRenderer, JsonRenderer};
use std::path::PathBuf;
use tracing::instrument;

#[tokio::main]
#[instrument(name = "cuenv_main")]
async fn main() {
    // Set up error handling first
    std::panic::set_hook(Box::new(|panic_info| {
        eprintln!("Application panicked: {panic_info}");
        eprintln!("Internal error occurred. Run with RUST_LOG=debug for more information.");
    }));

    // Run the CLI and handle any errors with proper exit codes
    let exit_code = run().await;
    std::process::exit(exit_code);
}

/// Main CLI runner that handles errors properly and returns exit codes
#[instrument(name = "cuenv_run")]
async fn run() -> i32 {
    match real_main().await {
        Ok(()) => EXIT_OK,
        Err(err) => {
            // Try to determine if JSON mode was requested
            let args: Vec<String> = std::env::args().collect();
            let json_mode = args.iter().any(|arg| arg == "--json");

            render_error(&err, json_mode);
            exit_code_for(&err)
        }
    }
}

/// Real main implementation that can return `CliError`
#[instrument(name = "cuenv_real_main")]
async fn real_main() -> Result<(), CliError> {
    // Check if we're being called as a supervisor process
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "__hook-supervisor" {
        return run_hook_supervisor(args).await;
    }

    // Check if we're being called as the coordinator server
    if args.len() > 1 && args[1] == "__coordinator" {
        return run_coordinator().await;
    }

    // Parse CLI arguments and initialize event system
    let init_result = match initialize_cli_and_tracing().await {
        Ok(result) => result,
        Err(e) => {
            return Err(CliError::config_with_help(
                format!("Failed to parse CLI arguments: {e}"),
                "Check your command line arguments and try again",
            ));
        }
    };

    // Convert CLI command to internal command
    let command: Command = init_result.cli.command.into();

    // Execute the command
    let result = execute_command_safe(command, init_result.cli.json).await;

    // Wait for renderer to finish processing any remaining events
    if let Some(handle) = init_result.renderer_handle {
        // Give renderer a moment to process final events, then abort if stuck
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), handle).await;
    }

    result
}

/// Result of CLI and tracing initialization
struct InitResult {
    cli: crate::cli::Cli,
    /// Handle to the renderer task (if running).
    /// This handle should be awaited before program exit to ensure
    /// all events are properly rendered.
    renderer_handle: Option<tokio::task::JoinHandle<()>>,
}

/// Initialize CLI parsing and tracing configuration
#[instrument(name = "cuenv_initialize_cli_and_tracing")]
async fn initialize_cli_and_tracing() -> Result<InitResult, CliError> {
    // Parse CLI arguments once
    let cli = parse();

    // Derive tracing configuration from parsed CLI
    // In normal mode, use Pretty which suppresses output unless DEBUG level
    // Dev format is verbose and always outputs - only use when explicitly requested
    let trace_format = if cli.json {
        TracingFormat::Json
    } else {
        TracingFormat::Pretty
    };

    let log_level = match cli.level {
        crate::tracing::LogLevel::Trace => Level::TRACE,
        crate::tracing::LogLevel::Debug => Level::DEBUG,
        crate::tracing::LogLevel::Info => Level::INFO,
        crate::tracing::LogLevel::Warn => Level::WARN,
        crate::tracing::LogLevel::Error => Level::ERROR,
    };

    // Initialize enhanced tracing with event capture
    let tracing_config = TracingConfig {
        format: trace_format.clone(),
        level: log_level,
        ..Default::default()
    };

    let event_bus = match crate::tracing::init_tracing_with_events(tracing_config) {
        Ok(bus) => bus,
        Err(e) => {
            return Err(CliError::config(format!(
                "Failed to initialize tracing: {e}"
            )));
        }
    };

    // Spawn appropriate renderer based on output mode
    let receiver = event_bus.subscribe();
    let renderer_handle = if cli.json {
        // JSON mode: output structured JSON events
        let renderer = JsonRenderer::new();
        Some(tokio::spawn(async move {
            renderer.run(receiver).await;
        }))
    } else {
        // CLI mode: pretty-print events to terminal
        let renderer = CliRenderer::new();
        Some(tokio::spawn(async move {
            renderer.run(receiver).await;
        }))
    };

    Ok(InitResult {
        cli,
        renderer_handle,
    })
}

/// Execute command safely without ? operator
#[allow(clippy::too_many_lines)]
#[instrument(name = "cuenv_execute_command_safe")]
async fn execute_command_safe(command: Command, json_mode: bool) -> Result<(), CliError> {
    match command {
        Command::Version { format: _ } => match execute_version_command_safe().await {
            Ok(()) => Ok(()),
            Err(e) => Err(CliError::other(format!("Version command failed: {e}"))),
        },
        Command::EnvPrint {
            path,
            package,
            format,
        } => match execute_env_print_command_safe(path, package, format, json_mode).await {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        },
        Command::Task {
            path,
            package,
            name,
            environment,
            materialize_outputs,
            show_cache_path,
            help,
        } => match execute_task_command_safe(
            path,
            package,
            name,
            environment,
            materialize_outputs,
            show_cache_path,
            help,
        )
        .await
        {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        },
        Command::Exec {
            path,
            package,
            command,
            args,
            environment,
        } => match execute_exec_command_safe(path, package, command, args, environment).await {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        },
        Command::EnvLoad { path, package } => {
            match execute_env_load_command_safe(path, package, json_mode).await {
                Ok(()) => Ok(()),
                Err(e) => Err(e),
            }
        }
        Command::EnvStatus {
            path,
            package,
            wait,
            timeout,
            format,
        } => match execute_env_status_command_safe(path, package, wait, timeout, format, json_mode)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        },
        Command::EnvInspect { path, package } => {
            match execute_env_inspect_command_safe(path, package, json_mode).await {
                Ok(()) => Ok(()),
                Err(e) => Err(e),
            }
        }
        Command::EnvCheck {
            path,
            package,
            shell,
        } => match execute_env_check_command_safe(path, package, shell, json_mode).await {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        },
        Command::ShellInit { shell } => execute_shell_init_command_safe(shell, json_mode),
        Command::Allow {
            path,
            package,
            note,
            yes,
        } => match execute_allow_command_safe(path, package, note, yes, json_mode).await {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        },
        Command::Deny { path, package, all } => {
            match execute_deny_command_safe(path, package, all, json_mode).await {
                Ok(()) => Ok(()),
                Err(e) => Err(e),
            }
        }
        Command::Export { shell, package } => {
            match execute_export_command_safe(shell, package).await {
                Ok(()) => Ok(()),
                Err(e) => Err(e),
            }
        }
        Command::Ci {
            dry_run,
            pipeline,
            generate,
        } => match execute_ci_command_safe(dry_run, pipeline, generate).await {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        },
        Command::Tui => match execute_tui_command().await {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        },
        Command::Web { port, host } => match execute_web_command(port, host).await {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        },
        Command::ChangesetAdd {
            path,
            summary,
            description,
            packages,
        } => {
            match execute_changeset_add_safe(path, summary, description, packages, json_mode).await
            {
                Ok(()) => Ok(()),
                Err(e) => Err(e),
            }
        }
        Command::ChangesetStatus { path } => {
            match execute_changeset_status_safe(path, json_mode).await {
                Ok(()) => Ok(()),
                Err(e) => Err(e),
            }
        }
        Command::ReleaseVersion { path, dry_run } => {
            match execute_release_version_safe(path, dry_run, json_mode).await {
                Ok(()) => Ok(()),
                Err(e) => Err(e),
            }
        }
        Command::ReleasePublish { path, dry_run } => {
            match execute_release_publish_safe(path, dry_run, json_mode).await {
                Ok(()) => Ok(()),
                Err(e) => Err(e),
            }
        }
    }
}

/// Execute CI command safely
#[instrument(name = "cuenv_execute_ci_safe")]
async fn execute_ci_command_safe(
    dry_run: bool,
    pipeline: Option<String>,
    generate: Option<String>,
) -> Result<(), CliError> {
    match commands::ci_cmd::execute_ci(dry_run, pipeline, generate).await {
        Ok(()) => Ok(()),
        Err(e) => Err(CliError::other(format!("CI execution failed: {e}"))),
    }
}

/// Execute TUI command - starts interactive event dashboard
#[instrument(name = "cuenv_execute_tui")]
async fn execute_tui_command() -> Result<(), CliError> {
    use crate::coordinator::client::CoordinatorClient;
    use crate::coordinator::protocol::UiType;

    // Connect to coordinator as a TUI consumer
    let Ok(mut client) = CoordinatorClient::connect_as_consumer(UiType::Tui).await else {
        return Err(CliError::other(
            "No cuenv coordinator is running.\n\n\
             The TUI connects to an event coordinator to display events from other cuenv commands.\n\
             To use the TUI:\n\
             1. Run a cuenv command (e.g., 'cuenv t') in another terminal\n\
             2. Then run 'cuenv tui' to watch the events\n\n\
             Note: The coordinator is started automatically when running task commands."
                .to_string(),
        ));
    };

    cuenv_events::emit_command_started!("tui");

    // Run the TUI event viewer
    match crate::tui::run_event_viewer(&mut client).await {
        Ok(()) => {
            cuenv_events::emit_command_completed!("tui", true, 0_u64);
            Ok(())
        }
        Err(e) => {
            cuenv_events::emit_command_completed!("tui", false, 0_u64);
            Err(CliError::other(format!("TUI error: {e}")))
        }
    }
}

/// Execute Web command - starts web server for event streaming
#[instrument(name = "cuenv_execute_web")]
async fn execute_web_command(port: u16, host: String) -> Result<(), CliError> {
    cuenv_events::emit_command_started!("web");

    // For now, just print a placeholder message
    // Full web server implementation would require adding a web framework dependency
    cuenv_events::emit_stdout!(format!(
        "Web server would start on http://{}:{}\nThis feature is not yet implemented.",
        host, port
    ));

    cuenv_events::emit_command_completed!("web", true, 0_u64);
    Ok(())
}

/// Execute version command safely
#[instrument(name = "cuenv_execute_version_safe")]
async fn execute_version_command_safe() -> Result<(), String> {
    let mut perf_guard = performance::PerformanceGuard::new("version_command");
    perf_guard.add_metadata("command_type", "version");

    let version_info = measure_perf!("get_version_info", {
        commands::version::get_version_info()
    });

    println!("{version_info}");
    perf_guard.finish(true);

    Ok(())
}

/// Execute env load command safely
#[instrument(name = "cuenv_execute_env_load_safe")]
async fn execute_env_load_command_safe(
    path: String,
    package: String,
    json_mode: bool,
) -> Result<(), CliError> {
    match commands::hooks::execute_env_load(&path, &package).await {
        Ok(output) => {
            if json_mode {
                let envelope = OkEnvelope::new(serde_json::json!({
                    "message": output
                }));
                match serde_json::to_string(&envelope) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        return Err(CliError::other(format!("JSON serialization failed: {e}")));
                    }
                }
            } else {
                println!("{output}");
            }
            Ok(())
        }
        Err(e) => Err(CliError::eval_with_help(
            format!("Env load failed: {e}"),
            "Check that your env.cue file is valid and the directory is approved",
        )),
    }
}

/// Execute env status command safely
#[instrument(name = "cuenv_execute_env_status_safe")]
async fn execute_env_status_command_safe(
    path: String,
    package: String,
    wait: bool,
    timeout: u64,
    format: crate::cli::StatusFormat,
    json_mode: bool,
) -> Result<(), CliError> {
    match commands::hooks::execute_env_status(&path, &package, wait, timeout, format).await {
        Ok(output) => {
            if json_mode {
                let envelope = OkEnvelope::new(serde_json::json!({
                    "status": output
                }));
                match serde_json::to_string(&envelope) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        return Err(CliError::other(format!("JSON serialization failed: {e}")));
                    }
                }
            } else {
                println!("{output}");
            }
            Ok(())
        }
        Err(e) => Err(CliError::eval_with_help(
            format!("Env status failed: {e}"),
            "Check that your env.cue file exists and hook execution has been started",
        )),
    }
}

/// Execute env check command safely
#[instrument(name = "cuenv_execute_env_check_safe")]
async fn execute_env_check_command_safe(
    path: String,
    package: String,
    shell: crate::cli::ShellType,
    json_mode: bool,
) -> Result<(), CliError> {
    match commands::hooks::execute_env_check(&path, &package, shell).await {
        Ok(output) => {
            if json_mode {
                let envelope = OkEnvelope::new(serde_json::json!({
                    "exports": output
                }));
                match serde_json::to_string(&envelope) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        return Err(CliError::other(format!("JSON serialization failed: {e}")));
                    }
                }
            } else {
                // Output shell export commands directly (no extra formatting)
                print!("{output}");
            }
            Ok(())
        }
        Err(e) => Err(CliError::eval_with_help(
            format!("Env check failed: {e}"),
            "Check that your env.cue file exists and hooks have completed successfully",
        )),
    }
}

/// Execute env inspect command safely
#[instrument(name = "cuenv_execute_env_inspect_safe")]
async fn execute_env_inspect_command_safe(
    path: String,
    package: String,
    json_mode: bool,
) -> Result<(), CliError> {
    match commands::hooks::execute_env_inspect(&path, &package).await {
        Ok(output) => {
            if json_mode {
                let envelope = OkEnvelope::new(serde_json::json!({
                    "state": output
                }));
                match serde_json::to_string(&envelope) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        return Err(CliError::other(format!("JSON serialization failed: {e}")));
                    }
                }
            } else {
                println!("{output}");
            }
            Ok(())
        }
        Err(e) => Err(CliError::eval_with_help(
            format!("Env inspect failed: {e}"),
            "Check that your env.cue is approved and hooks have run at least once",
        )),
    }
}

/// Execute shell init command safely
#[instrument(name = "cuenv_execute_shell_init_safe")]
fn execute_shell_init_command_safe(
    shell: crate::cli::ShellType,
    json_mode: bool,
) -> Result<(), CliError> {
    let output = commands::hooks::execute_shell_init(shell);

    if json_mode {
        let envelope = OkEnvelope::new(serde_json::json!({
            "script": output
        }));
        match serde_json::to_string(&envelope) {
            Ok(json) => println!("{json}"),
            Err(e) => return Err(CliError::other(format!("JSON serialization failed: {e}"))),
        }
    } else {
        println!("{output}");
    }
    Ok(())
}

/// Execute allow command safely
#[instrument(name = "cuenv_execute_allow_safe")]
async fn execute_allow_command_safe(
    path: String,
    package: String,
    note: Option<String>,
    yes: bool,
    json_mode: bool,
) -> Result<(), CliError> {
    match commands::hooks::execute_allow(&path, &package, note, yes).await {
        Ok(output) => {
            if json_mode {
                let envelope = OkEnvelope::new(serde_json::json!({
                    "message": output
                }));
                match serde_json::to_string(&envelope) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        return Err(CliError::other(format!("JSON serialization failed: {e}")));
                    }
                }
            } else {
                println!("{output}");
            }
            Ok(())
        }
        Err(e) => Err(CliError::eval_with_help(
            format!("Allow failed: {e}"),
            "Check that your env.cue file is valid and the directory exists",
        )),
    }
}

/// Execute deny command safely
#[instrument(name = "cuenv_execute_deny_safe")]
async fn execute_deny_command_safe(
    path: String,
    package: String,
    all: bool,
    json_mode: bool,
) -> Result<(), CliError> {
    match commands::hooks::execute_deny(&path, &package, all).await {
        Ok(output) => {
            if json_mode {
                let envelope = OkEnvelope::new(serde_json::json!({
                    "message": output
                }));
                match serde_json::to_string(&envelope) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        return Err(CliError::other(format!("JSON serialization failed: {e}")));
                    }
                }
            } else {
                println!("{output}");
            }
            Ok(())
        }
        Err(e) => Err(CliError::eval_with_help(
            format!("Deny failed: {e}"),
            "Check that the directory path is correct",
        )),
    }
}

/// Execute export command safely
#[instrument(name = "cuenv_execute_export_safe", skip_all)]
async fn execute_export_command_safe(
    shell: Option<String>,
    package: String,
) -> Result<(), CliError> {
    match commands::export::execute_export(shell.as_deref(), &package).await {
        Ok(output) => {
            // Export command always outputs raw shell commands - no JSON mode
            print!("{output}");
            Ok(())
        }
        Err(e) => Err(CliError::eval_with_help(
            format!("Export failed: {e}"),
            "Check that your env.cue file is valid",
        )),
    }
}

/// Execute env print command safely
#[instrument(name = "cuenv_execute_env_print_safe")]
async fn execute_env_print_command_safe(
    path: String,
    package: String,
    format: String,
    json_mode: bool,
) -> Result<(), CliError> {
    let mut perf_guard = performance::PerformanceGuard::new("env_print_command");
    perf_guard.add_metadata("command_type", "env_print");
    perf_guard.add_metadata("package", &package);
    perf_guard.add_metadata("format", &format);

    let output = measure_perf!("env_print_execution", {
        commands::env::execute_env_print(&path, &package, &format).await
    });

    match output {
        Ok(result) => {
            println!("{result}");
            perf_guard.finish(true);
            Ok(())
        }
        Err(e) => {
            perf_guard.finish(false);
            Err(CliError::eval_with_help(
                format!("Failed to print environment variables: {e}"),
                "Check your CUE files and package configuration",
            ))
        }
    }
}

/// Execute task command safely
#[instrument(name = "cuenv_execute_task_safe")]
async fn execute_task_command_safe(
    path: String,
    package: String,
    name: Option<String>,
    environment: Option<String>,
    materialize_outputs: Option<String>,
    show_cache_path: bool,
    help: bool,
) -> Result<(), CliError> {
    let mut perf_guard = performance::PerformanceGuard::new("task_command");
    perf_guard.add_metadata("command_type", "task");

    let result = commands::task::execute_task(
        &path,
        &package,
        name.as_deref(),
        environment.as_deref(),
        false,
        materialize_outputs.as_deref(),
        show_cache_path,
        help,
    )
    .await;

    match result {
        Ok(output) => {
            println!("{output}");
            perf_guard.finish(true);
            Ok(())
        }
        Err(e) => {
            perf_guard.finish(false);
            Err(CliError::eval_with_help(
                e.to_string(),
                "Re-run with --level=debug to stream task output from child processes",
            ))
        }
    }
}

/// Execute exec command safely
#[instrument(name = "cuenv_execute_exec_safe")]
async fn execute_exec_command_safe(
    path: String,
    package: String,
    command: String,
    args: Vec<String>,
    environment: Option<String>,
) -> Result<(), CliError> {
    let mut perf_guard = performance::PerformanceGuard::new("exec_command");
    perf_guard.add_metadata("command_type", "exec");

    let result =
        commands::exec::execute_exec(&path, &package, &command, &args, environment.as_deref())
            .await;

    match result {
        Ok(exit_code) => {
            perf_guard.finish(exit_code == 0);
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
            Ok(())
        }
        Err(e) => {
            perf_guard.finish(false);
            Err(CliError::eval(e.to_string()))
        }
    }
}

/// Run as the coordinator server (internal - spawned by discovery)
async fn run_coordinator() -> Result<(), CliError> {
    use crate::coordinator::server::EventCoordinator;

    let coordinator = EventCoordinator::new();
    coordinator
        .run()
        .await
        .map_err(|e| CliError::other(format!("Coordinator failed: {e}")))
}

/// Run as a hook supervisor process
#[allow(clippy::too_many_lines)]
async fn run_hook_supervisor(args: Vec<String>) -> Result<(), CliError> {
    // Parse supervisor arguments
    let mut directory_path = PathBuf::new();
    let mut instance_hash = String::new();
    let mut hooks_file = PathBuf::new();
    let mut config_file = PathBuf::new();

    let mut i = 2; // Skip program name and "__hook-supervisor"
    while i < args.len() {
        match args[i].as_str() {
            "--directory" => {
                directory_path = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--instance-hash" => {
                instance_hash.clone_from(&args[i + 1]);
                i += 2;
            }
            "--config-hash" => {
                // Config hash is passed but not currently used in supervisor
                i += 2;
            }
            "--hooks-file" => {
                hooks_file = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--config-file" => {
                config_file = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            _ => i += 1,
        }
    }

    // Initialize basic logging for supervisor to stderr
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init();

    // Change to the target directory so hooks run in the correct context
    if let Err(e) = std::env::set_current_dir(&directory_path) {
        cuenv_events::emit_supervisor_log!(
            "supervisor",
            format!(
                "Failed to change directory to {}: {}",
                directory_path.display(),
                e
            )
        );
        return Err(CliError::other(format!("Failed to change directory: {e}")));
    }

    cuenv_events::emit_supervisor_log!("supervisor", format!("Starting with args: {args:?}"));
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Directory: {}", directory_path.display())
    );
    cuenv_events::emit_supervisor_log!("supervisor", format!("Instance hash: {instance_hash}"));
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Hooks file: {}", hooks_file.display())
    );
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Config file: {}", config_file.display())
    );

    // Read and deserialize hooks and config from files
    let hooks_json = std::fs::read_to_string(&hooks_file)
        .map_err(|e| CliError::other(format!("Failed to read hooks file: {e}")))?;
    let config_json = std::fs::read_to_string(&config_file)
        .map_err(|e| CliError::other(format!("Failed to read config file: {e}")))?;

    let hooks: Vec<Hook> = serde_json::from_str(&hooks_json)
        .map_err(|e| CliError::other(format!("Failed to deserialize hooks: {e}")))?;
    let config: HookExecutionConfig = serde_json::from_str(&config_json)
        .map_err(|e| CliError::other(format!("Failed to deserialize config: {e}")))?;

    // Clean up temp files after reading
    std::fs::remove_file(&hooks_file).ok();
    std::fs::remove_file(&config_file).ok();

    // Write PID file
    let state_dir = config
        .state_dir
        .clone()
        .unwrap_or_else(|| StateManager::default_state_dir().unwrap());
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Using state dir: {}", state_dir.display())
    );
    let state_manager = StateManager::new(state_dir);
    let state_file = state_manager.get_state_file_path(&instance_hash);
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Looking for state file: {}", state_file.display())
    );

    let pid_file = state_file.with_extension("pid");
    std::fs::write(&pid_file, format!("{}", std::process::id()))
        .map_err(|e| CliError::other(format!("Failed to write PID file: {e}")))?;

    // Load the current state
    let mut state = state_manager
        .load_state(&instance_hash)
        .await
        .map_err(|e| CliError::other(format!("Failed to load state: {e}")))?
        .ok_or_else(|| CliError::other("State not found for supervisor"))?;

    // Execute the hooks
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!(
            "Executing {} hooks for directory: {}",
            hooks.len(),
            directory_path.display()
        )
    );
    let result = execute_hooks(hooks, &directory_path, &config, &state_manager, &mut state).await;

    if let Err(e) = result {
        cuenv_events::emit_supervisor_log!("supervisor", format!("Hook execution failed: {e}"));
        state.status = ExecutionStatus::Failed;
        state.error_message = Some(e.to_string());
        state.finished_at = Some(chrono::Utc::now());
        state_manager
            .save_state(&state)
            .await
            .map_err(|e| CliError::other(format!("Failed to save error state: {e}")))?;
        return Err(CliError::other(format!("Hook execution failed: {e}")));
    }

    // Save the final state with environment variables from source hooks
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!(
            "Saving final state with {} environment variables",
            state.environment_vars.len()
        )
    );
    state_manager
        .save_state(&state)
        .await
        .map_err(|e| CliError::other(format!("Failed to save final state: {e}")))?;

    // Clean up PID file
    std::fs::remove_file(&pid_file).ok();

    cuenv_events::emit_supervisor_log!("supervisor", "Completed successfully");
    Ok(())
}

/// Execute changeset add command safely
#[instrument(name = "cuenv_execute_changeset_add_safe")]
async fn execute_changeset_add_safe(
    path: String,
    summary: String,
    description: Option<String>,
    packages: Vec<(String, String)>,
    json_mode: bool,
) -> Result<(), CliError> {
    match commands::release::execute_changeset_add(
        &path,
        &packages,
        &summary,
        description.as_deref(),
    ) {
        Ok(output) => {
            if json_mode {
                let envelope = OkEnvelope::new(serde_json::json!({
                    "message": output
                }));
                match serde_json::to_string(&envelope) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        return Err(CliError::other(format!("JSON serialization failed: {e}")));
                    }
                }
            } else {
                println!("{output}");
            }
            Ok(())
        }
        Err(e) => Err(CliError::eval_with_help(
            format!("Changeset add failed: {e}"),
            "Check package names and bump types (major, minor, patch)",
        )),
    }
}

/// Execute changeset status command safely
#[instrument(name = "cuenv_execute_changeset_status_safe")]
async fn execute_changeset_status_safe(path: String, json_mode: bool) -> Result<(), CliError> {
    match commands::release::execute_changeset_status(&path) {
        Ok(output) => {
            if json_mode {
                let envelope = OkEnvelope::new(serde_json::json!({
                    "status": output
                }));
                match serde_json::to_string(&envelope) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        return Err(CliError::other(format!("JSON serialization failed: {e}")));
                    }
                }
            } else {
                println!("{output}");
            }
            Ok(())
        }
        Err(e) => Err(CliError::eval_with_help(
            format!("Changeset status failed: {e}"),
            "Check that the path is valid",
        )),
    }
}

/// Execute release version command safely
#[instrument(name = "cuenv_execute_release_version_safe")]
async fn execute_release_version_safe(
    path: String,
    dry_run: bool,
    json_mode: bool,
) -> Result<(), CliError> {
    match commands::release::execute_release_version(&path, dry_run) {
        Ok(output) => {
            if json_mode {
                let envelope = OkEnvelope::new(serde_json::json!({
                    "result": output
                }));
                match serde_json::to_string(&envelope) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        return Err(CliError::other(format!("JSON serialization failed: {e}")));
                    }
                }
            } else {
                println!("{output}");
            }
            Ok(())
        }
        Err(e) => Err(CliError::eval_with_help(
            format!("Release version failed: {e}"),
            "Create changesets first with 'cuenv changeset add'",
        )),
    }
}

/// Execute release publish command safely
#[instrument(name = "cuenv_execute_release_publish_safe")]
async fn execute_release_publish_safe(
    path: String,
    dry_run: bool,
    json_mode: bool,
) -> Result<(), CliError> {
    match commands::release::execute_release_publish(&path, dry_run) {
        Ok(output) => {
            if json_mode {
                let envelope = OkEnvelope::new(serde_json::json!({
                    "result": output
                }));
                match serde_json::to_string(&envelope) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        return Err(CliError::other(format!("JSON serialization failed: {e}")));
                    }
                }
            } else {
                println!("{output}");
            }
            Ok(())
        }
        Err(e) => Err(CliError::eval_with_help(
            format!("Release publish failed: {e}"),
            "Check that packages are ready for publishing",
        )),
    }
}

// Note: These functions are currently unused but reserved for future async main implementation

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_panic_hook() {
        // Test that panic hook is properly set
        // Note: We can't easily test the panic hook directly
        // Just verify that we can set and take a hook
        let _ = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let _ = std::panic::take_hook();
        // Test passes if no panic occurs
    }

    #[test]
    fn test_cli_args_json_flag() {
        let cli_args = ["cuenv".to_string(), "--json".to_string()];
        let json_flag = cli_args.iter().any(|arg| arg == "--json");
        assert!(json_flag);
    }

    #[test]
    fn test_cli_args_level_flag() {
        let cli_args = [
            "cuenv".to_string(),
            "--level".to_string(),
            "debug".to_string(),
        ];
        let level_flag = cli_args.windows(2).find_map(|args| {
            if args[0] == "--level" || args[0] == "-l" {
                Some(args[1].as_str())
            } else {
                None
            }
        });
        assert_eq!(level_flag, Some("debug"));
    }

    #[test]
    fn test_trace_format_selection() {
        let json_flag = true;
        let trace_format = if json_flag {
            TracingFormat::Json
        } else {
            TracingFormat::Pretty
        };
        assert!(matches!(trace_format, TracingFormat::Json));

        let json_flag = false;
        let trace_format = if json_flag {
            TracingFormat::Json
        } else {
            TracingFormat::Pretty
        };
        assert!(matches!(trace_format, TracingFormat::Pretty));
    }

    #[test]
    fn test_log_level_parsing() {
        let test_cases = vec![
            (Some("trace"), Level::TRACE),
            (Some("debug"), Level::DEBUG),
            (Some("info"), Level::INFO),
            (Some("warn"), Level::WARN),
            (Some("error"), Level::ERROR),
            (None, Level::WARN),            // Default
            (Some("invalid"), Level::WARN), // Invalid falls back to default
        ];

        for (input, expected) in test_cases {
            let log_level = match input {
                Some("trace") => Level::TRACE,
                Some("debug") => Level::DEBUG,
                Some("info") => Level::INFO,
                Some("error") => Level::ERROR,
                _ => Level::WARN,
            };
            assert_eq!(log_level, expected);
        }
    }

    #[test]
    fn test_tracing_config_default() {
        let tracing_config = TracingConfig {
            format: TracingFormat::Dev,
            level: Level::WARN,
            ..Default::default()
        };
        assert!(matches!(tracing_config.format, TracingFormat::Dev));
        assert_eq!(tracing_config.level, Level::WARN);
    }

    #[tokio::test]
    async fn test_command_conversion() {
        use crate::cli::{Commands, OutputFormat};

        // Test Version command conversion
        let cli_command = Commands::Version {
            output_format: OutputFormat::Simple,
        };
        let command: Command = cli_command.into();
        match command {
            Command::Version { format } => assert_eq!(format, "simple"),
            _ => panic!("Expected Command::Version"),
        }
    }
}
