//! In-memory state store for tests and dry runs.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;

use super::{ManagedResource, ResourceAddress, StateLock, StateStore};
use crate::error::{InfrastructureError, Result};
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

fn poisoned() -> InfrastructureError {
    InfrastructureError::state("in-memory state store mutex poisoned")
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
            .map(|resources| resources.values().cloned().collect())
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
        if let Some(resources) = rows.get_mut(tenant) {
            resources.remove(address);
        }
        Ok(())
    }

    async fn lock(&self, tenant: &TenantKey, holder: &str) -> Result<StateLock> {
        let mut locks = self.locks.lock().map_err(|_| poisoned())?;
        if let Some((lock_identifier, existing)) = locks.get(tenant) {
            return Err(InfrastructureError::Locked {
                tenant: tenant.to_string(),
                lock_identifier: lock_identifier.clone(),
                holder: existing.clone(),
                acquired_at: "earlier in this process".to_string(),
            });
        }
        let lock_identifier = uuid::Uuid::new_v4().to_string();
        locks.insert(
            tenant.clone(),
            (lock_identifier.clone(), holder.to_string()),
        );
        Ok(StateLock { lock_identifier })
    }

    async fn unlock(&self, tenant: &TenantKey, lock: &StateLock) -> Result<()> {
        let mut locks = self.locks.lock().map_err(|_| poisoned())?;
        if locks
            .get(tenant)
            .is_some_and(|(lock_identifier, _)| *lock_identifier == lock.lock_identifier)
        {
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
        let web = TenantKey::new("example.com/shop", "web").unwrap();
        let api = TenantKey::new("example.com/shop", "api").unwrap();
        store.put(&web, &resource("one")).await.unwrap();
        assert_eq!(store.list(&web).await.unwrap().len(), 1);
        assert!(store.list(&api).await.unwrap().is_empty());
        store
            .delete(&web, &ResourceAddress::new("random_pet", "one"))
            .await
            .unwrap();
        assert!(store.list(&web).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn lock_is_exclusive_until_released() {
        let store = MemoryStateStore::new();
        let tenant = TenantKey::new("example.com/shop", "web").unwrap();
        let lock = store.lock(&tenant, "first").await.unwrap();
        assert!(matches!(
            store.lock(&tenant, "second").await,
            Err(InfrastructureError::Locked { .. })
        ));
        store.unlock(&tenant, &lock).await.unwrap();
        store.lock(&tenant, "third").await.unwrap();
        store.force_unlock(&tenant).await.unwrap();
        store.lock(&tenant, "fourth").await.unwrap();
    }
}
