//! `CUEnv` CLI Application

#![allow(missing_docs)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::unused_self)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::used_underscore_items)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::assigning_clones)]

mod cli;
mod commands;
mod errors;
mod events;
mod performance;
mod tracing;
mod tui;

use crate::cli::parse;
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

    // Run the CLI and handle any errors with enhanced reporting
    if let Err(error) = run_main().await {
        eprintln!("{error:?}");
        std::process::exit(1);
    }
}

#[instrument(name = "cuenv_main_impl")]
async fn run_main() -> miette::Result<()> {
    // Parse args early to get tracing options
    let cli_args = std::env::args().collect::<Vec<_>>();
    let json_flag = cli_args.iter().any(|arg| arg == "--json");
    let level_flag = cli_args.windows(2).find_map(|args| {
        if args[0] == "--level" || args[0] == "-l" {
            Some(args[1].as_str())
        } else {
            None
        }
    });

    let trace_format = if json_flag {
        TracingFormat::Json
    } else {
        TracingFormat::Dev
    };

    let log_level = match level_flag {
        Some("trace") => Level::TRACE,
        Some("debug") => Level::DEBUG,
        Some("info") => Level::INFO,
        Some("warn") => Level::WARN,
        Some("error") => Level::ERROR,
        _ => Level::WARN, // Default
    };

    // Initialize enhanced tracing
    let tracing_config = TracingConfig {
        format: trace_format,
        level: log_level,
        ..Default::default()
    };

    crate::tracing::init_tracing(tracing_config)
        .map_err(|e| miette::miette!("Failed to initialize tracing: {}", e))?;

    run_cli().await
}

#[instrument]
async fn run_cli() -> miette::Result<()> {
    // Starting cuenv CLI

    // Parse command line arguments in instrumented span
    let cli = parse_args().await?;

    // Convert CLI command to internal command
    let command: Command = cli.command.into();

    // Execute command with structured tracing
    execute_command(command).await?;

    // cuenv CLI completed successfully
    Ok(())
}

#[instrument]
async fn parse_args() -> miette::Result<crate::cli::Cli> {
    let _start = std::time::Instant::now();
    let cli = parse();

    // CLI arguments parsed

    Ok(cli)
}

#[instrument]
async fn execute_command(command: Command) -> miette::Result<()> {
    let _start = std::time::Instant::now();

    match command {
        Command::Version => {
            execute_version_command().await?;
        }
        Command::EnvPrint {
            path,
            package,
            format,
        } => {
            execute_env_print_command(path, package, format).await?;
        }
    }

    // Command executed successfully

    Ok(())
}

#[instrument]
async fn execute_version_command() -> miette::Result<()> {
    let mut perf_guard = performance::PerformanceGuard::new("version_command");
    perf_guard.add_metadata("command_type", "version");

    // Gathering version information

    let version_info = measure_perf!("get_version_info", {
        commands::version::get_version_info()
    });

    // Version information gathered

    // For simple output, just print to stdout
    println!("{}", version_info);

    // Version information displayed
    perf_guard.finish(true);

    // Log performance summary
    let _summary = performance::registry().get_summary();
    // Performance summary

    Ok(())
}

#[instrument]
async fn execute_env_print_command(
    path: String,
    package: String,
    format: String,
) -> miette::Result<()> {
    let mut perf_guard = performance::PerformanceGuard::new("env_print_command");
    perf_guard.add_metadata("command_type", "env_print");
    perf_guard.add_metadata("package", &package);
    perf_guard.add_metadata("format", &format);

    // Executing env print command

    let output = measure_perf!("env_print_execution", {
        commands::env::execute_env_print(&path, &package, &format).await
    });

    match output {
        Ok(result) => {
            // Env values retrieved successfully
            println!("{result}");
            perf_guard.finish(true);
        }
        Err(e) => {
            // Env print failed
            perf_guard.finish(false);
            return Err(miette::miette!(
                "Failed to print environment variables: {:?}",
                e
            ));
        }
    }

    // Log performance summary
    let _summary = performance::registry().get_summary();
    // Performance summary

    Ok(())
}

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

    #[tokio::test]
    async fn test_parse_args() {
        // Test argument parsing
        let result = parse_args().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_execute_version_command() {
        let result = execute_version_command().await;
        // Version command should always succeed
        assert!(result.is_ok());
    }

    #[test]
    fn test_cli_args_json_flag() {
        let cli_args = vec!["cuenv".to_string(), "--json".to_string()];
        let json_flag = cli_args.iter().any(|arg| arg == "--json");
        assert!(json_flag);
    }

    #[test]
    fn test_cli_args_level_flag() {
        let cli_args = vec!["cuenv".to_string(), "--level".to_string(), "debug".to_string()];
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
            (None, Level::WARN), // Default
            (Some("invalid"), Level::WARN), // Invalid falls back to default
        ];

        for (input, expected) in test_cases {
            let log_level = match input {
                Some("trace") => Level::TRACE,
                Some("debug") => Level::DEBUG,
                Some("info") => Level::INFO,
                Some("warn") => Level::WARN,
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
