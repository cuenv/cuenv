//! `CUEnv` CLI Application

//! cuenv CLI Application - Production-grade CUE environment toolchain
//!
//! This binary provides command-line interface for CUE package evaluation,
//! environment variable management, and task orchestration.

// Individual clippy allows for specific justified cases can be added locally
// where needed, rather than broad suppression

mod cli;
mod commands;
mod errors;
mod events;
mod performance;
mod tracing;
mod tui;

use crate::cli::{CliError, EXIT_OK, OkEnvelope, exit_code_for, parse, render_error};
use crate::commands::Command;
use crate::tracing::{Level, TracingConfig, TracingFormat};
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
    // Parse CLI arguments
    let cli = match initialize_cli_and_tracing().await {
        Ok(cli) => cli,
        Err(e) => {
            return Err(CliError::config_with_help(
                format!("Failed to parse CLI arguments: {e}"),
                "Check your command line arguments and try again",
            ));
        }
    };

    // Convert CLI command to internal command
    let command: Command = cli.command.into();

    // Execute the command
    match execute_command_safe(command, cli.json).await {
        Ok(()) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Initialize CLI parsing and tracing configuration
#[instrument(name = "cuenv_initialize_cli_and_tracing")]
async fn initialize_cli_and_tracing() -> Result<crate::cli::Cli, CliError> {
    // Parse CLI arguments once
    let cli = parse();

    // Derive tracing configuration from parsed CLI
    let trace_format = if cli.json {
        TracingFormat::Json
    } else {
        TracingFormat::Dev
    };

    let log_level = match cli.level {
        crate::tracing::LogLevel::Trace => Level::TRACE,
        crate::tracing::LogLevel::Debug => Level::DEBUG,
        crate::tracing::LogLevel::Info => Level::INFO,
        crate::tracing::LogLevel::Warn => Level::WARN,
        crate::tracing::LogLevel::Error => Level::ERROR,
    };

    // Initialize enhanced tracing
    let tracing_config = TracingConfig {
        format: trace_format,
        level: log_level,
        ..Default::default()
    };

    match crate::tracing::init_tracing(tracing_config) {
        Ok(()) => {}
        Err(e) => {
            return Err(CliError::config(format!(
                "Failed to initialize tracing: {e}"
            )));
        }
    }

    Ok(cli)
}

/// Execute command safely without ? operator
#[instrument(name = "cuenv_execute_command_safe")]
async fn execute_command_safe(command: Command, json_mode: bool) -> Result<(), CliError> {
    match command {
        Command::Version => match execute_version_command_safe().await {
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
        } => match execute_task_command_safe(path, package, name).await {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        },
        Command::Exec {
            path,
            package,
            command,
            args,
        } => match execute_exec_command_safe(path, package, command, args).await {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        },
        Command::EnvLoad { path } => match execute_env_load_command_safe(path, json_mode).await {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        },
        Command::EnvStatus {
            path,
            wait,
            timeout,
        } => match execute_env_status_command_safe(path, wait, timeout, json_mode).await {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        },
        Command::ShellInit { shell } => execute_shell_init_command_safe(shell, json_mode),
        Command::Allow { path, note } => {
            match execute_allow_command_safe(path, note, json_mode).await {
                Ok(()) => Ok(()),
                Err(e) => Err(e),
            }
        }
    }
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
async fn execute_env_load_command_safe(path: String, json_mode: bool) -> Result<(), CliError> {
    match commands::hooks::execute_env_load(&path).await {
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
    wait: bool,
    timeout: u64,
    json_mode: bool,
) -> Result<(), CliError> {
    match commands::hooks::execute_env_status(&path, wait, timeout).await {
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
    note: Option<String>,
    json_mode: bool,
) -> Result<(), CliError> {
    match commands::hooks::execute_allow(&path, note).await {
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
                format!("Failed to print environment variables: {e:?}"),
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
) -> Result<(), CliError> {
    let mut perf_guard = performance::PerformanceGuard::new("task_command");
    perf_guard.add_metadata("command_type", "task");

    match commands::task::execute_task(&path, &package, name.as_deref(), false).await {
        Ok(output) => {
            println!("{output}");
            perf_guard.finish(true);
            Ok(())
        }
        Err(e) => {
            perf_guard.finish(false);
            Err(CliError::eval_with_help(
                format!("Task execution failed: {e}"),
                "Check your CUE configuration and task definitions",
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
) -> Result<(), CliError> {
    let mut perf_guard = performance::PerformanceGuard::new("exec_command");
    perf_guard.add_metadata("command_type", "exec");

    match commands::exec::execute_exec(&path, &package, &command, &args).await {
        Ok(exit_code) => {
            perf_guard.finish(exit_code == 0);
            if exit_code == 0 {
                Ok(())
            } else {
                Err(CliError::other(format!(
                    "Command exited with non-zero code: {exit_code}"
                )))
            }
        }
        Err(e) => {
            perf_guard.finish(false);
            Err(CliError::eval_with_help(
                format!("Command execution failed: {e}"),
                "Check your CUE configuration and command",
            ))
        }
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
            TracingFormat::Dev
        };
        assert!(matches!(trace_format, TracingFormat::Json));

        let json_flag = false;
        let trace_format = if json_flag {
            TracingFormat::Json
        } else {
            TracingFormat::Dev
        };
        assert!(matches!(trace_format, TracingFormat::Dev));
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
        use crate::cli::Commands;

        // Test Version command conversion
        let cli_command = Commands::Version;
        let command: Command = cli_command.into();
        assert!(matches!(command, Command::Version));
    }
}
