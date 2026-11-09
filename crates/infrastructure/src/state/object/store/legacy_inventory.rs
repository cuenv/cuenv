//! Fail-closed inventory of a legacy migration target project.

use std::collections::BTreeSet;
use std::fmt;

use super::super::key;
use super::ObjectStateStore;
use crate::error::{InfrastructureError, Result};
use crate::tenant::TenantKey;

/// The state of one environment discovered in object storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyTargetState {
    /// A live state document exists.
    Live,
    /// A state document exists but is tombstoned.
    Tombstoned,
}

/// An environment discovered under the target project prefix.
#[derive(Clone, PartialEq, Eq)]
pub struct LegacyTargetEnvironment {
    /// Environment name, or the no-flag identity.
    pub environment: Option<String>,
    /// Whether the document is live or tombstoned.
    pub state: LegacyTargetState,
}

impl fmt::Debug for LegacyTargetEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LegacyTargetEnvironment(<redacted>)")
    }
}

impl ObjectStateStore {
    /// List and validate every target state document in this project.
    ///
    /// The caller must enforce the external freeze and use a backend with
    /// strongly consistent listings and reads. A separate object may otherwise
    /// appear after this inventory.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable, vanished, unsupported or malformed state.
    pub async fn legacy_project_inventory(
        &self,
        project: &TenantKey,
    ) -> Result<Vec<LegacyTargetEnvironment>> {
        let path = self
            .path(project)
            .map_err(|_| InfrastructureError::configuration("invalid migration project"))?;
        let keys = self
            .list_keys(&path.project_prefix())
            .await
            .map_err(|_| InfrastructureError::state("cannot list migration target project"))?;
        let mut seen = BTreeSet::new();
        let mut environments = Vec::new();
        for key in keys.into_iter().filter(|key| key.ends_with("/state.json")) {
            let found = key::parse_state_key(self.root.as_deref(), &key).ok_or_else(|| {
                InfrastructureError::state("migration target contains a malformed state key")
            })?;
            if found.module_path() != path.module_path()
                || found.package() != path.package()
                || found.project() != path.project()
                || found.state_key() != key
            {
                return Err(InfrastructureError::state(
                    "migration target contains a foreign state key",
                ));
            }
            let environment = found.environment().map(ToString::to_string);
            if !seen.insert(environment.clone()) {
                return Err(InfrastructureError::state(
                    "migration target contains duplicate environments",
                ));
            }
            let (document, _) = self
                .read(&found)
                .await
                .map_err(|_| InfrastructureError::state("cannot read migration target state"))?
                .ok_or_else(|| InfrastructureError::state("migration target state disappeared"))?;
            document.require_writable(&key).map_err(|_| {
                InfrastructureError::state("migration target state format is unsupported")
            })?;
            environments.push(LegacyTargetEnvironment {
                environment,
                state: if document.tombstone.is_some() {
                    LegacyTargetState::Tombstoned
                } else {
                    LegacyTargetState::Live
                },
            });
        }
        environments.sort_by(|left, right| left.environment.cmp(&right.environment));
        Ok(environments)
    }
}

#[cfg(test)]
mod tests {
    use super::{LegacyTargetState, *};
    use crate::state::object::Credentials;
    use crate::state::object::fake::{BarrierPosition, FakeS3, FaultAction};
    use crate::state::object::store::{AddressingChoice, ObjectStoreConfiguration};
    use crate::state::{LockRequest, StateLock, StateStore};
    use std::time::Duration;

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    fn store(server: &FakeS3) -> Result<ObjectStateStore> {
        ObjectStateStore::try_new(ObjectStoreConfiguration {
            endpoint: Some(server.endpoint.clone()),
            bucket: "state".into(),
            region: "us-east-1".into(),
            root: None,
            addressing: AddressingChoice::Path,
            credentials: Credentials {
                access_key_id: "access".into(),
                secret_access_key: "secret".into(),
                session_token: None,
            },
            package: "cuenv".into(),
            history: 0,
            minimum_write_interval: None,
            probe_tenant: None,
        })
    }

    async fn create(store: &ObjectStateStore, tenant: &TenantKey) -> Result<()> {
        let lock = StateLock::generate();
        store
            .acquire_lock(
                tenant,
                &LockRequest {
                    lock: &lock,
                    holder: "test",
                    instance: None,
                },
            )
            .await?;
        store.unlock(tenant, &lock).await?;
        Ok(())
    }

    #[tokio::test]
    async fn inventory_includes_extra_live_and_tombstoned_environments() -> TestResult {
        let server = FakeS3::start().await?;
        let store = store(&server)?;
        let project = TenantKey::try_new("example.com/project", "api")?;
        let extra = TenantKey::with_environment("example.com/project", "api", "extra")?;
        create(&store, &project).await?;
        create(&store, &extra).await?;
        let entries = store.legacy_project_inventory(&project).await?;
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .any(|entry| entry.environment.as_deref() == Some("extra")
                    && entry.state == LegacyTargetState::Live)
        );
        let lock = StateLock::generate();
        store
            .acquire_lock(
                &extra,
                &LockRequest {
                    lock: &lock,
                    holder: "test",
                    instance: None,
                },
            )
            .await?;
        store.tombstone(&extra, &lock, "source").await?;
        store.unlock(&extra, &lock).await?;
        let entries = store.legacy_project_inventory(&project).await?;
        assert!(
            entries
                .iter()
                .any(|entry| entry.environment.as_deref() == Some("extra")
                    && entry.state == LegacyTargetState::Tombstoned)
        );
        Ok(())
    }

    #[tokio::test]
    async fn inventory_refuses_disappearing_unreadable_and_malformed_state() -> TestResult {
        let server = FakeS3::start().await?;
        let store = store(&server)?;
        let project = TenantKey::try_new("example.com/project", "api")?;
        create(&store, &project).await?;
        let key = store.path(&project)?.state_key();
        let mut barrier = server.pause_next("GET", "state.json", BarrierPosition::BeforeApply);
        let racing = async {
            barrier.reached(Duration::from_secs(10)).await?;
            server.remove_object("state", &key);
            barrier.release(Duration::from_secs(10)).await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        let (result, race) = tokio::join!(store.legacy_project_inventory(&project), racing);
        race?;
        assert!(result.is_err());
        create(&store, &project).await?;
        server.fault(
            "GET",
            "state.json",
            FaultAction::Status(403, "unreadable".into()),
        );
        assert!(store.legacy_project_inventory(&project).await.is_err());
        let malformed = format!(
            "{}bad!name/state.json",
            store.path(&project)?.project_prefix()
        );
        server.put_object("state", &malformed, b"{}".to_vec());
        assert!(store.legacy_project_inventory(&project).await.is_err());
        Ok(())
    }
}
