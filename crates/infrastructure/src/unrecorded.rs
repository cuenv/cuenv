//! Changes a provider made that the state store could not record.
//!
//! When `ApplyResourceChange` succeeds but writing the resulting record to
//! the state store fails, the resource exists but state does not know it; a
//! later apply would create it a second time. The record is therefore saved
//! to a private file in the user's state directory
//! (`$XDG_STATE_HOME/cuenv/infrastructure/unrecorded/`, falling back to the
//! local data directory), outside the project so it is never committed.
//!
//! Directories are created with mode 0700 and files with 0600, because a
//! record holds the resource's full state, secrets included. Each file names
//! its tenant; files are grouped per tenant so one tenant's leftovers never
//! block another. Planning refuses to run while a tenant has unrecorded
//! changes, and [`UnrecordedStore::recover`] writes them to the state store
//! under the tenant's lock.
//!
//! Nothing here ever puts a state value in an error message.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{InfrastructureError, Result, json_error_category};
use crate::state::{ManagedResource, ResourceAddress, StateLock, StateStore};
use crate::tenant::TenantKey;

/// Format version written into every file.
const FILE_FORMAT_VERSION: u32 = 1;

/// The directory holding unrecorded changes for every tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnrecordedStore {
    root: PathBuf,
}

/// One saved record waiting to be written to the state store.
#[derive(Debug, Clone, PartialEq)]
pub struct UnrecordedRecord {
    /// File holding the record.
    pub file: PathBuf,
    /// When the record was saved (RFC 3339).
    pub saved_at: String,
    /// The record the provider's change produced.
    pub record: ManagedResource,
}

/// On-disk form of one unrecorded record.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnrecordedFile {
    version: u32,
    tenant: TenantIdentity,
    saved_at: String,
    resource: ManagedResource,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TenantIdentity {
    module_path: String,
    project: String,
}

impl TenantIdentity {
    fn of(tenant: &TenantKey) -> Self {
        Self {
            module_path: tenant.module_path().to_string(),
            project: tenant.project().to_string(),
        }
    }
}

impl UnrecordedStore {
    /// The default location: `cuenv/infrastructure/unrecorded` under the
    /// user's state directory, or the local data directory where the
    /// platform has no state directory.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] when neither directory
    /// can be determined (no home directory).
    pub fn default_location() -> Result<Self> {
        dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .map(|base| Self::at(base.join("cuenv").join("infrastructure").join("unrecorded")))
            .ok_or_else(|| {
                InfrastructureError::configuration(
                    "cannot determine the user state directory for unrecorded infrastructure \
                     changes; set XDG_STATE_HOME or HOME",
                )
            })
    }

