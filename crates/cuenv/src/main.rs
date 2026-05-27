//! cuenv CLI Application
//!
//! Production-grade CUE environment toolchain providing command-line interface
//! for CUE package evaluation, environment variable management, and task orchestration.
//!
//! ## Future Direction
//!
//! This binary is transitioning to a library-first architecture (ADR-0006).
//! The eventual goal is to move CLI dispatch behind the library provider
//! registry. Currently, command dispatch remains here while the library exposes
//! provider registration and dynamic sync command construction through
//! `cuenv::Cuenv`.

// Import everything from the library
use crossterm::ExecutableCommand;
use cuenv::cli::{self, CliError, EXIT_OK, OutputFormat, exit_code_for, parse, render_error};
use cuenv::commands::{self, Command, CommandExecutor};
use cuenv::coordinator;
use cuenv::tracing::{self, Level, TracingConfig, TracingFormat};
use cuenv_events::renderers::{CliRenderer, JsonRenderer};
use std::sync::Arc;
use tracing::instrument;

mod async_dispatch;
mod hook_supervisor;
mod oci_activate;
mod sync_dispatch;
use async_dispatch::execute_command_safe;
use hook_supervisor::run_hook_supervisor;
#[cfg(test)]
use oci_activate::activate_lockfile_artifacts;
use oci_activate::run_oci_activate;
use sync_dispatch::execute_sync_command;

/// Exit code for SIGINT (128 + signal number 2)
const EXIT_SIGINT: i32 = 130;

/// LLM context content (llms.txt + CUE schemas concatenated at build time)
const LLMS_CONTENT: &str = include_str!(concat!(env!("OUT_DIR"), "/llms-full.txt"));

/// Main entry point - determines sync vs async execution path
fn main() {
    // Install the rustls crypto provider before any HTTP clients are created.
    // Required because reqwest uses `rustls-no-provider` to avoid bundling aws-lc-sys.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Set up error handling first. Use direct redacted stderr output because
    // tracing infrastructure may be corrupted during a panic.
    // Restore terminal state before printing so the panic output isn't garbled
    // by leftover raw-mode / alt-screen state from a TUI that didn't drop cleanly.
    std::panic::set_hook(Box::new(|panic_info| {
        cleanup_terminal();
        cuenv_events::eprintln_redacted(&format!("Application panicked: {panic_info}"));
        cuenv_events::eprintln_redacted(
            "Internal error occurred. Run with RUST_LOG=debug for more information.",
        );
    }));

    // Register known credential environment variables for redaction.
    // This ensures any output containing these values is automatically redacted.
    for name in [
        "OP_SERVICE_ACCOUNT_TOKEN",
        "INFISICAL_TOKEN",
        "INFISICAL_CLIENT_SECRET",
    ] {
        if let Ok(token) = std::env::var(name)
            && !token.is_empty()
        {
            cuenv_events::register_secret(token);
        }
    }

    // Handle shell completion requests first (before any other processing)
    if cli::try_complete() {
        std::process::exit(EXIT_OK);
    }

    // Check for special internal commands that always need async
    let args: Vec<String> = std::env::args().collect();

    // Hidden process-babysitter wrapper (primarily for macOS where
    // PR_SET_PDEATHSIG is unavailable). Exits directly without touching
    // the main CLI machinery.
    #[cfg(unix)]
    if args.len() > 1 && args[1] == "__supervise" {
        let rest: Vec<String> = args.iter().skip(2).cloned().collect();
        let code = commands::supervise::run(&rest);
        std::process::exit(code);
    }

    if args.len() > 1 && (args[1] == "__hook-supervisor" || args[1] == "__coordinator") {
        // For supervisor, detach from controlling terminal if on Unix
        // This is done here instead of via pre_exec in the parent to avoid
        // fork-safety issues when the parent is multi-threaded (Go runtime)
        #[cfg(unix)]
        if args[1] == "__hook-supervisor" {
            // SAFETY: setsid() creates a new session and process group.
            // This is safe to call at startup. We ignore errors (e.g. if already leader).
            #[expect(
                unsafe_code,
                reason = "Required for POSIX process detachment via setsid()"
            )]
            unsafe {
                libc::setsid();
            }
        }

        // These internal commands always need tokio
        let exit_code = run_with_tokio();
        std::process::exit(exit_code);
    }

    // Parse CLI arguments synchronously to determine execution path
    let cli = cli::parse();

    // Check if command needs async runtime
    if requires_async_runtime(&cli) {
        let exit_code = run_with_tokio();
        std::process::exit(exit_code);
    } else {
        let exit_code = run_sync(cli);
        std::process::exit(exit_code);
    }
}

