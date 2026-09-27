//! In-memory state store for tests and dry runs.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;

use super::{ManagedResource, ResourceAddress, StateLock, StateStore};
use crate::error::{InfraError, Result};
use crate::tenant::TenantKey;

type Rows = HashMap<TenantKey, BTreeMap<ResourceAddress, ManagedResource>>;

/// A [`StateStore`] held in process memory.
#[derive(Debug, Default)]
pub struct MemoryStateStore {
    rows: Mutex<Rows>,
    locks: Mutex<HashMap<TenantKey, (String, String)>>,
}

impl MemoryStateStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

fn poisoned() -> InfraError {
    InfraError::state("in-memory state store mutex poisoned")
}

#[async_trait]
impl StateStore for MemoryStateStore {
    async fn migrate(&self) -> Result<()> {
        Ok(())
    }

    async fn list(&self, tenant: &TenantKey) -> Result<Vec<ManagedResource>> {
        let rows = self.rows.lock().map_err(|_| poisoned())?;
        Ok(rows
            .get(tenant)
            .map(|r| r.values().cloned().collect())
            .unwrap_or_default())
    }

    async fn put(&self, tenant: &TenantKey, resource: &ManagedResource) -> Result<()> {
        let mut rows = self.rows.lock().map_err(|_| poisoned())?;
        rows.entry(tenant.clone())
            .or_default()
            .insert(resource.address.clone(), resource.clone());
        Ok(())
    }

    async fn delete(&self, tenant: &TenantKey, address: &ResourceAddress) -> Result<()> {
        let mut rows = self.rows.lock().map_err(|_| poisoned())?;
        if let Some(r) = rows.get_mut(tenant) {
            r.remove(address);
        }
        Ok(())
    }

    async fn lock(&self, tenant: &TenantKey, holder: &str) -> Result<StateLock> {
        let mut locks = self.locks.lock().map_err(|_| poisoned())?;
        if let Some((lock_id, existing)) = locks.get(tenant) {
            return Err(InfraError::Locked {
                tenant: tenant.to_string(),
                lock_id: lock_id.clone(),
                holder: existing.clone(),
                acquired_at: "earlier in this process".to_string(),
            });
        }
        let lock_id = uuid::Uuid::new_v4().to_string();
        locks.insert(tenant.clone(), (lock_id.clone(), holder.to_string()));
        Ok(StateLock { lock_id })
    }

    async fn unlock(&self, tenant: &TenantKey, lock: &StateLock) -> Result<()> {
        let mut locks = self.locks.lock().map_err(|_| poisoned())?;
        if locks.get(tenant).is_some_and(|(id, _)| *id == lock.lock_id) {
            locks.remove(tenant);
        }
        Ok(())
    }

    async fn force_unlock(&self, tenant: &TenantKey) -> Result<()> {
        self.locks.lock().map_err(|_| poisoned())?.remove(tenant);
        Ok(())
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
        }
    }

    #[tokio::test]
    async fn rows_are_isolated_per_tenant() {
        let store = MemoryStateStore::new();
        let a = TenantKey::new("example.com/a", "web").unwrap();
        let b = TenantKey::new("example.com/a", "api").unwrap();
        store.put(&a, &resource("one")).await.unwrap();
        assert_eq!(store.list(&a).await.unwrap().len(), 1);
        assert!(store.list(&b).await.unwrap().is_empty());
        store
            .delete(&a, &ResourceAddress::new("random_pet", "one"))
            .await
            .unwrap();
        assert!(store.list(&a).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn lock_is_exclusive_until_released() {
        let store = MemoryStateStore::new();
        let t = TenantKey::new("example.com/a", "web").unwrap();
        let lock = store.lock(&t, "first").await.unwrap();
        assert!(matches!(
            store.lock(&t, "second").await,
            Err(InfraError::Locked { .. })
        ));
        store.unlock(&t, &lock).await.unwrap();
        store.lock(&t, "third").await.unwrap();
        store.force_unlock(&t).await.unwrap();
        store.lock(&t, "fourth").await.unwrap();
    }
}
