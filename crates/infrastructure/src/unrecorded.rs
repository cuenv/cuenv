//! Changes a provider made that the state store could not record.
//!
//! When `ApplyResourceChange` succeeds but writing the resulting record to
//! the state store fails, the resource exists but state does not know it; a
//! later apply would create it a second time. The record is therefore saved
//! to a private file in the user's state directory
//! (`$XDG_STATE_HOME/cuenv/infrastructure/unrecorded/`, falling back to the
//! local data directory), outside the project so it is never committed.
//!
//! A record holds the resource's full state, secrets included, and is
//! written back to the state store later, so the files are guarded:
//!
//! - Directories are created with mode 0700 and files with 0600. A directory
//!   or file that is a symbolic link, is owned by another user, or (for
//!   files) is accessible to other users is refused, never followed or read.
//!   Files are opened with `O_NOFOLLOW`.
//! - Files are written atomically: to a hidden temporary file, flushed to
//!   disk, then renamed into place, so a crash never leaves half a record.
//! - Each file names its tenant and the version of the stored record it
//!   replaces ([`RecordVersion`]), so [`UnrecordedStore::recover`] writes it
//!   only while the stored record is still the one it replaces.
//!
//! Files are grouped per tenant so one tenant's leftovers never block
//! another. Planning refuses to run while a tenant has unrecorded changes.
//!
//! The file format is JSON in camelCase throughout, with an explicit
//! `formatVersion`. Nothing here ever puts a state value in an error
//! message; problems with a file are [`InfrastructureError::UnrecordedFile`]
//! errors naming the file.

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{InfrastructureError, Result, describe_json_error, json_error_category};
use crate::state::{
    ConditionalPut, ManagedResource, RecordVersion, ResourceAddress, StateLock, StateStore,
};
use crate::tenant::TenantKey;

/// Format version written into every file.
const FILE_FORMAT_VERSION: u32 = 2;

/// Largest unrecorded file read, in bytes (64 MiB, the state store's own
/// response limit).
const MAXIMUM_FILE_BYTES: u64 = 64 * 1024 * 1024;

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
    /// The version of the stored record this one replaces.
    pub expected: RecordVersion,
    /// The record the provider's change produced.
    pub record: ManagedResource,
}

/// Whether [`UnrecordedStore::recover`] may overwrite a stored record that
/// changed since the unrecorded record was saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverOverwrite {
    /// Write each record only while the stored record is still the one it
    /// replaces; otherwise stop with
    /// [`InfrastructureError::StateChanged`].
    IfUnchanged,
    /// Write every record whatever is stored (an explicit `--force`).
    Always,
}

/// Options for [`UnrecordedStore::recover`].
#[derive(Debug, Clone, Copy)]
pub struct RecoverOptions<'options> {
    /// The tenant's lock, which fences every write.
    pub lock: &'options StateLock,
    /// Whether newer stored records may be overwritten.
    pub overwrite: RecoverOverwrite,
}

/// On-disk form of one unrecorded record.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnrecordedFile {
    format_version: u32,
    tenant: TenantIdentity,
    saved_at: String,
    expected: RecordVersion,
    resource: FileResource,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TenantIdentity {
    module_path: String,
    project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    environment: Option<String>,
}

impl TenantIdentity {
    fn of(tenant: &TenantKey) -> Self {
        Self {
            module_path: tenant.module_path().to_string(),
            project: tenant.project().to_string(),
            environment: tenant.environment().map(ToString::to_string),
        }
    }
}

/// On-disk form of a [`ManagedResource`], frozen independently of it.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileResource {
    resource_type: String,
    name: String,
    provider: String,
    provider_source: String,
    schema_version: i64,
    state: serde_json::Value,
    #[serde(with = "crate::state::base64_bytes")]
    private: Vec<u8>,
    dependencies: Vec<String>,
    tainted: bool,
    identity: Option<serde_json::Value>,
}

impl FileResource {
    fn of(record: &ManagedResource) -> Self {
        Self {
            resource_type: record.address.resource_type.clone(),
            name: record.address.name.clone(),
            provider: record.provider.clone(),
            provider_source: record.provider_source.clone(),
            schema_version: record.schema_version,
            state: record.state.clone(),
            private: record.private.clone(),
            dependencies: record.dependencies.clone(),
            tainted: record.tainted,
            identity: record.identity.clone(),
        }
    }

