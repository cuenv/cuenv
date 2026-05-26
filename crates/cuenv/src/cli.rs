mod command_conversion;
mod commands;
mod error;
mod output;
mod subcommands;

use clap::{CommandFactory, Parser};
use clap_complete::Shell;

pub use commands::Commands;
pub use error::{CliError, EXIT_CLI, EXIT_EVAL, EXIT_OK, exit_code_for, render_error};
pub use output::{ErrorEnvelope, OkEnvelope, OutputFormat};
pub use subcommands::{
    ChangesetCommands, EnvCommands, OciCommands, ReleaseCommands, RuntimeCommands, SecretsCommands,
    SecretsProvider, ShellCommands, ShellType, StatusFormat, SyncCommands, ToolsCommands,
};

/// Main CLI entry point for cuenv.
///
/// A modern application build toolchain with typed environments and CUE-powered task orchestration.
#[derive(Parser, Debug)]
#[command(name = "cuenv")]
#[command(
    about = "A modern application build toolchain with typed environments and CUE-powered task orchestration"
)]
#[command(long_about = None)]
#[command(version)]
pub struct Cli {
    /// The subcommand to execute.
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Logging verbosity level.
    #[arg(
        short = 'L',
        long,
        global = true,
        help = "Set logging level",
        default_value = "warn",
        value_enum
    )]
    pub level: crate::tracing::LogLevel,

    /// Emit JSON envelope regardless of format.
    #[arg(long, global = true, help = "Emit JSON envelope regardless of format")]
    pub json: bool,

    /// Environment-specific overrides (e.g., development, production).
    #[arg(
        long = "env",
        short = 'e',
        global = true,
        help = "Apply environment-specific overrides (e.g., development, production)"
    )]
    pub environment: Option<String>,

    /// Print LLM context information (llms.txt).
    #[arg(long, global = true, help = "Print LLM context information (llms.txt)")]
    pub llms: bool,
}

/// Generate shell completions using `clap_complete`'s dynamic completion system
///
/// The binary itself handles completion requests via environment variables.
/// This function outputs instructions for the user to set up completions.
#[allow(clippy::print_stdout)] // Static completion instructions, no secrets
pub fn generate_completions(shell: Shell) {
    let shell_name = match shell {
        Shell::Bash => "bash",
        Shell::Fish => "fish",
        Shell::Zsh => "zsh",
        Shell::Elvish => "elvish",
        Shell::PowerShell => "powershell",
        _ => "unknown",
    };

    // Print instructions for the user
    println!("# cuenv shell completions for {shell_name}");
    println!("#");
    println!("# Add the following to your shell config:");
    println!();

    match shell {
        Shell::Bash => {
            println!(r"source <(COMPLETE=bash cuenv)");
        }
        Shell::Zsh => {
            println!(r"source <(COMPLETE=zsh cuenv)");
        }
        Shell::Fish => {
            println!(r"COMPLETE=fish cuenv | source");
        }
        Shell::Elvish => {
            println!(r"eval (E:COMPLETE=elvish cuenv | slurp)");
        }
        Shell::PowerShell => {
            println!(
                r#"$env:COMPLETE = "powershell"; cuenv | Out-String | Invoke-Expression; Remove-Item Env:\COMPLETE"#
            );
        }
        _ => {
            println!("# Shell not supported for dynamic completions");
        }
    }
}

/// Try to handle a completion request. Returns true if this was a completion request.
///
/// Call this early in `main()` - if it returns true, exit immediately.
pub fn try_complete() -> bool {
    use clap_complete::env::CompleteEnv;

    // Check if COMPLETE env var is set - if so, handle completion and return true
    if std::env::var("COMPLETE").is_ok() {
        CompleteEnv::with_factory(Cli::command).complete();
        return true;
    }
    false
}

/// Parse command line arguments into a CLI structure.
#[must_use]
pub fn parse() -> Cli {
    Cli::parse()
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
