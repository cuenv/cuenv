//! cuenv command-line application library.
//!
//! The binary entry points use this crate for CLI parsing, command dispatch,
//! event rendering, and the built-in sync providers. Sync dispatch is an
//! internal implementation detail so the public API does not promise an
//! extension surface that the real CLI does not execute.
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
/// CI/CODEOWNERS detection and `.rules.cue` evaluation helpers.
pub mod providers;

pub mod secret_registry;
/// Tracing and logging configuration.
pub mod tracing;
/// Terminal UI components.
pub mod tui;

pub use cuenv_core::Result;

/// LLM context content (llms.txt + CUE schemas concatenated at build time)
pub const LLMS_CONTENT: &str = include_str!(concat!(env!("OUT_DIR"), "/llms-full.txt"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interrupted_exit_code_is_the_sigint_convention() {
        // 128 + SIGINT (2): one constant for every interrupted command.
        assert_eq!(cli::EXIT_INTERRUPTED, 130);
    }

    #[test]
    fn test_llms_content_contains_project_context() {
        assert!(
            LLMS_CONTENT.contains("cuenv"),
            "generated LLM content should include project context"
        );
    }
}