/// Create tokio runtime and run async path
fn run_with_tokio() -> i32 {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            // Use direct redacted stderr output; tracing/event system is not
            // initialized at this point in startup.
            cuenv_events::eprintln_redacted(&format!(
                "Fatal error: Failed to create tokio runtime: {e}"
            ));
            return 1;
        }
    };

    rt.block_on(run())
}

/// Determine if a command requires the async runtime
const fn requires_async_runtime(cli: &cli::Cli) -> bool {
    // Handle --llms flag (doesn't need async)
    if cli.llms {
        return false;
    }

    match &cli.command {
        None => false, // No subcommand - will show help/error
        Some(cmd) => match cmd {
            // Commands that DON'T need tokio (fast path)
            // Export uses sync fast path with lightweight runtime for performance
            // (shell prompt integration requires sub-10ms response time)
            cli::Commands::Version { .. }
            | cli::Commands::Info { .. }
            | cli::Commands::Completions { .. }
            | cli::Commands::Changeset { .. }
            | cli::Commands::Secrets { .. }
            | cli::Commands::Export { .. }
            | cli::Commands::Fmt { .. } => false,
            cli::Commands::Shell { subcommand } => match subcommand {
                cli::ShellCommands::Init { .. } => false,
            },
            cli::Commands::Release { subcommand } => match subcommand {
                // Version, Publish, and Prepare are sync (CUE/cargo/git operations)
                cli::ReleaseCommands::Version { .. }
                | cli::ReleaseCommands::Publish { .. }
                | cli::ReleaseCommands::Prepare { .. } => false,
                // Binaries needs async for HTTP/process execution
                cli::ReleaseCommands::Binaries { .. } => true,
            },
            cli::Commands::Env { subcommand } => match subcommand {
                // env status without --wait, print, and list are sync (CUE evaluation is sync FFI)
                cli::EnvCommands::Status { wait: false, .. }
                | cli::EnvCommands::Print { .. }
                | cli::EnvCommands::List { .. } => false,
                // Other env commands need async
                _ => true,
            },

            // Commands that NEED tokio
            cli::Commands::Task { .. }
            | cli::Commands::Exec { .. }
            | cli::Commands::Ci { .. }
            | cli::Commands::Build { .. }
            | cli::Commands::Web { .. }
            | cli::Commands::Allow { .. }
            | cli::Commands::Deny { .. }
            | cli::Commands::Sync { .. }
            | cli::Commands::Runtime { .. }
            | cli::Commands::Up { .. }
            | cli::Commands::Down { .. }
            | cli::Commands::Logs { .. }
            | cli::Commands::Ps { .. }
            | cli::Commands::Restart { .. } => true,
            // Tools commands - download/activate need async, list is sync
            cli::Commands::Tools { subcommand } => match subcommand {
                cli::ToolsCommands::Download | cli::ToolsCommands::Activate => true,
                cli::ToolsCommands::List => false,
            },
        },
    }
}

/// Run synchronous commands without tokio runtime
/// This is the fast path for commands that don't need async
fn run_sync(cli: cli::Cli) -> i32 {
    // Set up signal handler for sync path
    let _ = ctrlc::set_handler(|| {
        // Terminate all child processes gracefully before exiting.
        // Need a mini runtime since terminate_all is async.
        if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            rt.block_on(async {
                let registry = cuenv_core::tasks::global_registry();
                registry
                    .terminate_all(std::time::Duration::from_secs(3))
                    .await;
            });
        }

        cleanup_terminal();
        std::process::exit(EXIT_SIGINT);
    });

    // Initialize tracing for sync path (simpler than async path, no event bus needed)
    let log_level = match cli.level {
        tracing::LogLevel::Trace => Level::TRACE,
        tracing::LogLevel::Debug => Level::DEBUG,
        tracing::LogLevel::Info => Level::INFO,
        tracing::LogLevel::Warn => Level::WARN,
        tracing::LogLevel::Error => Level::ERROR,
    };
    let tracing_config = tracing::TracingConfig {
        format: if cli.json {
            tracing::TracingFormat::Json
        } else {
            tracing::TracingFormat::Pretty
        },
        level: log_level,
        ..Default::default()
    };
    // Ignore error if tracing already initialized (e.g., in tests)
    let _ = tracing::init_tracing(tracing_config);

    // Handle --llms flag
    if cli.llms {
        cuenv_events::print_redacted(LLMS_CONTENT);
        return EXIT_OK;
    }

    let json_format = OutputFormat::from_json_flag(cli.json);

    // Ensure a subcommand was provided
    let Some(cli_command) = cli.command else {
        render_error(
            &CliError::config_with_help(
                "No subcommand provided",
                "Run 'cuenv --help' for usage information",
            ),
            json_format,
        );
        return exit_code_for(&CliError::config("No subcommand provided"));
    };

    // Handle completions command
    if let cli::Commands::Completions { shell } = &cli_command {
        cli::generate_completions(*shell);
        return EXIT_OK;
    }

    // Convert CLI command to internal command
    let command: Command = cli_command.into_command(cli.environment.clone());

    // Execute synchronously
    match execute_sync_command(command, json_format) {
        Ok(()) => EXIT_OK,
        Err(err) => {
            render_error(&err, json_format);
            exit_code_for(&err)
        }
    }
}

