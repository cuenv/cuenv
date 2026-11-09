//! State management for hook execution tracking

mod bounded_json;
mod broker;
mod coordinator;
mod execution;
mod manager;
mod persisted;

pub use bounded_json::serialize_bounded;
pub use broker::validate_snapshot;
pub use coordinator::{
    HookChildStart, HookChildUpdate, HookRecoveryOutcome, HookReservation, HookReservationPhase,
    HookRunIdentity, HookRunJoin, HookRunStart, HookRunUpdate, HookStartMode, HookWorkerStart,
    MAX_SUPERVISOR_PAYLOAD, SupervisorPayloadRef,
};
pub use execution::{
    HookExecutionIdentity, HookExecutionState, compute_execution_hash, compute_instance_hash,
};
pub use manager::StateManager;

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