    /// Use `root` as the directory.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory holding every tenant's unrecorded changes.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory holding one tenant's unrecorded changes. Tenants are
    /// hashed so module paths never shape file system paths.
    #[must_use]
    pub fn tenant_directory(&self, tenant: &TenantKey) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(tenant.module_path().as_bytes());
        hasher.update([0]);
        hasher.update(tenant.project().as_bytes());
        let digest = hex::encode(hasher.finalize());
        self.root.join(&digest[..32])
    }

    /// Save a record the state store could not take.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::InputOutput`] when the directory or
    /// file cannot be created or written. The error never contains the
    /// record's state.
    pub fn save(&self, tenant: &TenantKey, record: &ManagedResource) -> Result<PathBuf> {
        let directory = self.tenant_directory(tenant);
        create_private_directory(&self.root)?;
        create_private_directory(&directory)?;
        let saved_at = chrono::Utc::now();
        let file = directory.join(format!(
            "{}-{}.json",
            saved_at.format("%Y%m%dT%H%M%S%.9fZ"),
            uuid::Uuid::new_v4().simple()
        ));
        let document = UnrecordedFile {
            version: FILE_FORMAT_VERSION,
            tenant: TenantIdentity::of(tenant),
            saved_at: saved_at.to_rfc3339(),
            resource: record.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&document).map_err(|error| {
            InfrastructureError::codec(format!(
                "cannot serialize the unrecorded record of {} ({})",
                record.address,
                json_error_category(&error)
            ))
        })?;
        write_private_file(&file, &bytes).map_err(|error| {
            InfrastructureError::input_output(format!("write {}", file.display()), error)
        })?;
        Ok(file)
    }

    /// Every unrecorded record of `tenant`, oldest first.
    ///
    /// # Errors
    ///
    /// Returns an error naming the file (never its content) when a file
    /// cannot be read, is malformed, or belongs to another tenant.
    pub fn list(&self, tenant: &TenantKey) -> Result<Vec<UnrecordedRecord>> {
        let directory = self.tenant_directory(tenant);
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(InfrastructureError::input_output(
                    format!("read {}", directory.display()),
                    error,
                ));
            }
        };
        let mut files = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|error| {
                    InfrastructureError::input_output(
                        format!("read {}", directory.display()),
                        error,
                    )
                })?
                .path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                files.push(path);
            }
        }
        // File names start with the save time, so name order is age order.
        files.sort();
        let identity = TenantIdentity::of(tenant);
        files
            .into_iter()
            .map(|file| {
                let document = read_unrecorded_file(&file)?;
                if document.tenant != identity {
                    return Err(InfrastructureError::state(format!(
                        "{} belongs to another tenant; move it out of {}",
                        file.display(),
                        directory.display()
                    )));
                }
                Ok(UnrecordedRecord {
                    file,
                    saved_at: document.saved_at,
                    record: document.resource,
                })
            })
            .collect()
    }

    /// Delete a record's file once the state store holds the record.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::InputOutput`] when the file cannot be
    /// removed.
    pub fn remove(&self, unrecorded: &UnrecordedRecord) -> Result<()> {
        std::fs::remove_file(&unrecorded.file).map_err(|error| {
            InfrastructureError::input_output(
                format!("remove {}", unrecorded.file.display()),
                error,
            )
        })
    }

    /// Write every unrecorded record of `tenant` to `store`, oldest first,
    /// deleting each file once its record is written. Writes are fenced by
    /// `lock`, which the caller must hold.
    ///
    /// Returns the addresses recorded, in order.
    ///
    /// # Errors
    ///
    /// Stops at the first failure; records written before it stay written
    /// and their files are gone, the rest remain for a later attempt.
    pub async fn recover(
        &self,
        store: &dyn StateStore,
        tenant: &TenantKey,
        lock: &StateLock,
    ) -> Result<Vec<ResourceAddress>> {
        let mut recorded = Vec::new();
        for unrecorded in self.list(tenant)? {
            store.put(tenant, lock, &unrecorded.record).await?;
            self.remove(&unrecorded)?;
            recorded.push(unrecorded.record.address);
        }
        Ok(recorded)
    }
}

fn read_unrecorded_file(file: &Path) -> Result<UnrecordedFile> {
    let bytes = std::fs::read(file).map_err(|error| {
        InfrastructureError::input_output(format!("read {}", file.display()), error)
    })?;
    let document: UnrecordedFile = serde_json::from_slice(&bytes).map_err(|error| {
        // serde messages can quote the offending value; report the
        // position only.
        InfrastructureError::state(format!(
            "{} is not a valid unrecorded record ({} at line {}, column {})",
            file.display(),
            json_error_category(&error),
            error.line(),
            error.column()
        ))
    })?;
    if document.version != FILE_FORMAT_VERSION {
        return Err(InfrastructureError::state(format!(
            "{} has format version {}, but this cuenv reads version {FILE_FORMAT_VERSION}",
            file.display(),
            document.version
        )));
    }
    Ok(document)
}

/// Create `directory` and any missing parents with mode 0700, and make sure
/// the final directory is private even if it already existed.
fn create_private_directory(directory: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory).map_err(|error| {
        InfrastructureError::input_output(format!("create {}", directory.display()), error)
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| {
                InfrastructureError::input_output(format!("protect {}", directory.display()), error)
            },
        )?;
    }
    Ok(())
}

