//! Behaviour every [`StateStore`] must show, run against each backend's
//! test double: the in-memory store, the D1 store over emulated SQLite and
//! the object store over the fake S3 server.

use serde_json::json;

use super::{
    ConditionalPut, LockRequest, ManagedResource, RecordVersion, ResourceAddress, StateLock,
    StateStore,
};
use crate::error::InfrastructureError;
use crate::tenant::{ProjectInstance, TenantKey};

fn record(name: &str, value: &str) -> ManagedResource {
    ManagedResource {
        address: ResourceAddress::new("random_pet", name),
        provider: "random".into(),
        provider_source: "registry.terraform.io/hashicorp/random".into(),
        schema_version: 2,
        state: json!({"id": value}),
        private: vec![0, 255, 7],
        dependencies: vec!["other".into()],
        sensitive_attributes: vec!["id".into()],
        tainted: false,
        identity: Some(json!({"id": value})),
        serial: 0,
        generation: uuid::Uuid::nil(),
    }
}

async fn take(
    store: &dyn StateStore,
    tenant: &TenantKey,
    instance: Option<&ProjectInstance>,
) -> crate::error::Result<StateLock> {
    let lock = StateLock::generate();
    store
        .acquire_lock(
            tenant,
            &LockRequest {
                lock: &lock,
                holder: "conformance",
                instance,
            },
        )
        .await
}

