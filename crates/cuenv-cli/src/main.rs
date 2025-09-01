mod cli;
mod commands;
mod errors;
mod events;
mod performance;
mod tui;
mod tracing;

use tracing::instrument;
use crate::cli::parse;
use crate::commands::Command;
use crate::tracing::{TracingConfig, TracingFormat, Level};

#[tokio::main]
#[instrument(name = "cuenv_main")]
async fn main() {
    // Set up error handling first
    std::panic::set_hook(Box::new(|panic_info| {
        eprintln!("Application panicked: {}", panic_info);
        eprintln!("Internal error occurred. Run with RUST_LOG=debug for more information.");
    }));
    
    // Run the CLI and handle any errors with enhanced reporting
    if let Err(error) = run_main().await {
        eprintln!("{:?}", error);
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
    
    crate::tracing::init_tracing(tracing_config).map_err(|e| {
        miette::miette!("Failed to initialize tracing: {}", e)
    })?;
    
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
        Command::EnvPrint { path, package, format } => {
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
async fn execute_env_print_command(path: String, package: String, format: String) -> miette::Result<()> {
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
            println!("{}", result);
            perf_guard.finish(true);
        }
        Err(e) => {
            // Env print failed
            perf_guard.finish(false);
            return Err(miette::miette!("Failed to print environment variables: {:?}", e));
        }
    }
    
    // Log performance summary
    let _summary = performance::registry().get_summary();
    // Performance summary
    
    Ok(())
}


