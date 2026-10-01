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
//! - Each file names its tenant (module, project and environment), the hash
//!   of its normalized state backend, and the version of the stored record
//!   it replaces ([`RecordVersion`]), so [`UnrecordedStore::recover`] writes
//!   it only to that backend while the stored record is still the one it
//!   replaces.
//!
//! Recovery has two independent overrides ([`RecoverOverrides`]), each
//! evaluated per file: overwriting a stored record that changed since the
//! file was saved, and accepting a file saved for a different state backend.
//! One never implies the other. A refused file is reported with its path,
//! the resource address and the specific reason.
//!
//! Files are grouped per tenant so one tenant's leftovers never block
//! another. Planning refuses to run while a tenant has unrecorded changes.
//!
//! The file format is JSON in camelCase throughout, with an explicit
//! `formatVersion` (currently 1) and a `kind` marker that tells a file of a
//! future format apart from one an unreleased development build wrote.
//! Nothing here ever puts a state value in an error message; problems with a
//! file are
//! [`InfrastructureError::UnrecordedFile`] errors naming the file.

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{
    InfrastructureError, Result, UnrecordedFileProblem, describe_json_error, json_error_category,
};
use crate::state::{
    ConditionalPut, ManagedResource, RecordVersion, ResourceAddress, StateLock, StateStore,
};
use crate::tenant::TenantKey;

/// Format version written into every file, and the only one read.
const FILE_FORMAT_VERSION: u32 = 1;

/// The marker every file carries as `kind`. Unreleased development builds
/// wrote format versions 1 to 4 without it, so a file lacking it is told
/// apart from a future format version, and later versions can use any
/// number.
const FILE_KIND: &str = "cuenv-infrastructure-unrecorded-change";

/// The newest format version an unreleased development build wrote.
const LAST_DEVELOPMENT_FORMAT_VERSION: u32 = 4;

/// Largest unrecorded file read, in bytes (64 MiB, the state store's own
/// response limit).
const MAXIMUM_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// The directory holding unrecorded changes for every tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnrecordedStore {
    root: PathBuf,
    backend_identity: Option<String>,
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
    /// Hash of the normalized backend that could not record this change.
    /// Kept private so callers cannot bypass the store's binding check.
    backend_identity: Option<String>,
}

/// What [`UnrecordedStore::recover`] does when the stored record is no
/// longer the one a file replaces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChangedRecord {
    /// Stop with [`InfrastructureError::StateChanged`] naming the file and
    /// the address; the file stays.
    #[default]
    Refuse,
    /// Write the saved record over whatever is stored.
    Overwrite,
}

/// What [`UnrecordedStore::recover`] does with a file saved for a different
/// state backend than the one in use (or without a backend binding).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BackendMismatch {
    /// Refuse with [`InfrastructureError::UnrecordedFile`] naming the file,
    /// the address and what differs; nothing is written.
    #[default]
    Refuse,
    /// Write the file to the backend in use. The record is still written
    /// only while the stored record is the one it replaces, unless
    /// [`ChangedRecord::Overwrite`] is also chosen.
    Accept,
}

/// The two independent overrides of [`UnrecordedStore::recover`]. The
/// default overrides nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoverOverrides {
    /// Overwrite a stored record that changed since the file was saved.
    pub changed_record: ChangedRecord,
    /// Accept a file saved for a different state backend.
    pub backend: BackendMismatch,
}

/// Options for [`UnrecordedStore::recover`].
#[derive(Debug, Clone, Copy)]
pub struct RecoverOptions<'options> {
    /// The tenant's lock, which fences every write.
    pub lock: &'options StateLock,
    /// Which refusals the caller chose to override.
    pub overrides: RecoverOverrides,
}

/// On-disk form of one unrecorded record.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnrecordedFile {
    kind: String,
    format_version: u32,
    tenant: TenantIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    backend_identity: Option<String>,
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
    generation: uuid::Uuid,
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
            generation: record.generation,
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
            generation: self.generation,
        }
    }
}

fn file_problem(path: &Path, problem: impl Into<String>) -> InfrastructureError {
    kinded_file_problem(path, UnrecordedFileProblem::Other, problem)
}

