//! In-memory state store for tests and dry runs.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;

use super::{LockInformation, ManagedResource, ResourceAddress, StateLock, StateStore};
use crate::error::{InfrastructureError, Result};
use crate::tenant::TenantKey;

type Rows = HashMap<TenantKey, BTreeMap<ResourceAddress, ManagedResource>>;

#[derive(Debug, Default)]
struct Contents {
    rows: Rows,
    locks: HashMap<TenantKey, LockInformation>,
}

/// A [`StateStore`] held in process memory.
#[derive(Debug, Default)]
pub struct MemoryStateStore {
    contents: Mutex<Contents>,
}

impl MemoryStateStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn contents(&self) -> Result<MutexGuard<'_, Contents>> {
        self.contents
            .lock()
            .map_err(|_| InfrastructureError::state("in-memory state store mutex poisoned"))
    }
}

fn require_lock(contents: &Contents, tenant: &TenantKey, lock: &StateLock) -> Result<()> {
    let held = contents
        .locks
        .get(tenant)
        .is_some_and(|information| information.lock_identifier == lock.lock_identifier);
    if held {
        Ok(())
    } else {
        Err(InfrastructureError::LockLost {
            tenant: tenant.to_string(),
            lock_identifier: lock.lock_identifier.clone(),
        })
    }
}

#[async_trait]
impl StateStore for MemoryStateStore {
    async fn migrate(&self) -> Result<()> {
        Ok(())
    }

    async fn list(&self, tenant: &TenantKey) -> Result<Vec<ManagedResource>> {
        Ok(self
            .contents()?
            .rows
            .get(tenant)
            .map(|resources| resources.values().cloned().collect())
            .unwrap_or_default())
    }

    async fn put(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        resource: &ManagedResource,
    ) -> Result<()> {
        let mut contents = self.contents()?;
        require_lock(&contents, tenant, lock)?;
        contents
            .rows
            .entry(tenant.clone())
            .or_default()
            .insert(resource.address.clone(), resource.clone());
        Ok(())
    }

    async fn delete(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        address: &ResourceAddress,
    ) -> Result<()> {
        let mut contents = self.contents()?;
        require_lock(&contents, tenant, lock)?;
        if let Some(resources) = contents.rows.get_mut(tenant) {
            resources.remove(address);
        }
        Ok(())
    }

    async fn lock(&self, tenant: &TenantKey, holder: &str) -> Result<StateLock> {
        let mut contents = self.contents()?;
        if let Some(existing) = contents.locks.get(tenant) {
            return Err(InfrastructureError::Locked {
                tenant: tenant.to_string(),
                lock_identifier: existing.lock_identifier.clone(),
                holder: existing.holder.clone(),
                acquired_at: existing.acquired_at.clone(),
            });
        }
        let lock_identifier = uuid::Uuid::new_v4().to_string();
        contents.locks.insert(
            tenant.clone(),
            LockInformation {
                lock_identifier: lock_identifier.clone(),
                holder: holder.to_string(),
                acquired_at: chrono::Utc::now().to_rfc3339(),
            },
        );
        Ok(StateLock { lock_identifier })
    }

    async fn unlock(&self, tenant: &TenantKey, lock: &StateLock) -> Result<()> {
        self.force_unlock(tenant, &lock.lock_identifier)
            .await
            .map(|_| ())
    }

    async fn current_lock(&self, tenant: &TenantKey) -> Result<Option<LockInformation>> {
        Ok(self.contents()?.locks.get(tenant).cloned())
    }

    async fn force_unlock(&self, tenant: &TenantKey, lock_identifier: &str) -> Result<bool> {
        let mut contents = self.contents()?;
        let matches = contents
            .locks
            .get(tenant)
            .is_some_and(|information| information.lock_identifier == lock_identifier);
        if matches {
            contents.locks.remove(tenant);
        }
        Ok(matches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(name: &str) -> ManagedResource {
        ManagedResource {
            address: ResourceAddress::new("random_pet", name),
            provider: "random".into(),
            provider_source: "registry.terraform.io/hashicorp/random".into(),
            schema_version: 0,
            state: serde_json::json!({"id": name}),
            private: vec![1, 2, 3],
            dependencies: Vec::new(),
            tainted: false,
            identity: None,
        }
    }

    #[tokio::test]
    async fn rows_are_isolated_per_tenant() {
        let store = MemoryStateStore::new();
        let web = TenantKey::new("example.com/a", "web").unwrap();
        let api = TenantKey::new("example.com/a", "api").unwrap();
        let lock = store.lock(&web, "test").await.unwrap();
        store.put(&web, &lock, &resource("one")).await.unwrap();
        assert_eq!(store.list(&web).await.unwrap().len(), 1);
        assert!(store.list(&api).await.unwrap().is_empty());
        store
            .delete(&web, &lock, &ResourceAddress::new("random_pet", "one"))
            .await
            .unwrap();
        assert!(store.list(&web).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn writes_require_the_current_lock() {
        let store = MemoryStateStore::new();
        let tenant = TenantKey::new("example.com/a", "web").unwrap();
        let first = store.lock(&tenant, "first").await.unwrap();
        assert!(
            store
                .force_unlock(&tenant, &first.lock_identifier)
                .await
                .unwrap()
        );
        let second = store.lock(&tenant, "second").await.unwrap();
        assert!(matches!(
            store.put(&tenant, &first, &resource("one")).await,
            Err(InfrastructureError::LockLost { .. })
        ));
        store.put(&tenant, &second, &resource("one")).await.unwrap();
    }

    #[tokio::test]
    async fn lock_is_exclusive_and_force_unlock_needs_the_identifier() {
        let store = MemoryStateStore::new();
        let tenant = TenantKey::new("example.com/a", "web").unwrap();
        let lock = store.lock(&tenant, "first").await.unwrap();
        assert!(matches!(
            store.lock(&tenant, "second").await,
            Err(InfrastructureError::Locked { .. })
        ));
        assert!(!store.force_unlock(&tenant, "wrong").await.unwrap());
        let information = store.current_lock(&tenant).await.unwrap().unwrap();
        assert_eq!(information.holder, "first");
        store.unlock(&tenant, &lock).await.unwrap();
        assert!(store.current_lock(&tenant).await.unwrap().is_none());
        store.lock(&tenant, "third").await.unwrap();
    }
}
