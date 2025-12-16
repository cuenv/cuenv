//! IaC event types and emit macros.
//!
//! This module provides IaC-specific events that integrate with the cuenv-events
//! system. Events are emitted using tracing and can be captured by the
//! `CuenvEventLayer` for rendering in CLI, TUI, or JSON formats.
//!
//! # Event Types
//!
//! - Resource lifecycle events (creating, created, updating, deleting, etc.)
//! - Provider events (starting, configured, stopped)
//! - Plan events (planning, plan ready, applying)
//! - Drift events (drift detected, drift resolved)
//!
//! # Usage
//!
//! ```rust,ignore
//! use cuenv_iac::events::*;
//!
//! // Emit a resource creating event
//! emit_iac_resource_creating!("vpc-main", "aws_vpc", "terraform-provider-aws");
//!
//! // Emit a drift detected event
//! emit_iac_drift_detected!("instance-web", "aws_instance", "modified", 3);
//! ```

use serde::{Deserialize, Serialize};

/// IaC resource lifecycle event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum IacResourceEvent {
    /// Resource creation started.
    Creating {
        /// Resource identifier
        resource_id: String,
        /// Resource type
        resource_type: String,
        /// Provider name
        provider: String,
    },
    /// Resource successfully created.
    Created {
        /// Resource identifier
        resource_id: String,
        /// Resource type
        resource_type: String,
        /// Cloud-assigned ID
        cloud_id: String,
        /// Duration in milliseconds
        duration_ms: u64,
    },
    /// Resource update started.
    Updating {
        /// Resource identifier
        resource_id: String,
        /// Resource type
        resource_type: String,
        /// Attributes being changed
        changed_attributes: Vec<String>,
    },
    /// Resource successfully updated.
    Updated {
        /// Resource identifier
        resource_id: String,
        /// Resource type
        resource_type: String,
        /// Duration in milliseconds
        duration_ms: u64,
    },
    /// Resource deletion started.
    Deleting {
        /// Resource identifier
        resource_id: String,
        /// Resource type
        resource_type: String,
    },
    /// Resource successfully deleted.
    Deleted {
        /// Resource identifier
        resource_id: String,
        /// Resource type
        resource_type: String,
        /// Duration in milliseconds
        duration_ms: u64,
    },
    /// Resource operation failed.
    Failed {
        /// Resource identifier
        resource_id: String,
        /// Resource type
        resource_type: String,
        /// Operation that failed
        operation: String,
        /// Error message
        error: String,
    },
    /// Resource refreshed from cloud.
    Refreshed {
        /// Resource identifier
        resource_id: String,
        /// Resource type
        resource_type: String,
        /// Whether state changed
        changed: bool,
    },
    /// Resource imported.
    Imported {
        /// Resource identifier
        resource_id: String,
        /// Resource type
        resource_type: String,
        /// Imported cloud ID
        cloud_id: String,
    },
}

/// IaC provider event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum IacProviderEvent {
    /// Provider starting.
    Starting {
        /// Provider name
        name: String,
        /// Provider source
        source: String,
    },
    /// Provider started and handshake complete.
    Started {
        /// Provider name
        name: String,
        /// Protocol version
        protocol_version: u32,
        /// gRPC address
        address: String,
    },
    /// Provider configured.
    Configured {
        /// Provider name
        name: String,
    },
    /// Provider stopping.
    Stopping {
        /// Provider name
        name: String,
    },
    /// Provider stopped.
    Stopped {
        /// Provider name
        name: String,
    },
    /// Provider error.
    Error {
        /// Provider name
        name: String,
        /// Error message
        error: String,
    },
}

/// IaC plan event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum IacPlanEvent {
    /// Plan generation started.
    Started {
        /// Number of resources to plan
        resource_count: usize,
    },
    /// Single resource planned.
    ResourcePlanned {
        /// Resource identifier
        resource_id: String,
        /// Action (create, update, destroy, no-op)
        action: String,
        /// Attributes that will change
        changed_attributes: Vec<String>,
    },
    /// Plan generation completed.
    Completed {
        /// Total resources in plan
        total: usize,
        /// Resources to create
        to_create: usize,
        /// Resources to update
        to_update: usize,
        /// Resources to destroy
        to_destroy: usize,
        /// Resources with no changes
        no_op: usize,
    },
    /// Apply started.
    ApplyStarted {
        /// Number of resources to apply
        resource_count: usize,
    },
    /// Apply completed.
    ApplyCompleted {
        /// Whether all resources applied successfully
        success: bool,
        /// Number of successful operations
        successful: usize,
        /// Number of failed operations
        failed: usize,
        /// Duration in milliseconds
        duration_ms: u64,
    },
}

