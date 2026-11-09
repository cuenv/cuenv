//! Cross-process coordination for hook executions and recovery.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::persisted::PersistedHookState;
use super::{HookExecutionState, StateManager};
use crate::types::{ExecutionStatus, Hook, HookExecutionConfig};
use crate::{Error, Result};

static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);
const MAX_BROKER_LEASES: usize = 32;
const MAX_METADATA_BYTES: u64 = 16 * 1024 * 1024;

/// Identifies one reserved hook run, including its original directory key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookRunIdentity {
    /// Full SHA-256 key of the canonical project directory.
    pub directory_key: String,
    /// Hash of the project directory and hook configuration.
    pub instance_hash: String,
    /// Unique execution identifier; nil identifies a legacy execution.
    pub run_id: Uuid,
}

/// The lifecycle phase recorded for a reserved directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HookReservationPhase {
    /// The directory is reserved, but the worker may not have started.
    Starting,
    /// The worker joined the reservation.
    Running,
    /// A worker is starting a child; interruption requires explicit recovery.
    ChildStarting,
    /// Cancellation was requested and the worker has not acknowledged stopping.
    Cancelling,
    /// A legacy running state has no reservation or run identifier.
    LegacyRunning,
    /// A legacy terminal snapshot may contain plaintext and needs recovery.
    LegacyState,
    /// An active state has lost its durable reservation.
    Orphaned,
    /// A broker lease belongs to another or an unavailable network namespace.
    ForeignBroker,
}

/// Durable, non-secret information needed to inspect or recover a hook run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookReservation {
    /// Canonical directory when the run was reserved.
    pub directory: PathBuf,
    /// Exact identity required for recovery.
    pub identity: HookRunIdentity,
    /// Shell or CI session owning the run; nil denotes a legacy reservation.
    #[serde(default)]
    pub session_id: Uuid,
    /// Current lifecycle phase.
    pub phase: HookReservationPhase,
    /// Last recorded worker PID, for diagnosis only.
    pub worker_pid: Option<u32>,
    /// Last recorded direct-child PID, for diagnosis only.
    pub child_pid: Option<u32>,
}

/// Result of recovering an exact, operator-drained run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookRecoveryOutcome {
    /// The matching reservation was recovered.
    Recovered,
    /// No matching reservation or legacy state exists.
    NotFound,
    /// The directory now belongs to a different run.
    Stale,
}

/// Result of a conditional worker transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookRunUpdate {
    /// The update was committed.
    Applied,
    /// This run was asked to stop.
    CancelRequested,
    /// This run no longer owns the directory.
    Superseded,
}

/// Controls whether starting a run may replace an existing session result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookStartMode {
    /// Start only when no matching completed generation has appeared.
    IfMissing,
    /// Explicitly replace this session's completed generation.
    Force,
}

/// Result of atomically checking and reserving a hook run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookRunStart {
    /// A fresh run owns the directory reservation.
    Started(HookRunIdentity),
    /// Another caller already published the matching session generation.
    AlreadyPresent,
}

/// Records the lifecycle of the direct child of a hook run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookChildUpdate {
    /// The exact child has been stopped and reaped.
    Reaped(u32),
}

pub enum HookChildStart {
    Started(tokio::process::Child),
    Failed(String),
    CancelRequested,
    Superseded,
}

enum HookSpawnEvent {
    Authorized,
    Spawned(u32),
}

/// Hooks and state handed to the unique supervisor for a run.
#[derive(Clone)]
pub struct HookWorkerStart {
    /// Persisted execution state for the exact run.
    pub state: HookExecutionState,
    /// Hooks reserved for this worker.
    pub hooks: Vec<Hook>,
    /// Execution settings reserved for this worker.
    pub config: HookExecutionConfig,
}

/// Result of joining a supervisor to its exact run.
#[derive(Clone)]
pub enum HookRunJoin {
    /// The worker owns the reservation and received its payload.
    Joined(Box<HookWorkerStart>),
    /// Cancellation was requested before the worker started.
    CancelRequested,
    /// The run no longer owns the reservation.
    Superseded,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupervisorPayload {
    hooks: Vec<Hook>,
    config: HookExecutionConfig,
}

#[derive(Serialize)]
pub struct SupervisorPayloadRef<'payload> {
    pub hooks: &'payload [Hook],
    pub config: &'payload HookExecutionConfig,
}

pub const MAX_SUPERVISOR_PAYLOAD: usize = 16 * 1024 * 1024;

async fn read_supervisor_payload(
    input: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<SupervisorPayload> {
    use tokio::io::AsyncReadExt;

    let length = input
        .read_u32()
        .await
        .map_err(|_| Error::configuration("Cannot read supervisor payload"))?
        as usize;
    if length > MAX_SUPERVISOR_PAYLOAD {
        return Err(Error::configuration("Hook supervisor payload is too large"));
    }
    let mut bytes = vec![0; length];
    input
        .read_exact(&mut bytes)
        .await
        .map_err(|_| Error::configuration("Cannot read supervisor payload"))?;
    let payload: SupervisorPayload = serde_json::from_slice(&bytes)
        .map_err(|_| Error::configuration("Invalid supervisor payload"))?;
    if payload.config.state_dir.is_some() {
        return Err(Error::configuration(
            "Supervisor payload must not contain a state directory",
        ));
    }
    Ok(payload)
}

struct LockedState {
    directory: PathBuf,
    _lock: File,
}

enum StateRecord {
    Current(HookExecutionState),
    Legacy(HookExecutionState),
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrokerLease {
    identity: HookRunIdentity,
    session_id: Uuid,
    #[cfg(target_os = "linux")]
    namespace: super::broker::BrokerNamespace,
}

fn io_error(source: std::io::Error, path: &Path, operation: &str) -> Error {
    Error::Io {
        source,
        path: Some(path.to_path_buf().into_boxed_path()),
        operation: operation.to_string(),
    }
}

fn no_follow(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
}

fn open_optional(path: &Path) -> Result<Option<File>> {
    let mut options = OpenOptions::new();
    options.read(true);
    no_follow(&mut options);
    match options.open(path) {
        Ok(file) => {
            let metadata = file
                .metadata()
                .map_err(|error| io_error(error, path, "inspect"))?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(Error::configuration(format!(
                    "Hook state file {} must be a regular file",
                    path.display()
                )));
            }
            Ok(Some(file))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(error, path, "open")),
    }
}

fn read_optional<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let Some(file) = open_optional(path)? else {
        return Ok(None);
    };
    let mut contents = Vec::new();
    file.take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|error| io_error(error, path, "read"))?;
    if u64::try_from(contents.len()).unwrap_or(u64::MAX) > MAX_METADATA_BYTES {
        return Err(Error::configuration(
            "Hook state file exceeds the 16 MiB metadata limit",
        ));
    }
    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(|error| {
            Error::serialization(format!(
                "Invalid hook state file {}: {error}",
                path.display()
            ))
        })
}

struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

fn write_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        Error::serialization(format!("Failed to serialize hook state: {error}"))
    })?;
    let (mut file, mut temporary) = loop {
        let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let temporary = path.with_extension(format!("{}.{}.tmp", std::process::id(), sequence));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        no_follow(&mut options);
        match options.open(&temporary) {
            Ok(file) => break (file, TemporaryFile(temporary)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(error, &temporary, "create temporary file")),
        }
    };
    file.write_all(&bytes)
        .map_err(|error| io_error(error, &temporary.0, "write"))?;
    file.sync_all()
        .map_err(|error| io_error(error, &temporary.0, "sync"))?;
    drop(file);
    std::fs::rename(&temporary.0, path).map_err(|error| io_error(error, path, "rename"))?;
    temporary.0.clear();
    Ok(())
}

fn validate_directory_key(key: &str) -> Result<()> {
    if key.len() != 64
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(Error::configuration("Invalid hook directory key"));
    }
    Ok(())
}

fn validate_session(session_id: Uuid) -> Result<()> {
    if session_id.is_nil() || session_id.get_version_num() != 4 {
        return Err(Error::configuration(
            "Hook session must be a non-nil version-4 UUID",
        ));
    }
    Ok(())
}

fn directory_key(directory: &Path) -> Result<String> {
    let directory = directory
        .to_str()
        .ok_or_else(|| Error::configuration("Hook directory path must be UTF-8"))?;
    Ok(hex::encode(Sha256::digest(directory.as_bytes())))
}