fn ensure_command_module_compatibility(command: &Command) -> Result<(), CliError> {
    if !should_precheck_command_module_compatibility(command) {
        return Ok(());
    }

    let Some(path) = cue_module_command_path(command) else {
        return Ok(());
    };
    commands::module_version::ensure_compatible_for_path(path).map_err(CliError::from)
}

fn should_precheck_command_module_compatibility(command: &Command) -> bool {
    matches!(command, Command::Info { .. })
}

fn cue_module_command_path(command: &Command) -> Option<&str> {
    match command {
        Command::Info { path, .. } => path.as_deref().or(Some(".")),
        Command::Version { .. }
        | Command::EnvPrint { .. }
        | Command::EnvList { .. }
        | Command::EnvLoad { .. }
        | Command::EnvStatus { .. }
        | Command::EnvCheck { .. }
        | Command::EnvInspect { .. }
        | Command::Allow { .. }
        | Command::Deny { .. }
        | Command::Export { .. }
        | Command::Exec { .. }
        | Command::Task { .. }
        | Command::Sync { .. }
        | Command::Fmt { .. }
        | Command::Build { .. }
        | Command::Up { .. }
        | Command::Down { .. }
        | Command::Logs { .. }
        | Command::Ps { .. }
        | Command::Restart { .. }
        | Command::Ci { .. }
        | Command::ShellInit { .. }
        | Command::Web { .. }
        | Command::Completions { .. }
        | Command::ChangesetAdd { .. }
        | Command::ChangesetStatus { .. }
        | Command::ChangesetFromCommits { .. }
        | Command::ReleasePrepare { .. }
        | Command::ReleaseVersion { .. }
        | Command::ReleasePublish { .. }
        | Command::ReleaseBinaries { .. }
        | Command::SecretsSetup { .. }
        | Command::RuntimeOciActivate
        | Command::ToolsDownload
        | Command::ToolsActivate
        | Command::ToolsList => None,
    }
}

/// Main CLI runner that handles errors properly and returns exit codes
#[instrument(name = "cuenv_run")]
async fn run() -> i32 {
    // Use biased select to prefer signal handling over normal completion
    // This ensures cleanup runs even if the child process exits simultaneously
    tokio::select! {
        biased;

        _ = tokio::signal::ctrl_c() => {
            // Terminate all child processes gracefully before exiting
            let registry = cuenv_core::tasks::global_registry();
            registry.terminate_all(std::time::Duration::from_secs(5)).await;

            // Clean up terminal state to prevent escape sequence garbage
            cleanup_terminal();
            EXIT_SIGINT
        }
        result = real_main() => {
            match result {
                Ok(()) => EXIT_OK,
                Err(err) => {
                    // Try to determine if JSON mode was requested
                    let args: Vec<String> = std::env::args().collect();
                    let json_mode = args.iter().any(|arg| arg == "--json");

                    render_error(&err, OutputFormat::from_json_flag(json_mode));
                    exit_code_for(&err)
                }
            }
        }
    }
}

/// Clean up terminal state on interrupt.
/// This prevents escape sequence garbage from being printed when the user
/// presses Ctrl-C while terminal queries are in-flight.
fn cleanup_terminal() {
    use std::io::Write;

    let mut stdout = std::io::stdout();

    // Disable raw mode if it was enabled (e.g., by TUI)
    let _ = crossterm::terminal::disable_raw_mode();

    // Pop keyboard enhancement flags (kitty protocol) if enabled
    let _ = stdout.execute(crossterm::event::PopKeyboardEnhancementFlags);

    // Show cursor if it was hidden
    let _ = stdout.execute(crossterm::cursor::Show);

    // Leave alternate screen if we were in it
    let _ = stdout.execute(crossterm::terminal::LeaveAlternateScreen);

    // Flush to ensure all escape sequences are sent
    let _ = stdout.flush();

    // Drain any pending input from stdin to consume terminal responses
    // that might have been sent by child processes or terminal queries
    drain_stdin();
}

