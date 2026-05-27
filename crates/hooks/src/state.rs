//! State management for hook execution tracking

mod cleanup;
mod execution;
mod manager;
mod markers;

pub use execution::{HookExecutionState, compute_execution_hash, compute_instance_hash};
pub use manager::StateManager;

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
