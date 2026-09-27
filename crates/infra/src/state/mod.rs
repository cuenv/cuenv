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
pub use turso::{TursoConfig, TursoStateStore};

/// Address of a managed resource within a tenant: `type.name`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ResourceAddress {
    /// Resource type, e.g. `random_pet`.
    pub resource_type: String,
    /// Resource name from the `infra.resources` map.
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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.resource_type, self.name)
    }
}

/// A managed resource as persisted in state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManagedResource {
    /// Resource address.
    pub address: ResourceAddress,
    /// Local provider name from `infra.providers`.
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
}

/// An acquired state lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateLock {
    /// Lock identifier, needed to release it.
    pub lock_id: String,
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
    async fn put(&self, tenant: &TenantKey, resource: &ManagedResource) -> Result<()>;

    /// Remove a managed resource.
    async fn delete(&self, tenant: &TenantKey, address: &ResourceAddress) -> Result<()>;

    /// Acquire the tenant's exclusive lock.
    ///
    /// Fails with [`crate::InfraError::Locked`] if another run holds it.
    async fn lock(&self, tenant: &TenantKey, holder: &str) -> Result<StateLock>;

    /// Release a lock acquired with [`StateStore::lock`].
    async fn unlock(&self, tenant: &TenantKey, lock: &StateLock) -> Result<()>;

    /// Release the tenant's lock regardless of holder.
    async fn force_unlock(&self, tenant: &TenantKey) -> Result<()>;
}

mod base64_bytes {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(d)?;
        STANDARD.decode(encoded).map_err(serde::de::Error::custom)
    }
}