fn kinded_file_problem(
    path: &Path,
    kind: UnrecordedFileProblem,
    problem: impl Into<String>,
) -> InfrastructureError {
    InfrastructureError::UnrecordedFile {
        path: path.display().to_string(),
        problem: problem.into(),
        kind,
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

    /// Use `root` as the directory without binding a backend. This supports
    /// in-memory stores; durable stores must call [`Self::with_backend_identity`].
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            backend_identity: None,
        }
    }

    /// Bind saved files and ordinary recovery to a normalized state backend.
    ///
    /// `identity` must be the lowercase hexadecimal SHA-256 digest returned
    /// by the state backend, computed from its validated, normalized endpoint.
    /// URLs and credentials are never accepted or written into recovery files.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] when the identity is not
    /// a lowercase SHA-256 digest. The error never quotes the supplied value.
    #[must_use = "the bound store must be used to save or recover changes"]
    pub fn with_backend_identity(mut self, identity: &str) -> Result<Self> {
        if !is_backend_identity(identity) {
            return Err(InfrastructureError::configuration(
                "invalid infrastructure state backend identity: expected a lowercase hexadecimal SHA-256 digest",
            ));
        }
        self.backend_identity = Some(identity.to_string());
        Ok(self)
    }

    /// Whether a saved file was saved for a different backend than this
    /// store's binding: its identity is missing or differs. Recovering it
    /// needs [`BackendMismatch::Accept`].
    ///
    /// Two unbound identities match, for in-memory stores. A bound file
    /// does not match an unbound store.
    #[must_use]
    pub fn differs_from_backend(&self, unrecorded: &UnrecordedRecord) -> bool {
        self.backend_identity != unrecorded.backend_identity
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
        let mut resource = FileResource::of(record);
        if resource.generation.is_nil() {
            resource.generation = match put.expected {
                RecordVersion::Generation { generation, .. } => generation,
                RecordVersion::Absent => uuid::Uuid::new_v4(),
            };
        }
        let document = UnrecordedFile {
            kind: FILE_KIND.to_string(),
            format_version: FILE_FORMAT_VERSION,
            tenant: TenantIdentity::of(tenant),
            backend_identity: self.backend_identity.clone(),
            saved_at: saved_at.to_rfc3339(),
            expected: put.expected,
            resource,
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
                    backend_identity: document.backend_identity,
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
    /// Each file is judged on its own, for two independent concerns:
    ///
    /// - Backend binding: a file saved for a different backend than the
    ///   configured one, or than `store` itself, is refused unless
    ///   [`BackendMismatch::Accept`] is chosen. Every file is checked before
    ///   any record is written, so a refusal never leaves a partial recovery.
    /// - Changed record: a record is written only while the stored record is
    ///   still the one it replaces; a stored record that holds the same
    ///   generation, resulting serial and content counts as recorded after a
    ///   lost response. A changed record is refused unless
    ///   [`ChangedRecord::Overwrite`] is chosen, and then only that file is
    ///   written over it.
    ///
    /// Accepting a different backend does not overwrite a changed record, and
    /// overwriting a changed record does not accept a different backend.
    ///
    /// Returns the addresses recorded, in order.
    ///
    /// # Errors
    ///
    /// Stops at the first failure; records written before it stay written
    /// and their files are gone, the rest remain for a later attempt. A
    /// refused backend binding is [`InfrastructureError::UnrecordedFile`]
    /// and a changed record is [`InfrastructureError::StateChanged`]; each
    /// names the file, the address and the reason.
    #[tracing::instrument(skip_all, fields(tenant = %tenant, overrides = ?options.overrides))]
    pub async fn recover(
        &self,
        store: &dyn StateStore,
        tenant: &TenantKey,
        options: &RecoverOptions<'_>,
    ) -> Result<Vec<ResourceAddress>> {
        let unrecorded_records = self.list(tenant)?;
        if options.overrides.backend == BackendMismatch::Refuse {
            let backend_identity = store.recovery_identity();
            for unrecorded in &unrecorded_records {
                if let Some(reason) = self.binding_refusal(unrecorded, backend_identity.as_deref())
                {
                    return Err(kinded_file_problem(
                        &unrecorded.file,
                        UnrecordedFileProblem::Backend,
                        format!(
                            "the saved record of {} {reason}; inspect the saved record and the \
                             configured backend, then either recover it while accepting a different \
                             backend to write it to this backend, or move the file aside to \
                             keep current state",
                            unrecorded.record.address
                        ),
                    ));
                }
            }
        }
        let mut recorded = Vec::new();
        for unrecorded in unrecorded_records {
            let record = &unrecorded.record;
            let put = ConditionalPut {
                resource: record,
                expected: unrecorded.expected,
            };
            match store.put_if_unchanged(tenant, options.lock, &put).await {
                Ok(()) => {}
                Err(InfrastructureError::StateChanged {
                    address,
                    expected,
                    found,
                    ..
                }) => {
                    let already_recorded = store
                        .list(tenant)
                        .await?
                        .iter()
                        .any(|stored| put.is_recorded(stored));
                    if already_recorded {
                        tracing::info!(
                            address = %record.address,
                            "the state store already holds this unrecorded change"
                        );
                    } else if options.overrides.changed_record == ChangedRecord::Overwrite {
                        tracing::warn!(
                            address = %record.address,
                            "overwriting a stored record that changed since the change was saved"
                        );
                        store.put(tenant, options.lock, record).await?;
                    } else {
                        return Err(InfrastructureError::StateChanged {
                            address,
                            expected,
                            found,
                            file: Some(unrecorded.file.display().to_string()),
                        });
                    }
                }
                Err(other) => return Err(other),
            }
            self.remove(&unrecorded)?;
            tracing::info!(address = %record.address, "recovered an unrecorded change");
            recorded.push(unrecorded.record.address);
        }
        Ok(recorded)
    }

    /// Why `unrecorded` cannot be written to `actual` (the identity of the
    /// store in use) without [`BackendMismatch::Accept`], as a clause
    /// continuing "the saved record of ADDRESS ...".
    fn binding_refusal(
        &self,
        unrecorded: &UnrecordedRecord,
        actual: Option<&str>,
    ) -> Option<&'static str> {
        match (
            unrecorded.backend_identity.as_deref(),
            self.backend_identity.as_deref(),
            actual,
        ) {
            (None, None, None) => None,
            (None, _, _) => Some("was saved without a state backend binding"),
            (Some(_), None, _) => {
                Some("was saved for a state backend, but this cuenv has none configured")
            }
            (Some(saved), Some(configured), _) if saved != configured => {
                Some("was saved for a different state backend than the configured one")
            }
            (Some(saved), Some(_), Some(actual)) if saved != actual => {
                Some("was saved for a different state backend than the store in use")
            }
            (Some(_), Some(_), None) => {
                Some("was saved for a state backend, but the store in use has none")
            }
            (Some(_), Some(_), Some(_)) => None,
        }
    }
}

