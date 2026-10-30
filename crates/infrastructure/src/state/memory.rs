//! In-memory state store for tests and dry runs.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;

use super::{
    ConditionalPut, LastWriter, LockInformation, LockRequest, ManagedResource, RecordVersion,
    ResourceAddress, StateLock, StateStore, TenantLock,
};
use crate::error::{InfrastructureError, Result};
use crate::tenant::TenantKey;

type Rows = HashMap<TenantKey, BTreeMap<ResourceAddress, ManagedResource>>;

#[derive(Debug, Default)]
struct Contents {
    rows: Rows,
    locks: HashMap<TenantKey, LockInformation>,
    writers: HashMap<TenantKey, LastWriter>,
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
        if let Some(instance) = request.instance {
            contents.writers.insert(
                tenant.clone(),
                LastWriter {
                    instance: instance.clone(),
                    recorded_at: chrono::Utc::now().to_rfc3339(),
                },
            );
        }
        Ok(request.lock.clone())
    }

    async fn unlock(&self, tenant: &TenantKey, lock: &StateLock) -> Result<bool> {
        self.force_unlock(tenant, &lock.lock_identifier).await
    }

    async fn current_lock(&self, tenant: &TenantKey) -> Result<Option<LockInformation>> {
        Ok(self.contents()?.locks.get(tenant).cloned())
    }

    async fn locks(&self) -> Result<Vec<TenantLock>> {
        let mut locks: Vec<TenantLock> = self
            .contents()?
            .locks
            .iter()
            .map(|(tenant, lock)| TenantLock {
                module_path: tenant.module_path().to_string(),
                package: None,
                project: tenant.project().to_string(),
                environment: tenant.environment().map(ToString::to_string),
                lock: lock.clone(),
            })
            .collect();
        locks.sort_by(|left, right| {
            (&left.lock.acquired_at, left.label()).cmp(&(&right.lock.acquired_at, right.label()))
        });
        Ok(locks)
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

    async fn environments(&self, module_path: &str, project: &str) -> Result<Vec<Option<String>>> {
        let contents = self.contents()?;
        let mut environments: Vec<Option<String>> = contents
            .rows
            .iter()
            .filter(|(_, rows)| !rows.is_empty())
            .map(|(tenant, _)| tenant)
            .chain(contents.locks.keys())
            .chain(contents.writers.keys())
            .filter(|tenant| tenant.module_path() == module_path && tenant.project() == project)
            .map(|tenant| tenant.environment().map(str::to_string))
            .collect();
        environments.sort();
        environments.dedup();
        Ok(environments)
    }

    async fn last_writer(&self, tenant: &TenantKey) -> Result<Option<LastWriter>> {
        Ok(self.contents()?.writers.get(tenant).cloned())
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
            sensitive_attributes: Vec::new(),
            tainted: false,
            identity: None,
            serial: 0,
            generation: uuid::Uuid::nil(),
        }
    }

    #[tokio::test]
    async fn conforms_to_the_state_store_contract() {
        crate::state::conformance::conforms(&MemoryStateStore::default()).await;
    }

    #[tokio::test]
    async fn sensitivity_defaults_and_metadata_only_updates_are_preserved() {
        let store = MemoryStateStore::new();
        let tenant = TenantKey::try_new("example.com/a", "web").unwrap();
        let lock = store.lock(&tenant, "test").await.unwrap();
        // Recovery records written before this field existed still decode.
        let mut legacy = serde_json::to_value(resource("one")).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("sensitive_attributes");
        let legacy: ManagedResource = serde_json::from_value(legacy).unwrap();
        assert!(legacy.sensitive_attributes.is_empty());
        store.put(&tenant, &lock, &legacy).await.unwrap();
        let initial = store.list(&tenant).await.unwrap().remove(0);
        let sensitive = ManagedResource {
            sensitive_attributes: vec!["id".into()],
            ..initial.clone()
        };
        assert!(!initial.same_content(&sensitive));
        let update = ConditionalPut {
            resource: &sensitive,
            expected: RecordVersion::of(Some(&initial)),
        };
        store
            .put_if_unchanged(&tenant, &lock, &update)
            .await
            .unwrap();
        store
            .put_if_unchanged(&tenant, &lock, &update)
            .await
            .unwrap();
        let stored = store.list(&tenant).await.unwrap().remove(0);
        assert_eq!(stored.sensitive_attributes, ["id"]);
        assert_eq!(stored.serial, 2);
        assert!(matches!(
            store
                .put_if_unchanged(
                    &tenant,
                    &lock,
                    &ConditionalPut {
                        resource: &initial,
                        expected: update.expected,
                    }
                )
                .await,
            Err(InfrastructureError::StateChanged { .. })
        ));
        store.put(&tenant, &lock, &legacy).await.unwrap();
        assert!(
            store.list(&tenant).await.unwrap()[0]
                .sensitive_attributes
                .is_empty()
        );
    }

    #[tokio::test]
    async fn rows_are_isolated_per_tenant() {
        let store = MemoryStateStore::new();
        let web = TenantKey::try_new("example.com/a", "web").unwrap();
        let api = TenantKey::try_new("example.com/a", "api").unwrap();
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
        let legacy = TenantKey::try_new("example.com/a", "web").unwrap();
        let dev = TenantKey::with_environment("example.com/a", "web", "Dev").unwrap();
        let staging = TenantKey::with_environment("example.com/a", "web", "Staging").unwrap();
        let root = ProjectInstance::try_new(".", "web").unwrap();
        let copy = ProjectInstance::try_new("copy", "web").unwrap();
        let legacy_lock = store.lock(&legacy, "legacy").await.unwrap();
        let take = |tenant, instance| {
            let store = &store;
            async move {
                let lock = StateLock::generate();
                store
                    .acquire_lock(
                        tenant,
                        &LockRequest {
                            lock: &lock,
                            holder: "test",
                            instance: Some(instance),
                        },
                    )
                    .await
            }
        };
        let dev_lock = take(&dev, &root).await.unwrap();
        let staging_lock = take(&staging, &copy).await.unwrap();
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

        assert!(store.last_writer(&legacy).await.unwrap().is_none());
        assert_eq!(
            store.last_writer(&dev).await.unwrap().unwrap().instance,
            root
        );
        assert_eq!(
            store.last_writer(&staging).await.unwrap().unwrap().instance,
            copy
        );
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
        let tenant = TenantKey::try_new("example.com/a", "web").unwrap();
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
        let tenant = TenantKey::try_new("example.com/a", "web").unwrap();
        let lock = store.lock(&tenant, "first").await.unwrap();
        assert!(matches!(
            store.lock(&tenant, "second").await,
            Err(InfrastructureError::Locked { .. })
        ));
        assert!(!store.force_unlock(&tenant, "wrong").await.unwrap());
        let information = store.current_lock(&tenant).await.unwrap().unwrap();
        assert_eq!(information.holder, "first");
        // Releasing reports whether the lock was still held.
        assert!(store.unlock(&tenant, &lock).await.unwrap());
        assert!(!store.unlock(&tenant, &lock).await.unwrap());
        assert!(store.current_lock(&tenant).await.unwrap().is_none());
        store.lock(&tenant, "third").await.unwrap();
    }

    #[tokio::test]
    async fn caller_chosen_lock_identifiers_are_used_and_checked() {
        let store = MemoryStateStore::new();
        let tenant = TenantKey::try_new("example.com/a", "web").unwrap();
        let chosen = StateLock::generate();
        let acquired = store
            .acquire_lock(
                &tenant,
                &LockRequest {
                    lock: &chosen,
                    holder: "test",
                    instance: None,
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
                    &TenantKey::try_new("example.com/b", "web").unwrap(),
                    &LockRequest {
                        lock: &invalid,
                        holder: "test",
                        instance: None,
                    },
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn serials_advance_and_conditional_writes_compare_them() {
        let store = MemoryStateStore::new();
        let tenant = TenantKey::try_new("example.com/a", "web").unwrap();
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
    async fn the_last_writer_is_recorded_with_the_lock_and_kept_without_an_instance() {
        let store = MemoryStateStore::new();
        let tenant = TenantKey::try_new("example.com/a", "web").unwrap();
        let root = ProjectInstance::try_new(".", "web").unwrap();
        let moved = ProjectInstance::try_new("services/web", "web").unwrap();
        assert!(store.last_writer(&tenant).await.unwrap().is_none());
        let acquire = |instance: Option<&ProjectInstance>| {
            let store = &store;
            let tenant = &tenant;
            let instance = instance.cloned();
            async move {
                let lock = StateLock::generate();
                store
                    .acquire_lock(
                        tenant,
                        &LockRequest {
                            lock: &lock,
                            holder: "test",
                            instance: instance.as_ref(),
                        },
                    )
                    .await
                    .unwrap()
            }
        };
        let lock = acquire(Some(&root)).await;
        store.unlock(&tenant, &lock).await.unwrap();
        assert_eq!(
            store.last_writer(&tenant).await.unwrap().unwrap().instance,
            root
        );
        // A lock taken without an instance (unlock, restore) keeps it.
        let lock = acquire(None).await;
        store.unlock(&tenant, &lock).await.unwrap();
        assert_eq!(
            store.last_writer(&tenant).await.unwrap().unwrap().instance,
            root
        );
        // A moved directory simply becomes the last writer; nothing refuses.
        let lock = acquire(Some(&moved)).await;
        store.unlock(&tenant, &lock).await.unwrap();
        assert_eq!(
            store.last_writer(&tenant).await.unwrap().unwrap().instance,
            moved
        );
    }

    #[tokio::test]
    async fn locks_lists_every_tenant_and_addresses_reads_keys() {
        let store = MemoryStateStore::new();
        let api = TenantKey::try_new("example.com/app", "api").unwrap();
        let dev = TenantKey::with_environment("example.com/app", "web", "Dev").unwrap();
        let api_lock = store.lock(&api, "first").await.unwrap();
        let dev_lock = store.lock(&dev, "second").await.unwrap();
        let locks = store.locks().await.unwrap();
        assert_eq!(locks.len(), 2);
        let labels: Vec<String> = locks.iter().map(TenantLock::label).collect();
        assert!(
            labels.contains(&"example.com/app#api".to_string()),
            "{labels:?}"
        );
        assert!(
            labels.contains(&"example.com/app#web@Dev".to_string()),
            "{labels:?}"
        );
        let dev_row = locks.iter().find(|held| held.project == "web").unwrap();
        assert_eq!(dev_row.environment.as_deref(), Some("Dev"));
        // A listed lock is released by its tenant alone.
        assert!(
            store
                .force_unlock(&dev_row.tenant().unwrap(), &dev_row.lock.lock_identifier)
                .await
                .unwrap()
        );
        assert_eq!(store.locks().await.unwrap().len(), 1);
        store.put(&api, &api_lock, &resource("pet")).await.unwrap();
        assert_eq!(
            store.addresses(&api).await.unwrap(),
            vec![ResourceAddress::new("random_pet", "pet")]
        );
        let _ = dev_lock;
    }
}
