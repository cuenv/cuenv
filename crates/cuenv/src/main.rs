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

// expect_used is allowed for infallible operations like writing to strings
#![allow(clippy::expect_used)]

// Import everything from the library
use crossterm::ExecutableCommand;
use cuenv::cli::{
    self, CliError, EXIT_OK, OkEnvelope, OutputFormat, exit_code_for, parse, render_error,
};
use cuenv::commands::{self, Command, CommandExecutor};
use cuenv::coordinator;
use cuenv::tracing::{self, Level, TracingConfig, TracingFormat};
use cuenv_events::renderers::{CliRenderer, JsonRenderer};
use cuenv_hooks::{
    ExecutionStatus, Hook, HookExecutionConfig, HookExecutionState, StateManager, execute_hooks,
};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::instrument;

/// Exit code for SIGINT (128 + signal number 2)
const EXIT_SIGINT: i32 = 130;

/// LLM context content (llms.txt + CUE schemas concatenated at build time)
const LLMS_CONTENT: &str = include_str!(concat!(env!("OUT_DIR"), "/llms-full.txt"));

/// Main entry point - determines sync vs async execution path
fn main() {
    // Install the rustls crypto provider before any HTTP clients are created.
    // Required because reqwest uses `rustls-no-provider` to avoid bundling aws-lc-sys.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install default rustls crypto provider");

    // Set up error handling first
    // NOTE: Using eprintln! in panic hook is intentional - tracing infrastructure
    // may be corrupted during a panic, so we use the most reliable output method.
    // Restore terminal state before printing so the panic output isn't garbled
    // by leftover raw-mode / alt-screen state from a TUI that didn't drop cleanly.
    #[allow(clippy::print_stderr)]
    std::panic::set_hook(Box::new(|panic_info| {
        cleanup_terminal();
        eprintln!("Application panicked: {panic_info}");
        eprintln!("Internal error occurred. Run with RUST_LOG=debug for more information.");
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
            | cli::Commands::Build { .. }
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
        #[allow(clippy::print_stdout)] // LLMS_CONTENT is static documentation, no secrets
        {
            print!("{LLMS_CONTENT}");
        }
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

/// Execute commands synchronously (no tokio runtime)
#[allow(clippy::too_many_lines)] // Command dispatcher naturally has many cases
fn execute_sync_command(command: Command, json_format: cli::OutputFormat) -> Result<(), CliError> {
    ensure_command_module_compatibility(&command)?;

    match command {
        Command::Version { format: _ } => {
            let version_info = commands::version::get_version_info();
            #[allow(clippy::print_stdout)] // Version info contains no secrets
            {
                println!("{version_info}");
            }
            Ok(())
        }

        Command::Info {
            path,
            package,
            meta,
        } => {
            let options = commands::info::InfoOptions {
                path: path.as_deref(),
                package: &package,
                json_output: json_format.is_json(),
                with_meta: meta,
            };
            match commands::info::execute_info(options) {
                Ok(output) => {
                    cuenv_events::print_redacted(&output);
                    Ok(())
                }
                Err(e) => Err(CliError::eval_with_help(
                    format!("Info command failed: {e}"),
                    "Check that you are in a CUE module with valid env.cue files",
                )),
            }
        }

        Command::ShellInit { shell } => execute_shell_init_command_safe(shell, json_format),

        Command::EnvStatus {
            path,
            package,
            wait: false,
            format,
            ..
        } => match commands::hooks::execute_env_status_sync(&path, &package, format) {
            Ok(output) => {
                if json_format.is_json() {
                    let envelope = OkEnvelope::new(serde_json::json!({
                        "status": output
                    }));
                    match serde_json::to_string(&envelope) {
                        Ok(json) => cuenv_events::println_redacted(&json),
                        Err(e) => {
                            return Err(CliError::other(format!("JSON serialization failed: {e}")));
                        }
                    }
                } else {
                    cuenv_events::println_redacted(&output);
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
                .enable_all()
                .build()
                .map_err(|e| CliError::other(format!("Runtime error: {e}")))?;

            let executor = create_executor(&package);
            rt.block_on(async {
                match commands::env::execute_env_print(
                    &path,
                    &format,
                    environment.as_deref(),
                    &executor,
                )
                .await
                {
                    Ok(result) => {
                        cuenv_events::println_redacted(&result);
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
                .enable_all()
                .build()
                .map_err(|e| CliError::other(format!("Runtime error: {e}")))?;

            let executor = create_executor(&package);
            rt.block_on(async {
                match commands::env::execute_env_list(&path, &format, &executor).await {
                    Ok(result) => {
                        cuenv_events::println_redacted(&result);
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
                if json_format.is_json() {
                    let envelope = OkEnvelope::new(serde_json::json!({ "message": output }));
                    match serde_json::to_string(&envelope) {
                        Ok(json) => cuenv_events::println_redacted(&json),
                        Err(e) => {
                            return Err(CliError::other(format!("JSON serialization failed: {e}")));
                        }
                    }
                } else {
                    cuenv_events::println_redacted(&output);
                }
                Ok(())
            }
            Err(e) => Err(CliError::eval_with_help(
                format!("Changeset add failed: {e}"),
                "Check package names and bump types (major, minor, patch)",
            )),
        },

        Command::ChangesetStatus { path, json } => {
            let merged_format = OutputFormat::from_json_flag(json || json_format.is_json());
            match commands::release::execute_changeset_status_with_format(&path, merged_format) {
                Ok(output) => {
                    cuenv_events::println_redacted(&output);
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
                    if json_format.is_json() {
                        let envelope = OkEnvelope::new(serde_json::json!({ "message": output }));
                        match serde_json::to_string(&envelope) {
                            Ok(json) => cuenv_events::println_redacted(&json),
                            Err(e) => {
                                return Err(CliError::other(format!(
                                    "JSON serialization failed: {e}"
                                )));
                            }
                        }
                    } else {
                        cuenv_events::println_redacted(&output);
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
                    if json_format.is_json() {
                        let envelope = OkEnvelope::new(serde_json::json!({ "result": output }));
                        match serde_json::to_string(&envelope) {
                            Ok(json) => cuenv_events::println_redacted(&json),
                            Err(e) => {
                                return Err(CliError::other(format!(
                                    "JSON serialization failed: {e}"
                                )));
                            }
                        }
                    } else {
                        cuenv_events::println_redacted(&output);
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
                    if json_format.is_json() {
                        let envelope = OkEnvelope::new(serde_json::json!({ "result": output }));
                        match serde_json::to_string(&envelope) {
                            Ok(json) => cuenv_events::println_redacted(&json),
                            Err(e) => {
                                return Err(CliError::other(format!(
                                    "JSON serialization failed: {e}"
                                )));
                            }
                        }
                    } else {
                        cuenv_events::println_redacted(&output);
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
            let format = if json_format.is_json() {
                commands::release::OutputFormat::Json
            } else {
                commands::release::OutputFormat::Human
            };
            match commands::release::execute_release_publish(&path, dry_run, format) {
                Ok(output) => {
                    if json_format.is_json() {
                        let envelope = OkEnvelope::new(serde_json::json!({ "result": output }));
                        match serde_json::to_string(&envelope) {
                            Ok(json) => cuenv_events::println_redacted(&json),
                            Err(e) => {
                                return Err(CliError::other(format!(
                                    "JSON serialization failed: {e}"
                                )));
                            }
                        }
                    } else {
                        cuenv_events::println_redacted(&output);
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

        Command::Fmt {
            path,
            package,
            fix,
            only,
        } => match commands::fmt::execute_fmt(&path, &package, fix, only.as_deref()) {
            Ok(output) => {
                cuenv_events::println_redacted(&output);
                Ok(())
            }
            Err(e) => Err(CliError::eval_with_help(
                format!("Format failed: {e}"),
                "Check your formatters configuration in env.cue",
            )),
        },

        Command::Export {
            shell,
            path,
            package,
        } => {
            // Try sync fast path first (handles no-env-cue, running, failed states)
            match commands::export::execute_export_sync(shell.as_deref(), &path, &package) {
                Ok(Some(output)) => {
                    // Fast path succeeded - output directly (may contain env vars)
                    cuenv_events::print_redacted(&output);
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
                        match commands::export::execute_export(
                            shell.as_deref(),
                            &path,
                            &package,
                            None,
                        )
                        .await
                        {
                            Ok(result) => {
                                cuenv_events::print_redacted(&result);
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
        #[allow(clippy::print_stdout)] // LLMS_CONTENT is static documentation, no secrets
        {
            print!("{LLMS_CONTENT}");
        }
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
    json_format: cli::OutputFormat,
    executor: &CommandExecutor,
) -> Result<(), CliError> {
    ensure_command_module_compatibility(&command)?;

    // Special commands that bypass the executor (they don't fit the event pattern)
    match &command {
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
            return commands::tools::execute_tools_activate();
        }
        Command::ToolsList => {
            return commands::tools::execute_tools_list();
        }
        Command::Build {
            path,
            package,
            names,
            labels,
        } => {
            let options = commands::build::BuildOptions {
                path: path.clone(),
                package: package.clone(),
                names: names.clone(),
                labels: labels.clone(),
            };
            return commands::build::execute_build(&options, executor)
                .map_err(|e| CliError::eval(format!("Build command failed: {e}")));
        }
        Command::Up {
            path,
            package,
            services,
            labels,
            environment,
        } => {
            let options = commands::up::UpOptions {
                path: path.clone(),
                package: package.clone(),
                services: services.clone(),
                labels: labels.clone(),
                environment: environment.clone(),
            };
            return commands::up::execute_up(&options, executor)
                .await
                .map_err(|e| CliError::eval(format!("Up command failed: {e}")));
        }
        Command::Down {
            path,
            package,
            services,
        } => {
            let options = commands::down::DownOptions {
                path: path.clone(),
                package: package.clone(),
                services: services.clone(),
            };
            return commands::down::execute_down(&options)
                .map(|_| ())
                .map_err(|e| CliError::eval(format!("Down command failed: {e}")));
        }
        Command::Logs {
            path,
            package,
            services,
            follow,
            lines,
        } => {
            let options = commands::logs::LogsOptions {
                path: path.clone(),
                package: package.clone(),
                services: services.clone(),
                follow: *follow,
                lines: *lines,
            };
            return commands::logs::execute_logs(&options)
                .map(|_| ())
                .map_err(|e| CliError::eval(format!("Logs command failed: {e}")));
        }
        Command::Ps {
            path,
            package,
            output_format,
        } => {
            let options = commands::ps::PsOptions {
                path: path.clone(),
                package: package.clone(),
                output_format: output_format.clone(),
            };
            return commands::ps::execute_ps(&options)
                .map(|_| ())
                .map_err(|e| CliError::eval(format!("Ps command failed: {e}")));
        }
        Command::Restart {
            path,
            package,
            services,
        } => {
            let options = commands::restart::RestartOptions {
                path: path.clone(),
                package: package.clone(),
                services: services.clone(),
            };
            return commands::restart::execute_restart(&options)
                .map(|_| ())
                .map_err(|e| CliError::eval(format!("Restart command failed: {e}")));
        }
        // Info command needs special handling for json_format and output
        Command::Info {
            path,
            package,
            meta,
        } => {
            let options = commands::info::InfoOptions {
                path: path.as_deref(),
                package,
                json_output: json_format.is_json(),
                with_meta: *meta,
            };
            return match commands::info::execute_info(options) {
                Ok(output) => {
                    cuenv_events::print_redacted(&output);
                    Ok(())
                }
                Err(e) => Err(CliError::eval_with_help(
                    format!("Info command failed: {e}"),
                    "Check that you are in a CUE module with valid env.cue files",
                )),
            };
        }
        // Changeset commands need special handling for json_format
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
                json_format,
            )
            .await;
        }
        Command::ChangesetStatus { path, json } => {
            let merged_format = OutputFormat::from_json_flag(*json || json_format.is_json());
            return execute_changeset_status_safe(path.clone(), merged_format).await;
        }
        Command::ChangesetFromCommits { path, since } => {
            return execute_changeset_from_commits_safe(path.clone(), since.clone(), json_format)
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
                json_format,
            )
            .await;
        }
        Command::ReleaseVersion { path, dry_run } => {
            return execute_release_version_safe(path.clone(), *dry_run, json_format).await;
        }
        Command::ReleasePublish { path, dry_run } => {
            return execute_release_publish_safe(path.clone(), *dry_run, json_format).await;
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

            return execute_release_binaries_safe(opts, json_format).await;
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
    json_format: cli::OutputFormat,
) -> Result<(), CliError> {
    let output = commands::hooks::execute_shell_init(shell);

    if json_format.is_json() {
        let envelope = OkEnvelope::new(serde_json::json!({
            "script": output
        }));
        match serde_json::to_string(&envelope) {
            Ok(json) => cuenv_events::println_redacted(&json),
            Err(e) => return Err(CliError::other(format!("JSON serialization failed: {e}"))),
        }
    } else {
        cuenv_events::println_redacted(&output);
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
/// and outputs PATH modifications to stdout (to be sourced by the hook system).
///
/// This command is typically invoked by the `#OCIActivate` hook defined in
/// `schema/oci.cue` to add OCI-managed binaries to the PATH.
async fn run_oci_activate() -> Result<(), CliError> {
    use cuenv_tools_oci::{OciCache, OciClient, current_platform};

    // Find the lockfile by walking up from current directory
    let lockfile_path = find_lockfile().ok_or_else(|| {
        CliError::config_with_help(
            "No cuenv.lock found",
            "Run 'cuenv sync lock' to create the lockfile",
        )
    })?;

    // Load the lockfile
    let lockfile = cuenv_core::lockfile::Lockfile::load(&lockfile_path)
        .map_err(|e| CliError::other(format!("Failed to load lockfile: {e}")))?
        .ok_or_else(|| {
            CliError::config_with_help(
                "Lockfile is empty",
                "Run 'cuenv sync lock' to populate the lockfile",
            )
        })?;

    // Initialize OCI client and cache
    let client = OciClient::new();
    let cache = OciCache::default();
    cache
        .ensure_dirs()
        .map_err(|e| CliError::other(format!("Failed to create cache directories: {e}")))?;

    let platform = current_platform();
    let bin_dirs = activate_lockfile_artifacts(&lockfile, &client, &cache, &platform).await?;

    // Output PATH modification through the events system so renderers can
    // capture / forward it. The shell-eval contract of `#OCIActivate` (see
    // `schema/oci.cue`) requires that the hook stdout be a sourceable
    // `export PATH=...` line; `println_redacted` is the existing
    // pass-through mechanism in `cuenv_events`.
    if !bin_dirs.is_empty() {
        let mut path_additions: Vec<String> =
            bin_dirs.iter().map(|p| p.display().to_string()).collect();
        // Sort for deterministic ordering across runs.
        path_additions.sort();
        cuenv_events::println_redacted(&format!(
            "export PATH=\"{}:$PATH\"",
            path_additions.join(":")
        ));
    }

    Ok(())
}

/// Activate every artifact in the lockfile that matches `platform`, returning
/// the set of `bin/` directories that should be prepended to PATH.
///
/// Extracted from [`run_oci_activate`] so it can be unit-tested without
/// depending on the user's home directory (`OciCache::default()`) or
/// `find_lockfile()`.
async fn activate_lockfile_artifacts(
    lockfile: &cuenv_core::lockfile::Lockfile,
    client: &cuenv_tools_oci::OciClient,
    cache: &cuenv_tools_oci::OciCache,
    platform: &cuenv_tools_oci::Platform,
) -> Result<std::collections::HashSet<PathBuf>, CliError> {
    use cuenv_core::lockfile::ArtifactKind;
    use cuenv_tools_oci::extract_from_layers;
    use std::collections::HashSet;

    let platform_str = platform.to_string();
    let mut bin_dirs: HashSet<PathBuf> = HashSet::new();

    for artifact in &lockfile.artifacts {
        let Some(platform_data) = artifact.platforms.get(&platform_str) else {
            continue;
        };

        let ArtifactKind::Image { image, extract } = &artifact.kind;

        if extract.is_empty() {
            ::tracing::warn!(
                image = %image,
                "OCI image has no extract entries in the lockfile; skipping activation. \
                 Add `extract: [{{ path: ... }}]` to the image in your CUE and re-run `cuenv sync lock`."
            );
            continue;
        }

        let digest = &platform_data.digest;

        // Fast path: every extract entry is already in the binary cache.
        let all_cached = extract
            .iter()
            .all(|entry| cache.get_binary(digest, &entry.binary_name()).is_some());
        if all_cached {
            for entry in extract {
                if let Some(cached_path) = cache.get_binary(digest, &entry.binary_name())
                    && let Some(parent) = cached_path.parent()
                {
                    bin_dirs.insert(parent.to_path_buf());
                }
            }
            continue;
        }

        // Need to pull layers and extract any missing binaries.
        let resolved = client
            .resolve_digest(image, platform)
            .await
            .map_err(|e| CliError::other(format!("Failed to resolve '{}': {}", image, e)))?;

        let layer_paths = client.pull_layers(&resolved, cache).await.map_err(|e| {
            CliError::other(format!("Failed to pull layers for '{}': {}", image, e))
        })?;

        if layer_paths.is_empty() {
            ::tracing::warn!(
                image = %image,
                "OCI image has no layers to extract; skipping activation"
            );
            continue;
        }

        for entry in extract {
            let binary_name = entry.binary_name();
            let dest = cache.binary_path(digest, &binary_name);

            if !dest.exists() {
                extract_from_layers(&layer_paths, &entry.path, &dest).map_err(|e| {
                    CliError::other(format!(
                        "Failed to extract '{}' from '{}': {}",
                        entry.path, image, e
                    ))
                })?;
            }

            if !dest.exists() {
                return Err(CliError::other(format!(
                    "Extraction of '{}' from '{}' did not produce a file at {}",
                    entry.path,
                    image,
                    dest.display()
                )));
            }

            if let Some(parent) = dest.parent() {
                bin_dirs.insert(parent.to_path_buf());
            }
        }
    }

    Ok(bin_dirs)
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

struct HookSupervisorArgs {
    directory_path: PathBuf,
    instance_hash: String,
    hooks_file: PathBuf,
    config_file: PathBuf,
}

struct HookSupervisorFiles {
    hooks: Vec<Hook>,
    config: HookExecutionConfig,
}

struct HookSupervisorState {
    manager: StateManager,
    pid_file: PathBuf,
}

struct HookSupervisorRun<'a> {
    files: HookSupervisorFiles,
    args: &'a HookSupervisorArgs,
    state_context: &'a HookSupervisorState,
    state: &'a mut HookExecutionState,
}

/// Run as a hook supervisor process.
async fn run_hook_supervisor(args: Vec<String>) -> Result<(), CliError> {
    let supervisor_args = parse_hook_supervisor_args(&args);

    init_hook_supervisor_logging();
    enter_hook_supervisor_directory(&supervisor_args.directory_path)?;
    log_hook_supervisor_start(&supervisor_args, &args);

    let files = load_hook_supervisor_files(&supervisor_args)?;
    let state_context = prepare_hook_supervisor_state(&supervisor_args, &files.config)?;
    let mut state =
        load_hook_supervisor_state(&state_context, &supervisor_args.instance_hash).await?;

    execute_hook_supervisor_run(HookSupervisorRun {
        files,
        args: &supervisor_args,
        state_context: &state_context,
        state: &mut state,
    })
    .await?;

    std::fs::remove_file(&state_context.pid_file).ok();
    cuenv_events::emit_supervisor_log!("supervisor", "Completed successfully");
    Ok(())
}

fn parse_hook_supervisor_args(args: &[String]) -> HookSupervisorArgs {
    let mut parsed = HookSupervisorArgs {
        directory_path: PathBuf::new(),
        instance_hash: String::new(),
        hooks_file: PathBuf::new(),
        config_file: PathBuf::new(),
    };

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--directory" => {
                parsed.directory_path = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--instance-hash" => {
                parsed.instance_hash.clone_from(&args[i + 1]);
                i += 2;
            }
            "--config-hash" => {
                i += 2;
            }
            "--hooks-file" => {
                parsed.hooks_file = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--config-file" => {
                parsed.config_file = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            _ => i += 1,
        }
    }

    parsed
}

fn init_hook_supervisor_logging() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init();
}

fn enter_hook_supervisor_directory(directory_path: &PathBuf) -> Result<(), CliError> {
    if let Err(e) = std::env::set_current_dir(directory_path) {
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

    Ok(())
}

fn log_hook_supervisor_start(supervisor_args: &HookSupervisorArgs, raw_args: &[String]) {
    cuenv_events::emit_supervisor_log!("supervisor", format!("Starting with args: {raw_args:?}"));
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Directory: {}", supervisor_args.directory_path.display())
    );
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Instance hash: {}", supervisor_args.instance_hash)
    );
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Hooks file: {}", supervisor_args.hooks_file.display())
    );
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Config file: {}", supervisor_args.config_file.display())
    );
}

fn load_hook_supervisor_files(
    supervisor_args: &HookSupervisorArgs,
) -> Result<HookSupervisorFiles, CliError> {
    let hooks_json = std::fs::read_to_string(&supervisor_args.hooks_file)
        .map_err(|e| CliError::other(format!("Failed to read hooks file: {e}")))?;
    let config_json = std::fs::read_to_string(&supervisor_args.config_file)
        .map_err(|e| CliError::other(format!("Failed to read config file: {e}")))?;

    let hooks = serde_json::from_str(&hooks_json)
        .map_err(|e| CliError::other(format!("Failed to deserialize hooks: {e}")))?;
    let config = serde_json::from_str(&config_json)
        .map_err(|e| CliError::other(format!("Failed to deserialize config: {e}")))?;

    std::fs::remove_file(&supervisor_args.hooks_file).ok();
    std::fs::remove_file(&supervisor_args.config_file).ok();

    Ok(HookSupervisorFiles { hooks, config })
}

fn prepare_hook_supervisor_state(
    supervisor_args: &HookSupervisorArgs,
    config: &HookExecutionConfig,
) -> Result<HookSupervisorState, CliError> {
    let state_dir = match config.state_dir.clone() {
        Some(dir) => dir,
        None => StateManager::default_state_dir()
            .map_err(|e| CliError::other(format!("failed to get default state dir: {e}")))?,
    };
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Using state dir: {}", state_dir.display())
    );

    let manager = StateManager::new(state_dir);
    let state_file = manager.get_state_file_path(&supervisor_args.instance_hash);
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Looking for state file: {}", state_file.display())
    );

    let pid_file = state_file.with_extension("pid");
    std::fs::write(&pid_file, format!("{}", std::process::id()))
        .map_err(|e| CliError::other(format!("Failed to write PID file: {e}")))?;

    Ok(HookSupervisorState { manager, pid_file })
}

async fn load_hook_supervisor_state(
    state_context: &HookSupervisorState,
    instance_hash: &str,
) -> Result<HookExecutionState, CliError> {
    state_context
        .manager
        .load_state(instance_hash)
        .await
        .map_err(|e| CliError::other(format!("Failed to load state: {e}")))?
        .ok_or_else(|| CliError::other("State not found for supervisor"))
}

async fn execute_hook_supervisor_run(run: HookSupervisorRun<'_>) -> Result<(), CliError> {
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!(
            "Executing {} hooks for directory: {}",
            run.files.hooks.len(),
            run.args.directory_path.display()
        )
    );

    let result = execute_hooks(
        run.files.hooks,
        &run.args.directory_path,
        &run.files.config,
        &run.state_context.manager,
        run.state,
    )
    .await;

    if let Err(e) = result {
        let error = e.to_string();
        save_hook_supervisor_failure(run.state_context, run.state, &error).await?;
        return Err(CliError::other(format!("Hook execution failed: {error}")));
    }

    save_hook_supervisor_success(run.state_context, run.state).await
}

async fn save_hook_supervisor_failure(
    state_context: &HookSupervisorState,
    state: &mut HookExecutionState,
    error: &str,
) -> Result<(), CliError> {
    cuenv_events::emit_supervisor_log!("supervisor", format!("Hook execution failed: {error}"));
    state.status = ExecutionStatus::Failed;
    state.error_message = Some(error.to_string());
    state.finished_at = Some(chrono::Utc::now());
    state_context
        .manager
        .save_state(state)
        .await
        .map_err(|e| CliError::other(format!("Failed to save error state: {e}")))
}

async fn save_hook_supervisor_success(
    state_context: &HookSupervisorState,
    state: &HookExecutionState,
) -> Result<(), CliError> {
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!(
            "Saving final state with {} environment variables",
            state.environment_vars.len()
        )
    );
    state_context
        .manager
        .save_state(state)
        .await
        .map_err(|e| CliError::other(format!("Failed to save final state: {e}")))
}

/// Execute changeset add command safely
#[instrument(name = "cuenv_execute_changeset_add_safe")]
async fn execute_changeset_add_safe(
    path: String,
    summary: Option<String>,
    description: Option<String>,
    packages: Vec<(String, String)>,
    json_format: cli::OutputFormat,
) -> Result<(), CliError> {
    match commands::release::execute_changeset_add(
        &path,
        &packages,
        summary.as_deref(),
        description.as_deref(),
    ) {
        Ok(output) => {
            if json_format.is_json() {
                let envelope = OkEnvelope::new(serde_json::json!({
                    "message": output
                }));
                match serde_json::to_string(&envelope) {
                    Ok(json) => cuenv_events::println_redacted(&json),
                    Err(e) => {
                        return Err(CliError::other(format!("JSON serialization failed: {e}")));
                    }
                }
            } else {
                cuenv_events::println_redacted(&output);
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
async fn execute_changeset_status_safe(
    path: String,
    json_format: cli::OutputFormat,
) -> Result<(), CliError> {
    // Use the format-aware function that returns proper JSON structure
    match commands::release::execute_changeset_status_with_format(&path, json_format) {
        Ok(output) => {
            cuenv_events::println_redacted(&output);
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
    json_format: cli::OutputFormat,
) -> Result<(), CliError> {
    match commands::release::execute_changeset_from_commits(&path, since.as_deref()) {
        Ok(output) => {
            if json_format.is_json() {
                let envelope = OkEnvelope::new(serde_json::json!({
                    "message": output
                }));
                match serde_json::to_string(&envelope) {
                    Ok(json) => cuenv_events::println_redacted(&json),
                    Err(e) => {
                        return Err(CliError::other(format!("JSON serialization failed: {e}")));
                    }
                }
            } else {
                cuenv_events::println_redacted(&output);
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
    dry_run: cuenv_core::DryRun,
    branch: String,
    no_pr: bool,
    json_format: cli::OutputFormat,
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
            if json_format.is_json() {
                let envelope = OkEnvelope::new(serde_json::json!({
                    "result": output
                }));
                match serde_json::to_string(&envelope) {
                    Ok(json) => cuenv_events::println_redacted(&json),
                    Err(e) => {
                        return Err(CliError::other(format!("JSON serialization failed: {e}")));
                    }
                }
            } else {
                cuenv_events::println_redacted(&output);
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
    dry_run: cuenv_core::DryRun,
    json_format: cli::OutputFormat,
) -> Result<(), CliError> {
    match commands::release::execute_release_version(&path, dry_run) {
        Ok(output) => {
            if json_format.is_json() {
                let envelope = OkEnvelope::new(serde_json::json!({
                    "result": output
                }));
                match serde_json::to_string(&envelope) {
                    Ok(json) => cuenv_events::println_redacted(&json),
                    Err(e) => {
                        return Err(CliError::other(format!("JSON serialization failed: {e}")));
                    }
                }
            } else {
                cuenv_events::println_redacted(&output);
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
    dry_run: cuenv_core::DryRun,
    json_format: cli::OutputFormat,
) -> Result<(), CliError> {
    // Use Human format for CLI, JSON can be accessed via --json flag
    let format = if json_format.is_json() {
        commands::release::OutputFormat::Json
    } else {
        commands::release::OutputFormat::Human
    };
    match commands::release::execute_release_publish(&path, dry_run, format) {
        Ok(output) => {
            if json_format.is_json() {
                let envelope = OkEnvelope::new(serde_json::json!({
                    "result": output
                }));
                match serde_json::to_string(&envelope) {
                    Ok(json) => cuenv_events::println_redacted(&json),
                    Err(e) => {
                        return Err(CliError::other(format!("JSON serialization failed: {e}")));
                    }
                }
            } else {
                cuenv_events::println_redacted(&output);
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
    json_format: cli::OutputFormat,
) -> Result<(), CliError> {
    match commands::release::execute_release_binaries(opts).await {
        Ok(output) => {
            if json_format.is_json() {
                let envelope = OkEnvelope::new(serde_json::json!({
                    "result": output
                }));
                match serde_json::to_string(&envelope) {
                    Ok(json) => cuenv_events::println_redacted(&json),
                    Err(e) => {
                        return Err(CliError::other(format!("JSON serialization failed: {e}")));
                    }
                }
            } else {
                cuenv_events::println_redacted(&output);
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
#[path = "main_tests.rs"]
mod tests;
