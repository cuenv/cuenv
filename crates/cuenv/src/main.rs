//! cuenv CLI Application
//!
//! Production-grade CUE environment toolchain providing command-line interface
//! for CUE package evaluation, environment variable management, and task orchestration.
//!
//! ## Future Direction
//!
//! This binary is transitioning to a library-first architecture (ADR-0006).
//! The eventual goal is:
//!
//! ```ignore
//! fn main() -> cuenv::Result<()> {
//!     cuenv::Cuenv::builder()
//!         .with_defaults()
//!         .build()
//!         .run()
//! }
//! ```
//!
//! Currently, the CLI logic remains here while the library infrastructure
//! is being developed. See `cuenv::Cuenv` for the library API.

// CLI binary needs to output to stdout/stderr - this is intentional
// expect_used is allowed for infallible operations like writing to strings
#![allow(clippy::print_stdout, clippy::print_stderr, clippy::expect_used)]

// Import everything from the library
use cuenv::cli::{self, CliError, EXIT_OK, OkEnvelope, exit_code_for, parse, render_error};
use cuenv::commands::{self, Command, CommandExecutor};
use cuenv::tracing::{self, Level, TracingConfig, TracingFormat};
use cuenv::{coordinator, tui};
use crossterm::ExecutableCommand;
use cuenv_core::hooks::execute_hooks;
use cuenv_core::hooks::state::StateManager;
use cuenv_core::hooks::{ExecutionStatus, Hook, HookExecutionConfig};
use cuenv_events::renderers::{CliRenderer, JsonRenderer};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::instrument;

/// Exit code for SIGINT (128 + signal number 2)
const EXIT_SIGINT: i32 = 130;

/// LLM context content (llms.txt + CUE schemas concatenated at build time)
const LLMS_CONTENT: &str = include_str!(concat!(env!("OUT_DIR"), "/llms-full.txt"));

