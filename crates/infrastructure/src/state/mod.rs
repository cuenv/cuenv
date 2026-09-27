//! Durable, multi-tenant state for managed resources.
//!
//! Each managed resource is one record keyed by
//! `(module_path, project, resource_type, resource_name)`. Records hold the
//! provider's state in cty JSON (the format `UpgradeResourceState` expects),
//! the schema version it was written with, and the provider's opaque
//! private bytes.

mod memory;
mod turso;

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::tenant::TenantKey;

pub use memory::MemoryStateStore;
pub use turso::{TursoConfiguration, TursoStateStore};

/// Address of a managed resource within a tenant: `type.name`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ResourceAddress {
    /// Resource type, e.g. `random_pet`.
    pub resource_type: String,
    /// Resource name from the `infrastructure.resources` map.
    pub name: String,
}

impl ResourceAddress {
    /// Build an address.
    pub fn new(resource_type: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            resource_type: resource_type.into(),
            name: name.into(),
        }
    }
}

impl fmt::Display for ResourceAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.resource_type, self.name)
    }
}

/// A managed resource as persisted in state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManagedResource {
    /// Resource address.
    pub address: ResourceAddress,
    /// Local provider name from `infrastructure.providers`.
    pub provider: String,
    /// Provider source address, e.g. `registry.terraform.io/hashicorp/random`.
    pub provider_source: String,
    /// Resource schema version the state was written with.
    pub schema_version: i64,
    /// Resource state as cty JSON.
    pub state: serde_json::Value,
    /// Provider private data, opaque to cuenv.
    #[serde(with = "base64_bytes")]
    pub private: Vec<u8>,
    /// Names of resources this one depends on, used to order destroys.
    pub dependencies: Vec<String>,
    /// The resource exists but its creation failed part way; the next plan
    /// must replace it.
    #[serde(default)]
    pub tainted: bool,
    /// Resource identity data as cty JSON, for providers that declare an
    /// identity schema.
    #[serde(default)]
    pub identity: Option<serde_json::Value>,
}

/// An acquired state lock. Every write must present it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateLock {
    /// Lock identifier, needed to write and to release it.
    pub lock_identifier: String,
}

/// Who holds a tenant's lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockInformation {
    /// Lock identifier.
    pub lock_identifier: String,
    /// Description of the holder (command, user, host, process).
    pub holder: String,
    /// When the lock was acquired (RFC 3339).
    pub acquired_at: String,
}

/// Storage backend for managed resource state.
///
/// Every operation is scoped to a [`TenantKey`]; implementations must never
/// read or write rows belonging to another tenant.
#[async_trait]
pub trait StateStore: Send + Sync {
    /// Create tables and indexes if they do not exist.
    async fn migrate(&self) -> Result<()>;

    /// List all managed resources of a tenant.
    async fn list(&self, tenant: &TenantKey) -> Result<Vec<ManagedResource>>;

    /// Insert or replace a managed resource.
    ///
    /// Fails with [`crate::InfrastructureError::LockLost`] unless `lock` is
    /// still the tenant's current lock; the check and the write are atomic.
    async fn put(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        resource: &ManagedResource,
    ) -> Result<()>;

    /// Remove a managed resource. Fenced by `lock` like [`StateStore::put`].
    async fn delete(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        address: &ResourceAddress,
    ) -> Result<()>;

    /// Acquire the tenant's exclusive lock.
    ///
    /// Fails with [`crate::InfrastructureError::Locked`] if another run holds it.
    async fn lock(&self, tenant: &TenantKey, holder: &str) -> Result<StateLock>;

    /// Release a lock acquired with [`StateStore::lock`].
    async fn unlock(&self, tenant: &TenantKey, lock: &StateLock) -> Result<()>;

    /// Describe the tenant's current lock, if any.
    async fn current_lock(&self, tenant: &TenantKey) -> Result<Option<LockInformation>>;

    /// Release the tenant's lock only if its identifier is `lock_identifier`.
    ///
    /// Returns `false` when no lock with that identifier is held.
    async fn force_unlock(&self, tenant: &TenantKey, lock_identifier: &str) -> Result<bool>;
}

mod base64_bytes {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<Output: Serializer>(
        bytes: &[u8],
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'input, Input: Deserializer<'input>>(
        deserializer: Input,
    ) -> Result<Vec<u8>, Input::Error> {
        let encoded = String::deserialize(deserializer)?;
        STANDARD.decode(encoded).map_err(serde::de::Error::custom)
    }
}