/// IaC drift event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum IacDriftEvent {
    /// Drift check started.
    CheckStarted {
        /// Number of resources to check
        resource_count: usize,
    },
    /// Drift detected on a resource.
    Detected {
        /// Resource identifier
        resource_id: String,
        /// Resource type
        resource_type: String,
        /// Drift type (modified, deleted, created)
        drift_type: String,
        /// Number of drifted attributes
        drifted_attributes: usize,
    },
    /// Drift attribute detail.
    AttributeDrift {
        /// Resource identifier
        resource_id: String,
        /// Attribute path
        attribute: String,
        /// Expected value (truncated)
        expected: String,
        /// Actual value (truncated)
        actual: String,
    },
    /// Drift check completed.
    CheckCompleted {
        /// Total resources checked
        total: usize,
        /// Resources with drift
        drifted: usize,
        /// Duration in milliseconds
        duration_ms: u64,
    },
    /// Drift resolved (resource brought back in sync).
    Resolved {
        /// Resource identifier
        resource_id: String,
    },
}

// ============================================================================
// Emit Macros
// ============================================================================

/// Emit a resource creating event.
#[macro_export]
macro_rules! emit_iac_resource_creating {
    ($resource_id:expr, $resource_type:expr, $provider:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.resource.creating",
            resource_id = %$resource_id,
            resource_type = %$resource_type,
            provider = %$provider,
        )
    };
}

/// Emit a resource created event.
#[macro_export]
macro_rules! emit_iac_resource_created {
    ($resource_id:expr, $resource_type:expr, $cloud_id:expr, $duration_ms:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.resource.created",
            resource_id = %$resource_id,
            resource_type = %$resource_type,
            cloud_id = %$cloud_id,
            duration_ms = $duration_ms,
        )
    };
}

/// Emit a resource updating event.
#[macro_export]
macro_rules! emit_iac_resource_updating {
    ($resource_id:expr, $resource_type:expr, $changed_attributes:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.resource.updating",
            resource_id = %$resource_id,
            resource_type = %$resource_type,
            changed_attributes = ?$changed_attributes,
        )
    };
}

/// Emit a resource updated event.
#[macro_export]
macro_rules! emit_iac_resource_updated {
    ($resource_id:expr, $resource_type:expr, $duration_ms:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.resource.updated",
            resource_id = %$resource_id,
            resource_type = %$resource_type,
            duration_ms = $duration_ms,
        )
    };
}

/// Emit a resource deleting event.
#[macro_export]
macro_rules! emit_iac_resource_deleting {
    ($resource_id:expr, $resource_type:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.resource.deleting",
            resource_id = %$resource_id,
            resource_type = %$resource_type,
        )
    };
}

/// Emit a resource deleted event.
#[macro_export]
macro_rules! emit_iac_resource_deleted {
    ($resource_id:expr, $resource_type:expr, $duration_ms:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.resource.deleted",
            resource_id = %$resource_id,
            resource_type = %$resource_type,
            duration_ms = $duration_ms,
        )
    };
}

/// Emit a resource failed event.
#[macro_export]
macro_rules! emit_iac_resource_failed {
    ($resource_id:expr, $resource_type:expr, $operation:expr, $error:expr) => {
        ::tracing::error!(
            target: "cuenv::iac",
            event_type = "iac.resource.failed",
            resource_id = %$resource_id,
            resource_type = %$resource_type,
            operation = %$operation,
            error = %$error,
        )
    };
}

/// Emit a provider starting event.
#[macro_export]
macro_rules! emit_iac_provider_starting {
    ($name:expr, $source:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.provider.starting",
            provider_name = %$name,
            provider_source = %$source,
        )
    };
}

/// Emit a provider started event.
#[macro_export]
macro_rules! emit_iac_provider_started {
    ($name:expr, $protocol_version:expr, $address:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.provider.started",
            provider_name = %$name,
            protocol_version = $protocol_version,
            address = %$address,
        )
    };
}

/// Emit a provider configured event.
#[macro_export]
macro_rules! emit_iac_provider_configured {
    ($name:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.provider.configured",
            provider_name = %$name,
        )
    };
}

/// Emit a provider stopped event.
#[macro_export]
macro_rules! emit_iac_provider_stopped {
    ($name:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.provider.stopped",
            provider_name = %$name,
        )
    };
}

/// Emit a provider error event.
#[macro_export]
macro_rules! emit_iac_provider_error {
    ($name:expr, $error:expr) => {
        ::tracing::error!(
            target: "cuenv::iac",
            event_type = "iac.provider.error",
            provider_name = %$name,
            error = %$error,
        )
    };
}

/// Emit a plan started event.
#[macro_export]
macro_rules! emit_iac_plan_started {
    ($resource_count:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.plan.started",
            resource_count = $resource_count,
        )
    };
}