/// Main entry point - determines sync vs async execution path
fn main() {
    // Set up error handling first
    // NOTE: Using eprintln! in panic hook is intentional - tracing infrastructure
    // may be corrupted during a panic, so we use the most reliable output method.
    #[allow(clippy::print_stderr)]
    std::panic::set_hook(Box::new(|panic_info| {
        eprintln!("Application panicked: {panic_info}");
        eprintln!("Internal error occurred. Run with RUST_LOG=debug for more information.");
    }));

    // Register known credential environment variables for redaction.
    // This ensures any output containing these values is automatically redacted.
    if let Ok(token) = std::env::var("OP_SERVICE_ACCOUNT_TOKEN") {
        cuenv_events::register_secret(token);
    }

    // Handle shell completion requests first (before any other processing)
    if cli::try_complete() {
        std::process::exit(EXIT_OK);
    }

    // Check for special internal commands that always need async
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && (args[1] == "__hook-supervisor" || args[1] == "__coordinator") {
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
            // NOTE: Using eprintln! here is intentional - tracing/event system
            // is not yet initialized at this point in startup.
            #[allow(clippy::print_stderr)]
            {
                eprintln!("Fatal error: Failed to create tokio runtime: {e}");
            }
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
            | cli::Commands::Export { .. } => false,
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
            | cli::Commands::Tui
            | cli::Commands::Web { .. }
            | cli::Commands::Allow { .. }
            | cli::Commands::Deny { .. }
            | cli::Commands::Sync { .. }
            | cli::Commands::Runtime { .. } => true,
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
        print!("{LLMS_CONTENT}");
        return EXIT_OK;
    }

    // Ensure a subcommand was provided
    let Some(cli_command) = cli.command else {
        render_error(
            &CliError::config_with_help(
                "No subcommand provided",
                "Run 'cuenv --help' for usage information",
            ),
            cli.json,
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
    match execute_sync_command(command, cli.json) {
        Ok(()) => EXIT_OK,
        Err(err) => {
            render_error(&err, cli.json);
            exit_code_for(&err)
        }
    }
}

/// Execute commands synchronously (no tokio runtime)
#[allow(clippy::too_many_lines)] // Command dispatcher naturally has many cases
fn execute_sync_command(command: Command, json_mode: bool) -> Result<(), CliError> {
    match command {
        Command::Version { format: _ } => {
            let version_info = commands::version::get_version_info();
            println!("{version_info}");
            Ok(())
        }

        Command::Info {
            path,
            package,
            meta,
        } => match commands::info::execute_info(path.as_deref(), &package, json_mode, meta) {
            Ok(output) => {
                print!("{output}");
                Ok(())
            }
            Err(e) => Err(CliError::eval_with_help(
                format!("Info command failed: {e}"),
                "Check that you are in a CUE module with valid env.cue files",
            )),
        },

        Command::ShellInit { shell } => execute_shell_init_command_safe(shell, json_mode),

        Command::EnvStatus {
            path,
            package,
            wait: false,
            format,
            ..
        } => match commands::hooks::execute_env_status_sync(&path, &package, format) {
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
                "Check that your env.cue file exists",
            )),
        },

        Command::EnvPrint {
            path,
            package,
            format,
            environment,
        } => {
            // CUE evaluation is sync FFI, so we can call the async function via a mini runtime
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .map_err(|e| CliError::other(format!("Runtime error: {e}")))?;

            rt.block_on(async {
                match commands::env::execute_env_print(
                    &path,
                    &package,
                    &format,
                    environment.as_deref(),
                    None,
                )
                .await
                {
                    Ok(result) => {
                        println!("{result}");
                        Ok(())
                    }
                    Err(e) => {
                        let cli_err: CliError = e.into();
                        Err(cli_err.with_help("Check your CUE files and package configuration"))
                    }
                }
            })
        }

        Command::EnvList {
            path,
            package,
            format,
        } => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .map_err(|e| CliError::other(format!("Runtime error: {e}")))?;

            rt.block_on(async {
                match commands::env::execute_env_list(&path, &package, &format, None).await {
                    Ok(result) => {
                        println!("{result}");
                        Ok(())
                    }
                    Err(e) => {
                        let cli_err: CliError = e.into();
                        Err(cli_err.with_help("Check your CUE files and package configuration"))
                    }
                }
            })
        }

        Command::ChangesetAdd {
            path,
            summary,
            description,
            packages,
        } => match commands::release::execute_changeset_add(
            &path,
            &packages,
            summary.as_deref(),
            description.as_deref(),
        ) {
            Ok(output) => {
                if json_mode {
                    let envelope = OkEnvelope::new(serde_json::json!({ "message": output }));
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
        },

        Command::ChangesetStatus { path, json } => {
            let use_json = json || json_mode;
            match commands::release::execute_changeset_status_with_format(&path, use_json) {
                Ok(output) => {
                    println!("{output}");
                    Ok(())
                }
                Err(e) => Err(CliError::eval_with_help(
                    format!("Changeset status failed: {e}"),
                    "Check that the path is valid",
                )),
            }
        }

        Command::ChangesetFromCommits { path, since } => {
            match commands::release::execute_changeset_from_commits(&path, since.as_deref()) {
                Ok(output) => {
                    if json_mode {
                        let envelope = OkEnvelope::new(serde_json::json!({ "message": output }));
                        match serde_json::to_string(&envelope) {
                            Ok(json) => println!("{json}"),
                            Err(e) => {
                                return Err(CliError::other(format!(
                                    "JSON serialization failed: {e}"
                                )));
                            }
                        }
                    } else {
                        println!("{output}");
                    }
                    Ok(())
                }
                Err(e) => Err(CliError::eval_with_help(
                    format!("Changeset from-commits failed: {e}"),
                    "Check that the path is a valid git repository",
                )),
            }
        }

        Command::ReleasePrepare {
            path,
            since,
            dry_run,
            branch,
            no_pr,
        } => {
            let opts = commands::release::ReleasePrepareOptions {
                path,
                since,
                dry_run,
                branch,
                no_pr,
            };
            match commands::release::execute_release_prepare(&opts) {
                Ok(output) => {
                    if json_mode {
                        let envelope = OkEnvelope::new(serde_json::json!({ "result": output }));
                        match serde_json::to_string(&envelope) {
                            Ok(json) => println!("{json}"),
                            Err(e) => {
                                return Err(CliError::other(format!(
                                    "JSON serialization failed: {e}"
                                )));
                            }
                        }
                    } else {
                        println!("{output}");
                    }
                    Ok(())
                }
                Err(e) => Err(CliError::eval_with_help(
                    format!("Release prepare failed: {e}"),
                    "Check git history and workspace configuration",
                )),
            }
        }

        Command::ReleaseVersion { path, dry_run } => {
            match commands::release::execute_release_version(&path, dry_run) {
                Ok(output) => {
                    if json_mode {
                        let envelope = OkEnvelope::new(serde_json::json!({ "result": output }));
                        match serde_json::to_string(&envelope) {
                            Ok(json) => println!("{json}"),
                            Err(e) => {
                                return Err(CliError::other(format!(
                                    "JSON serialization failed: {e}"
                                )));
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

        Command::ReleasePublish { path, dry_run } => {
            let format = if json_mode {
                commands::release::OutputFormat::Json
            } else {
                commands::release::OutputFormat::Human
            };
            match commands::release::execute_release_publish(&path, dry_run, format) {
                Ok(output) => {
                    if json_mode {
                        let envelope = OkEnvelope::new(serde_json::json!({ "result": output }));
                        match serde_json::to_string(&envelope) {
                            Ok(json) => println!("{json}"),
                            Err(e) => {
                                return Err(CliError::other(format!(
                                    "JSON serialization failed: {e}"
                                )));
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

        Command::Completions { shell } => {
            cli::generate_completions(shell);
            Ok(())
        }

        Command::SecretsSetup { provider, wasm_url } => {
            commands::secrets::execute_secrets_setup(provider, wasm_url.as_deref())
        }

        Command::ToolsList => commands::tools::execute_tools_list(),

        Command::Export { shell, package } => {
            // Try sync fast path first (handles no-env-cue, running, failed states)
            match commands::export::execute_export_sync(shell.as_deref(), &package) {
                Ok(Some(output)) => {
                    // Fast path succeeded - output directly
                    print!("{output}");
                    Ok(())
                }
                Ok(None) => {
                    // Need async path - use lightweight single-thread runtime
                    // (like EnvPrint does for CUE evaluation)
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| CliError::other(format!("Runtime error: {e}")))?;

                    rt.block_on(async {
                        match commands::export::execute_export(shell.as_deref(), &package, None)
                            .await
                        {
                            Ok(result) => {
                                print!("{result}");
                                Ok(())
                            }
                            Err(e) => {
                                let cli_err: CliError = e.into();
                                Err(cli_err.with_help("Check your CUE configuration"))
                            }
                        }
                    })
                }
                Err(e) => {
                    let cli_err: CliError = e.into();
                    Err(cli_err.with_help("Check your CUE configuration"))
                }
            }
        }

        // All other commands should have been routed to async path
        _ => Err(CliError::other(
            "Internal error: async command reached sync path",
        )),
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

                    render_error(&err, json_mode);
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
        print!("{LLMS_CONTENT}");
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
    let result = execute_command_safe(command, init_result.cli.json, &executor).await;

    // Wait for renderer to finish processing any remaining events
    if let Some(handle) = init_result.renderer_handle {
        // Give renderer a moment to process final events, then abort if stuck
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), handle).await;
    }

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
    let tui_mode = matches!(
        &cli.command,
        Some(cli::Commands::Task { tui: true, .. })
    );

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

/// Execute command safely without ? operator
///
/// Execute a command using the `CommandExecutor`.
///
/// Special commands (Tui, Web, Completions) are handled directly here since they
/// don't fit the standard command execution pattern. All other commands are delegated
/// to the executor's event-driven `execute()` method.
#[allow(clippy::too_many_lines)]
#[instrument(name = "cuenv_execute_command_safe", skip(executor))]
async fn execute_command_safe(
    command: Command,
    json_mode: bool,
    executor: &CommandExecutor,
) -> Result<(), CliError> {
    // Special commands that bypass the executor (they don't fit the event pattern)
    match &command {
        Command::Tui => {
            return execute_tui_command()
                .await
                .map_err(|e| CliError::other(e.to_string()));
        }
        Command::Web { port, host } => {
            return execute_web_command(*port, host.clone())
                .await
                .map_err(|e| CliError::other(e.to_string()));
        }
        Command::Completions { shell } => {
            // Completions are handled early in real_main, this is just for exhaustiveness
            cli::generate_completions(*shell);
            return Ok(());
        }
        Command::SecretsSetup { provider, wasm_url } => {
            // Secrets setup is handled early in real_main, this is just for exhaustiveness
            return commands::secrets::execute_secrets_setup(*provider, wasm_url.as_deref());
        }
        Command::RuntimeOciActivate => {
            return run_oci_activate().await;
        }
        Command::ToolsDownload => {
            return commands::tools::execute_tools_download().await;
        }
        Command::ToolsActivate => {
            return commands::tools::execute_tools_activate().await;
        }
        Command::ToolsList => {
            return commands::tools::execute_tools_list();
        }
        // Info command needs special handling for json_mode and output
        Command::Info {
            path,
            package,
            meta,
        } => {
            return match commands::info::execute_info(path.as_deref(), package, json_mode, *meta) {
                Ok(output) => {
                    print!("{output}");
                    Ok(())
                }
                Err(e) => Err(CliError::eval_with_help(
                    format!("Info command failed: {e}"),
                    "Check that you are in a CUE module with valid env.cue files",
                )),
            };
        }
        // Changeset commands need special handling for json_mode
        Command::ChangesetAdd {
            path,
            summary,
            description,
            packages,
        } => {
            return execute_changeset_add_safe(
                path.clone(),
                summary.clone(),
                description.clone(),
                packages.clone(),
                json_mode,
            )
            .await;
        }
        Command::ChangesetStatus { path, json } => {
            let use_json = *json || json_mode;
            return execute_changeset_status_safe(path.clone(), use_json).await;
        }
        Command::ChangesetFromCommits { path, since } => {
            return execute_changeset_from_commits_safe(path.clone(), since.clone(), json_mode)
                .await;
        }
        Command::ReleasePrepare {
            path,
            since,
            dry_run,
            branch,
            no_pr,
        } => {
            return execute_release_prepare_safe(
                path.clone(),
                since.clone(),
                *dry_run,
                branch.clone(),
                *no_pr,
                json_mode,
            )
            .await;
        }
        Command::ReleaseVersion { path, dry_run } => {
            return execute_release_version_safe(path.clone(), *dry_run, json_mode).await;
        }
        Command::ReleasePublish { path, dry_run } => {
            return execute_release_publish_safe(path.clone(), *dry_run, json_mode).await;
        }
        Command::ReleaseBinaries {
            path,
            dry_run,
            backends,
            build_only,
            package_only,
            publish_only,
            targets,
            version,
        } => {
            use commands::release::{ReleaseBinariesOptions, ReleaseBinariesPhase};

            let phase = if *build_only {
                ReleaseBinariesPhase::Build
            } else if *package_only {
                ReleaseBinariesPhase::Package
            } else if *publish_only {
                ReleaseBinariesPhase::Publish
            } else {
                ReleaseBinariesPhase::Full
            };

            let opts = ReleaseBinariesOptions::new(path.clone())
                .with_dry_run(*dry_run)
                .with_backends(backends.clone())
                .with_phase(phase)
                .with_targets(targets.clone())
                .with_version(version.clone());

            return execute_release_binaries_safe(opts, json_mode).await;
        }
        _ => {}
    }

    // All other commands go through the executor's event-driven execute() method
    // Use the proper From conversion to preserve error type semantics
    executor.execute(command).await.map_err(|e| {
        let cli_err: CliError = e.into();
        cli_err.with_help("Run with --help for usage information")
    })
}

/// Execute TUI command - starts interactive event dashboard
#[instrument(name = "cuenv_execute_tui")]
async fn execute_tui_command() -> Result<(), CliError> {
    use coordinator::client::CoordinatorClient;
    use coordinator::protocol::UiType;

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
    match tui::run_event_viewer(&mut client).await {
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

/// Execute shell init command safely
#[instrument(name = "cuenv_execute_shell_init_safe")]
fn execute_shell_init_command_safe(
    shell: cli::ShellType,
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

/// Run as the coordinator server (internal - spawned by discovery)
async fn run_coordinator() -> Result<(), CliError> {
    use coordinator::server::EventCoordinator;

    let coordinator = EventCoordinator::new();
    coordinator
        .run()
        .await
        .map_err(|e| CliError::other(format!("Coordinator failed: {e}")))
}

/// Run OCI binary activation (`cuenv runtime oci activate`).
///
/// Reads the lockfile, pulls/extracts binaries for the current platform,
/// and outputs PATH and library path modifications to stdout (to be sourced
/// by the hook system).
///
/// For Homebrew formulas, extracts full bottles (bin/ + lib/) and outputs
/// both PATH and DYLD_LIBRARY_PATH/LD_LIBRARY_PATH for dynamic linking.
///
/// This command is typically invoked by the `#OCIActivate` hook defined in
/// `schema/oci.cue` to add OCI-managed binaries to the PATH.
async fn run_oci_activate() -> Result<(), CliError> {
    use cuenv_core::lockfile::{ArtifactKind, Lockfile};
    use cuenv_tools_oci::{
        OciCache, OciClient, current_platform, extract_homebrew_binary, extract_homebrew_bottle,
        is_homebrew_image, relocate_homebrew_bottle, to_homebrew_platform,
    };
    use std::collections::{HashMap, HashSet};

    // Find the lockfile by walking up from current directory
    let lockfile_path = find_lockfile().ok_or_else(|| {
        CliError::config_with_help(
            "No cuenv.lock found",
            "Run 'cuenv sync lock' to create the lockfile",
        )
    })?;

    // Load the lockfile
    let lockfile = Lockfile::load(&lockfile_path)
        .map_err(|e| CliError::other(format!("Failed to load lockfile: {e}")))?
        .ok_or_else(|| {
            CliError::config_with_help(
                "Lockfile is empty",
                "Run 'cuenv sync lock' to populate the lockfile",
            )
        })?;

    // Get current platform
    let platform = current_platform();
    let platform_str = platform.to_string();

    // Initialize OCI client and cache
    let client = OciClient::new();
    let cache = OciCache::default();
    cache.ensure_dirs().map_err(|e| {
        CliError::other(format!("Failed to create cache directories: {e}"))
    })?;

    // Track directories to add to PATH and library paths
    let mut bin_dirs: HashSet<PathBuf> = HashSet::new();
    let mut lib_dirs: HashSet<PathBuf> = HashSet::new();

    // Build a map of formula name -> version for dependency resolution
    let mut formula_versions: HashMap<String, String> = HashMap::new();
    for artifact in &lockfile.artifacts {
        if let ArtifactKind::Homebrew { name, version, .. } = &artifact.kind {
            formula_versions.insert(name.clone(), version.clone());
        }
    }

    // Track formulas that need relocation (extracted this run)
    let mut formulas_to_relocate: Vec<(String, String)> = Vec::new();

    for artifact in &lockfile.artifacts {
        // Check if this artifact has data for our platform
        let Some(platform_data) = artifact.platforms.get(&platform_str) else {
            // Skip artifacts not available for this platform
            continue;
        };

        match &artifact.kind {
            ArtifactKind::Homebrew {
                name, version, ..
            } => {
                // Check if formula is already extracted
                if cache.has_formula(name, version) {
                    let bin = cache.formula_bin(name, version);
                    let lib = cache.formula_lib(name, version);
                    if bin.exists() {
                        bin_dirs.insert(bin);
                    }
                    if lib.exists() {
                        lib_dirs.insert(lib);
                    }
                    continue;
                }

                // Need to fetch and extract the bottle
                // Verify platform is supported by Homebrew
                let _homebrew_platform = to_homebrew_platform(&platform_str).ok_or_else(|| {
                    CliError::other(format!(
                        "Platform '{}' not supported by Homebrew",
                        platform_str
                    ))
                })?;

                // Build the bottle image reference
                let image = format!("ghcr.io/homebrew/core/{}:{}", name, version);

                // Pull the bottle
                let resolved = client
                    .resolve_digest(&image, &platform)
                    .await
                    .map_err(|e| CliError::other(format!("Failed to resolve '{}': {}", image, e)))?;

                let layer_paths = client
                    .pull_layers(&resolved, &cache)
                    .await
                    .map_err(|e| {
                        CliError::other(format!("Failed to pull layers for '{}': {}", image, e))
                    })?;

                // Extract the full bottle to formula directory
                let dest_dir = cache.formula_dir(name, version);
                if let Some(layer_path) = layer_paths.first() {
                    extract_homebrew_bottle(layer_path, &dest_dir).map_err(|e| {
                        CliError::other(format!(
                            "Failed to extract Homebrew bottle '{}': {}",
                            name, e
                        ))
                    })?;

                    // Mark for relocation
                    formulas_to_relocate.push((name.clone(), version.clone()));
                }

                // Add bin and lib directories if they exist
                let bin = cache.formula_bin(name, version);
                let lib = cache.formula_lib(name, version);
                if bin.exists() {
                    bin_dirs.insert(bin);
                }
                if lib.exists() {
                    lib_dirs.insert(lib);
                }
            }

            ArtifactKind::Image { image } => {
                let digest = &platform_data.digest;
                let binary_name = extract_binary_name_from_image(image);

                // Check if binary is already cached
                if let Some(cached_path) = cache.get_binary(digest, &binary_name) {
                    if let Some(parent) = cached_path.parent() {
                        bin_dirs.insert(parent.to_path_buf());
                    }
                    continue;
                }

                // Need to pull and extract
                let resolved = client
                    .resolve_digest(image, &platform)
                    .await
                    .map_err(|e| CliError::other(format!("Failed to resolve '{}': {}", image, e)))?;

                let layer_paths = client
                    .pull_layers(&resolved, &cache)
                    .await
                    .map_err(|e| {
                        CliError::other(format!("Failed to pull layers for '{}': {}", image, e))
                    })?;

                // Extract binary based on image type
                let dest = cache.binary_path(digest, &binary_name);

                if is_homebrew_image(image) {
                    // Legacy: Homebrew bottle via Image artifact (for backwards compat)
                    if let Some(layer_path) = layer_paths.first() {
                        extract_homebrew_binary(layer_path, &binary_name, &dest).map_err(|e| {
                            CliError::other(format!(
                                "Failed to extract '{}' from Homebrew bottle: {}",
                                binary_name, e
                            ))
                        })?;
                    }
                } else {
                    // Non-Homebrew image - would need extract config from CUE
                    // For now, skip with warning
                    eprintln!(
                        "Warning: Non-Homebrew image '{}' requires explicit extract paths",
                        image
                    );
                    continue;
                }

                if let Some(parent) = dest.parent() {
                    bin_dirs.insert(parent.to_path_buf());
                }
            }
        }
    }

    // Relocate newly extracted formulas (patch Homebrew placeholder paths)
    let homebrew_cache = cache.root().join("homebrew");
    for (name, version) in &formulas_to_relocate {
        let formula_dir = cache.formula_dir(name, version);
        relocate_homebrew_bottle(&formula_dir, &homebrew_cache, name, version, &formula_versions)
            .map_err(|e| {
                CliError::other(format!(
                    "Failed to relocate Homebrew bottle '{}': {}",
                    name, e
                ))
            })?;
    }

    // Output library path modification first (dependencies must be available)
    if !lib_dirs.is_empty() {
        let lib_path: Vec<String> = lib_dirs.iter().map(|p| p.display().to_string()).collect();
        let lib_path_str = lib_path.join(":");

        // Use appropriate library path variable for the platform
        #[cfg(target_os = "macos")]
        println!("export DYLD_LIBRARY_PATH=\"{}:$DYLD_LIBRARY_PATH\"", lib_path_str);

        #[cfg(not(target_os = "macos"))]
        println!("export LD_LIBRARY_PATH=\"{}:$LD_LIBRARY_PATH\"", lib_path_str);
    }

    // Output PATH modification
    if !bin_dirs.is_empty() {
        let path_additions: Vec<String> = bin_dirs
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        println!("export PATH=\"{}:$PATH\"", path_additions.join(":"));
    }

    Ok(())
}

/// Find the lockfile by walking up from current directory
fn find_lockfile() -> Option<PathBuf> {
    use cuenv_core::lockfile::LOCKFILE_NAME;

    let mut current = std::env::current_dir().ok()?;
    loop {
        let lockfile_path = current.join(LOCKFILE_NAME);
        if lockfile_path.exists() {
            return Some(lockfile_path);
        }

        // Also check in cue.mod directory
        let cue_mod_lockfile = current.join("cue.mod").join(LOCKFILE_NAME);
        if cue_mod_lockfile.exists() {
            return Some(cue_mod_lockfile);
        }

        if !current.pop() {
            return None;
        }
    }
}

/// Extract binary name from image reference
///
/// For Homebrew bottles: `ghcr.io/homebrew/core/jq:1.7.1` -> `jq`
/// For other images: extract the last path component before the tag
fn extract_binary_name_from_image(image: &str) -> String {
    // Remove tag/digest suffix
    let without_tag = image.split(':').next().unwrap_or(image);
    let without_digest = without_tag.split('@').next().unwrap_or(without_tag);

    // Get last path component
    without_digest
        .rsplit('/')
        .next()
        .unwrap_or("binary")
        .to_string()
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
    let state_dir = match config.state_dir.clone() {
        Some(dir) => dir,
        None => StateManager::default_state_dir()
            .map_err(|e| CliError::other(format!("failed to get default state dir: {e}")))?,
    };
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
    summary: Option<String>,
    description: Option<String>,
    packages: Vec<(String, String)>,
    json_mode: bool,
) -> Result<(), CliError> {
    match commands::release::execute_changeset_add(
        &path,
        &packages,
        summary.as_deref(),
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
    // Use the format-aware function that returns proper JSON structure
    match commands::release::execute_changeset_status_with_format(&path, json_mode) {
        Ok(output) => {
            println!("{output}");
            Ok(())
        }
        Err(e) => Err(CliError::eval_with_help(
            format!("Changeset status failed: {e}"),
            "Check that the path is valid",
        )),
    }
}

/// Execute changeset from-commits command safely
#[instrument(name = "cuenv_execute_changeset_from_commits_safe")]
async fn execute_changeset_from_commits_safe(
    path: String,
    since: Option<String>,
    json_mode: bool,
) -> Result<(), CliError> {
    match commands::release::execute_changeset_from_commits(&path, since.as_deref()) {
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
            format!("Changeset from-commits failed: {e}"),
            "Check that the path is a valid git repository",
        )),
    }
}

/// Execute release prepare command safely
#[instrument(name = "cuenv_execute_release_prepare_safe")]
async fn execute_release_prepare_safe(
    path: String,
    since: Option<String>,
    dry_run: bool,
    branch: String,
    no_pr: bool,
    json_mode: bool,
) -> Result<(), CliError> {
    let opts = commands::release::ReleasePrepareOptions {
        path,
        since,
        dry_run,
        branch,
        no_pr,
    };
    match commands::release::execute_release_prepare(&opts) {
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
            format!("Release prepare failed: {e}"),
            "Check git history and workspace configuration",
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
    // Use Human format for CLI, JSON can be accessed via --json flag
    let format = if json_mode {
        commands::release::OutputFormat::Json
    } else {
        commands::release::OutputFormat::Human
    };
    match commands::release::execute_release_publish(&path, dry_run, format) {
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

async fn execute_release_binaries_safe(
    opts: commands::release::ReleaseBinariesOptions,
    json_mode: bool,
) -> Result<(), CliError> {
    match commands::release::execute_release_binaries(opts).await {
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
            format!("Release binaries failed: {e}"),
            "Check that binaries are built and artifacts directory exists",
        )),
    }
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
        use cli::{Commands, OutputFormat};

        // Test Version command conversion
        let cli_command = Commands::Version {
            output_format: OutputFormat::Text,
        };
        let command: Command = cli_command.into_command(None);
        match command {
            Command::Version { format } => assert_eq!(format, "text"),
            _ => panic!("Expected Command::Version"),
        }
    }
}