    fn into_record(self) -> ManagedResource {
        ManagedResource {
            address: ResourceAddress::new(self.resource_type, self.name),
            provider: self.provider,
            provider_source: self.provider_source,
            schema_version: self.schema_version,
            state: self.state,
            private: self.private,
            dependencies: self.dependencies,
            tainted: self.tainted,
            identity: self.identity,
            serial: 0,
        }
    }
}

fn file_problem(path: &Path, problem: impl Into<String>) -> InfrastructureError {
    InfrastructureError::UnrecordedFile {
        path: path.display().to_string(),
        problem: problem.into(),
    }
}

/// Whether a directory must already exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Creation {
    /// Create it (mode 0700) when missing.
    CreateIfMissing,
    /// Report it missing.
    Existing,
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
        if let Some(environment) = tenant.environment() {
            hasher.update([0]);
            hasher.update(environment.as_bytes());
        }
        let digest = hex::encode(hasher.finalize());
        self.root.join(&digest[..32])
    }

    /// Save a record the state store could not take, with the version of
    /// the stored record it replaces.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::InputOutput`] when the directory or
    /// file cannot be created or written, and
    /// [`InfrastructureError::UnrecordedFile`] when a directory is not
    /// private to this user. The error never contains the record's state.
    #[tracing::instrument(
        skip_all,
        fields(tenant = %tenant, address = %put.resource.address, expected = %put.expected)
    )]
    pub fn save(&self, tenant: &TenantKey, put: &ConditionalPut<'_>) -> Result<PathBuf> {
        let record = put.resource;
        let directory = self.tenant_directory(tenant);
        if let Some(parent) = self.root.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                InfrastructureError::input_output(format!("create {}", parent.display()), error)
            })?;
        }
        private_directory(&self.root, Creation::CreateIfMissing)?;
        private_directory(&directory, Creation::CreateIfMissing)?;
        let saved_at = chrono::Utc::now();
        let name = format!(
            "{}-{}.json",
            saved_at.format("%Y%m%dT%H%M%S%.9fZ"),
            uuid::Uuid::new_v4().simple()
        );
        let document = UnrecordedFile {
            format_version: FILE_FORMAT_VERSION,
            tenant: TenantIdentity::of(tenant),
            saved_at: saved_at.to_rfc3339(),
            expected: put.expected,
            resource: FileResource::of(record),
        };
        let bytes = serde_json::to_vec_pretty(&document).map_err(|error| {
            InfrastructureError::codec(format!(
                "cannot serialize the unrecorded record of {} ({})",
                record.address,
                json_error_category(&error)
            ))
        })?;
        let file = directory.join(&name);
        write_atomically(&AtomicWrite {
            directory: &directory,
            name: &name,
            bytes: &bytes,
        })
        .map_err(|error| {
            InfrastructureError::input_output(format!("write {}", file.display()), error)
        })?;
        tracing::info!(file = %file.display(), "saved an unrecorded change");
        Ok(file)
    }

    /// Whether `tenant` has any unrecorded change, without reading the
    /// files. Cheap enough to call before taking a lock.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::UnrecordedFile`] when the directory
    /// is unsafe or unreadable.
    pub fn has_pending(&self, tenant: &TenantKey) -> Result<bool> {
        Ok(!self.pending_files(tenant)?.is_empty())
    }

    /// The candidate files of `tenant`, oldest first, after checking both
    /// directories are private to this user.
    fn pending_files(&self, tenant: &TenantKey) -> Result<Vec<PathBuf>> {
        let directory = self.tenant_directory(tenant);
        if !private_directory(&self.root, Creation::Existing)?
            || !private_directory(&directory, Creation::Existing)?
        {
            return Ok(Vec::new());
        }
        let unreadable =
            |error: std::io::Error| file_problem(&directory, format!("cannot be read ({error})"));
        let mut files = Vec::new();
        for entry in std::fs::read_dir(&directory).map_err(unreadable)? {
            let name = entry.map_err(unreadable)?.file_name();
            let name = name.to_string_lossy();
            // Hidden names are temporary files of a save in progress.
            if !name.starts_with('.') && name.ends_with(".json") {
                files.push(directory.join(name.as_ref()));
            }
        }
        // File names start with the save time, so name order is age order.
        files.sort();
        Ok(files)
    }

    /// Every unrecorded record of `tenant`, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::UnrecordedFile`] naming the file
    /// (never its content) when a file cannot be read, is malformed, is of
    /// another format version, belongs to another tenant, or is not private
    /// to this user.
    pub fn list(&self, tenant: &TenantKey) -> Result<Vec<UnrecordedRecord>> {
        let identity = TenantIdentity::of(tenant);
        self.pending_files(tenant)?
            .into_iter()
            .map(|file| {
                let document = read_unrecorded_file(&file)?;
                if document.tenant != identity {
                    return Err(file_problem(
                        &file,
                        "it belongs to another tenant; move it out of this directory",
                    ));
                }
                Ok(UnrecordedRecord {
                    file,
                    saved_at: document.saved_at,
                    expected: document.expected,
                    record: document.resource.into_record(),
                })
            })
            .collect()
    }

    /// Delete a record's file once the state store holds the record.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::UnrecordedFile`] when the file cannot
    /// be removed.
    pub fn remove(&self, unrecorded: &UnrecordedRecord) -> Result<()> {
        std::fs::remove_file(&unrecorded.file)
            .map_err(|error| file_problem(&unrecorded.file, format!("cannot be removed ({error})")))
    }

    /// Write every unrecorded record of `tenant` to `store`, oldest first,
    /// deleting each file once its record is written. Writes are fenced by
    /// `options.lock`, which the caller must hold.
    ///
    /// With [`RecoverOverwrite::IfUnchanged`] a record is written only
    /// while the stored record is still the one it replaces; a stored
    /// record that already holds exactly its content counts as recorded.
    ///
    /// Returns the addresses recorded, in order.
    ///
    /// # Errors
    ///
    /// Stops at the first failure; records written before it stay written
    /// and their files are gone, the rest remain for a later attempt. A
    /// stored record that changed is
    /// [`InfrastructureError::StateChanged`] naming the address.
    #[tracing::instrument(skip_all, fields(tenant = %tenant, overwrite = ?options.overwrite))]
    pub async fn recover(
        &self,
        store: &dyn StateStore,
        tenant: &TenantKey,
        options: &RecoverOptions<'_>,
    ) -> Result<Vec<ResourceAddress>> {
        let mut recorded = Vec::new();
        for unrecorded in self.list(tenant)? {
            let record = &unrecorded.record;
            match options.overwrite {
                RecoverOverwrite::Always => store.put(tenant, options.lock, record).await?,
                RecoverOverwrite::IfUnchanged => {
                    let put = ConditionalPut {
                        resource: record,
                        expected: unrecorded.expected,
                    };
                    match store.put_if_unchanged(tenant, options.lock, &put).await {
                        Ok(()) => {}
                        Err(changed @ InfrastructureError::StateChanged { .. }) => {
                            let already_recorded = store
                                .list(tenant)
                                .await?
                                .iter()
                                .any(|stored| stored.same_content(record));
                            if !already_recorded {
                                return Err(changed);
                            }
                            tracing::info!(
                                address = %record.address,
                                "the state store already holds this unrecorded change"
                            );
                        }
                        Err(other) => return Err(other),
                    }
                }
            }
            self.remove(&unrecorded)?;
            tracing::info!(address = %record.address, "recovered an unrecorded change");
            recorded.push(unrecorded.record.address);
        }
        Ok(recorded)
    }
}