fn canonical_directory(directory: &Path) -> Result<PathBuf> {
    let path = std::fs::canonicalize(directory)
        .map_err(|error| io_error(error, directory, "canonicalize hook directory"))?;
    if !path.is_dir() {
        return Err(Error::configuration("Hook directory must be a directory"));
    }
    directory_key(&path)?;
    Ok(path)
}

fn stop_child_in_transaction(
    runtime: &tokio::runtime::Handle,
    child: &mut tokio::process::Child,
) -> Result<()> {
    runtime.block_on(async {
        if child
            .try_wait()
            .map_err(|error| Error::process(format!("Failed to inspect hook child: {error}")))?
            .is_none()
        {
            tokio::time::timeout(std::time::Duration::from_secs(5), child.kill())
                .await
                .map_err(|_| {
                    Error::process("Timed out stopping hook child; explicit recovery is required")
                })?
                .map_err(|error| Error::process(format!("Failed to stop hook child: {error}")))?;
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .map_err(|_| {
                Error::process("Timed out reaping hook child; explicit recovery is required")
            })?
            .map_err(|error| Error::process(format!("Failed to reap hook child: {error}")))?;
        Ok(())
    })
}

impl LockedState {
    fn acquire(manager: &StateManager) -> Result<Self> {
        let lock_path = manager.state_dir().join(".hook-state.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        no_follow(&mut options);
        let file = options
            .open(&lock_path)
            .map_err(|error| io_error(error, &lock_path, "open state lock"))?;
        let metadata = file
            .metadata()
            .map_err(|error| io_error(error, &lock_path, "inspect state lock"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(Error::configuration(
                "Hook state lock must be a regular file",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0
            {
                return Err(Error::configuration(
                    "Hook state lock must be private and owned by the current user",
                ));
            }
        }
        FileExt::lock_exclusive(&file)
            .map_err(|error| io_error(error, &lock_path, "lock state"))?;
        Ok(Self {
            directory: manager.state_dir().to_path_buf(),
            _lock: file,
        })
    }

    fn state_path(&self, instance_hash: &str) -> Result<PathBuf> {
        StateManager::new(self.directory.clone()).try_state_file_path(instance_hash)
    }

    fn reservation_path(&self, key: &str) -> Result<PathBuf> {
        validate_directory_key(key)?;
        Ok(self.directory.join(format!("{key}.reservation")))
    }

    fn marker_path(&self, key: &str, session_id: Uuid) -> Result<PathBuf> {
        validate_directory_key(key)?;
        validate_session(session_id)?;
        Ok(self.directory.join(format!("{key}.{session_id}.marker")))
    }

    fn generation_path(&self, key: &str, session_id: Uuid) -> Result<PathBuf> {
        validate_directory_key(key)?;
        validate_session(session_id)?;
        Ok(self
            .directory
            .join(format!("{key}.{session_id}.generation")))
    }

    fn broker_lease_path(&self, identity: &HookRunIdentity) -> Result<PathBuf> {
        validate_directory_key(&identity.directory_key)?;
        if identity.run_id.is_nil() || identity.run_id.get_version_num() != 4 {
            return Err(Error::configuration("Invalid hook broker identity"));
        }
        Ok(self
            .directory
            .join(format!(".broker-{}.lease", identity.run_id.simple())))
    }

    fn broker_leases(&self) -> Result<Vec<BrokerLease>> {
        let mut leases = Vec::new();
        let entries = std::fs::read_dir(&self.directory)
            .map_err(|error| io_error(error, &self.directory, "list hook broker leases"))?;
        for entry in entries {
            let entry = entry
                .map_err(|error| io_error(error, &self.directory, "read hook broker lease"))?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| Error::configuration("Invalid hook state filename"))?;
            if name.starts_with(".broker-")
                && Path::new(name)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("lease"))
            {
                if leases.len() == MAX_BROKER_LEASES {
                    return Err(Error::configuration(
                        "Hook broker lease count exceeds the limit",
                    ));
                }
                let lease: BrokerLease = read_optional(&entry.path())?
                    .ok_or_else(|| Error::configuration("Hook broker lease disappeared"))?;
                validate_session(lease.session_id)?;
                if self.broker_lease_path(&lease.identity)? != entry.path() {
                    return Err(Error::configuration(
                        "Hook broker lease identity does not match its filename",
                    ));
                }
                leases.push(lease);
            }
        }
        Ok(leases)
    }

    fn broker_lease(&self, identity: &HookRunIdentity) -> Result<Option<BrokerLease>> {
        let lease: Option<BrokerLease> = read_optional(&self.broker_lease_path(identity)?)?;
        if lease
            .as_ref()
            .is_some_and(|lease| lease.identity != *identity)
        {
            return Err(Error::configuration(
                "Hook broker lease identity does not match",
            ));
        }
        Ok(lease)
    }

    fn broker_generation_matches(&self, lease: &BrokerLease) -> Result<bool> {
        let generation: Option<HookRunIdentity> =
            read_optional(&self.generation_path(&lease.identity.directory_key, lease.session_id)?)?;
        if generation.as_ref() != Some(&lease.identity) {
            return Ok(false);
        }
        if self.broker_lease(&lease.identity)?.as_ref() != Some(lease) {
            return Err(Error::configuration("Hook broker lease changed"));
        }
        Ok(true)
    }

    fn remove_broker_lease(&self, lease: &BrokerLease) -> Result<()> {
        let path = self.broker_lease_path(&lease.identity)?;
        let current: Option<BrokerLease> = read_optional(&path)?;
        if current.as_ref() == Some(lease) {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error(error, &path, "remove hook broker lease")),
            }
        }
        Ok(())
    }

    fn payload_path(&self, identity: &HookRunIdentity) -> Result<PathBuf> {
        self.state_path(&identity.instance_hash)?;
        if identity.run_id.is_nil() {
            return Err(Error::configuration(
                "Legacy hook runs have no supervisor payload",
            ));
        }
        Ok(self.directory.join(format!(
            "{}.{}.payload",
            identity.instance_hash, identity.run_id
        )))
    }

    fn reservation(&self, key: &str) -> Result<Option<HookReservation>> {
        let path = self.reservation_path(key)?;
        let reservation: Option<HookReservation> = read_optional(&path)?;
        if let Some(reservation) = &reservation {
            if reservation.identity.directory_key != key
                || reservation.identity.run_id.is_nil()
                || matches!(
                    reservation.phase,
                    HookReservationPhase::LegacyRunning
                        | HookReservationPhase::LegacyState
                        | HookReservationPhase::Orphaned
                        | HookReservationPhase::ForeignBroker
                )
            {
                return Err(Error::configuration("Invalid hook reservation identity"));
            }
            self.state_path(&reservation.identity.instance_hash)?;
            if directory_key(&reservation.directory)? != key {
                return Err(Error::configuration(
                    "Hook reservation directory does not match its key",
                ));
            }
        }
        Ok(reservation)
    }

    fn state_record(&self, instance_hash: &str) -> Result<Option<StateRecord>> {
        let Some(value): Option<serde_json::Value> =
            read_optional(&self.state_path(instance_hash)?)?
        else {
            return Ok(None);
        };
        let record = if value.get("format_version").is_some() {
            let persisted: PersistedHookState = serde_json::from_value(value)
                .map_err(|_| Error::configuration("Invalid hook state metadata"))?;
            if persisted.format_version != 2
                || persisted.session_id.is_nil()
                || persisted.session_id.get_version_num() != 4
            {
                return Err(Error::configuration(
                    "Invalid hook state metadata version or session",
                ));
            }
            StateRecord::Current(persisted.into())
        } else {
            let legacy: HookExecutionState = serde_json::from_value(value)
                .map_err(|_| Error::configuration("Invalid legacy hook state"))?;
            StateRecord::Legacy(legacy)
        };
        let state = match &record {
            StateRecord::Current(state) | StateRecord::Legacy(state) => state,
        };
        if state.instance_hash != instance_hash {
            return Err(Error::configuration(
                "Hook state identifier does not match its filename",
            ));
        }
        Ok(Some(record))
    }

    fn state(&self, instance_hash: &str) -> Result<Option<HookExecutionState>> {
        match self.state_record(instance_hash)? {
            Some(StateRecord::Current(state)) => Ok(Some(state)),
            Some(StateRecord::Legacy(_)) => Err(Error::configuration(
                "Legacy hook state requires explicit drained recovery",
            )),
            None => Ok(None),
        }
    }

    fn write_state(&self, state: &HookExecutionState) -> Result<()> {
        write_atomic(
            &self.state_path(&state.instance_hash)?,
            &PersistedHookState::from(state),
        )
    }

    fn write_reservation(&self, reservation: &HookReservation) -> Result<()> {
        write_atomic(
            &self.reservation_path(&reservation.identity.directory_key)?,
            reservation,
        )
    }

    fn remove_reservation(&self, key: &str) -> Result<()> {
        let path = self.reservation_path(key)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error(error, &path, "remove reservation")),
        }
    }

    fn invalidate_generation(&self, identity: &HookRunIdentity, session_id: Uuid) -> Result<()> {
        let path = self.generation_path(&identity.directory_key, session_id)?;
        let recorded: Option<HookRunIdentity> = read_optional(&path)?;
        if recorded.as_ref() == Some(identity) {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error(error, &path, "invalidate hook generation")),
            }
        }
        Ok(())
    }
}

