//! The [`StateStore`] trait every state backend implements.

use async_trait::async_trait;

use super::lock::{LockInformation, LockRequest, StateLock, TenantLock};
use super::object::ObjectStateFeatures;
use super::resource::{ConditionalPut, ManagedResource, ResourceAddress};
use super::writer::LastWriter;
use crate::error::Result;
use crate::tenant::TenantKey;

/// Storage backend for managed resource state.
///
/// Every operation is scoped to a [`TenantKey`]; implementations must never
/// read or write rows belonging to another tenant.
#[async_trait]
pub trait StateStore: Send + Sync {
    /// Non-secret, stable backend identity for binding local recovery files.
    /// In-memory or custom stores can remain unbound.
    fn recovery_identity(&self) -> Option<String> {
        None
    }

    /// Create tables and indexes if they do not exist, and bring the schema
    /// up to date.
    ///
    /// Fails with [`crate::InfrastructureError::StateSchemaNewer`] when the
    /// stored schema is newer than this build knows, with
    /// [`crate::InfrastructureError::StateMigrationBlocked`] (listing the
    /// locks that block it) when a migration would run while any state lock
    /// is held: a run that holds a lock must never have its writes land in a
    /// half-migrated shape. (The first schema creates the lock table, so only
    /// later migrations are fenced.) A database holding tables an unreleased
    /// development build wrote, or tables cuenv did not create, fails with
    /// [`crate::InfrastructureError::StateUnreleasedLayout`] or
    /// [`crate::InfrastructureError::StateSchemaConflict`].
    async fn migrate(&self) -> Result<()>;

    /// List all managed resources of a tenant, with generation and serial.
    ///
    /// Never migrates: a store that was never migrated has no resources.
    async fn list(&self, tenant: &TenantKey) -> Result<Vec<ManagedResource>>;

    /// Insert or replace a managed resource.
    /// New insertions receive a fresh generation; updates retain it.
    ///
    /// Fails with [`crate::InfrastructureError::LockLost`] unless `lock` is
    /// still the tenant's current lock; the check and the write are atomic.
    async fn put(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        resource: &ManagedResource,
    ) -> Result<()>;

    /// Insert or replace a managed resource only while the stored record is
    /// still at `put.expected` (compare and swap). Fenced by `lock` like
    /// [`StateStore::put`]; the lock check, the version check and the write
    /// are atomic.
    /// A create payload with a non-nil generation preserves that generation
    /// on insertion, so an exact retry can acknowledge its own earlier write.
    ///
    /// Fails with [`crate::InfrastructureError::StateChanged`] naming the
    /// address when the stored version differs.
    async fn put_if_unchanged(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        put: &ConditionalPut<'_>,
    ) -> Result<()>;

    /// Remove a managed resource. Fenced by `lock` like [`StateStore::put`].
    async fn delete(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        address: &ResourceAddress,
    ) -> Result<()>;

    /// Acquire the tenant's exclusive lock under the identifier the caller
    /// chose.
    ///
    /// Fails with [`crate::InfrastructureError::Locked`] if another run holds
    /// it, and fails unless [`StateStore::migrate`] brought the store to the
    /// schema this build writes. When the outcome is uncertain (the
    /// response was lost), the error names the identifier so the caller can
    /// release it.
    async fn acquire_lock(
        &self,
        tenant: &TenantKey,
        request: &LockRequest<'_>,
    ) -> Result<StateLock>;

    /// Acquire the tenant's exclusive lock under a fresh identifier; see
    /// [`StateStore::acquire_lock`].
    async fn lock(&self, tenant: &TenantKey, holder: &str) -> Result<StateLock> {
        let lock = StateLock::generate();
        self.acquire_lock(
            tenant,
            &LockRequest {
                lock: &lock,
                holder,
                instance: None,
            },
        )
        .await
    }

    /// Release a lock acquired with [`StateStore::lock`].
    ///
    /// Returns whether the lock was still held and is now released: `false`
    /// means nothing was released because the lock was already gone (another
    /// actor released it, or took it over), which the caller reports instead
    /// of claiming a release.
    async fn unlock(&self, tenant: &TenantKey, lock: &StateLock) -> Result<bool>;

    /// Describe the tenant's current lock, if any. Never migrates.
    async fn current_lock(&self, tenant: &TenantKey) -> Result<Option<LockInformation>>;

    /// Every lock held in the store, for every tenant, oldest first. Never
    /// migrates: a store that was never migrated holds none.
    ///
    /// This is how a lock left behind by a project that no longer evaluates
    /// (or by another repository sharing the database) is found and released
    /// with [`StateStore::force_unlock`]. Every store must enumerate its
    /// locks: a store that answered "none" without looking would hide the
    /// locks that block a migration or a run.
    async fn locks(&self) -> Result<Vec<TenantLock>>;

    /// The addresses of the tenant's records, read from their key columns
    /// alone. Unlike [`StateStore::list`] it succeeds when a record's content
    /// cannot be decoded, so such a record can still be named and removed.
    /// Never migrates.
    async fn addresses(&self, tenant: &TenantKey) -> Result<Vec<ResourceAddress>> {
        Ok(self
            .list(tenant)
            .await?
            .into_iter()
            .map(|resource| resource.address)
            .collect())
    }

    /// Release the tenant's lock only if its identifier is `lock_identifier`.
    ///
    /// Returns `false` when no lock with that identifier is held.
    async fn force_unlock(&self, tenant: &TenantKey, lock_identifier: &str) -> Result<bool>;

    /// The instance that last acquired the tenant's lock to change its
    /// state, if one is recorded. Information only: it never refuses a run.
    ///
    /// Never migrates: a store that was never migrated has none recorded.
    async fn last_writer(&self, tenant: &TenantKey) -> Result<Option<LastWriter>>;

    /// Insert or replace several managed resources, fenced by `lock` like
    /// [`StateStore::put`]. A store that can write them in one request does
    /// so atomically; this default writes them one at a time and is not
    /// atomic. The engine uses it for refreshed records only: a refresh
    /// created nothing, so a record lost with a failed write is refreshed
    /// again by the next run.
    async fn put_many(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        resources: &[ManagedResource],
    ) -> Result<()> {
        for resource in resources {
            self.put(tenant, lock, resource).await?;
        }
        Ok(())
    }

    /// Where the tenant's state lives, for display (`s3://bucket/key`).
    /// `None` for stores that have no such address to show.
    fn location(&self, _tenant: &TenantKey) -> Option<String> {
        None
    }

    /// Operations only an object store offers: history, restore, rename
    /// detection, renaming and migration manifests. `None` for every other
    /// store.
    fn object_features(&self) -> Option<&dyn ObjectStateFeatures> {
        None
    }
}