fn read_unrecorded_file(file: &Path) -> Result<UnrecordedFile> {
    let mut opened = open_private_file(file)?;
    let mut bytes = Vec::new();
    (&mut opened)
        .take(MAXIMUM_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| file_problem(file, format!("cannot be read ({error})")))?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAXIMUM_FILE_BYTES) {
        return Err(file_problem(
            file,
            format!("it exceeds the {MAXIMUM_FILE_BYTES}-byte limit"),
        ));
    }
    // Check the version before the full shape, so an old or new format is
    // reported as such rather than as malformed.
    let version = serde_json::from_slice::<FormatVersion>(&bytes)
        .map_err(|error| {
            file_problem(
                file,
                format!("it is not valid JSON ({})", describe_json_error(&error)),
            )
        })?
        .format_version;
    if version != Some(FILE_FORMAT_VERSION) {
        return Err(file_problem(
            file,
            format!(
                "it has format version {}, but this cuenv reads version {FILE_FORMAT_VERSION}",
                version.map_or_else(|| "(none)".to_string(), |version| version.to_string())
            ),
        ));
    }
    serde_json::from_slice(&bytes).map_err(|error| {
        // serde messages can quote the offending value; report the
        // position only.
        file_problem(
            file,
            format!(
                "it is not a valid unrecorded record ({})",
                describe_json_error(&error)
            ),
        )
    })
}