impl StateManager {
    async fn with_state_lock<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&LockedState) -> Result<T> + Send + 'static,
    {
        self.ensure_state_dir().await?;
        let manager = self.clone();
        tokio::task::spawn_blocking(move || operation(&LockedState::acquire(&manager)?))
            .await
            .map_err(|error| {
                Error::configuration(format!("Hook state transaction failed: {error}"))
            })?
    }

    /// Read the current directory marker and state under the coordination lock.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory or recorded state is inconsistent.
    pub async fn fast_status(
        &self,
        directory: &Path,
        session_id: Uuid,
    ) -> Result<Option<HookExecutionState>> {
        let directory = directory.to_path_buf();
        self.with_state_lock(move |locked| locked.fast_status(&directory, session_id))
            .await
    }

    /// Synchronously read the current directory marker and matching state.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory or recorded state is inconsistent.
    pub fn fast_status_sync(
        &self,
        directory: &Path,
        session_id: Uuid,
    ) -> Result<Option<HookExecutionState>> {
        validate_session(session_id)?;
        if !self.state_dir_exists_sync()? {
            return Ok(None);
        }
        LockedState::acquire(self)?.fast_status(directory, session_id)
    }

    /// Lists durable reservations and active states without a reservation.
    ///
    /// This also finds abandoned runs whose original project directory no
    /// longer exists. PIDs are diagnostic hints, not proof that processes stopped.
    ///
    /// # Errors
    ///
    /// Returns an error on an inaccessible, malformed, or inconsistent record.
    pub async fn reservations(&self) -> Result<Vec<HookReservation>> {
        self.with_state_lock(LockedState::all_reservations).await
    }

    /// Recover an exact run after the operator has drained its processes.
    ///
    /// This method relies on the caller's explicit drain attestation. It does
    /// not infer quiescence from a missing PID, marker, or project directory.
    ///
    /// # Errors
    ///
    /// Returns an error on an inaccessible, malformed, or inconsistent record.
    pub async fn recover_drained(&self, identity: &HookRunIdentity) -> Result<HookRecoveryOutcome> {
        let candidate = identity.clone();
        let (session, lease) = self
            .with_state_lock(move |locked| {
                let session = locked.recovery_session(&candidate)?;
                let lease = if session.is_some() && !candidate.run_id.is_nil() {
                    locked.broker_lease(&candidate)?
                } else {
                    None
                };
                if lease
                    .as_ref()
                    .is_some_and(|lease| Some(lease.session_id) != session)
                {
                    return Err(Error::configuration(
                        "Hook broker lease session does not match",
                    ));
                }
                Ok((session, lease))
            })
            .await?;
        if session.is_some() && !identity.run_id.is_nil() {
            #[cfg(target_os = "linux")]
            if let Ok(namespace) = super::broker::current_namespace()
                && lease
                    .as_ref()
                    .is_none_or(|lease| lease.namespace == namespace)
            {
                super::broker::remove_stale(self.state_dir(), identity, namespace).await?;
            }
            #[cfg(not(target_os = "linux"))]
            super::broker::remove_stale(self.state_dir(), identity).await?;
        }
        let identity = identity.clone();
        self.with_state_lock(move |locked| {
            if session.is_some()
                && !identity.run_id.is_nil()
                && locked.broker_lease(&identity)? != lease
            {
                return Err(Error::configuration(
                    "Hook broker lease changed during recovery",
                ));
            }
            let outcome = locked.recover_drained(&identity)?;
            if outcome == HookRecoveryOutcome::Recovered
                && let Some(lease) = lease
            {
                locked.remove_broker_lease(&lease)?;
            }
            Ok(outcome)
        })
        .await
    }

    /// Atomically reserves a directory for a fresh run.
    ///
    /// The reservation is persisted before the initial state, so an interrupted
    /// start remains visible for explicit recovery.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory is already reserved or state cannot
    /// be inspected and written safely.
    pub async fn begin_run(&self, state: &mut HookExecutionState) -> Result<HookRunIdentity> {
        match self
            .begin_run_with_mode(state, HookStartMode::Force)
            .await?
        {
            HookRunStart::Started(identity) => Ok(identity),
            HookRunStart::AlreadyPresent => {
                Err(Error::configuration("Forced hook run was not reserved"))
            }
        }
    }

    /// Atomically reserve a run, optionally retaining a matching generation.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory is busy or its state is inconsistent.
    pub async fn begin_run_with_mode(
        &self,
        state: &mut HookExecutionState,
        mode: HookStartMode,
    ) -> Result<HookRunStart> {
        let mut candidate = state.clone();
        let (outcome, updated) = self.with_state_lock(move |locked| {
            let directory = canonical_directory(&candidate.directory_path)?;
            let key = directory_key(&directory)?;
            if candidate.run_id.is_nil() {
                return Err(Error::configuration("A new hook run requires a non-nil run identifier"));
            }
            validate_session(candidate.session_id)?;
            if locked.reservation(&key)?.is_some() {
                return Err(Error::Busy);
            }
            for reservation in locked.all_reservations()? {
                if reservation.identity.directory_key == key
                    && reservation.phase != HookReservationPhase::ForeignBroker
                {
                    return Err(Error::configuration("Hook directory has an active or abandoned run; inspect or recover it first"));
                }
            }
            candidate.directory_path.clone_from(&directory);
            candidate.instance_hash = super::compute_instance_hash(&directory, &candidate.config_hash, candidate.session_id);
            let generation_path = locked.generation_path(&key, candidate.session_id)?;
            if let Some(previous) = read_optional::<HookRunIdentity>(&generation_path)? {
                if previous.directory_key != key {
                    return Err(Error::configuration("Hook generation has an invalid directory identity"));
                }
                let current = locked.state(&previous.instance_hash)?.ok_or_else(|| Error::configuration("Hook generation state is missing"))?;
                LockedState::validate_run_state(&previous, &current)?;
                if current.session_id != candidate.session_id || current.status != ExecutionStatus::Completed {
                    return Err(Error::configuration("Hook generation state is inconsistent"));
                }
                if mode == HookStartMode::IfMissing && previous.instance_hash == candidate.instance_hash {
                    return Ok((HookRunStart::AlreadyPresent, candidate));
                }
                locked.invalidate_generation(&previous, candidate.session_id)?;
            }
            let identity = HookRunIdentity {
                directory_key: key,
                instance_hash: candidate.instance_hash.clone(),
                run_id: candidate.run_id,
            };
            let reservation = HookReservation {
                directory,
                identity: identity.clone(),
                session_id: candidate.session_id,
                phase: HookReservationPhase::Starting,
                worker_pid: None,
                child_pid: None,
            };
            locked.write_reservation(&reservation)?;
            write_atomic(&locked.marker_path(&identity.directory_key, candidate.session_id)?, &identity)?;
            locked.write_state(&candidate)?;
            Ok((HookRunStart::Started(identity), candidate))
        }).await?;
        *state = updated;
        Ok(outcome)
    }

    /// Join a reserved supervisor and receive its payload from standard input.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, missing, or inconsistent run data.
    pub async fn join_supervisor_run(&self, identity: &HookRunIdentity) -> Result<HookRunJoin> {
        let owner = identity.clone();
        let joined = self
            .with_state_lock(move |locked| {
                let Some(mut reservation) = locked.matching_reservation(&owner)? else {
                    return Ok(HookRunJoin::Superseded);
                };
                if reservation.phase == HookReservationPhase::Cancelling {
                    return Ok(HookRunJoin::CancelRequested);
                }
                if reservation.phase != HookReservationPhase::Starting {
                    return Err(Error::configuration("Hook run already has a worker"));
                }
                let state = locked.state(&owner.instance_hash)?.ok_or_else(|| {
                    Error::configuration(
                        "Reserved hook state is missing; explicit recovery is required",
                    )
                })?;
                LockedState::validate_run_state(&owner, &state)?;
                if reservation.session_id != state.session_id {
                    return Err(Error::configuration(
                        "Hook reservation has a different session",
                    ));
                }
                reservation.phase = HookReservationPhase::Running;
                reservation.worker_pid = Some(std::process::id());
                locked.write_reservation(&reservation)?;
                Ok(HookRunJoin::Joined(Box::new(HookWorkerStart {
                    state,
                    hooks: Vec::new(),
                    config: HookExecutionConfig::default(),
                })))
            })
            .await?;
        let HookRunJoin::Joined(mut start) = joined else {
            return Ok(joined);
        };
        let mut input = tokio::io::stdin();
        let read = read_supervisor_payload(&mut input);
        tokio::pin!(read);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let payload = loop {
            tokio::select! {
                payload = &mut read => break Ok(payload),
                () = tokio::time::sleep_until(deadline) => break Err(()),
                () = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                    match self.run_status(identity).await? {
                        HookRunUpdate::Applied => {}
                        HookRunUpdate::CancelRequested => {
                            self.acknowledge_cancellation(identity).await?;
                            return Ok(HookRunJoin::CancelRequested);
                        }
                        HookRunUpdate::Superseded => return Ok(HookRunJoin::Superseded),
                    }
                }
            }
        };
        match payload {
            Ok(Ok(payload)) => {
                start.state.hooks.clone_from(&payload.hooks);
                start.hooks = payload.hooks;
                start.config = payload.config;
                Ok(HookRunJoin::Joined(start))
            }
            Ok(Err(error)) => {
                start.state.status = ExecutionStatus::Failed;
                start.state.finished_at = Some(chrono::Utc::now());
                match self.update_run(identity, &start.state).await? {
                    HookRunUpdate::CancelRequested => {
                        self.acknowledge_cancellation(identity).await?;
                        Ok(HookRunJoin::CancelRequested)
                    }
                    _ => Err(error),
                }
            }
            Err(()) => Err(Error::configuration(
                "Timed out reading supervisor payload; explicit recovery is required",
            )),
        }
    }

    /// Find the current reservation for a canonicalized directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory or its reservation cannot be inspected.
    pub async fn current_reservation(&self, directory: &Path) -> Result<Option<HookReservation>> {
        let directory = directory.to_path_buf();
        self.with_state_lock(move |locked| {
            let directory = canonical_directory(&directory)?;
            let key = directory_key(&directory)?;
            locked.reservation(&key)
        })
        .await
    }

    /// Read one configuration's state while validating any current reservation.
    ///
    /// # Errors
    ///
    /// Returns an error if another run owns the directory or a reservation
    /// and state do not identify the same run.
    pub async fn state_for_instance(
        &self,
        directory: &Path,
        config_hash: &str,
        session_id: Uuid,
    ) -> Result<Option<HookExecutionState>> {
        let directory = directory.to_path_buf();
        let config_hash = config_hash.to_string();
        validate_session(session_id)?;
        let query_directory = directory.clone();
        let query_hash = config_hash.clone();
        let observed = self.with_state_lock(move |locked| {
            let directory = canonical_directory(&query_directory)?;
            let key = directory_key(&directory)?;
            let instance_hash = super::compute_instance_hash(&directory, &query_hash, session_id);
            let reservation = locked.reservation(&key)?;
            if let Some(reservation) = &reservation {
                if reservation.session_id == session_id && reservation.identity.instance_hash != instance_hash {
                    return Err(Error::Busy);
                }
                if reservation.session_id == session_id {
                    let state = locked.state(&instance_hash)?.ok_or_else(|| {
                        Error::configuration(
                            "Reserved hook state is missing; explicit recovery is required",
                        )
                    })?;
                    LockedState::validate_run_state(&reservation.identity, &state)?;
                    LockedState::validate_reserved_status(reservation, &state)?;
                    if state.is_complete() {
                        return Err(Error::configuration(
                            "Terminal hook state is still reserved; explicit recovery is required",
                        ));
                    }
                    return Ok(Some((state, None)));
                }
            }
            let state = locked.state(&instance_hash)?;
            if state.as_ref().is_some_and(|state| {
                state.directory_path != directory || state.config_hash != query_hash || state.session_id != session_id
            }) {
                return Err(Error::configuration(
                    "Hook state does not match the requested directory and configuration",
                ));
            }
            if state.as_ref().is_some_and(|state| !state.is_complete()) {
                return Err(Error::configuration(
                    "Active hook state has no reservation; explicit recovery is required",
                ));
            }
            let Some(state) = state else {
                return if reservation.is_some() { Err(Error::Busy) } else { Ok(None) };
            };
            if state.status != ExecutionStatus::Completed {
                return Ok(Some((state, None)));
            }
            let generation: Option<HookRunIdentity> = read_optional(&locked.generation_path(&key, session_id)?)?;
            let Some(generation) = generation.filter(|generation| {
                generation.directory_key == key
                    && generation.instance_hash == instance_hash
                    && generation.run_id == state.run_id
            }) else {
                return Ok(None);
            };
            let lease = locked.broker_lease(&generation)?.ok_or_else(|| {
                Error::configuration("Completed hook generation has no broker lease")
            })?;
            if lease.session_id != session_id {
                return Err(Error::configuration("Hook broker lease session does not match"));
            }
            Ok(Some((state, Some(lease))))
        })
        .await?;
        let Some((state, lease)) = observed else {
            return Ok(None);
        };
        let Some(lease) = lease else {
            return Ok(Some(state));
        };
        let Some(snapshot) = super::broker::read_snapshot(
            self.state_dir(),
            &lease.identity,
            #[cfg(target_os = "linux")]
            lease.namespace,
        )
        .await?
        else {
            self.invalidate_lease_generation(&lease).await?;
            return Ok(None);
        };
        if snapshot.session_id != session_id
            || snapshot.config_hash != config_hash
            || snapshot.directory_path != state.directory_path
            || snapshot.status != ExecutionStatus::Completed
        {
            return Err(Error::configuration(
                "Hook broker returned inconsistent state",
            ));
        }
        let lease_for_check = lease.clone();
        let still_current = self
            .with_state_lock(move |locked| locked.broker_generation_matches(&lease_for_check))
            .await?;
        Ok(still_current.then_some(snapshot))
    }

    async fn invalidate_lease_generation(&self, lease: &BrokerLease) -> Result<()> {
        let lease = lease.clone();
        self.with_state_lock(move |locked| {
            if locked.broker_generation_matches(&lease)? {
                locked.invalidate_generation(&lease.identity, lease.session_id)?;
            }
            Ok(())
        })
        .await
    }

    async fn generation_is_current(&self, lease: &BrokerLease) -> Result<bool> {
        let lease = lease.clone();
        self.with_state_lock(move |locked| locked.broker_generation_matches(&lease))
            .await
    }

    async fn release_broker_lease(&self, lease: BrokerLease) -> Result<()> {
        self.with_state_lock(move |locked| locked.remove_broker_lease(&lease))
            .await
    }

    async fn reclaim_absent_brokers(&self) -> Result<()> {
        let leases = self
            .with_state_lock(|locked| {
                let mut eligible = Vec::new();
                for lease in locked.broker_leases()? {
                    if !locked
                        .reservation(&lease.identity.directory_key)?
                        .is_some_and(|reservation| reservation.identity == lease.identity)
                    {
                        eligible.push(lease);
                    }
                }
                Ok(eligible)
            })
            .await?;
        let mut probes = tokio::task::JoinSet::new();
        for lease in leases {
            let directory = self.state_dir().to_path_buf();
            probes.spawn(async move {
                let absent = matches!(
                    super::broker::is_available(
                        &directory,
                        &lease.identity,
                        #[cfg(target_os = "linux")]
                        lease.namespace,
                    )
                    .await,
                    Ok(false)
                );
                if absent
                    && super::broker::remove_stale(
                        &directory,
                        &lease.identity,
                        #[cfg(target_os = "linux")]
                        lease.namespace,
                    )
                    .await
                    .is_ok()
                {
                    Some(lease)
                } else {
                    None
                }
            });
        }
        while let Some(result) = probes.join_next().await {
            let Ok(Some(lease)) = result else { continue };
            self.with_state_lock(move |locked| {
                let path = locked.broker_lease_path(&lease.identity)?;
                let current: Option<BrokerLease> = read_optional(&path)?;
                if current.as_ref() == Some(&lease)
                    && !locked
                        .reservation(&lease.identity.directory_key)?
                        .is_some_and(|reservation| reservation.identity == lease.identity)
                {
                    locked.invalidate_generation(&lease.identity, lease.session_id)?;
                    locked.remove_broker_lease(&lease)?;
                }
                Ok(())
            })
            .await?;
        }
        Ok(())
    }

    /// Publish a completed result through an authenticated in-memory broker.
    ///
    /// The listener is ready before completion is recorded. This method then
    /// serves the exact generation until it becomes idle.
    ///
    /// # Errors
    ///
    /// Returns an error if the listener, reservation, or generation cannot be
    /// checked and updated safely.
    pub async fn publish_and_serve(
        &self,
        identity: &HookRunIdentity,
        state: &HookExecutionState,
    ) -> Result<HookRunUpdate> {
        if state.status != ExecutionStatus::Completed {
            return Err(Error::configuration(
                "Only completed hook runs can be published",
            ));
        }
        validate_session(state.session_id)?;
        let broker = super::broker::Broker::prepare(self.state_dir(), identity, state)?;
        #[cfg(target_os = "linux")]
        let namespace = broker.namespace();
        self.reclaim_absent_brokers().await?;
        let candidate = state.clone();
        let owner = identity.clone();
        let outcome = self
            .with_state_lock(move |locked| {
                #[cfg(target_os = "linux")]
                super::broker::verify_namespace(namespace)?;
                let Some(reservation) = locked.matching_reservation(&owner)? else {
                    return Ok(HookRunUpdate::Superseded);
                };
                if reservation.phase == HookReservationPhase::Cancelling {
                    return Ok(HookRunUpdate::CancelRequested);
                }
                if reservation.phase != HookReservationPhase::Running
                    || reservation.worker_pid != Some(std::process::id())
                    || reservation.child_pid.is_some()
                    || reservation.session_id != candidate.session_id
                {
                    return Err(Error::configuration("Hook run cannot publish its result"));
                }
                LockedState::validate_run_state(&owner, &candidate)?;
                let current = locked.state(&owner.instance_hash)?.ok_or_else(|| {
                    Error::configuration(
                        "Reserved hook state is missing; explicit recovery is required",
                    )
                })?;
                LockedState::validate_run_state(&owner, &current)?;
                if current.is_complete() || current.session_id != candidate.session_id {
                    return Err(Error::configuration(
                        "Hook run state cannot publish its result",
                    ));
                }
                if locked.broker_leases()?.len() >= MAX_BROKER_LEASES {
                    return Err(Error::configuration("Hook broker capacity is exhausted"));
                }
                let lease = BrokerLease {
                    identity: owner.clone(),
                    session_id: candidate.session_id,
                    #[cfg(target_os = "linux")]
                    namespace,
                };
                let lease_path = locked.broker_lease_path(&owner)?;
                if open_optional(&lease_path)?.is_some() {
                    return Err(Error::configuration("Hook broker lease already exists"));
                }
                write_atomic(&lease_path, &lease)?;
                write_atomic(
                    &locked.generation_path(&owner.directory_key, candidate.session_id)?,
                    &owner,
                )?;
                locked.write_state(&candidate)?;
                locked.remove_reservation(&owner.directory_key)?;
                Ok(HookRunUpdate::Applied)
            })
            .await?;
        if outcome == HookRunUpdate::Applied {
            let check_manager = self.clone();
            let stop_manager = self.clone();
            let release_manager = self.clone();
            let lease = BrokerLease {
                identity: identity.clone(),
                session_id: state.session_id,
                #[cfg(target_os = "linux")]
                namespace,
            };
            let check_lease = lease.clone();
            let stop_lease = lease.clone();
            broker
                .serve(super::broker::BrokerLifecycle {
                    is_current: move || {
                        let manager = check_manager.clone();
                        let lease = check_lease.clone();
                        async move { manager.generation_is_current(&lease).await }
                    },
                    before_stop: move || async move { stop_manager.invalidate_lease_generation(&stop_lease).await },
                    after_stop: move || async move { release_manager.release_broker_lease(lease).await },
                })
                .await?;
        }
        Ok(outcome)
    }

    /// Marks an exact reservation as joined by the current worker.
    ///
    /// # Errors
    ///
    /// Returns an error if the recorded state cannot be checked or updated.
    pub async fn join_run(&self, identity: &HookRunIdentity) -> Result<HookRunUpdate> {
        let identity = identity.clone();
        self.with_state_lock(move |locked| {
            let Some(mut reservation) = locked.matching_reservation(&identity)? else {
                return Ok(HookRunUpdate::Superseded);
            };
            if reservation.phase == HookReservationPhase::Cancelling {
                return Ok(HookRunUpdate::CancelRequested);
            }
            if reservation.phase != HookReservationPhase::Starting {
                return Err(Error::configuration("Hook run already has a worker"));
            }
            let state = locked.state(&identity.instance_hash)?.ok_or_else(|| {
                Error::configuration(
                    "Reserved hook state is missing; explicit recovery is required",
                )
            })?;
            LockedState::validate_run_state(&identity, &state)?;
            reservation.phase = HookReservationPhase::Running;
            reservation.worker_pid = Some(std::process::id());
            locked.write_reservation(&reservation)?;
            Ok(HookRunUpdate::Applied)
        })
        .await
    }

    pub(crate) async fn spawn_run_child(
        &self,
        identity: &HookRunIdentity,
        command: tokio::process::Command,
    ) -> Result<HookChildStart> {
        self.spawn_run_child_with(identity, command, |event| match event {
            HookSpawnEvent::Authorized => tracing::trace!("Hook child start authorized"),
            HookSpawnEvent::Spawned(pid) => tracing::trace!(pid, "Hook child started"),
        })
        .await
    }

    async fn spawn_run_child_with<F>(
        &self,
        identity: &HookRunIdentity,
        mut command: tokio::process::Command,
        mut observe: F,
    ) -> Result<HookChildStart>
    where
        F: FnMut(HookSpawnEvent) + Send + 'static,
    {
        let identity = identity.clone();
        let runtime = tokio::runtime::Handle::current();
        self.with_state_lock(move |locked| {
            let Some(mut reservation) = locked.matching_reservation(&identity)? else {
                return Ok(HookChildStart::Superseded);
            };
            if reservation.phase == HookReservationPhase::Cancelling {
                return Ok(HookChildStart::CancelRequested);
            }
            if reservation.phase != HookReservationPhase::Running
                || reservation.worker_pid != Some(std::process::id())
                || reservation.child_pid.is_some()
            {
                return Err(Error::configuration(
                    "Hook worker cannot start another child",
                ));
            }
            let state = locked.state(&identity.instance_hash)?.ok_or_else(|| {
                Error::configuration(
                    "Reserved hook state is missing; explicit recovery is required",
                )
            })?;
            LockedState::validate_run_state(&identity, &state)?;
            reservation.phase = HookReservationPhase::ChildStarting;
            locked.write_reservation(&reservation)?;
            observe(HookSpawnEvent::Authorized);
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(error) => {
                    reservation.phase = HookReservationPhase::Running;
                    locked.write_reservation(&reservation)?;
                    return Ok(HookChildStart::Failed(error.to_string()));
                }
            };
            let Some(pid) = child.id() else {
                stop_child_in_transaction(&runtime, &mut child)?;
                reservation.phase = HookReservationPhase::Running;
                locked.write_reservation(&reservation)?;
                return Err(Error::process("Started hook child has no PID"));
            };
            observe(HookSpawnEvent::Spawned(pid));
            reservation.child_pid = Some(pid);
            reservation.phase = HookReservationPhase::Running;
            if let Err(error) = locked.write_reservation(&reservation) {
                stop_child_in_transaction(&runtime, &mut child)?;
                reservation.child_pid = None;
                reservation.phase = HookReservationPhase::Running;
                locked.write_reservation(&reservation)?;
                return Err(error);
            }
            Ok(HookChildStart::Started(child))
        })
        .await
    }

    /// Records the direct child of an exact run, or clears it after reaping.
    ///
    /// # Errors
    ///
    /// Returns an error if the recorded state cannot be checked or updated.
    pub async fn record_child(
        &self,
        identity: &HookRunIdentity,
        child: HookChildUpdate,
    ) -> Result<HookRunUpdate> {
        let identity = identity.clone();
        self.with_state_lock(move |locked| {
            let Some(mut reservation) = locked.matching_reservation(&identity)? else {
                return Ok(HookRunUpdate::Superseded);
            };
            if reservation.worker_pid != Some(std::process::id()) {
                return Err(Error::configuration(
                    "Hook child update is not from the reserved worker",
                ));
            }
            match child {
                HookChildUpdate::Reaped(pid) if reservation.child_pid == Some(pid) => {
                    reservation.child_pid = None;
                }
                HookChildUpdate::Reaped(_) => {
                    return Err(Error::configuration(
                        "Hook child does not match the registered process",
                    ));
                }
            }
            let update = if reservation.phase == HookReservationPhase::Cancelling {
                HookRunUpdate::CancelRequested
            } else {
                HookRunUpdate::Applied
            };
            locked.write_reservation(&reservation)?;
            Ok(update)
        })
        .await
    }

    /// Checks whether an exact run is still authorized to execute hooks.
    ///
    /// # Errors
    ///
    /// Returns an error if its reservation cannot be read safely.
    pub async fn run_status(&self, identity: &HookRunIdentity) -> Result<HookRunUpdate> {
        let identity = identity.clone();
        self.with_state_lock(move |locked| {
            let Some(reservation) = locked.matching_reservation(&identity)? else {
                return Ok(HookRunUpdate::Superseded);
            };
            if reservation.worker_pid != Some(std::process::id()) {
                return Err(Error::configuration(
                    "Hook status check is not from the reserved worker",
                ));
            }
            let state = locked.state(&identity.instance_hash)?.ok_or_else(|| {
                Error::configuration(
                    "Reserved hook state is missing; explicit recovery is required",
                )
            })?;
            LockedState::validate_run_state(&identity, &state)?;
            if reservation.phase == HookReservationPhase::ChildStarting {
                return Err(Error::configuration(
                    "Hook child start was interrupted; explicit recovery is required",
                ));
            }
            Ok(if reservation.phase == HookReservationPhase::Cancelling {
                HookRunUpdate::CancelRequested
            } else {
                HookRunUpdate::Applied
            })
        })
        .await
    }

    /// Saves a snapshot only while the exact run still owns its reservation.
    /// Terminal snapshots release the reservation after the state is written.
    ///
    /// # Errors
    ///
    /// Returns an error if the state cannot be checked or written safely.
    pub async fn update_run(
        &self,
        identity: &HookRunIdentity,
        state: &HookExecutionState,
    ) -> Result<HookRunUpdate> {
        let identity = identity.clone();
        let state = state.clone();
        self.with_state_lock(move |locked| {
            let Some(reservation) = locked.matching_reservation(&identity)? else {
                return Ok(HookRunUpdate::Superseded);
            };
            if reservation.worker_pid != Some(std::process::id()) {
                return Err(Error::configuration("Hook update is not from the reserved worker"));
            }
            if reservation.phase == HookReservationPhase::Cancelling {
                return Ok(HookRunUpdate::CancelRequested);
            }
            if reservation.phase != HookReservationPhase::Running {
                return Err(Error::configuration("Hook run is not in a writable phase; explicit recovery is required"));
            }
            LockedState::validate_run_state(&identity, &state)?;
            let current = locked.state(&identity.instance_hash)?
                .ok_or_else(|| Error::configuration("Reserved hook state is missing; explicit recovery is required"))?;
            LockedState::validate_run_state(&identity, &current)?;
            if current.is_complete() {
                return Err(Error::configuration("Hook state is terminal while its reservation remains; explicit recovery is required"));
            }
            if state.is_complete() && reservation.child_pid.is_some() {
                return Err(Error::configuration("Cannot finish hook run before its child is reaped"));
            }
            if state.is_complete() && state.status != ExecutionStatus::Completed {
                locked.invalidate_generation(&identity, state.session_id)?;
            }
            locked.write_state(&state)?;
            if state.is_complete() {
                locked.remove_reservation(&identity.directory_key)?;
            }
            Ok(HookRunUpdate::Applied)
        }).await
    }

    pub(crate) async fn fail_start(
        &self,
        identity: &HookRunIdentity,
        state: &HookExecutionState,
    ) -> Result<HookRunUpdate> {
        let identity = identity.clone();
        let state = state.clone();
        self.with_state_lock(move |locked| {
            let Some(reservation) = locked.matching_reservation(&identity)? else {
                return Ok(HookRunUpdate::Superseded);
            };
            if reservation.phase == HookReservationPhase::Cancelling {
                return Ok(HookRunUpdate::CancelRequested);
            }
            if reservation.phase != HookReservationPhase::Starting
                || reservation.worker_pid.is_some()
                || reservation.child_pid.is_some()
                || state.status != ExecutionStatus::Failed
            {
                return Err(Error::configuration(
                    "Cannot fail a hook start after a worker joined",
                ));
            }
            LockedState::validate_run_state(&identity, &state)?;
            let current = locked.state(&identity.instance_hash)?.ok_or_else(|| {
                Error::configuration(
                    "Reserved hook state is missing; explicit recovery is required",
                )
            })?;
            LockedState::validate_run_state(&identity, &current)?;
            locked.write_state(&state)?;
            locked.remove_payload(&identity)?;
            locked.remove_reservation(&identity.directory_key)?;
            Ok(HookRunUpdate::Applied)
        })
        .await
    }

    /// Requests cancellation of an exact observed run.
    ///
    /// # Errors
    ///
    /// Returns an error if its reservation or state cannot be updated safely.
    pub async fn request_cancellation(
        &self,
        identity: &HookRunIdentity,
        reason: Option<String>,
    ) -> Result<HookRunUpdate> {
        let identity = identity.clone();
        self.with_state_lock(move |locked| {
            let Some(mut reservation) = locked.matching_reservation(&identity)? else {
                return Ok(HookRunUpdate::Superseded);
            };
            if reservation.phase == HookReservationPhase::ChildStarting {
                return Err(Error::configuration("Hook child start was interrupted; explicit recovery is required"));
            }
            let mut state = locked.state(&identity.instance_hash)?
                .ok_or_else(|| Error::configuration("Reserved hook state is missing; explicit recovery is required"))?;
            LockedState::validate_run_state(&identity, &state)?;
            if state.is_complete() {
                return Err(Error::configuration("Hook state is terminal while its reservation remains; explicit recovery is required"));
            }
            reservation.phase = HookReservationPhase::Cancelling;
            locked.write_reservation(&reservation)?;
            state.status = ExecutionStatus::Cancelling;
            state.error_message = reason;
            locked.write_state(&state)?;
            Ok(HookRunUpdate::CancelRequested)
        }).await
    }

    /// Acknowledges cancellation after the direct child has been stopped and reaped.
    ///
    /// # Errors
    ///
    /// Returns an error if the child is still registered or state cannot be written.
    pub async fn acknowledge_cancellation(
        &self,
        identity: &HookRunIdentity,
    ) -> Result<HookRunUpdate> {
        let identity = identity.clone();
        self.with_state_lock(move |locked| {
            let Some(reservation) = locked.matching_reservation(&identity)? else {
                return Ok(HookRunUpdate::Superseded);
            };
            if reservation.phase != HookReservationPhase::Cancelling {
                return Err(Error::configuration("Hook cancellation was not requested"));
            }
            if reservation
                .worker_pid
                .is_some_and(|pid| pid != std::process::id())
            {
                return Err(Error::configuration(
                    "Cancellation acknowledgement is not from the reserved worker",
                ));
            }
            if reservation.child_pid.is_some() {
                return Err(Error::configuration(
                    "Cannot acknowledge cancellation before the child is reaped",
                ));
            }
            let mut state = locked.state(&identity.instance_hash)?.ok_or_else(|| {
                Error::configuration(
                    "Reserved hook state is missing; explicit recovery is required",
                )
            })?;
            LockedState::validate_run_state(&identity, &state)?;
            state.mark_cancelled(state.error_message.clone());
            locked.write_state(&state)?;
            locked.remove_payload(&identity)?;
            locked.remove_reservation(&identity.directory_key)?;
            Ok(HookRunUpdate::Applied)
        })
        .await
    }
}