/// Create `path` with mode 0600, refusing to overwrite anything.
fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    std::io::Write::write_all(&mut file, bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::MemoryStateStore;

    fn record(name: &str, identifier: &str) -> ManagedResource {
        ManagedResource {
            address: ResourceAddress::new("random_pet", name),
            provider: "random".into(),
            provider_source: "registry.terraform.io/hashicorp/random".into(),
            schema_version: 0,
            state: serde_json::json!({"id": identifier, "secret": "hunter2"}),
            private: vec![1, 2, 3],
            dependencies: Vec::new(),
            tainted: false,
            identity: None,
        }
    }

    fn tenant(project: &str) -> TenantKey {
        TenantKey::new("example.com/app", project).unwrap()
    }

    #[test]
    fn saves_privately_and_lists_per_tenant_in_order() {
        let root = tempfile::tempdir().unwrap();
        let store = UnrecordedStore::at(root.path().join("unrecorded"));
        let web = tenant("web");
        let first = store.save(&web, &record("first", "a")).unwrap();
        let second = store.save(&web, &record("second", "b")).unwrap();
        store.save(&tenant("api"), &record("other", "c")).unwrap();

        let listed = store.list(&web).unwrap();
        assert_eq!(
            listed.iter().map(|entry| &entry.file).collect::<Vec<_>>(),
            vec![&first, &second]
        );
        assert_eq!(listed[0].record, record("first", "a"));
        assert!(store.list(&tenant("elsewhere")).unwrap().is_empty());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&first), 0o600);
            assert_eq!(mode(first.parent().unwrap()), 0o700);
            assert_eq!(mode(store.root()), 0o700);
        }
    }

    #[test]
    fn malformed_files_are_reported_without_their_content() {
        let root = tempfile::tempdir().unwrap();
        let store = UnrecordedStore::at(root.path());
        let web = tenant("web");
        let directory = store.tenant_directory(&web);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("broken.json"),
            r#"{"version": 1, "tenant": "hunter2"}"#,
        )
        .unwrap();
        let error = store.list(&web).unwrap_err().to_string();
        assert!(error.contains("broken.json"), "{error}");
        assert!(!error.contains("hunter2"), "{error}");
    }

    #[test]
    fn files_of_another_tenant_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let store = UnrecordedStore::at(root.path());
        let saved = store.save(&tenant("api"), &record("pet", "a")).unwrap();
        let web_directory = store.tenant_directory(&tenant("web"));
        std::fs::create_dir_all(&web_directory).unwrap();
        std::fs::copy(&saved, web_directory.join("copied.json")).unwrap();
        assert!(store.list(&tenant("web")).is_err());
    }

    #[tokio::test]
    async fn recover_records_under_the_lock_and_removes_files() {
        let root = tempfile::tempdir().unwrap();
        let unrecorded = UnrecordedStore::at(root.path());
        let web = tenant("web");
        unrecorded.save(&web, &record("pet", "old")).unwrap();
        unrecorded.save(&web, &record("pet", "new")).unwrap();
        unrecorded.save(&web, &record("other", "x")).unwrap();

        let state = MemoryStateStore::new();
        let lock = state.lock(&web, "test").await.unwrap();
        let recovered = unrecorded.recover(&state, &web, &lock).await.unwrap();
        assert_eq!(recovered.len(), 3);
        assert!(unrecorded.list(&web).unwrap().is_empty());
        let rows = state.list(&web).await.unwrap();
        let pet = rows.iter().find(|row| row.address.name == "pet").unwrap();
        // The newest save of an address wins.
        assert_eq!(pet.state["id"], "new");
    }

    #[tokio::test]
    async fn recover_without_the_lock_keeps_the_files() {
        let root = tempfile::tempdir().unwrap();
        let unrecorded = UnrecordedStore::at(root.path());
        let web = tenant("web");
        unrecorded.save(&web, &record("pet", "a")).unwrap();
        let state = MemoryStateStore::new();
        let lock_not_held = StateLock {
            lock_identifier: "not-held".into(),
        };
        assert!(
            unrecorded
                .recover(&state, &web, &lock_not_held)
                .await
                .is_err()
        );
        assert_eq!(unrecorded.list(&web).unwrap().len(), 1);
    }
}
