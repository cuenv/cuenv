//! In-memory state store for tests and dry runs.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;

use super::{
    ConditionalPut, LockInformation, LockRequest, ManagedResource, OwnerClaim, OwnerClaimMode,
    RecordVersion, ResourceAddress, StateLock, StateStore, TenantOwner,
};
use crate::error::{InfrastructureError, Result};
use crate::tenant::TenantKey;

type Rows = HashMap<TenantKey, BTreeMap<ResourceAddress, ManagedResource>>;

#[derive(Debug, Default)]
struct Contents {
    rows: Rows,
    locks: HashMap<TenantKey, LockInformation>,
    owners: HashMap<TenantKey, TenantOwner>,
}

impl Contents {
    fn version(&self, tenant: &TenantKey, address: &ResourceAddress) -> RecordVersion {
        RecordVersion::of(
            self.rows
                .get(tenant)
                .and_then(|resources| resources.get(address)),
        )
    }

    /// Write `resource`, advancing its serial as the Turso store does.
    fn write(&mut self, tenant: &TenantKey, resource: &ManagedResource, insertion: uuid::Uuid) {
        let previous = self
            .rows
            .get(tenant)
            .and_then(|rows| rows.get(&resource.address));
        let serial = previous.map_or(1, |record| record.serial.saturating_add(1));
        let generation = previous.map_or(insertion, |record| record.generation);
        self.rows.entry(tenant.clone()).or_default().insert(
            resource.address.clone(),
            ManagedResource {
                serial,
                generation,
                ..resource.clone()
            },
        );
    }
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
        contents.write(tenant, resource, uuid::Uuid::new_v4());
        Ok(())
    }

    async fn put_if_unchanged(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        put: &ConditionalPut<'_>,
    ) -> Result<()> {
        let mut contents = self.contents()?;
        require_lock(&contents, tenant, lock)?;
        let found = contents.version(tenant, &put.resource.address);
        if found != put.expected {
            if contents
                .rows
                .get(tenant)
                .and_then(|rows| rows.get(&put.resource.address))
                .is_some_and(|stored| put.is_recorded(stored))
            {
                return Ok(());
            }
            return Err(InfrastructureError::StateChanged {
                address: put.resource.address.to_string(),
                expected: put.expected.to_string(),
                found: found.to_string(),
                file: None,
            });
        }
        let insertion = if put.resource.generation.is_nil() {
            uuid::Uuid::new_v4()
        } else {
            put.resource.generation
        };
        contents.write(tenant, put.resource, insertion);
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

    async fn acquire_lock(
        &self,
        tenant: &TenantKey,
        request: &LockRequest<'_>,
    ) -> Result<StateLock> {
        request.lock.validate()?;
        let mut contents = self.contents()?;
        if let Some(existing) = contents.locks.get(tenant) {
            if existing.lock_identifier == request.lock.lock_identifier {
                return Ok(request.lock.clone());
            }
            return Err(InfrastructureError::Locked {
                tenant: tenant.to_string(),
                lock_identifier: existing.lock_identifier.clone(),
                holder: existing.holder.clone(),
                acquired_at: existing.acquired_at.clone(),
            });
        }
        contents.locks.insert(
            tenant.clone(),
            LockInformation {
                lock_identifier: request.lock.lock_identifier.clone(),
                holder: request.holder.to_string(),
                acquired_at: chrono::Utc::now().to_rfc3339(),
            },
        );
        Ok(request.lock.clone())
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

    async fn owner(&self, tenant: &TenantKey) -> Result<Option<TenantOwner>> {
        Ok(self.contents()?.owners.get(tenant).cloned())
    }

    async fn claim_owner(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        claim: &OwnerClaim<'_>,
    ) -> Result<TenantOwner> {
        let mut contents = self.contents()?;
        require_lock(&contents, tenant, lock)?;
        if claim.mode == OwnerClaimMode::IfUnowned
            && let Some(existing) = contents.owners.get(tenant)
        {
            return Ok(existing.clone());
        }
        let owner = TenantOwner {
            instance: claim.instance.clone(),
            claimed_at: chrono::Utc::now().to_rfc3339(),
        };
        contents.owners.insert(tenant.clone(), owner.clone());
        Ok(owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tenant::ProjectInstance;

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
            serial: 0,
            generation: uuid::Uuid::nil(),
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
    async fn named_environments_isolate_resources_locks_owners_and_conditional_writes() {
        let store = MemoryStateStore::new();
        let legacy = TenantKey::new("example.com/a", "web").unwrap();
        let dev = TenantKey::with_environment("example.com/a", "web", "Dev").unwrap();
        let staging = TenantKey::with_environment("example.com/a", "web", "Staging").unwrap();
        let root = ProjectInstance::new(".", "web").unwrap();
        let copy = ProjectInstance::new("copy", "web").unwrap();
        let legacy_lock = store.lock(&legacy, "legacy").await.unwrap();
        let dev_lock = store.lock(&dev, "dev").await.unwrap();
        let staging_lock = store.lock(&staging, "staging").await.unwrap();
        let record = resource("same");

        store.put(&legacy, &legacy_lock, &record).await.unwrap();
        store.put(&dev, &dev_lock, &record).await.unwrap();
        assert!(store.list(&staging).await.unwrap().is_empty());
        assert!(matches!(
            store.put(&staging, &dev_lock, &record).await,
            Err(InfrastructureError::LockLost { .. })
        ));
        store
            .put_if_unchanged(
                &staging,
                &staging_lock,
                &ConditionalPut {
                    resource: &record,
                    expected: RecordVersion::Absent,
                },
            )
            .await
            .unwrap();
        assert_eq!(store.list(&staging).await.unwrap()[0].serial, 1);
        store.put(&dev, &dev_lock, &record).await.unwrap();
        assert_eq!(store.list(&dev).await.unwrap()[0].serial, 2);
        assert_eq!(store.list(&legacy).await.unwrap()[0].serial, 1);
        assert_eq!(store.list(&staging).await.unwrap()[0].serial, 1);

        store
            .claim_owner(
                &dev,
                &dev_lock,
                &OwnerClaim {
                    instance: &root,
                    mode: OwnerClaimMode::IfUnowned,
                },
            )
            .await
            .unwrap();
        store
            .claim_owner(
                &staging,
                &staging_lock,
                &OwnerClaim {
                    instance: &copy,
                    mode: OwnerClaimMode::IfUnowned,
                },
            )
            .await
            .unwrap();
        assert!(store.owner(&legacy).await.unwrap().is_none());
        assert_eq!(store.owner(&dev).await.unwrap().unwrap().instance, root);
        assert_eq!(store.owner(&staging).await.unwrap().unwrap().instance, copy);
        assert!(
            !store
                .force_unlock(&staging, &dev_lock.lock_identifier)
                .await
                .unwrap()
        );
        assert!(store.current_lock(&dev).await.unwrap().is_some());
        store
            .delete(&dev, &dev_lock, &record.address)
            .await
            .unwrap();
        assert!(store.list(&dev).await.unwrap().is_empty());
        assert_eq!(store.list(&legacy).await.unwrap().len(), 1);
        assert_eq!(store.list(&staging).await.unwrap().len(), 1);
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

    #[tokio::test]
    async fn caller_chosen_lock_identifiers_are_used_and_checked() {
        let store = MemoryStateStore::new();
        let tenant = TenantKey::new("example.com/a", "web").unwrap();
        let chosen = StateLock::generate();
        let acquired = store
            .acquire_lock(
                &tenant,
                &LockRequest {
                    lock: &chosen,
                    holder: "test",
                },
            )
            .await
            .unwrap();
        assert_eq!(acquired, chosen);
        assert_eq!(
            store
                .current_lock(&tenant)
                .await
                .unwrap()
                .unwrap()
                .lock_identifier,
            chosen.lock_identifier
        );
        let invalid = StateLock {
            lock_identifier: "no spaces".into(),
        };
        assert!(
            store
                .acquire_lock(
                    &TenantKey::new("example.com/b", "web").unwrap(),
                    &LockRequest {
                        lock: &invalid,
                        holder: "test",
                    },
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn serials_advance_and_conditional_writes_compare_them() {
        let store = MemoryStateStore::new();
        let tenant = TenantKey::new("example.com/a", "web").unwrap();
        let lock = store.lock(&tenant, "test").await.unwrap();
        let record = resource("one");
        store
            .put_if_unchanged(
                &tenant,
                &lock,
                &ConditionalPut {
                    resource: &record,
                    expected: RecordVersion::Absent,
                },
            )
            .await
            .unwrap();
        let initial = store.list(&tenant).await.unwrap()[0].clone();
        assert_eq!(initial.serial, 1);
        store.put(&tenant, &lock, &record).await.unwrap();
        assert_eq!(store.list(&tenant).await.unwrap()[0].serial, 2);

        let stale = store
            .put_if_unchanged(
                &tenant,
                &lock,
                &ConditionalPut {
                    resource: &record,
                    expected: RecordVersion::of(Some(&initial)),
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&stale, InfrastructureError::StateChanged { address, .. } if address == "random_pet.one"),
            "{stale}"
        );
        assert!(stale.to_string().contains("serial 2"), "{stale}");
        store
            .put_if_unchanged(
                &tenant,
                &lock,
                &ConditionalPut {
                    resource: &record,
                    expected: RecordVersion::Generation {
                        generation: initial.generation,
                        serial: 2,
                    },
                },
            )
            .await
            .unwrap();
        assert_eq!(store.list(&tenant).await.unwrap()[0].serial, 3);
    }

    #[tokio::test]
    async fn owners_are_claimed_once_and_transferred_explicitly_under_the_lock() {
        let store = MemoryStateStore::new();
        let tenant = TenantKey::new("example.com/a", "web").unwrap();
        let root = ProjectInstance::new(".", "web").unwrap();
        let copy = ProjectInstance::new("_copy", "web").unwrap();
        assert!(store.owner(&tenant).await.unwrap().is_none());
        let not_held = StateLock::generate();
        assert!(matches!(
            store
                .claim_owner(
                    &tenant,
                    &not_held,
                    &OwnerClaim {
                        instance: &root,
                        mode: OwnerClaimMode::IfUnowned,
                    },
                )
                .await,
            Err(InfrastructureError::LockLost { .. })
        ));
        let lock = store.lock(&tenant, "test").await.unwrap();
        let claim = |instance, mode| OwnerClaim { instance, mode };
        let owner = store
            .claim_owner(&tenant, &lock, &claim(&root, OwnerClaimMode::IfUnowned))
            .await
            .unwrap();
        assert_eq!(owner.instance, root);
        let kept = store
            .claim_owner(&tenant, &lock, &claim(&copy, OwnerClaimMode::IfUnowned))
            .await
            .unwrap();
        assert_eq!(kept.instance, root);
        let transferred = store
            .claim_owner(&tenant, &lock, &claim(&copy, OwnerClaimMode::Transfer))
            .await
            .unwrap();
        assert_eq!(transferred.instance, copy);
        assert_eq!(store.owner(&tenant).await.unwrap().unwrap().instance, copy);
    }
}