/// Drain pending input from stdin without blocking.
/// This consumes any terminal responses that are waiting in the input buffer.
fn drain_stdin() {
    use std::time::Duration;

    // Wait briefly for any in-flight terminal responses to arrive
    std::thread::sleep(Duration::from_millis(50));

    // Poll for events with short timeout to drain any pending input
    // This uses crossterm's event system which handles the non-blocking read safely
    while crossterm::event::poll(Duration::from_millis(10)).unwrap_or(false) {
        // Read and discard the event
        let _ = crossterm::event::read();
    }
}

/// Real main implementation that can return `CliError`
#[instrument(name = "cuenv_real_main")]
async fn real_main() -> Result<(), CliError> {
    // Handle shell completion requests first (before any other processing)
    // The shell calls us with special env vars to request completions
    if cli::try_complete() {
        return Ok(());
    }

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

    // Handle --llms flag (print LLM context and exit)
    if init_result.cli.llms {
        cuenv_events::print_redacted(LLMS_CONTENT);
        return Ok(());
    }

    // Ensure a subcommand was provided
    let Some(cli_command) = init_result.cli.command else {
        return Err(CliError::config_with_help(
            "No subcommand provided",
            "Run 'cuenv --help' for usage information",
        ));
    };

    // Handle completions command specially (before converting to internal command)
    if let cli::Commands::Completions { shell } = &cli_command {
        cli::generate_completions(*shell);
        return Ok(());
    }

    // Create executor with the command's package for correct module caching.
    // Each command specifies its package (--package flag, defaults to "cuenv"),
    // and the executor caches the module evaluation for that specific package.
    let executor = create_executor(cli_command.package());

    // Convert CLI command to internal command, passing global environment
    let command: Command = cli_command.into_command(init_result.cli.environment.clone());

    // Execute the command with the shared executor for module caching
    let json_format = OutputFormat::from_json_flag(init_result.cli.json);
    let result = execute_command_safe(command, json_format, &executor).await;

    // Signal renderers to finish processing and exit gracefully
    cuenv_events::emit_shutdown!();

    // Wait for renderer to finish processing any remaining events
    if let Some(handle) = init_result.renderer_handle {
        // Give renderer time to process final events, then abort if stuck
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    }

    // Shut down the global event bus BEFORE the tokio runtime exits.
    // This closes the mpsc channel that the forwarding task waits on,
    // allowing the task to exit and preventing a runtime shutdown deadlock.
    tracing::shutdown_global_events();

    result
}

/// Result of CLI and tracing initialization
struct InitResult {
    cli: cli::Cli,
    /// Handle to the renderer task (if running).
    /// This handle should be awaited before program exit to ensure
    /// all events are properly rendered.
    renderer_handle: Option<tokio::task::JoinHandle<()>>,
}

/// Create a [`CommandExecutor`] with the given package name.
///
/// The executor provides centralized module evaluation - commands that need
/// CUE access can call `executor.get_module()` to get the cached evaluation.
fn create_executor(package: &str) -> Arc<CommandExecutor> {
    let (event_sender, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    Arc::new(CommandExecutor::new(event_sender, package.to_string()))
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
        tracing::LogLevel::Trace => Level::TRACE,
        tracing::LogLevel::Debug => Level::DEBUG,
        tracing::LogLevel::Info => Level::INFO,
        tracing::LogLevel::Warn => Level::WARN,
        tracing::LogLevel::Error => Level::ERROR,
    };

    // Initialize enhanced tracing with event capture
    let tracing_config = TracingConfig {
        format: trace_format,
        level: log_level,
        ..Default::default()
    };

    // Initialize tracing and get the event receiver for the main renderer
    let receiver = match tracing::init_tracing_with_events(tracing_config) {
        Ok(rx) => rx,
        Err(e) => {
            return Err(CliError::config(format!(
                "Failed to initialize tracing: {e}"
            )));
        }
    };

    // Check if TUI mode is enabled (Task command with --tui flag)
    let tui_mode = matches!(&cli.command, Some(cli::Commands::Task { tui: true, .. }));

    // Spawn appropriate renderer based on output mode
    // Skip CLI renderer in TUI mode - TUI handles its own event rendering
    let renderer_handle = if cli.json {
        // JSON mode: output structured JSON events
        let renderer = JsonRenderer::new();
        Some(tokio::spawn(async move {
            renderer.run(receiver).await;
        }))
    } else if tui_mode {
        // TUI mode: don't spawn CLI renderer, TUI subscribes to events directly
        // Drop the receiver to avoid memory buildup
        drop(receiver);
        None
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

/// Run as the coordinator server (internal - spawned by discovery)
async fn run_coordinator() -> Result<(), CliError> {
    use coordinator::server::EventCoordinator;

    let coordinator = EventCoordinator::new();
    coordinator
        .run()
        .await
        .map_err(|e| CliError::other(format!("Coordinator failed: {e}")))
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