/// Just the format version of a file.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FormatVersion {
    #[serde(default)]
    format_version: Option<u32>,
}

/// The effective user identifier of this process.
#[cfg(unix)]
#[expect(unsafe_code, reason = "geteuid has no safe standard library wrapper")]
fn effective_user() -> u32 {
    // SAFETY: geteuid takes no arguments, cannot fail and has no side
    // effects; POSIX specifies it as always successful.
    unsafe { libc::geteuid() }
}

/// Check `path` is a real directory (not a symbolic link) owned by this
/// user, creating it with mode 0700 when allowed, and take away any access
/// other users have to it. Returns whether it exists.
#[cfg(unix)]
fn private_directory(path: &Path, creation: Creation) -> Result<bool> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match creation {
            Creation::Existing => return Ok(false),
            Creation::CreateIfMissing => {
                let mut builder = std::fs::DirBuilder::new();
                builder.mode(0o700);
                match builder.create(path) {
                    Ok(()) => {}
                    // Created concurrently: checked below like any other.
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(InfrastructureError::input_output(
                            format!("create {}", path.display()),
                            error,
                        ));
                    }
                }
            }
        },
        Err(error) => {
            return Err(file_problem(path, format!("cannot be inspected ({error})")));
        }
    }
    // Open without following a final symbolic link, then inspect and fix
    // what was opened, so nothing can be swapped in between.
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(path)
        .map_err(|error| {
            file_problem(
                path,
                format!("is not a directory this user can open without following links ({error})"),
            )
        })?;
    let metadata = directory
        .metadata()
        .map_err(|error| file_problem(path, format!("cannot be inspected ({error})")))?;
    if metadata.uid() != effective_user() {
        return Err(file_problem(path, "it is owned by another user"));
    }
    if metadata.mode() & 0o077 != 0 {
        directory
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|error| file_problem(path, format!("cannot be made private ({error})")))?;
    }
    Ok(true)
}

#[cfg(not(unix))]
fn private_directory(path: &Path, creation: Creation) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(file_problem(path, "it is not a directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match creation {
            Creation::Existing => Ok(false),
            Creation::CreateIfMissing => {
                std::fs::create_dir(path).map(|()| true).map_err(|error| {
                    InfrastructureError::input_output(format!("create {}", path.display()), error)
                })
            }
        },
        Err(error) => Err(file_problem(path, format!("cannot be inspected ({error})"))),
    }
}

/// Open an unrecorded file for reading without following a symbolic link,
/// refusing anything but a regular file private to this user.
#[cfg(unix)]
fn open_private_file(file: &Path) -> Result<File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(file)
        .map_err(|error| {
            file_problem(
                file,
                format!("cannot be opened without following links ({error})"),
            )
        })?;
    let metadata = opened
        .metadata()
        .map_err(|error| file_problem(file, format!("cannot be inspected ({error})")))?;
    if !metadata.is_file() {
        return Err(file_problem(file, "it is not a regular file"));
    }
    if metadata.uid() != effective_user() {
        return Err(file_problem(file, "it is owned by another user"));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(file_problem(
            file,
            "other users can access it (expected mode 0600)",
        ));
    }
    Ok(opened)
}

#[cfg(not(unix))]
fn open_private_file(file: &Path) -> Result<File> {
    File::open(file).map_err(|error| file_problem(file, format!("cannot be opened ({error})")))
}

/// A file to write atomically into `directory` as `name`.
struct AtomicWrite<'write> {
    directory: &'write Path,
    name: &'write str,
    bytes: &'write [u8],
}

