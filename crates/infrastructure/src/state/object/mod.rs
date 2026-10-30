//! State in an S3-compatible object store (AWS S3, Cloudflare R2, MinIO).
//!
//! Each tenant is one object, `state.json`, under a key derived from the CUE
//! module path, the package, the project and the environment:
//!
//! ```text
//! [<root>/]<module>/_<package>/<project>/<env | _default>/state.json
//! ```
//!
//! The object holds the format versions, the tenant identity, the last
//! writer, the lock and every record. Every write is a conditional PUT on the
//! ETag of the version it was derived from, made only after checking that the
//! lock in that version is the writer's own, so the lock check and the write
//! are atomic. See ADR-0012 and
//! `docs/design/specs/2026-10-29-infrastructure-object-storage-state.md`.

pub mod client;
pub mod document;
#[cfg(any(test, feature = "fake-s3"))]
pub mod fake;
pub mod key;
pub mod sigv4;
pub mod store;
pub mod xml;

#[cfg(test)]
mod tests;

pub use sigv4::Credentials;
pub use store::{
    AddressingChoice, ManifestEntry, MigrationManifest, ObjectLocation, ObjectStateStore,
    ObjectStoreConfiguration, TenantImport,
};

use async_trait::async_trait;

use super::lock::{StateLock, TenantLock};
use super::writer::LastWriter;
use crate::error::Result;
use crate::tenant::TenantKey;

/// One tenant an object store holds under a module path, as rename
/// detection sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredTenant {
    /// CUE package of the tenant.
    pub package: String,
    /// Project name of the tenant.
    pub project: String,
    /// Named environment; `None` for the identity without `--env`.
    pub environment: Option<String>,
    /// The recorded last writer, if any.
    pub last_writer: Option<LastWriter>,
    /// How many records the tenant holds.
    pub records: usize,
    /// Whether a lock is held.
    pub locked: bool,
    /// Where the tenant's state lives (`s3://bucket/key`).
    pub location: String,
}

/// One history snapshot of a tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// The serial of the snapshot.
    pub serial: u64,
    /// The cuenv version that wrote it.
    pub written_by: String,
    /// The addresses it records.
    pub addresses: Vec<String>,
}

/// What `state restore` writes back.
#[derive(Debug, Clone, Copy)]
pub struct RestoreRequest<'request> {
    /// The snapshot serial to restore; the newest when `None`.
    pub serial: Option<u64>,
    /// Holder description for the lock the restore takes.
    pub holder: &'request str,
    /// The lock to take, with its identifier already chosen.
    pub lock: &'request StateLock,
}

/// What an object store records about a held lock beyond
/// [`super::LockInformation`]. Untrusted stored text, control characters
/// removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockDetails {
    /// The cuenv version that took the lock.
    pub cuenv_version: String,
    /// The last record the lock holder wrote: `put` or `delete`, the
    /// address and when.
    pub last_change: Option<String>,
}

/// The size of a tenant's state object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateSize {
    /// Bytes of `state.json`.
    pub bytes: usize,
    /// The largest `state.json` the store writes.
    pub limit: usize,
}

/// The package-qualified identity of a tenant in an object store, for
/// operations that cross packages (renames).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectTenant {
    /// CUE package.
    pub package: String,
    /// The tenant within the package.
    pub tenant: TenantKey,
}

/// Operations only an object store offers. Reached through
/// [`super::StateStore::object_features`].
#[async_trait]
pub trait ObjectStateFeatures: Send + Sync {
    /// Every tenant stored under `module_path`, in every package.
    async fn tenants(&self, module_path: &str) -> Result<Vec<StoredTenant>>;

    /// The size of the tenant's state object; `None` when it is absent.
    async fn size(&self, tenant: &TenantKey) -> Result<Option<StateSize>>;

    /// Whether the project's migration manifest lists the tenant as not yet
    /// migrated (its state is still in the source backend).
    async fn awaiting_migration(&self, tenant: &TenantKey) -> Result<bool>;

    /// The locks held for the projects of `module_path`, listed under that
    /// module's prefix only.
    async fn module_locks(&self, module_path: &str) -> Result<Vec<TenantLock>>;

    /// More about the lock the tenant holds, if any.
    async fn lock_details(&self, tenant: &TenantKey) -> Result<Option<LockDetails>>;

    /// The tenant's history snapshots, oldest first.
    async fn history(&self, tenant: &TenantKey) -> Result<Vec<HistoryEntry>>;

    /// Write a history snapshot back as the tenant's state, under a lock it
    /// takes and releases itself. Returns the restored entry.
    async fn restore(
        &self,
        tenant: &TenantKey,
        request: &RestoreRequest<'_>,
    ) -> Result<HistoryEntry>;

    /// Move a tenant's state to another identity, leaving a tombstone that
    /// names the new location. Takes and releases both locks itself.
    async fn rename(&self, from: &ObjectTenant, to: &ObjectTenant, holder: &str) -> Result<()>;
}