/// Emit a resource planned event.
#[macro_export]
macro_rules! emit_iac_resource_planned {
    ($resource_id:expr, $action:expr, $changed_attributes:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.plan.resource_planned",
            resource_id = %$resource_id,
            action = %$action,
            changed_attributes = ?$changed_attributes,
        )
    };
}

/// Emit a plan completed event.
#[macro_export]
macro_rules! emit_iac_plan_completed {
    ($total:expr, $to_create:expr, $to_update:expr, $to_destroy:expr, $no_op:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.plan.completed",
            total = $total,
            to_create = $to_create,
            to_update = $to_update,
            to_destroy = $to_destroy,
            no_op = $no_op,
        )
    };
}

/// Emit an apply started event.
#[macro_export]
macro_rules! emit_iac_apply_started {
    ($resource_count:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.apply.started",
            resource_count = $resource_count,
        )
    };
}

/// Emit an apply completed event.
#[macro_export]
macro_rules! emit_iac_apply_completed {
    ($success:expr, $successful:expr, $failed:expr, $duration_ms:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.apply.completed",
            success = $success,
            successful = $successful,
            failed = $failed,
            duration_ms = $duration_ms,
        )
    };
}

/// Emit a drift check started event.
#[macro_export]
macro_rules! emit_iac_drift_check_started {
    ($resource_count:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.drift.check_started",
            resource_count = $resource_count,
        )
    };
}

/// Emit a drift detected event.
#[macro_export]
macro_rules! emit_iac_drift_detected {
    ($resource_id:expr, $resource_type:expr, $drift_type:expr, $drifted_attributes:expr) => {
        ::tracing::warn!(
            target: "cuenv::iac",
            event_type = "iac.drift.detected",
            resource_id = %$resource_id,
            resource_type = %$resource_type,
            drift_type = %$drift_type,
            drifted_attributes = $drifted_attributes,
        )
    };
}

/// Emit a drift attribute event.
#[macro_export]
macro_rules! emit_iac_drift_attribute {
    ($resource_id:expr, $attribute:expr, $expected:expr, $actual:expr) => {
        ::tracing::warn!(
            target: "cuenv::iac",
            event_type = "iac.drift.attribute",
            resource_id = %$resource_id,
            attribute = %$attribute,
            expected = %$expected,
            actual = %$actual,
        )
    };
}

/// Emit a drift check completed event.
#[macro_export]
macro_rules! emit_iac_drift_check_completed {
    ($total:expr, $drifted:expr, $duration_ms:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.drift.check_completed",
            total = $total,
            drifted = $drifted,
            duration_ms = $duration_ms,
        )
    };
}

/// Emit a drift resolved event.
#[macro_export]
macro_rules! emit_iac_drift_resolved {
    ($resource_id:expr) => {
        ::tracing::info!(
            target: "cuenv::iac",
            event_type = "iac.drift.resolved",
            resource_id = %$resource_id,
        )
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_emit_macros_compile() {
        // Just verify the macros compile - they require tracing to be set up
        // to actually emit events
        let _ = || {
            emit_iac_resource_creating!("vpc-1", "aws_vpc", "aws");
            emit_iac_resource_created!("vpc-1", "aws_vpc", "vpc-12345", 1000_u64);
            emit_iac_resource_updating!("vpc-1", "aws_vpc", vec!["tags"]);
            emit_iac_resource_updated!("vpc-1", "aws_vpc", 500_u64);
            emit_iac_resource_deleting!("vpc-1", "aws_vpc");
            emit_iac_resource_deleted!("vpc-1", "aws_vpc", 200_u64);
            emit_iac_resource_failed!("vpc-1", "aws_vpc", "create", "API error");

            emit_iac_provider_starting!("aws", "hashicorp/aws");
            emit_iac_provider_started!("aws", 6_u32, "127.0.0.1:12345");
            emit_iac_provider_configured!("aws");
            emit_iac_provider_stopped!("aws");
            emit_iac_provider_error!("aws", "connection failed");

            emit_iac_plan_started!(5_usize);
            emit_iac_resource_planned!("vpc-1", "create", vec!["cidr_block"]);
            emit_iac_plan_completed!(5_usize, 2_usize, 1_usize, 1_usize, 1_usize);
            emit_iac_apply_started!(4_usize);
            emit_iac_apply_completed!(true, 4_usize, 0_usize, 5000_u64);

            emit_iac_drift_check_started!(10_usize);
            emit_iac_drift_detected!("vpc-1", "aws_vpc", "modified", 2_usize);
            emit_iac_drift_attribute!("vpc-1", "tags.Name", "expected", "actual");
            emit_iac_drift_check_completed!(10_usize, 1_usize, 1000_u64);
            emit_iac_drift_resolved!("vpc-1");
        };
    }
}
