mod application;
mod command_conversion;
mod commands;
mod error;
mod output;
mod subcommands;

pub use commands::Commands;
pub use error::{
    CliError, EXIT_CANCELLED, EXIT_CLI, EXIT_EVAL, EXIT_INFRASTRUCTURE, EXIT_INTERRUPTED,
    EXIT_LOCKED, EXIT_OK, InfrastructureFailureKind, LockStatus, error_code_for, error_envelope,
    error_report_text, exit_code_for, render_error,
};
pub use output::{ErrorEnvelope, OkEnvelope, OutputFormat};
pub use subcommands::{
    ChangesetCommands, EnvCommands, InfrastructureCommands, InfrastructureProviderCommands,
    InfrastructureStateCommands, OciCommands, ReleaseCommands, RuntimeCommands, SecretsCommands,
    SecretsProvider, ShellCommands, ShellType, StatusFormat, SyncCommands, ToolsCommands,
};

pub use application::{Cli, generate_completions, parse, try_complete};

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