fn is_backend_identity(identity: &str) -> bool {
    identity.len() == 64
        && identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
    let header = serde_json::from_slice::<FileHeader>(&bytes).map_err(|error| {
        kinded_file_problem(
            file,
            UnrecordedFileProblem::Format,
            format!("it is not valid JSON ({})", describe_json_error(&error)),
        )
    })?;
    match (header.kind.as_deref(), header.format_version) {
        (Some(FILE_KIND), Some(FILE_FORMAT_VERSION)) => {}
        // Unreleased development builds wrote format versions 1 to 4 without
        // the marker; no released cuenv wrote any of them.
        (None, Some(version)) if (1..=LAST_DEVELOPMENT_FORMAT_VERSION).contains(&version) => {
            return Err(kinded_file_problem(
                file,
                UnrecordedFileProblem::DevelopmentBuild,
                format!(
                    "it was written by an unreleased development build of cuenv (format version \
                     {version}); this cuenv does not read it"
                ),
            ));
        }
        (_, version) => {
            return Err(kinded_file_problem(
                file,
                UnrecordedFileProblem::Format,
                format!(
                    "it has format version {}, but this cuenv reads version {FILE_FORMAT_VERSION}",
                    version.map_or_else(|| "(none)".to_string(), |version| version.to_string())
                ),
            ));
        }
    }
    let document: UnrecordedFile = serde_json::from_slice(&bytes).map_err(|error| {
        // serde messages can quote the offending value; report the
        // position only.
        kinded_file_problem(
            file,
            UnrecordedFileProblem::Format,
            format!(
                "it is not a valid unrecorded record ({})",
                describe_json_error(&error)
            ),
        )
    })?;
    if document
        .backend_identity
        .as_deref()
        .is_some_and(|identity| !is_backend_identity(identity))
    {
        return Err(file_problem(
            file,
            "it has an invalid state backend identity",
        ));
    }
    if document.resource.generation.is_nil() {
        return Err(file_problem(
            file,
            "it records no insertion generation, so the stored record it replaces cannot be identified",
        ));
    }
    Ok(document)
}