impl LockedState {
    fn recovery_session(&self, identity: &HookRunIdentity) -> Result<Option<Uuid>> {
        validate_directory_key(&identity.directory_key)?;
        self.state_path(&identity.instance_hash)?;
        if !identity.run_id.is_nil()
            && let Some(lease) = self.broker_lease(identity)?
        {
            return Ok(Some(lease.session_id));
        }
        if let Some(reservation) = self.reservation(&identity.directory_key)? {
            return Ok((reservation.identity == *identity).then_some(reservation.session_id));
        }
        let Some(record) = self.state_record(&identity.instance_hash)? else {
            return Ok(None);
        };
        let (state, legacy) = match record {
            StateRecord::Current(state) => (state, false),
            StateRecord::Legacy(state) => (state, true),
        };
        if state.run_id == identity.run_id
            && directory_key(&state.directory_path)? == identity.directory_key
            && (legacy || !state.is_complete())
        {
            Ok(Some(state.session_id))
        } else {
            Ok(None)
        }
    }

    fn fast_status(
        &self,
        directory: &Path,
        session_id: Uuid,
    ) -> Result<Option<HookExecutionState>> {
        let directory = canonical_directory(directory)?;
        let key = directory_key(&directory)?;
        let reservation = self
            .reservation(&key)?
            .filter(|reservation| reservation.session_id == session_id);
        let marker: Option<HookRunIdentity> = read_optional(&self.marker_path(&key, session_id)?)?;
        let Some(marker) = marker else {
            return if reservation.is_some() {
                Err(Error::configuration(
                    "Hook reservation has no marker; explicit recovery is required",
                ))
            } else {
                Ok(None)
            };
        };
        if marker.directory_key != key
            || reservation
                .as_ref()
                .is_some_and(|reservation| reservation.identity != marker)
        {
            return Err(Error::configuration(
                "Hook marker does not match its reservation",
            ));
        }
        let state = self.state(&marker.instance_hash)?.ok_or_else(|| {
            Error::configuration("Hook marker has no state; explicit recovery is required")
        })?;
        Self::validate_run_state(&marker, &state)?;
        if state.session_id != session_id {
            return Err(Error::configuration(
                "Hook marker belongs to a different session",
            ));
        }
        if let Some(reservation) = &reservation {
            Self::validate_reserved_status(reservation, &state)?;
        }
        if state.is_complete() && !state.should_display_completed() {
            if reservation.is_some() {
                return Err(Error::configuration(
                    "Terminal hook state is still reserved; explicit recovery is required",
                ));
            }
            self.remove_matching_marker(&marker)?;
            return Ok(None);
        }
        Ok(Some(state))
    }

