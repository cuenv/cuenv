//! Hook execution system for cuenv
//!
//! This crate provides the core hook execution functionality, including:
//! - Hook definition and serialization
//! - Background hook execution with state tracking
//! - Approval-based security system
//! - Shell integration support
//!
//! # Overview
//!
//! The hooks system enables environment-triggered command execution:
//! - `onEnter` hooks run when entering a directory with a cuenv configuration
//! - `onExit` hooks run when leaving such a directory
//! - `prePush` hooks run before git push operations
//!
//! # Security
//!
//! All hook configurations must be approved by the user before execution.
//! This prevents malicious configurations from executing arbitrary commands.
//! In CI environments, hooks are auto-approved since the environment is
//! assumed to be already secured.

mod approval;
mod error;
mod executor;
mod state;
mod types;

// Re-export error types at crate root
pub use error::{Error, Result};

// Re-export types
pub use types::{ExecutionStatus, Hook, HookExecutionConfig, HookResult, Hooks};

// Re-export state management
pub use state::{
    compute_execution_hash, compute_instance_hash, HookExecutionState, StateManager,
};

// Re-export executor
pub use executor::{execute_hooks, HookExecutor};

// Re-export approval management
pub use approval::{
    check_approval_status, compute_approval_hash, compute_directory_key, is_ci, ApprovalManager,
    ApprovalRecord, ApprovalStatus, ConfigSummary,
};