/// Just the marker and the format version of a file.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileHeader {
    #[serde(default)]
    kind: Option<String>,
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
    use crate::state::{
        LockInformation, LockRequest, MemoryStateStore, OwnerClaim, TenantOwner,
        TursoConfiguration, TursoStateStore,
    };

    /// Only the backend binding is overridden.
    fn accept_backend() -> RecoverOverrides {
        RecoverOverrides {
            backend: BackendMismatch::Accept,
            ..RecoverOverrides::default()
        }
    }

    /// Only a changed stored record is overridden.
    fn overwrite_changed() -> RecoverOverrides {
        RecoverOverrides {
            changed_record: ChangedRecord::Overwrite,
            ..RecoverOverrides::default()
        }
    }

    struct BoundMemoryStateStore {
        inner: MemoryStateStore,
        identity: String,
    }

    impl BoundMemoryStateStore {
        fn for_backend(url: &str) -> Self {
            Self {
                inner: MemoryStateStore::new(),
                identity: backend_identity(url),
            }
        }
    }

    #[async_trait::async_trait]
    impl StateStore for BoundMemoryStateStore {
        fn recovery_identity(&self) -> Option<String> {
            Some(self.identity.clone())
        }

        async fn migrate(&self) -> Result<()> {
            self.inner.migrate().await
        }

        async fn list(&self, tenant: &TenantKey) -> Result<Vec<ManagedResource>> {
            self.inner.list(tenant).await
        }

        async fn put(
            &self,
            tenant: &TenantKey,
            lock: &StateLock,
            resource: &ManagedResource,
        ) -> Result<()> {
            self.inner.put(tenant, lock, resource).await
        }

        async fn put_if_unchanged(
            &self,
            tenant: &TenantKey,
            lock: &StateLock,
            put: &ConditionalPut<'_>,
        ) -> Result<()> {
            self.inner.put_if_unchanged(tenant, lock, put).await
        }

        async fn delete(
            &self,
            tenant: &TenantKey,
            lock: &StateLock,
            address: &ResourceAddress,
        ) -> Result<()> {
            self.inner.delete(tenant, lock, address).await
        }

        async fn acquire_lock(
            &self,
            tenant: &TenantKey,
            request: &LockRequest<'_>,
        ) -> Result<StateLock> {
            self.inner.acquire_lock(tenant, request).await
        }

        async fn unlock(&self, tenant: &TenantKey, lock: &StateLock) -> Result<()> {
            self.inner.unlock(tenant, lock).await
        }

        async fn current_lock(&self, tenant: &TenantKey) -> Result<Option<LockInformation>> {
            self.inner.current_lock(tenant).await
        }

        async fn locks(&self) -> Result<Vec<crate::state::TenantLock>> {
            self.inner.locks().await
        }

        async fn force_unlock(&self, tenant: &TenantKey, lock_identifier: &str) -> Result<bool> {
            self.inner.force_unlock(tenant, lock_identifier).await
        }

        async fn owner(&self, tenant: &TenantKey) -> Result<Option<TenantOwner>> {
            self.inner.owner(tenant).await
        }

        async fn claim_owner(
            &self,
            tenant: &TenantKey,
            lock: &StateLock,
            claim: &OwnerClaim<'_>,
        ) -> Result<TenantOwner> {
            self.inner.claim_owner(tenant, lock, claim).await
        }
    }

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
            generation: uuid::Uuid::nil(),
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

    fn backend_identity(url: &str) -> String {
        TursoStateStore::new(TursoConfiguration {
            url: url.to_string(),
            authentication_token: None,
        })
        .unwrap()
        .recovery_identity()
        .unwrap()
    }

    fn bound_store(root: &Path, url: &str) -> UnrecordedStore {
        UnrecordedStore::at(root)
            .with_backend_identity(&backend_identity(url))
            .unwrap()
    }

    #[test]
    fn saves_privately_and_lists_per_tenant_in_order() {
        let root = tempfile::tempdir().unwrap();
        let store = UnrecordedStore::at(root.path().join("unrecorded"));
        let web = tenant("web");
        let first = save(&store, &web, &record("first", "a"));
        let replaced = RecordVersion::Generation {
            generation: uuid::Uuid::from_u128(4),
            serial: 4,
        };
        let second = store
            .save(
                &web,
                &ConditionalPut {
                    resource: &record("second", "b"),
                    expected: replaced,
                },
            )
            .unwrap();
        save(&store, &tenant("api"), &record("other", "c"));

        let listed = store.list(&web).unwrap();
        assert_eq!(
            listed.iter().map(|entry| &entry.file).collect::<Vec<_>>(),
            vec![&first, &second]
        );
        assert!(listed[0].record.same_content(&record("first", "a")));
        assert!(!listed[0].record.generation.is_nil());
        assert_eq!(listed[0].expected, RecordVersion::Absent);
        assert_eq!(listed[1].expected, replaced);
        // A record saved over an existing one keeps that insertion's identity.
        assert_eq!(listed[1].record.generation, uuid::Uuid::from_u128(4));
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
        let store = bound_store(root.path(), "libsql://state-a.turso.io");
        let file = save(&store, &tenant("web"), &record("pet", "a"));
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(document["formatVersion"], FILE_FORMAT_VERSION);
        assert!(is_backend_identity(
            document["backendIdentity"].as_str().unwrap()
        ));
        assert!(!document.to_string().contains("state-a.turso.io"));
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
    fn backend_identity_normalizes_transport_aliases_and_url_representation() {
        let root = tempfile::tempdir().unwrap();
        let encrypted = bound_store(root.path(), "https://state-a.turso.io/prefix");
        for url in [
            "libsql://STATE-A.TURSO.IO/prefix/",
            "WSS://state-a.turso.io:443/prefix///",
            "https://state-a.turso.io/other/../prefix/",
        ] {
            assert_eq!(encrypted, bound_store(root.path(), url), "{url}");
        }
        assert_eq!(
            bound_store(root.path(), "ws://LOCALHOST:80/prefix/"),
            bound_store(root.path(), "http://localhost/prefix")
        );
        assert_eq!(
            bound_store(root.path(), "libsql://[2001:0DB8:0:0:0:0:0:1]:443"),
            bound_store(root.path(), "https://[2001:db8::1]/")
        );
        // The machine's own loopback is one backend however it is spelled,
        // so a file saved through `localhost` recovers through `127.0.0.1`.
        let local = bound_store(root.path(), "http://localhost:8080/state");
        for url in [
            "http://127.0.0.1:8080/state",
            "http://[::1]:8080/state/",
            "ws://LocalHost:8080/state",
        ] {
            assert_eq!(local, bound_store(root.path(), url), "{url}");
        }
        for url in [
            "http://localhost:8081/state",
            "http://127.0.0.2:8080/state",
            "http://localhost:8080/other",
        ] {
            assert_ne!(local, bound_store(root.path(), url), "{url}");
        }
        assert_ne!(
            encrypted,
            bound_store(root.path(), "https://state-a.turso.io/another-prefix")
        );
    }

    #[test]
    fn backend_binding_rejects_raw_urls_and_malformed_hashes_without_quoting_them() {
        let root = tempfile::tempdir().unwrap();
        for identity in [
            "https://user:recovery-secret@state-a.turso.io",
            "https://state-a.turso.io?token=recovery-secret",
            "recovery-secret",
            "ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789",
        ] {
            let error = UnrecordedStore::at(root.path())
                .with_backend_identity(identity)
                .unwrap_err();
            let message = error.to_string();
            assert!(message.contains("SHA-256"), "{message}");
            assert!(!message.contains(identity), "{message}");
            assert!(!message.contains("recovery-secret"), "{message}");
        }
    }

    #[test]
    fn a_malformed_saved_backend_identity_is_reported_without_its_content() {
        let root = tempfile::tempdir().unwrap();
        let store = bound_store(root.path(), "libsql://state-a.turso.io");
        let tenant = tenant("web");
        let file = save(&store, &tenant, &record("pet", "pending"));
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        document["backendIdentity"] =
            serde_json::json!("https://user:recovery-secret@state-a.turso.io");
        std::fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
        let message = store.list(&tenant).unwrap_err().to_string();
        assert!(
            message.contains("invalid state backend identity"),
            "{message}"
        );
        assert!(!message.contains("recovery-secret"), "{message}");
        assert!(!message.contains("state-a.turso.io"), "{message}");
        assert!(file.exists());
    }

    #[tokio::test]
    async fn recovery_refuses_an_absent_record_saved_for_another_empty_backend_until_accepted() {
        for environment in [None, Some("Dev")] {
            let root = tempfile::tempdir().unwrap();
            let backend_a = bound_store(root.path(), "libsql://state-a.turso.io");
            let backend_b = bound_store(root.path(), "https://state-b.turso.io");
            let state_a = BoundMemoryStateStore::for_backend("libsql://state-a.turso.io");
            let state_b = BoundMemoryStateStore::for_backend("libsql://state-b.turso.io");
            let tenant = environment.map_or_else(
                || tenant("web"),
                |name| TenantKey::with_environment("example.com/app", "web", name).unwrap(),
            );
            let saved = record("pet", "created-in-backend-a");
            let file = save(&backend_a, &tenant, &saved);
            let listed = backend_b.list(&tenant).unwrap();
            assert_eq!(listed[0].expected, RecordVersion::Absent);
            assert!(backend_b.differs_from_backend(&listed[0]));
            assert!(!backend_a.differs_from_backend(&listed[0]));
            let lock = state_b.lock(&tenant, "recover").await.unwrap();
            let error = backend_b
                .recover(
                    &state_b,
                    &tenant,
                    &RecoverOptions {
                        lock: &lock,
                        overrides: RecoverOverrides::default(),
                    },
                )
                .await
                .unwrap_err();
            assert!(matches!(error, InfrastructureError::UnrecordedFile { .. }));
            // The refusal names the file, the address and the reason, and
            // offers the backend override, not a general force.
            let message = error.to_string();
            assert!(message.contains(&file.display().to_string()), "{message}");
            assert!(message.contains("random_pet.pet"), "{message}");
            assert!(
                message.contains("different state backend than the configured one"),
                "{message}"
            );
            assert!(
                message.contains("accepting a different backend"),
                "{message}"
            );
            assert!(!message.contains("cuenv infrastructure"), "{message}");
            assert!(!message.contains("--force"), "{message}");
            assert!(!message.contains("hunter2"), "{message}");
            assert!(file.exists());
            assert!(state_b.list(&tenant).await.unwrap().is_empty());
            assert!(state_a.list(&tenant).await.unwrap().is_empty());

            let recovered = backend_b
                .recover(
                    &state_b,
                    &tenant,
                    &RecoverOptions {
                        lock: &lock,
                        overrides: accept_backend(),
                    },
                )
                .await
                .unwrap();
            assert_eq!(recovered, vec![saved.address.clone()]);
            assert!(state_b.list(&tenant).await.unwrap()[0].same_content(&saved));
            assert!(state_a.list(&tenant).await.unwrap().is_empty());
            assert!(!file.exists());
        }
    }

    #[tokio::test]
    async fn ordinary_recovery_accepts_an_equivalent_normalized_backend() {
        let root = tempfile::tempdir().unwrap();
        let backend = bound_store(root.path(), "libsql://STATE-A.TURSO.IO/prefix/");
        let equivalent = bound_store(root.path(), "https://state-a.turso.io:443/prefix");
        let state = BoundMemoryStateStore::for_backend("https://state-a.turso.io/prefix");
        let tenant = tenant("web");
        let saved = record("pet", "pending");
        let file = save(&backend, &tenant, &saved);
        let listed = equivalent.list(&tenant).unwrap();
        assert!(!equivalent.differs_from_backend(&listed[0]));
        let lock = state.lock(&tenant, "recover").await.unwrap();
        equivalent
            .recover(
                &state,
                &tenant,
                &RecoverOptions {
                    lock: &lock,
                    overrides: RecoverOverrides::default(),
                },
            )
            .await
            .unwrap();
        assert!(state.list(&tenant).await.unwrap()[0].same_content(&saved));
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn bound_recovery_requires_the_backend_override_for_files_without_a_binding() {
        let root = tempfile::tempdir().unwrap();
        let unbound = UnrecordedStore::at(root.path());
        let bound = bound_store(root.path(), "libsql://state-a.turso.io");
        let state = BoundMemoryStateStore::for_backend("libsql://state-a.turso.io");
        let tenant = tenant("web");
        let saved = record("pet", "pending");
        let file = save(&unbound, &tenant, &saved);
        let listed = bound.list(&tenant).unwrap();
        assert!(bound.differs_from_backend(&listed[0]));
        let lock = state.lock(&tenant, "recover").await.unwrap();
        // Overwriting a changed record is a different concern and does not
        // accept an unbound file.
        for overrides in [RecoverOverrides::default(), overwrite_changed()] {
            let error = bound
                .recover(
                    &state,
                    &tenant,
                    &RecoverOptions {
                        lock: &lock,
                        overrides,
                    },
                )
                .await
                .unwrap_err();
            let message = error.to_string();
            assert!(
                message.contains("without a state backend binding"),
                "{message}"
            );
            assert!(message.contains("random_pet.pet"), "{message}");
            assert!(message.contains(&file.display().to_string()), "{message}");
            assert!(state.list(&tenant).await.unwrap().is_empty());
            assert!(file.exists());
        }
        bound
            .recover(
                &state,
                &tenant,
                &RecoverOptions {
                    lock: &lock,
                    overrides: accept_backend(),
                },
            )
            .await
            .unwrap();
        assert!(state.list(&tenant).await.unwrap()[0].same_content(&saved));
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn a_changed_record_override_does_not_accept_another_backend_and_vice_versa() {
        let root = tempfile::tempdir().unwrap();
        let backend_a = bound_store(root.path(), "libsql://state-a.turso.io");
        let backend_b = bound_store(root.path(), "libsql://state-b.turso.io");
        let state_b = BoundMemoryStateStore::for_backend("libsql://state-b.turso.io");
        let tenant = tenant("web");
        // Saved for backend A when no record was stored there; backend B
        // has since recorded another record at the address.
        let file = save(&backend_a, &tenant, &record("pet", "saved"));
        let lock = state_b.lock(&tenant, "recover").await.unwrap();
        state_b
            .put(&tenant, &lock, &record("pet", "stored-in-b"))
            .await
            .unwrap();
        let recover = |overrides| {
            let backend_b = &backend_b;
            let state_b = &state_b;
            let tenant = &tenant;
            let lock = &lock;
            async move {
                backend_b
                    .recover(state_b, tenant, &RecoverOptions { lock, overrides })
                    .await
            }
        };

        // The changed-record override alone leaves the backend refusal.
        let error = recover(overwrite_changed()).await.unwrap_err();
        assert!(matches!(error, InfrastructureError::UnrecordedFile { .. }));
        assert!(
            error.to_string().contains("different state backend"),
            "{error}"
        );
        // The backend override alone leaves the changed-record refusal, and
        // it names the file too.
        let error = recover(accept_backend()).await.unwrap_err();
        assert!(
            matches!(&error, InfrastructureError::StateChanged { address, file: Some(path), .. }
                if address == "random_pet.pet" && path == &file.display().to_string()),
            "{error:?}"
        );
        assert!(
            error.to_string().starts_with(&file.display().to_string()),
            "{error}"
        );
        assert_eq!(
            state_b.list(&tenant).await.unwrap()[0].state["id"],
            "stored-in-b"
        );
        assert!(file.exists());

        // Both together write it.
        let both = RecoverOverrides {
            changed_record: ChangedRecord::Overwrite,
            backend: BackendMismatch::Accept,
        };
        recover(both).await.unwrap();
        assert_eq!(state_b.list(&tenant).await.unwrap()[0].state["id"], "saved");
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn the_changed_record_override_applies_only_to_the_files_that_need_it() {
        let root = tempfile::tempdir().unwrap();
        let unrecorded = UnrecordedStore::at(root.path());
        let state = MemoryStateStore::new();
        let tenant = tenant("web");
        let lock = state.lock(&tenant, "recover").await.unwrap();
        let clean = save(&unrecorded, &tenant, &record("clean", "saved-clean"));
        let changed = save(&unrecorded, &tenant, &record("changed", "saved-changed"));
        state
            .put(&tenant, &lock, &record("changed", "stored-changed"))
            .await
            .unwrap();
        let options = |overrides| RecoverOptions {
            lock: &lock,
            overrides,
        };

        // Without the override the unchanged file is recorded and the
        // changed one is refused with its own path.
        let error = unrecorded
            .recover(&state, &tenant, &options(RecoverOverrides::default()))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, InfrastructureError::StateChanged { address, file: Some(path), .. }
                if address == "random_pet.changed" && path == &changed.display().to_string()),
            "{error:?}"
        );
        assert!(!clean.exists());
        assert!(changed.exists());
        let stored = state.list(&tenant).await.unwrap();
        assert_eq!(stored.len(), 2);

        // With it, only that file overwrites; its neighbour was already
        // recorded the ordinary way.
        let recovered = unrecorded
            .recover(&state, &tenant, &options(overwrite_changed()))
            .await
            .unwrap();
        assert_eq!(
            recovered,
            vec![ResourceAddress::new("random_pet", "changed")]
        );
        let stored = state.list(&tenant).await.unwrap();
        let state_of = |name: &str| {
            stored
                .iter()
                .find(|record| record.address.name == name)
                .unwrap()
                .state["id"]
                .clone()
        };
        assert_eq!(state_of("clean"), "saved-clean");
        assert_eq!(state_of("changed"), "saved-changed");
    }

    #[tokio::test]
    async fn backend_binding_is_checked_for_all_files_before_any_recovery_write() {
        let root = tempfile::tempdir().unwrap();
        let backend_a = bound_store(root.path(), "libsql://state-a.turso.io");
        let backend_b = bound_store(root.path(), "libsql://state-b.turso.io");
        let state = BoundMemoryStateStore::for_backend("libsql://state-a.turso.io");
        let tenant = tenant("web");
        let first = save(&backend_a, &tenant, &record("first", "same-backend"));
        let second = save(&backend_b, &tenant, &record("second", "other-backend"));
        let lock = state.lock(&tenant, "recover").await.unwrap();
        backend_a
            .recover(
                &state,
                &tenant,
                &RecoverOptions {
                    lock: &lock,
                    overrides: RecoverOverrides::default(),
                },
            )
            .await
            .unwrap_err();
        assert!(state.list(&tenant).await.unwrap().is_empty());
        assert!(first.exists());
        assert!(second.exists());
    }

    #[tokio::test]
    async fn ordinary_recovery_checks_the_actual_backend_even_when_the_wrapper_matches() {
        let root = tempfile::tempdir().unwrap();
        let backend_a = bound_store(root.path(), "libsql://state-a.turso.io");
        let state_b = BoundMemoryStateStore::for_backend("libsql://state-b.turso.io");
        let tenant = tenant("web");
        let saved = record("pet", "pending");
        let file = save(&backend_a, &tenant, &saved);
        let listed = backend_a.list(&tenant).unwrap();
        assert!(!backend_a.differs_from_backend(&listed[0]));
        let lock = state_b.lock(&tenant, "recover").await.unwrap();
        let error = backend_a
            .recover(
                &state_b,
                &tenant,
                &RecoverOptions {
                    lock: &lock,
                    overrides: RecoverOverrides::default(),
                },
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("different state backend than the store in use")
        );
        assert!(state_b.list(&tenant).await.unwrap().is_empty());
        assert!(file.exists());
        backend_a
            .recover(
                &state_b,
                &tenant,
                &RecoverOptions {
                    lock: &lock,
                    overrides: accept_backend(),
                },
            )
            .await
            .unwrap();
        assert!(state_b.list(&tenant).await.unwrap()[0].same_content(&saved));
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn a_bound_file_cannot_be_recovered_ordinarily_into_an_unbound_actual_store() {
        let root = tempfile::tempdir().unwrap();
        let bound = bound_store(root.path(), "libsql://state-a.turso.io");
        let state = MemoryStateStore::new();
        let tenant = tenant("web");
        let file = save(&bound, &tenant, &record("pet", "pending"));
        let lock = state.lock(&tenant, "recover").await.unwrap();
        let error = bound
            .recover(
                &state,
                &tenant,
                &RecoverOptions {
                    lock: &lock,
                    overrides: RecoverOverrides::default(),
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("state backend"));
        assert!(state.list(&tenant).await.unwrap().is_empty());
        assert!(file.exists());
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
    async fn recovery_refuses_a_deleted_and_recreated_record_at_the_same_serial() {
        for environment in [None, Some("Dev")] {
            let root = tempfile::tempdir().unwrap();
            let unrecorded = UnrecordedStore::at(root.path());
            let state = MemoryStateStore::new();
            let tenant = environment.map_or_else(
                || tenant("web"),
                |name| TenantKey::with_environment("example.com/app", "web", name).unwrap(),
            );
            let lock = state.lock(&tenant, "test").await.unwrap();
            let original = record("pet", "original");
            state.put(&tenant, &lock, &original).await.unwrap();
            let previous = state.list(&tenant).await.unwrap()[0].clone();
            let saved = record("pet", "pending");
            unrecorded
                .save(
                    &tenant,
                    &ConditionalPut {
                        resource: &saved,
                        expected: RecordVersion::of(Some(&previous)),
                    },
                )
                .unwrap();
            state
                .delete(&tenant, &lock, &original.address)
                .await
                .unwrap();
            state
                .put(&tenant, &lock, &record("pet", "recreated"))
                .await
                .unwrap();
            let recreated = state.list(&tenant).await.unwrap()[0].clone();
            assert_eq!(previous.serial, recreated.serial);
            assert_ne!(previous.generation, recreated.generation);
            let error = unrecorded
                .recover(
                    &state,
                    &tenant,
                    &RecoverOptions {
                        lock: &lock,
                        overrides: RecoverOverrides::default(),
                    },
                )
                .await
                .unwrap_err();
            assert!(matches!(error, InfrastructureError::StateChanged { .. }));
            assert_eq!(state.list(&tenant).await.unwrap(), vec![recreated]);
            assert_eq!(unrecorded.list(&tenant).unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn recovery_refuses_an_independent_identical_insertion() {
        let root = tempfile::tempdir().unwrap();
        let unrecorded = UnrecordedStore::at(root.path());
        let state = MemoryStateStore::new();
        let tenant = tenant("web");
        let saved = record("pet", "same");
        let file = save(&unrecorded, &tenant, &saved);
        let pending = unrecorded.list(&tenant).unwrap()[0].record.clone();
        let lock = state.lock(&tenant, "test").await.unwrap();
        state.put(&tenant, &lock, &pending).await.unwrap();
        let independent = state.list(&tenant).await.unwrap()[0].clone();
        assert_ne!(pending.generation, independent.generation);
        assert!(pending.same_content(&independent));
        let error = unrecorded
            .recover(
                &state,
                &tenant,
                &RecoverOptions {
                    lock: &lock,
                    overrides: RecoverOverrides::default(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, InfrastructureError::StateChanged { .. }));
        assert_eq!(state.list(&tenant).await.unwrap(), vec![independent]);
        assert!(file.exists());
    }

    #[tokio::test]
    async fn recovery_acknowledges_only_the_same_insertion_generation() {
        let root = tempfile::tempdir().unwrap();
        let unrecorded = UnrecordedStore::at(root.path());
        let state = MemoryStateStore::new();
        let tenant = tenant("web");
        let file = save(&unrecorded, &tenant, &record("pet", "same"));
        let pending = unrecorded.list(&tenant).unwrap()[0].record.clone();
        let lock = state.lock(&tenant, "test").await.unwrap();
        let put = ConditionalPut {
            resource: &pending,
            expected: RecordVersion::Absent,
        };
        state.put_if_unchanged(&tenant, &lock, &put).await.unwrap();
        state.put_if_unchanged(&tenant, &lock, &put).await.unwrap();
        assert_eq!(
            state.list(&tenant).await.unwrap()[0].generation,
            pending.generation
        );
        unrecorded
            .recover(
                &state,
                &tenant,
                &RecoverOptions {
                    lock: &lock,
                    overrides: RecoverOverrides::default(),
                },
            )
            .await
            .unwrap();
        assert!(!file.exists());
        assert_eq!(state.list(&tenant).await.unwrap()[0].serial, 1);
    }

    #[test]
    fn only_format_version_one_is_read() {
        let root = tempfile::tempdir().unwrap();
        let unrecorded = UnrecordedStore::at(root.path());
        let tenant = tenant("web");
        let file = save(&unrecorded, &tenant, &record("pet", "pending"));
        let original = std::fs::read(&file).unwrap();
        let document: serde_json::Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(document["formatVersion"], 1);
        assert_eq!(unrecorded.list(&tenant).unwrap().len(), 1);

        // Earlier layouts were never released and have no reader: the file
        // is refused as another format, and left in place.
        for version in [0, 2, 3, 4] {
            let mut other = document.clone();
            other["formatVersion"] = serde_json::json!(version);
            std::fs::write(&file, serde_json::to_vec(&other).unwrap()).unwrap();
            let message = unrecorded.list(&tenant).unwrap_err().to_string();
            assert!(
                message.contains(&format!(
                    "format version {version}, but this cuenv reads version 1"
                )),
                "{message}"
            );
            assert!(message.contains(&file.display().to_string()), "{message}");
            assert!(!message.contains("hunter2"), "{message}");
            assert!(file.exists());
        }

        // Unreleased development builds wrote versions 1 to 4 without the
        // `kind` marker; they are named as such, never as a newer format,
        // and what names a remedy for a backend binding does not apply.
        for version in 1..=4 {
            let mut development = document.clone();
            development["formatVersion"] = serde_json::json!(version);
            development.as_object_mut().unwrap().remove("kind");
            std::fs::write(&file, serde_json::to_vec(&development).unwrap()).unwrap();
            let error = unrecorded.list(&tenant).unwrap_err();
            let message = error.to_string();
            assert!(
                message.contains("written by an unreleased development build of cuenv"),
                "{message}"
            );
            assert!(!message.contains("newer"), "{message}");
            assert!(
                matches!(
                    error,
                    InfrastructureError::UnrecordedFile {
                        kind: UnrecordedFileProblem::DevelopmentBuild,
                        ..
                    }
                ),
                "{message}"
            );
            assert!(file.exists());
        }
        // The marker with another version is a format problem, and a file
        // without the marker or a version is not a cuenv file at all.
        let mut future = document.clone();
        future["formatVersion"] = serde_json::json!(5);
        std::fs::write(&file, serde_json::to_vec(&future).unwrap()).unwrap();
        assert!(matches!(
            unrecorded.list(&tenant).unwrap_err(),
            InfrastructureError::UnrecordedFile {
                kind: UnrecordedFileProblem::Format,
                ..
            }
        ));

        // A serial-only expectation, or a record without an insertion
        // generation, cannot identify what the file replaces.
        let mut serial_only = document.clone();
        serial_only["expected"] = serde_json::json!({"serial": 1});
        std::fs::write(&file, serde_json::to_vec(&serial_only).unwrap()).unwrap();
        let message = unrecorded.list(&tenant).unwrap_err().to_string();
        assert!(
            message.contains("not a valid unrecorded record"),
            "{message}"
        );
        let mut no_generation = document.clone();
        no_generation["resource"]
            .as_object_mut()
            .unwrap()
            .remove("generation");
        std::fs::write(&file, serde_json::to_vec(&no_generation).unwrap()).unwrap();
        let message = unrecorded.list(&tenant).unwrap_err().to_string();
        assert!(
            message.contains("not a valid unrecorded record"),
            "{message}"
        );
        let mut nil_generation = document;
        nil_generation["resource"]["generation"] = serde_json::json!(uuid::Uuid::nil());
        std::fs::write(&file, serde_json::to_vec(&nil_generation).unwrap()).unwrap();
        let message = unrecorded.list(&tenant).unwrap_err().to_string();
        assert!(message.contains("no insertion generation"), "{message}");

        std::fs::write(&file, original).unwrap();
        assert_eq!(unrecorded.list(&tenant).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_saved_file_carries_its_tenant_environment_generation_and_binding() {
        let root = tempfile::tempdir().unwrap();
        let store = bound_store(root.path(), "libsql://state-a.turso.io");
        let dev = TenantKey::with_environment("example.com/app", "web", "Dev").unwrap();
        let state = MemoryStateStore::new();
        let lock = state.lock(&dev, "test").await.unwrap();
        state
            .put(&dev, &lock, &record("pet", "stored"))
            .await
            .unwrap();
        let previous = state.list(&dev).await.unwrap()[0].clone();
        let file = store
            .save(
                &dev,
                &ConditionalPut {
                    resource: &record("pet", "pending"),
                    expected: RecordVersion::of(Some(&previous)),
                },
            )
            .unwrap();
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(document["formatVersion"], 1);
        assert_eq!(document["tenant"]["environment"], "Dev");
        assert!(is_backend_identity(
            document["backendIdentity"].as_str().unwrap()
        ));
        assert_eq!(
            document["resource"]["generation"],
            previous.generation.to_string()
        );
        assert_eq!(
            document["expected"]["generation"]["generation"],
            previous.generation.to_string()
        );
        assert_eq!(
            document["expected"]["generation"]["serial"],
            previous.serial
        );
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
                    overrides: RecoverOverrides::default(),
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
            r#"{"kind": "cuenv-infrastructure-unrecorded-change", "formatVersion": 1, "tenant": "hunter2"}"#,
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
                    overrides: RecoverOverrides::default(),
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
    async fn recover_refuses_to_overwrite_a_newer_record_unless_overridden() {
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
        let options = |overrides| RecoverOptions {
            lock: &lock,
            overrides,
        };
        let error = unrecorded
            .recover(&state, &web, &options(RecoverOverrides::default()))
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
            .recover(&state, &web, &options(overwrite_changed()))
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
        let pending = unrecorded.list(&web).unwrap()[0].record.clone();
        state
            .put_if_unchanged(
                &web,
                &lock,
                &ConditionalPut {
                    resource: &pending,
                    expected: RecordVersion::Absent,
                },
            )
            .await
            .unwrap();
        let recovered = unrecorded
            .recover(
                &state,
                &web,
                &RecoverOptions {
                    lock: &lock,
                    overrides: RecoverOverrides::default(),
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
                        overrides: RecoverOverrides::default(),
                    },
                )
                .await
                .is_err()
        );
        assert_eq!(unrecorded.list(&web).unwrap().len(), 1);
    }
}