    fn matching_reservation(&self, identity: &HookRunIdentity) -> Result<Option<HookReservation>> {
        Ok(self
            .reservation(&identity.directory_key)?
            .filter(|reservation| reservation.identity == *identity))
    }

    fn validate_run_state(identity: &HookRunIdentity, state: &HookExecutionState) -> Result<()> {
        if state.instance_hash != identity.instance_hash
            || state.run_id != identity.run_id
            || directory_key(&state.directory_path)? != identity.directory_key
        {
            return Err(Error::configuration(
                "Hook state does not match its reserved run",
            ));
        }
        Ok(())
    }

    fn validate_reserved_status(
        reservation: &HookReservation,
        state: &HookExecutionState,
    ) -> Result<()> {
        let consistent = match reservation.phase {
            HookReservationPhase::Starting | HookReservationPhase::Running => {
                state.status == ExecutionStatus::Running
            }
            HookReservationPhase::Cancelling => state.status == ExecutionStatus::Cancelling,
            HookReservationPhase::ChildStarting
            | HookReservationPhase::LegacyRunning
            | HookReservationPhase::LegacyState
            | HookReservationPhase::Orphaned
            | HookReservationPhase::ForeignBroker => false,
        };
        if !consistent {
            return Err(Error::configuration(
                "Hook reservation and state are inconsistent; explicit recovery is required",
            ));
        }
        Ok(())
    }

