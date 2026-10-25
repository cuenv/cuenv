//! Durable, multi-tenant state for managed resources.
//!
//! Each managed resource is one record keyed by
//! `(module_path, project, environment, resource_type, resource_name)`, where
//! the environment is empty for a run without `--env`. The no-flag identity
//! and every named environment are distinct tenants that never read each
//! other's records. Records hold the provider's state in cty JSON (the format
//! `UpgradeResourceState` expects), the schema version it was written with,
//! the provider's opaque private bytes, a generation identifying the
//! insertion, and a serial the store increments on every write.
//!
//! Every tenant also has at most one owner record naming the CUE instance
//! that owns its state ([`TenantOwner`]), and at most one lock.

/// Serde helpers storing bytes as standard base64 text.
pub(crate) mod base64_bytes;
mod d1;
#[cfg(test)]
mod fake_server;
mod http;
mod lock;
mod memory;
mod owner;
mod resource;
mod sql;
mod store;
mod turso;

pub use d1::{D1Configuration, D1StateStore};
pub use lock::{LockInformation, LockRequest, StateLock, TenantLock};
pub use memory::MemoryStateStore;
pub use owner::{OwnerClaim, OwnerClaimMode, TenantOwner};
pub use resource::{ConditionalPut, ManagedResource, RecordVersion, ResourceAddress};
pub use store::StateStore;
pub use turso::{TursoConfiguration, TursoStateStore};
