//! Export command - the heart of cuenv's shell integration.
//!
//! This command is called by the shell hook on every prompt to:
//! 1. Check if environment is ready (instant)
//! 2. Start supervisor if needed (async)
//! 3. Return environment diff for shell evaluation

mod implementation;
mod shell_state;

pub(super) use implementation::{
    EnvironmentContext, format_environment, preflight_shell_transition,
};
pub use implementation::{
    ExportOptions, StaticEnvConsumer, execute_export, execute_export_sync, extract_static_env_vars,
    shell_env_vars,
};
