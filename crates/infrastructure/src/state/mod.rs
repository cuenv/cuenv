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
//! Every tenant also has at most one lock, and records the CUE instance that
//! last acquired that lock to change its state ([`LastWriter`]), for
//! information only.

/// Serde helpers storing bytes as standard base64 text.
pub(crate) mod base64_bytes;
#[cfg(test)]
mod conformance;
mod d1;
#[cfg(test)]
mod fake_server;
mod http;
mod legacy_copy;
mod lock;
mod memory;
mod migration;
pub mod object;
mod resource;
mod sql;
pub use sql::MIGRATION_FENCE_PREFIX;
mod store;
mod turso;
mod writer;

pub use d1::{D1Configuration, D1StateStore};
pub use legacy_copy::LegacyMigrationCopyPlan;
pub use lock::{LockIdentity, LockInformation, LockRequest, StateLock, TenantLock};
pub use memory::MemoryStateStore;
pub use migration::{
    BackendIdentity, LegacyMigrationAccess, LegacyMigrationCopy, LegacyMigrationDeleteRequest,
    LegacyMigrationRecordRequest, LegacyMigrationSnapshot, LegacyMigrationSnapshotRead,
    LegacyMigrationSource, LegacyMigrationWriterRequest, MigrationMutation,
};
pub use object::{
    AddressingChoice, Credentials, HistoryEntry, LockDetails, ManifestEntry, MigrationManifest,
    ObjectLocation, ObjectStateFeatures, ObjectStateStore, ObjectStoreConfiguration, ObjectTenant,
    RestoreRequest, StateSize, StoredTenant, TenantImport,
};
pub use resource::{ConditionalPut, ManagedResource, RecordVersion, ResourceAddress};
pub use store::StateStore;
pub use turso::{TursoConfiguration, TursoStateStore};
pub use writer::LastWriter;
