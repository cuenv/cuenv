//! cuenv - CUE-powered environment management library
//!
//! This crate ships the `cuenv` binary. Command dispatch lives in
//! [`commands`]; generated-file synchronization is handled by the
//! [`SyncProvider`](commands::sync::SyncProvider) implementations registered
//! in the [`SyncRegistry`](commands::sync::SyncRegistry) (see
//! [`commands::sync::default_registry`]).

/// CLI argument parsing and exit codes.
pub mod cli;
/// Command implementations (task, env, sync, etc.).
pub mod commands;
/// Shell completion generation.
pub mod completions;
/// Multi-process event coordination.
pub mod coordinator;
/// Event handling and routing.
pub mod events;
/// Performance measurement utilities.
pub mod performance;
/// Provider detection and rules-file evaluation helpers.
pub mod providers;

pub mod secret_registry;
/// Tracing and logging configuration.
pub mod tracing;
/// Terminal UI components.
pub mod tui;

pub use cuenv_core::Result;

/// Exit code for SIGINT (128 + signal number 2)
pub const EXIT_SIGINT: i32 = 130;

/// LLM context content (llms.txt + CUE schemas concatenated at build time)
pub const LLMS_CONTENT: &str = include_str!(concat!(env!("OUT_DIR"), "/llms-full.txt"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_sigint_constant() {
        // SIGINT exit code is 128 + 2 = 130
        assert_eq!(EXIT_SIGINT, 130);
    }

    #[test]
    fn test_llms_content_contains_project_context() {
        assert!(
            LLMS_CONTENT.contains("cuenv"),
            "generated LLM content should include project context"
        );
    }
}
