//! Hook execution system for cuenv
//!
//! This module provides the core hook execution functionality, including:
//! - Hook definition and serialization
//! - Background hook execution with state tracking  
//! - Approval-based security system
//! - Shell integration support

pub mod approval;
pub mod executor;
pub mod state;
pub mod types;

pub use approval::{ApprovalManager, ApprovalStatus};
pub use executor::{HookExecutor, execute_hooks};
pub use state::{HookExecutionState, StateManager};
pub use types::*;