/// Run every check against `store`, which must be empty and migrated.
pub async fn conforms(store: &dyn StateStore) {
    let tenant = TenantKey::try_new("example.com/conformance", "web").unwrap();
    let dev = TenantKey::with_environment("example.com/conformance", "web", "Dev").unwrap();
    let root = ProjectInstance::try_new(".", "cuenv").unwrap();
    let moved = ProjectInstance::try_new("services/web", "cuenv").unwrap();

    // Reads of a tenant with nothing stored are empty.
    assert!(store.list(&tenant).await.unwrap().is_empty());
    assert!(store.addresses(&tenant).await.unwrap().is_empty());
    assert_eq!(store.current_lock(&tenant).await.unwrap(), None);
    assert_eq!(store.last_writer(&tenant).await.unwrap(), None);
    assert!(!store.force_unlock(&tenant, "nothing").await.unwrap());

    // The lock is exclusive, and taking it records the last writer.
    let lock = take(store, &tenant, Some(&root)).await.unwrap();
    assert!(matches!(
        take(store, &tenant, None).await,
        Err(InfrastructureError::Locked { .. })
    ));
    let held = store.current_lock(&tenant).await.unwrap().unwrap();
    assert_eq!(held.lock_identifier, lock.lock_identifier);
    assert_eq!(held.holder, "conformance");
    assert_eq!(
        store.last_writer(&tenant).await.unwrap().unwrap().instance,
        root
    );
    let listed = store.locks().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].lock.lock_identifier, lock.lock_identifier);
    assert_eq!(listed[0].tenant().unwrap(), tenant);

    // Writes need the current lock; each tenant is isolated.
    let pet = record("pet", "one");
    assert!(matches!(
        store.put(&tenant, &StateLock::generate(), &pet).await,
        Err(InfrastructureError::LockLost { .. })
    ));
    store.put(&tenant, &lock, &pet).await.unwrap();
    assert!(store.list(&dev).await.unwrap().is_empty());
    let stored = store.list(&tenant).await.unwrap().remove(0);
    assert!(stored.same_content(&pet), "{stored:?}");
    assert_eq!(stored.serial, 1);
    assert!(!stored.generation.is_nil());

    // Serials advance; generations are kept by updates.
    let updated = record("pet", "two");
    store.put(&tenant, &lock, &updated).await.unwrap();
    let second = store.list(&tenant).await.unwrap().remove(0);
    assert_eq!(second.serial, 2);
    assert_eq!(second.generation, stored.generation);

    // Compare and swap: a stale version is refused, an exact retry of an
    // applied write is acknowledged.
    let third = record("pet", "three");
    let swap = ConditionalPut {
        resource: &ManagedResource {
            generation: second.generation,
            ..third.clone()
        },
        expected: RecordVersion::of(Some(&second)),
    };
    store.put_if_unchanged(&tenant, &lock, &swap).await.unwrap();
    store.put_if_unchanged(&tenant, &lock, &swap).await.unwrap();
    assert!(matches!(
        store
            .put_if_unchanged(
                &tenant,
                &lock,
                &ConditionalPut {
                    resource: &record("pet", "stale"),
                    expected: RecordVersion::of(Some(&second)),
                },
            )
            .await,
        Err(InfrastructureError::StateChanged { .. })
    ));
    let created = record("cat", "new");
    store
        .put_if_unchanged(
            &tenant,
            &lock,
            &ConditionalPut {
                resource: &created,
                expected: RecordVersion::Absent,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .put_if_unchanged(
                &tenant,
                &lock,
                &ConditionalPut {
                    resource: &created,
                    expected: RecordVersion::Absent,
                },
            )
            .await,
        Err(InfrastructureError::StateChanged { .. })
    ));

    // Several records at once.
    store
        .put_many(&tenant, &lock, &[record("fox", "a"), record("owl", "b")])
        .await
        .unwrap();
    let mut addresses: Vec<String> = store
        .addresses(&tenant)
        .await
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    addresses.sort();
    assert_eq!(
        addresses,
        [
            "random_pet.cat",
            "random_pet.fox",
            "random_pet.owl",
            "random_pet.pet"
        ]
    );

    // Deleting forgets one record; deleting again changes nothing.
    store
        .delete(&tenant, &lock, &ResourceAddress::new("random_pet", "owl"))
        .await
        .unwrap();
    store
        .delete(&tenant, &lock, &ResourceAddress::new("random_pet", "owl"))
        .await
        .unwrap();
    assert_eq!(store.list(&tenant).await.unwrap().len(), 3);

    // Releasing ends the lock; writes with it fail afterwards.
    assert!(store.unlock(&tenant, &lock).await.unwrap());
    assert!(!store.unlock(&tenant, &lock).await.unwrap());
    assert!(matches!(
        store.put(&tenant, &lock, &pet).await,
        Err(InfrastructureError::LockLost { .. })
    ));
    assert_eq!(store.current_lock(&tenant).await.unwrap(), None);

    // A lock taken without an instance keeps the last writer; one taken by
    // a moved directory replaces it.
    let anonymous = take(store, &tenant, None).await.unwrap();
    store.unlock(&tenant, &anonymous).await.unwrap();
    assert_eq!(
        store.last_writer(&tenant).await.unwrap().unwrap().instance,
        root
    );
    let relocated = take(store, &tenant, Some(&moved)).await.unwrap();
    assert_eq!(
        store.last_writer(&tenant).await.unwrap().unwrap().instance,
        moved
    );

    // A forced unlock needs the exact identifier, and a run whose lock was
    // released can no longer write.
    assert!(!store.force_unlock(&tenant, "someone-else").await.unwrap());
    assert!(
        store
            .force_unlock(&tenant, &relocated.lock_identifier)
            .await
            .unwrap()
    );
    assert!(matches!(
        store.put(&tenant, &relocated, &pet).await,
        Err(InfrastructureError::LockLost { .. })
    ));
    let taken_over = take(store, &tenant, None).await.unwrap();
    assert!(matches!(
        store.put(&tenant, &relocated, &pet).await,
        Err(InfrastructureError::LockLost { .. })
    ));
    store.unlock(&tenant, &taken_over).await.unwrap();

    // Another environment's lock is its own.
    let dev_lock = take(store, &dev, None).await.unwrap();
    assert_eq!(store.current_lock(&tenant).await.unwrap(), None);
    assert_eq!(
        store
            .environments("example.com/conformance", "web")
            .await
            .unwrap(),
        [None, Some("Dev".to_string())]
    );
    assert!(
        store
            .environments("example.com/conformance", "api")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.locks().await.unwrap().len(), 1);
    store.unlock(&dev, &dev_lock).await.unwrap();
    assert!(store.locks().await.unwrap().is_empty());
}