    fn all_reservations(&self) -> Result<Vec<HookReservation>> {
        let mut reservations = Vec::new();
        let mut unreserved_states = Vec::new();
        let entries = std::fs::read_dir(&self.directory)
            .map_err(|error| io_error(error, &self.directory, "list hook state"))?;
        for entry in entries {
            let entry =
                entry.map_err(|error| io_error(error, &self.directory, "read hook state entry"))?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                return Err(Error::configuration(
                    "Hook state directory contains a non-UTF-8 filename",
                ));
            };
            if let Some(key) = name.strip_suffix(".reservation") {
                let reservation = self.reservation(key)?.ok_or_else(|| {
                    Error::configuration("Hook reservation disappeared while locked")
                })?;
                reservations.push(reservation);
            } else if let Some(stem) = name.strip_suffix(".json")
                && matches!(stem.len(), 16 | 64)
                && stem
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                let record = self
                    .state_record(stem)?
                    .ok_or_else(|| Error::configuration("Hook state disappeared while locked"))?;
                match record {
                    StateRecord::Current(state)
                        if matches!(
                            state.status,
                            ExecutionStatus::Running | ExecutionStatus::Cancelling
                        ) =>
                    {
                        unreserved_states.push((state, false));
                    }
                    StateRecord::Current(_) => {}
                    StateRecord::Legacy(state) => unreserved_states.push((state, true)),
                }
            }
        }
        for (state, legacy) in unreserved_states {
            let key = directory_key(&state.directory_path)?;
            let identity = HookRunIdentity {
                directory_key: key,
                instance_hash: state.instance_hash.clone(),
                run_id: state.run_id,
            };
            if !reservations
                .iter()
                .any(|reservation| reservation.identity == identity)
            {
                let phase = if legacy {
                    if state.is_complete() {
                        HookReservationPhase::LegacyState
                    } else {
                        HookReservationPhase::LegacyRunning
                    }
                } else {
                    HookReservationPhase::Orphaned
                };
                reservations.push(HookReservation {
                    directory: state.directory_path,
                    identity,
                    session_id: state.session_id,
                    phase,
                    worker_pid: None,
                    child_pid: None,
                });
            }
        }
        #[cfg(target_os = "linux")]
        {
            let namespace = super::broker::current_namespace().ok();
            for lease in self.broker_leases()? {
                if namespace == Some(lease.namespace)
                    || reservations
                        .iter()
                        .any(|reservation| reservation.identity == lease.identity)
                {
                    continue;
                }
                let directory = match self.state_record(&lease.identity.instance_hash)? {
                    Some(StateRecord::Current(state) | StateRecord::Legacy(state))
                        if state.run_id == lease.identity.run_id
                            && state.session_id == lease.session_id =>
                    {
                        state.directory_path
                    }
                    _ => PathBuf::new(),
                };
                reservations.push(HookReservation {
                    directory,
                    identity: lease.identity,
                    session_id: lease.session_id,
                    phase: HookReservationPhase::ForeignBroker,
                    worker_pid: None,
                    child_pid: None,
                });
            }
        }
        reservations.sort_by(|left, right| {
            (
                &left.identity.directory_key,
                &left.identity.instance_hash,
                left.identity.run_id,
            )
                .cmp(&(
                    &right.identity.directory_key,
                    &right.identity.instance_hash,
                    right.identity.run_id,
                ))
        });
        Ok(reservations)
    }

    fn recover_drained(&self, identity: &HookRunIdentity) -> Result<HookRecoveryOutcome> {
        validate_directory_key(&identity.directory_key)?;
        self.state_path(&identity.instance_hash)?;
        if !identity.run_id.is_nil()
            && let Some(lease) = self.broker_lease(identity)?
        {
            let reservation = self.reservation(&identity.directory_key)?;
            if reservation
                .as_ref()
                .is_none_or(|reservation| reservation.identity != *identity)
            {
                let active_state = match self.state_record(&identity.instance_hash)? {
                    Some(StateRecord::Current(state) | StateRecord::Legacy(state)) => {
                        state.run_id == identity.run_id
                            && matches!(
                                state.status,
                                ExecutionStatus::Running | ExecutionStatus::Cancelling
                            )
                    }
                    None => false,
                };
                if !active_state || reservation.is_some() {
                    self.invalidate_generation(identity, lease.session_id)?;
                    self.remove_matching_marker(identity)?;
                    return Ok(HookRecoveryOutcome::Recovered);
                }
            }
        }
        if let Some(reservation) = self.reservation(&identity.directory_key)? {
            if reservation.identity != *identity {
                return Ok(HookRecoveryOutcome::Stale);
            }
            if let Some(record) = self.state_record(&identity.instance_hash)? {
                let (mut state, legacy) = match record {
                    StateRecord::Current(state) => (state, false),
                    StateRecord::Legacy(state) => (state, true),
                };
                if state.run_id == identity.run_id {
                    Self::validate_run_state(identity, &state)?;
                    if legacy {
                        self.remove_legacy_payloads(identity)?;
                        let path = self.state_path(&identity.instance_hash)?;
                        std::fs::remove_file(&path)
                            .map_err(|error| io_error(error, &path, "remove legacy hook state"))?;
                    } else if !state.is_complete() {
                        state.mark_cancelled(Some(
                            "Recovered after operator-attested process drain".to_string(),
                        ));
                        self.write_state(&state)?;
                    }
                } else if reservation.phase != HookReservationPhase::Starting
                    || !state.is_complete()
                    || directory_key(&state.directory_path)? != identity.directory_key
                {
                    return Err(Error::configuration(
                        "Hook reservation conflicts with a different active state",
                    ));
                }
            }
            self.remove_matching_marker(identity)?;
            self.remove_payload(identity)?;
            if !reservation.session_id.is_nil() {
                self.invalidate_generation(identity, reservation.session_id)?;
            }
            self.remove_reservation(&identity.directory_key)?;
            return Ok(HookRecoveryOutcome::Recovered);
        }

        let Some(record) = self.state_record(&identity.instance_hash)? else {
            return Ok(HookRecoveryOutcome::NotFound);
        };
        let (mut state, legacy) = match record {
            StateRecord::Current(state) => (state, false),
            StateRecord::Legacy(state) => (state, true),
        };
        if state.run_id != identity.run_id
            || directory_key(&state.directory_path)? != identity.directory_key
        {
            return Ok(HookRecoveryOutcome::Stale);
        }
        if !legacy
            && !matches!(
                state.status,
                ExecutionStatus::Running | ExecutionStatus::Cancelling
            )
        {
            return Ok(HookRecoveryOutcome::NotFound);
        }
        if legacy {
            self.remove_legacy_payloads(identity)?;
            let path = self.state_path(&identity.instance_hash)?;
            std::fs::remove_file(&path)
                .map_err(|error| io_error(error, &path, "remove legacy hook state"))?;
        } else {
            state.mark_cancelled(Some(
                "Recovered after operator-attested process drain".to_string(),
            ));
            self.write_state(&state)?;
        }
        self.remove_matching_marker(identity)?;
        self.remove_payload(identity)?;
        if !state.session_id.is_nil() {
            self.invalidate_generation(identity, state.session_id)?;
        }
        Ok(HookRecoveryOutcome::Recovered)
    }

    fn remove_matching_marker(&self, identity: &HookRunIdentity) -> Result<()> {
        validate_directory_key(&identity.directory_key)?;
        let prefix = format!("{}.", identity.directory_key);
        let entries = std::fs::read_dir(&self.directory)
            .map_err(|error| io_error(error, &self.directory, "list hook markers"))?;
        for entry in entries {
            let entry =
                entry.map_err(|error| io_error(error, &self.directory, "read hook marker"))?;
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                Error::configuration("Hook state directory contains a non-UTF-8 filename")
            })?;
            if name.starts_with(&prefix) && name.ends_with(".marker") {
                let path = entry.path();
                let marker: Option<HookRunIdentity> = read_optional(&path)?;
                if marker.as_ref() == Some(identity) {
                    match std::fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(io_error(error, &path, "remove hook marker")),
                    }
                }
            }
        }
        Ok(())
    }

    fn remove_payload(&self, identity: &HookRunIdentity) -> Result<()> {
        if identity.run_id.is_nil() {
            return Ok(());
        }
        let path = self.payload_path(identity)?;
        if open_optional(&path)?.is_some() {
            std::fs::remove_file(&path)
                .map_err(|error| io_error(error, &path, "remove hook payload"))?;
        }
        Ok(())
    }

    fn remove_legacy_payloads(&self, identity: &HookRunIdentity) -> Result<()> {
        self.state_path(&identity.instance_hash)?;
        for suffix in ["_hooks.json", "_config.json"] {
            let path = self
                .directory
                .join(format!("{}{suffix}", identity.instance_hash));
            if open_optional(&path)?.is_some() {
                std::fs::remove_file(&path)
                    .map_err(|error| io_error(error, &path, "remove legacy hook payload"))?;
            }
        }
        Ok(())
    }
}

