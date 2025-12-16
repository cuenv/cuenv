//! Terraform provider integration via gRPC.
//!
//! This module handles spawning Terraform provider processes, performing the
//! go-plugin handshake, and communicating via the tfplugin6 gRPC protocol.

mod client;
mod handshake;
mod manager;

pub use client::ProviderClient;
pub use manager::{ProviderConfig, ProviderManager};

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::Result;
use crate::proto::Schema;

/// Information about a Terraform provider.
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    /// Provider name (e.g., "aws", "google", "azurerm")
    pub name: String,

    /// Provider source address (e.g., "hashicorp/aws")
    pub source: String,

    /// Provider version
    pub version: String,

    /// Provider schema
    pub schema: Option<Arc<ProviderSchema>>,
}

/// Provider schema information.
#[derive(Debug, Clone)]
pub struct ProviderSchema {
    /// Provider configuration schema
    pub provider: Option<Schema>,

    /// Resource schemas by type name
    pub resources: HashMap<String, Schema>,

    /// Data source schemas by type name
    pub data_sources: HashMap<String, Schema>,
}

impl ProviderSchema {
    /// Gets the schema for a resource type.
    #[must_use]
    pub fn resource_schema(&self, type_name: &str) -> Option<&Schema> {
        self.resources.get(type_name)
    }

    /// Gets the schema for a data source type.
    #[must_use]
    pub fn data_source_schema(&self, type_name: &str) -> Option<&Schema> {
        self.data_sources.get(type_name)
    }

    /// Returns all supported resource types.
    #[must_use]
    pub fn resource_types(&self) -> Vec<&str> {
        self.resources.keys().map(String::as_str).collect()
    }

    /// Returns all supported data source types.
    #[must_use]
    pub fn data_source_types(&self) -> Vec<&str> {
        self.data_sources.keys().map(String::as_str).collect()
    }
}

/// Result of a resource operation.
#[derive(Debug, Clone)]
pub struct ResourceOperationResult {
    /// The new state after the operation
    pub state: Option<serde_json::Value>,

    /// Private data to be stored with the resource
    pub private_data: Vec<u8>,

    /// Whether the operation requires resource replacement
    pub requires_replace: bool,
}

/// Planned changes for a resource.
#[derive(Debug, Clone)]
pub struct PlannedChange {
    /// The planned state after apply
    pub planned_state: serde_json::Value,

    /// Attributes that require resource replacement
    pub requires_replace: Vec<String>,

    /// Private data for the planned state
    pub planned_private: Vec<u8>,
}

/// Imported resource from ImportResourceState.
#[derive(Debug, Clone)]
pub struct ImportedResource {
    /// Resource type name
    pub type_name: String,

    /// Resource state
    pub state: serde_json::Value,

    /// Private data
    pub private_data: Vec<u8>,
}

/// Provider diagnostic message.
#[derive(Debug, Clone)]
pub struct ProviderDiagnostic {
    /// Severity level
    pub severity: DiagnosticSeverity,

    /// Summary message
    pub summary: String,

    /// Detailed message
    pub detail: String,

    /// Attribute path (if applicable)
    pub attribute_path: Option<Vec<String>>,
}

/// Diagnostic severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    /// Error that prevents operation
    Error,
    /// Warning that doesn't prevent operation
    Warning,
}

impl From<i32> for DiagnosticSeverity {
    fn from(value: i32) -> Self {
        if value == 1 {
            Self::Error
        } else {
            Self::Warning
        }
    }
}