/// Write `write.bytes` to a hidden temporary file (mode 0600, never
/// following or replacing anything), flush it to disk, rename it into
/// place and flush the directory, so the file appears whole or not at all.
fn write_atomically(write: &AtomicWrite<'_>) -> std::io::Result<()> {
    let temporary = write.directory.join(format!(
        ".{}.{}.tmp",
        write.name,
        uuid::Uuid::new_v4().simple()
    ));
    let result = write_new_private_file(&temporary, write.bytes)
        .and_then(|()| std::fs::rename(&temporary, write.directory.join(write.name)))
        .and_then(|()| sync_directory(write.directory));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn write_new_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> std::io::Result<()> {
    File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> std::io::Result<()> {
    Ok(())
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
            serial: 0,
        }
    }

    fn absent(record: &ManagedResource) -> ConditionalPut<'_> {
        ConditionalPut {
            resource: record,
            expected: RecordVersion::Absent,
        }
    }

    fn tenant(project: &str) -> TenantKey {
        TenantKey::new("example.com/app", project).unwrap()
    }

    fn save(store: &UnrecordedStore, tenant: &TenantKey, record: &ManagedResource) -> PathBuf {
        store.save(tenant, &absent(record)).unwrap()
    }

    #[test]
    fn saves_privately_and_lists_per_tenant_in_order() {
        let root = tempfile::tempdir().unwrap();
        let store = UnrecordedStore::at(root.path().join("unrecorded"));
        let web = tenant("web");
        let first = save(&store, &web, &record("first", "a"));
        let second = store
            .save(
                &web,
                &ConditionalPut {
                    resource: &record("second", "b"),
                    expected: RecordVersion::Serial(4),
                },
            )
            .unwrap();
        save(&store, &tenant("api"), &record("other", "c"));

        let listed = store.list(&web).unwrap();
        assert_eq!(
            listed.iter().map(|entry| &entry.file).collect::<Vec<_>>(),
            vec![&first, &second]
        );
        assert_eq!(listed[0].record, record("first", "a"));
        assert_eq!(listed[0].expected, RecordVersion::Absent);
        assert_eq!(listed[1].expected, RecordVersion::Serial(4));
        assert!(store.list(&tenant("elsewhere")).unwrap().is_empty());
        assert!(store.has_pending(&web).unwrap());
        assert!(!store.has_pending(&tenant("elsewhere")).unwrap());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&first), 0o600);
            assert_eq!(mode(first.parent().unwrap()), 0o700);
            assert_eq!(mode(store.root()), 0o700);
        }
        // No temporary file is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(first.parent().unwrap())
            .unwrap()
            .filter_map(|entry| {
                let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                name.starts_with('.').then_some(name)
            })
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn the_file_format_is_camel_case_throughout_and_versioned() {
        let root = tempfile::tempdir().unwrap();
        let store = UnrecordedStore::at(root.path());
        let file = save(&store, &tenant("web"), &record("pet", "a"));
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(document["formatVersion"], 2);
        assert_eq!(document["expected"], "absent");
        assert_eq!(document["tenant"]["modulePath"], "example.com/app");
        assert!(document["tenant"].get("environment").is_none());
        let resource = &document["resource"];
        for key in [
            "resourceType",
            "name",
            "providerSource",
            "schemaVersion",
            "private",
            "tainted",
        ] {
            assert!(resource.get(key).is_some(), "{key} missing: {resource}");
        }
        assert!(resource.get("provider_source").is_none());
    }

    #[test]
    fn named_environment_files_never_share_the_legacy_recovery_directory() {
        let root = tempfile::tempdir().unwrap();
        let store = UnrecordedStore::at(root.path().join("unrecorded"));
        let legacy = tenant("web");
        let dev = TenantKey::with_environment("example.com/app", "web", "Dev").unwrap();
        let staging = TenantKey::with_environment("example.com/app", "web", "Staging").unwrap();
        let legacy_file = save(&store, &legacy, &record("pet", "legacy"));
        let dev_file = save(&store, &dev, &record("pet", "dev"));
        let staging_file = save(&store, &staging, &record("pet", "staging"));

        assert_ne!(legacy_file.parent(), dev_file.parent());
        assert_ne!(dev_file.parent(), staging_file.parent());
        assert_eq!(store.list(&legacy).unwrap().len(), 1);
        assert_eq!(store.list(&dev).unwrap().len(), 1);
        assert_eq!(store.list(&staging).unwrap().len(), 1);
        let legacy_document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&legacy_file).unwrap()).unwrap();
        assert!(legacy_document["tenant"].get("environment").is_none());
        let dev_document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&dev_file).unwrap()).unwrap();
        assert_eq!(dev_document["tenant"]["environment"], "Dev");
    }

    #[tokio::test]
    async fn recovery_only_records_the_selected_environment() {
        let root = tempfile::tempdir().unwrap();
        let unrecorded = UnrecordedStore::at(root.path().join("unrecorded"));
        let state = crate::state::MemoryStateStore::new();
        let legacy = tenant("web");
        let dev = TenantKey::with_environment("example.com/app", "web", "Dev").unwrap();
        let staging = TenantKey::with_environment("example.com/app", "web", "Staging").unwrap();
        save(&unrecorded, &legacy, &record("pet", "legacy"));
        save(&unrecorded, &dev, &record("pet", "dev"));
        save(&unrecorded, &staging, &record("pet", "staging"));
        let lock = state.lock(&dev, "recover").await.unwrap();
        let recovered = unrecorded
            .recover(
                &state,
                &dev,
                &RecoverOptions {
                    lock: &lock,
                    overwrite: RecoverOverwrite::IfUnchanged,
                },
            )
            .await
            .unwrap();
        assert_eq!(recovered, vec![ResourceAddress::new("random_pet", "pet")]);
        assert!(unrecorded.list(&dev).unwrap().is_empty());
        assert!(unrecorded.has_pending(&legacy).unwrap());
        assert!(unrecorded.has_pending(&staging).unwrap());
        assert!(state.list(&legacy).await.unwrap().is_empty());
        assert!(state.list(&staging).await.unwrap().is_empty());
        assert_eq!(state.list(&dev).await.unwrap()[0].state["id"], "dev");
    }

    #[test]
    fn malformed_and_foreign_version_files_are_reported_without_their_content() {
        let root = tempfile::tempdir().unwrap();
        let store = UnrecordedStore::at(root.path());
        let web = tenant("web");
        // Create the directories the way the store does.
        let saved = save(&store, &web, &record("pet", "a"));
        std::fs::remove_file(saved).unwrap();
        let directory = store.tenant_directory(&web);
        let write_private = |name: &str, contents: &str| {
            write_new_private_file(&directory.join(name), contents.as_bytes()).unwrap();
        };
        write_private(
            "broken.json",
            r#"{"formatVersion": 2, "tenant": "hunter2"}"#,
        );
        let error = store.list(&web).unwrap_err();
        assert!(
            matches!(error, InfrastructureError::UnrecordedFile { .. }),
            "{error}"
        );
        let message = error.to_string();
        assert!(message.contains("broken.json"), "{message}");
        assert!(!message.contains("hunter2"), "{message}");
        std::fs::remove_file(directory.join("broken.json")).unwrap();

        write_private("old.json", r#"{"version": 1, "secret": "hunter2"}"#);
        let message = store.list(&web).unwrap_err().to_string();
        assert!(message.contains("format version (none)"), "{message}");
        assert!(!message.contains("hunter2"), "{message}");
    }

    #[test]
    fn files_of_another_tenant_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let store = UnrecordedStore::at(root.path());
        let saved = save(&store, &tenant("api"), &record("pet", "a"));
        save(&store, &tenant("web"), &record("pet", "b"));
        let web_directory = store.tenant_directory(&tenant("web"));
        let bytes = std::fs::read(&saved).unwrap();
        write_new_private_file(&web_directory.join("copied.json"), &bytes).unwrap();
        let error = store.list(&tenant("web")).unwrap_err();
        assert!(error.to_string().contains("another tenant"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_and_shared_files_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let store = UnrecordedStore::at(root.path().join("unrecorded"));
        let web = tenant("web");
        let saved = save(&store, &web, &record("pet", "a"));

        // A planted file readable by others is refused.
        std::fs::set_permissions(&saved, std::fs::Permissions::from_mode(0o644)).unwrap();
        let message = store.list(&web).unwrap_err().to_string();
        assert!(message.contains("other users can access it"), "{message}");
        std::fs::set_permissions(&saved, std::fs::Permissions::from_mode(0o600)).unwrap();

        // A symbolic link to a record is refused, not followed.
        let elsewhere = root.path().join("elsewhere.json");
        std::fs::rename(&saved, &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &saved).unwrap();
        let message = store.list(&web).unwrap_err().to_string();
        assert!(message.contains("without following links"), "{message}");
        std::fs::remove_file(&saved).unwrap();

        // A tenant directory that is a symbolic link is refused.
        let directory = store.tenant_directory(&web);
        std::fs::remove_dir_all(&directory).unwrap();
        let target = root.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &directory).unwrap();
        assert!(matches!(
            store.list(&web),
            Err(InfrastructureError::UnrecordedFile { .. })
        ));
        assert!(store.save(&web, &absent(&record("pet", "b"))).is_err());
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn shared_directories_are_made_private_again() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let store = UnrecordedStore::at(root.path().join("unrecorded"));
        let web = tenant("web");
        save(&store, &web, &record("pet", "a"));
        std::fs::set_permissions(store.root(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(store.list(&web).unwrap().len(), 1);
        let mode = std::fs::metadata(store.root())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[tokio::test]
    async fn recover_records_under_the_lock_and_removes_files() {
        let root = tempfile::tempdir().unwrap();
        let unrecorded = UnrecordedStore::at(root.path());
        let web = tenant("web");
        save(&unrecorded, &web, &record("pet", "new"));
        save(&unrecorded, &web, &record("other", "x"));

        let state = MemoryStateStore::new();
        let lock = state.lock(&web, "test").await.unwrap();
        let recovered = unrecorded
            .recover(
                &state,
                &web,
                &RecoverOptions {
                    lock: &lock,
                    overwrite: RecoverOverwrite::IfUnchanged,
                },
            )
            .await
            .unwrap();
        assert_eq!(recovered.len(), 2);
        assert!(unrecorded.list(&web).unwrap().is_empty());
        let rows = state.list(&web).await.unwrap();
        let pet = rows.iter().find(|row| row.address.name == "pet").unwrap();
        assert_eq!(pet.state["id"], "new");
    }

    #[tokio::test]
    async fn recover_refuses_to_overwrite_a_newer_record_unless_forced() {
        let root = tempfile::tempdir().unwrap();
        let unrecorded = UnrecordedStore::at(root.path());
        let web = tenant("web");
        let state = MemoryStateStore::new();
        let lock = state.lock(&web, "test").await.unwrap();
        // The record was saved when nothing was stored; another run has
        // since recorded the resource.
        save(&unrecorded, &web, &record("pet", "saved"));
        state
            .put(&web, &lock, &record("pet", "newer"))
            .await
            .unwrap();
        let options = |overwrite| RecoverOptions {
            lock: &lock,
            overwrite,
        };
        let error = unrecorded
            .recover(&state, &web, &options(RecoverOverwrite::IfUnchanged))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, InfrastructureError::StateChanged { address, .. } if address == "random_pet.pet"),
            "{error}"
        );
        assert!(!error.to_string().contains("hunter2"), "{error}");
        assert_eq!(state.list(&web).await.unwrap()[0].state["id"], "newer");
        assert_eq!(unrecorded.list(&web).unwrap().len(), 1);

        unrecorded
            .recover(&state, &web, &options(RecoverOverwrite::Always))
            .await
            .unwrap();
        assert_eq!(state.list(&web).await.unwrap()[0].state["id"], "saved");
        assert!(unrecorded.list(&web).unwrap().is_empty());
    }

    #[tokio::test]
    async fn recover_accepts_a_record_the_store_already_holds() {
        let root = tempfile::tempdir().unwrap();
        let unrecorded = UnrecordedStore::at(root.path());
        let web = tenant("web");
        let state = MemoryStateStore::new();
        let lock = state.lock(&web, "test").await.unwrap();
        // The write whose failure was reported had in fact committed.
        save(&unrecorded, &web, &record("pet", "same"));
        state
            .put(&web, &lock, &record("pet", "same"))
            .await
            .unwrap();
        let recovered = unrecorded
            .recover(
                &state,
                &web,
                &RecoverOptions {
                    lock: &lock,
                    overwrite: RecoverOverwrite::IfUnchanged,
                },
            )
            .await
            .unwrap();
        assert_eq!(recovered.len(), 1);
        assert!(unrecorded.list(&web).unwrap().is_empty());
    }

    #[tokio::test]
    async fn recover_without_the_lock_keeps_the_files() {
        let root = tempfile::tempdir().unwrap();
        let unrecorded = UnrecordedStore::at(root.path());
        let web = tenant("web");
        save(&unrecorded, &web, &record("pet", "a"));
        let state = MemoryStateStore::new();
        let lock_not_held = StateLock {
            lock_identifier: "not-held".into(),
        };
        assert!(
            unrecorded
                .recover(
                    &state,
                    &web,
                    &RecoverOptions {
                        lock: &lock_not_held,
                        overwrite: RecoverOverwrite::IfUnchanged,
                    },
                )
                .await
                .is_err()
        );
        assert_eq!(unrecorded.list(&web).unwrap().len(), 1);
    }
}