impl StateManager {
    #[cfg(test)]
    pub(crate) async fn seed_test_state(&self, state: &HookExecutionState) -> Result<()> {
        let state = state.clone();
        self.with_state_lock(move |locked| locked.write_state(&state))
            .await
    }

    /// Read a state snapshot under the cross-process coordination lock.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid identifier or unreadable state.
    pub async fn load_state(&self, instance_hash: &str) -> Result<Option<HookExecutionState>> {
        self.try_state_file_path(instance_hash)?;
        if !self.state_dir_exists().await? {
            return Ok(None);
        }
        let instance_hash = instance_hash.to_string();
        self.with_state_lock(move |locked| locked.state(&instance_hash))
            .await
    }

    /// Synchronously read a state snapshot under the coordination lock.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid identifier or unreadable state.
    pub fn load_state_sync(&self, instance_hash: &str) -> Result<Option<HookExecutionState>> {
        self.try_state_file_path(instance_hash)?;
        if !self.state_dir_exists_sync()? {
            return Ok(None);
        }
        LockedState::acquire(self)?.state(instance_hash)
    }

    /// List readable state snapshots, failing if a state file is malformed.
    ///
    /// # Errors
    ///
    /// Returns an error when the state directory or a state file is unreadable.
    pub async fn list_active_states(&self) -> Result<Vec<HookExecutionState>> {
        if !self.state_dir_exists().await? {
            return Ok(Vec::new());
        }
        self.with_state_lock(LockedState::states).await
    }

    /// Remove a terminal state only if its observed run has not changed.
    ///
    /// # Errors
    ///
    /// Returns an error when a state or marker cannot be checked safely.
    pub async fn remove_terminal_state(&self, observed: &HookExecutionState) -> Result<bool> {
        let observed = observed.clone();
        self.with_state_lock(move |locked| locked.remove_terminal_state(&observed))
            .await
    }

    /// Remove a terminal state by identifier; active runs require explicit recovery.
    ///
    /// # Errors
    ///
    /// Returns an error if the state is active, reserved, or inaccessible.
    pub async fn remove_state(&self, instance_hash: &str) -> Result<()> {
        self.try_state_file_path(instance_hash)?;
        if !self.state_dir_exists().await? {
            return Ok(());
        }
        let instance_hash = instance_hash.to_string();
        self.with_state_lock(move |locked| {
            let Some(state) = locked.state(&instance_hash)? else {
                return Ok(());
            };
            if !locked.remove_terminal_state(&state)? {
                return Err(Error::configuration(
                    "Active or reserved hook state requires explicit recovery",
                ));
            }
            Ok(())
        })
        .await
    }

    /// Remove terminal states whose recorded runs are no longer reserved.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed or inaccessible state.
    pub async fn cleanup_state_directory(&self) -> Result<usize> {
        if !self.state_dir_exists().await? {
            return Ok(0);
        }
        self.with_state_lock(|locked| {
            let mut count = 0;
            for state in locked.states()? {
                if state.is_complete() && locked.remove_terminal_state(&state)? {
                    count += 1;
                }
            }
            Ok(count)
        })
        .await
    }
}

impl LockedState {
    fn states(&self) -> Result<Vec<HookExecutionState>> {
        let mut states = Vec::new();
        let entries = std::fs::read_dir(&self.directory)
            .map_err(|error| io_error(error, &self.directory, "list hook state"))?;
        for entry in entries {
            let entry =
                entry.map_err(|error| io_error(error, &self.directory, "read hook state entry"))?;
            let name = entry.file_name().into_string().map_err(|_| {
                Error::configuration("Hook state directory contains a non-UTF-8 filename")
            })?;
            if let Some(stem) = name.strip_suffix(".json") {
                if stem.ends_with("_hooks") || stem.ends_with("_config") {
                    continue;
                }
                let state = self
                    .state(stem)?
                    .ok_or_else(|| Error::configuration("Hook state disappeared while locked"))?;
                states.push(state);
            }
        }
        states.sort_by(|left, right| left.instance_hash.cmp(&right.instance_hash));
        Ok(states)
    }

    fn remove_terminal_state(&self, observed: &HookExecutionState) -> Result<bool> {
        let Some(current) = self.state(&observed.instance_hash)? else {
            return Ok(false);
        };
        if !current.is_complete()
            || current.run_id != observed.run_id
            || current.status != observed.status
            || current.finished_at != observed.finished_at
        {
            return Ok(false);
        }
        let key = directory_key(&current.directory_path)?;
        if self.reservation(&key)?.is_some_and(|reservation| {
            reservation.identity.instance_hash == current.instance_hash
                && reservation.identity.run_id == current.run_id
        }) {
            return Ok(false);
        }
        let identity = HookRunIdentity {
            directory_key: key,
            instance_hash: current.instance_hash.clone(),
            run_id: current.run_id,
        };
        self.invalidate_generation(&identity, current.session_id)?;
        self.remove_matching_marker(&identity)?;
        let path = self.state_path(&current.instance_hash)?;
        std::fs::remove_file(&path).map_err(|error| io_error(error, &path, "remove hook state"))?;
        Ok(true)
    }
}

#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod tests;
