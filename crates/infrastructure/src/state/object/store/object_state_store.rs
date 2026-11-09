//! [`ObjectStateStore`]: tenant state in an S3-compatible bucket.
//!
//! Every mutation reads `state.json`, checks the lock in that version,
//! derives the next version and writes it with `If-Match` on the ETag it
//! read. A lost or ambiguous response is resolved by reading the object
//! back, never by sending the write again blind: the `writer` nonce in the
//! body says whether the write landed. Locks never expire; a marker under
//! `_locks/` (written before the lock, deleted after it) lets `locks()` find
//! every held lock with one listing.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Url;
use serde_json::json;
use sha2::Digest;

use super::super::client::{
    Addressing, Attempt, BucketAccess, Condition, S3Client, S3Failure, StoredObject,
};
use super::super::document::{
    DocumentLock, DocumentWriter, LOCK_KIND, LastChange, Origin, StateDocument, Tombstone,
};
use super::super::key::TenantPath;
use super::super::sigv4::Credentials;
use super::super::{
    HistoryEntry, LockDetails, ObjectStateFeatures, ObjectTenant, RestoreRequest, StateSize,
    StoredTenant,
};
use crate::error::{InfrastructureError, Result};
use crate::state::{
    ConditionalPut, LastWriter, LegacyMigrationAccess, LegacyMigrationCopy,
    LegacyMigrationDeleteRequest, LegacyMigrationRecordRequest, LegacyMigrationSnapshotRead,
    LegacyMigrationSource, LegacyMigrationWriterRequest, LockInformation, LockRequest,
    ManagedResource, MigrationMutation, RecordVersion, ResourceAddress, StateLock, StateStore,
    TenantLock,
};
use crate::tenant::{ProjectInstance, TenantKey};

use super::legacy_migration;

/// Largest `state.json` a writer produces (16 MiB).
pub const MAXIMUM_STATE_BYTES: usize = 16 * 1024 * 1024;

/// Attempts a mutation makes before giving up.
pub(super) const MUTATION_ATTEMPTS: u32 = 8;

/// Attempts a read makes.
const READ_ATTEMPTS: u32 = 4;

/// The wait after a 429 without `Retry-After`.
const RATE_LIMIT_WAIT: Duration = Duration::from_secs(1);

/// The host suffix of Cloudflare R2's S3 API.
const R2_HOST_SUFFIX: &str = ".r2.cloudflarestorage.com";

/// How the bucket appears in URLs, as configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AddressingChoice {
    /// Path style for an IP or `localhost` host, or a bucket with a dot;
    /// virtual-hosted otherwise.
    #[default]
    Auto,
    /// Always virtual-hosted.
    VirtualHosted,
    /// Always path style.
    Path,
}

/// Where an object store keeps state, as configured.
#[derive(Debug, Clone, Copy)]
pub struct ObjectLocation<'location> {
    /// Endpoint URL; the AWS endpoint of `region` when `None`.
    pub endpoint: Option<&'location str>,
    /// Bucket name.
    pub bucket: &'location str,
    /// Signing region.
    pub region: &'location str,
    /// Key prefix.
    pub root: Option<&'location str>,
    /// How requests name the bucket.
    pub addressing: AddressingChoice,
}

/// Everything an [`ObjectStateStore`] needs.
#[derive(Debug, Clone)]
pub struct ObjectStoreConfiguration {
    /// Endpoint origin; `https://s3.<region>.amazonaws.com` when absent.
    pub endpoint: Option<String>,
    /// Bucket name.
    pub bucket: String,
    /// Signing region (`auto` for R2).
    pub region: String,
    /// Optional key prefix for every object.
    pub root: Option<String>,
    /// How the bucket appears in URLs.
    pub addressing: AddressingChoice,
    /// Credentials.
    pub credentials: Credentials,
    /// The CUE package the invocation evaluated.
    pub package: String,
    /// History snapshots kept per tenant.
    pub history: u32,
    /// Least time between two writes to one key; one second on R2 when
    /// `None`.
    pub minimum_write_interval: Option<Duration>,
    /// The tenant whose prefix the capability probe writes in, so
    /// prefix-scoped credentials work. No probe runs without one.
    pub probe_tenant: Option<TenantKey>,
}

/// State in an S3-compatible bucket.
#[derive(Debug)]
pub struct ObjectStateStore {
    pub(super) client: S3Client,
    pub(super) root: Option<String>,
    pub(super) package: String,
    history: u32,
    probe_tenant: Option<TenantKey>,
    probed: tokio::sync::OnceCell<()>,
    migration_probed: tokio::sync::OnceCell<()>,
    checked_public: tokio::sync::OnceCell<()>,
    /// Serialises this process's mutations of one store.
    pub(super) write_gate: tokio::sync::Mutex<()>,
    /// The serial each held lock was acquired at, to tell whether a run
    /// changed anything (and so needs a history snapshot).
    acquired_at_serial: Mutex<HashMap<String, u64>>,
}

/// The target and lock for a state mutation.
struct MutationContext<'mutation> {
    path: &'mutation TenantPath,
    tenant: &'mutation TenantKey,
    lock: &'mutation StateLock,
}

/// Whether acquiring a lock may create a missing state document.
#[derive(Clone, Copy)]
enum MissingDocument {
    Create,
    Refuse,
}

/// The document and policy for a lock acquisition.
struct DocumentAcquisition<'acquisition> {
    path: &'acquisition TenantPath,
    tenant: &'acquisition TenantKey,
    request: &'acquisition LockRequest<'acquisition>,
    missing: MissingDocument,
}

/// The IP address a URL host names, when it is a literal.
fn host_address(host: &str) -> Option<std::net::IpAddr> {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .ok()
}

/// Whether `host` is a loopback name or address.
fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host_address(host).is_some_and(|address| address.is_loopback())
}

/// Whether `host` is an IP literal or `localhost`.
fn is_address_or_localhost(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host_address(host).is_some()
}

impl ObjectStateStore {
    /// Check a bucket name.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for an invalid name.
    pub fn validate_bucket(bucket: &str) -> Result<()> {
        let bytes = bucket.as_bytes();
        let valid = (3..=63).contains(&bytes.len())
            && bytes.iter().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-".contains(byte)
            })
            && bytes[0].is_ascii_alphanumeric()
            && bytes[bytes.len() - 1].is_ascii_alphanumeric()
            && !bucket.contains("..");
        if valid {
            Ok(())
        } else {
            Err(InfrastructureError::configuration(
                "the bucket name must be 3 to 63 lowercase letters, digits, '.' and '-', \
                 starting and ending with a letter or digit",
            ))
        }
    }

    /// Check a region name.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for an invalid region.
    pub fn validate_region(region: &str) -> Result<()> {
        if !region.is_empty()
            && region
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            Ok(())
        } else {
            Err(InfrastructureError::configuration(
                "the region must hold only lowercase letters, digits and '-' (R2: \"auto\")",
            ))
        }
    }

    /// The endpoint origin, validated: `https://`, or `http://` only for a
    /// loopback host, with no credentials, path, query or fragment. Errors
    /// never quote it.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for anything else.
    pub fn validate_endpoint(endpoint: Option<&str>, region: &str) -> Result<Url> {
        let text = match endpoint {
            Some(endpoint) => endpoint.to_string(),
            None if region == "auto" => {
                return Err(InfrastructureError::configuration(
                    "region \"auto\" needs an explicit endpoint (R2: \
                     https://<ACCOUNT_ID>.r2.cloudflarestorage.com)",
                ));
            }
            None => format!("https://s3.{region}.amazonaws.com"),
        };
        let invalid = || {
            InfrastructureError::configuration(
                "the object-storage endpoint must be https:// with a host and optional port \
                 (http:// only for a loopback host), without credentials, path, query or \
                 fragment",
            )
        };
        let url = Url::parse(&text).map_err(|_| invalid())?;
        let host = url.host_str().ok_or_else(invalid)?;
        let scheme_allowed = match url.scheme() {
            "https" => true,
            "http" => is_loopback(host),
            _ => false,
        };
        if !scheme_allowed
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
        {
            return Err(invalid());
        }
        Ok(url)
    }

    /// Validate where the state lives, before any credential is resolved
    /// or any request is made.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for an invalid bucket,
    /// region, root or endpoint, or a bucket name with a dot and
    /// virtual-hosted addressing.
    pub fn validate_location(location: &ObjectLocation<'_>) -> Result<()> {
        Self::validate_bucket(location.bucket)?;
        Self::validate_region(location.region)?;
        if let Some(root) = location.root {
            super::super::key::validate_root(root)?;
        }
        Self::validate_endpoint(location.endpoint, location.region)?;
        if location.addressing == AddressingChoice::VirtualHosted && location.bucket.contains('.') {
            return Err(InfrastructureError::configuration(
                "a bucket name containing '.' needs path addressing",
            ));
        }
        Ok(())
    }

    /// Build a store. Nothing is sent.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for an invalid
    /// configuration.
    pub fn try_new(configuration: ObjectStoreConfiguration) -> Result<Self> {
        Self::validate_location(&ObjectLocation {
            endpoint: configuration.endpoint.as_deref(),
            bucket: &configuration.bucket,
            region: &configuration.region,
            root: configuration.root.as_deref(),
            addressing: configuration.addressing,
        })?;
        super::super::key::validate_package(&configuration.package)?;
        let endpoint =
            Self::validate_endpoint(configuration.endpoint.as_deref(), &configuration.region)?;
        let host = endpoint.host_str().ok_or_else(|| {
            InfrastructureError::configuration("the object-storage endpoint has no host")
        })?;
        let dotted = configuration.bucket.contains('.');
        let addressing = match configuration.addressing {
            AddressingChoice::Path => Addressing::Path,
            AddressingChoice::Auto if dotted || is_address_or_localhost(host) => Addressing::Path,
            AddressingChoice::VirtualHosted if dotted => {
                return Err(InfrastructureError::configuration(
                    "a bucket name containing '.' needs path addressing",
                ));
            }
            AddressingChoice::VirtualHosted | AddressingChoice::Auto => Addressing::VirtualHosted,
        };
        let on_r2 = endpoint
            .host_str()
            .is_some_and(|host| host.to_ascii_lowercase().ends_with(R2_HOST_SUFFIX));
        let minimum_write_interval = configuration.minimum_write_interval.unwrap_or(if on_r2 {
            Duration::from_secs(1)
        } else {
            Duration::ZERO
        });
        let client = S3Client::try_new(BucketAccess {
            endpoint,
            bucket: configuration.bucket,
            region: configuration.region,
            addressing,
            credentials: configuration.credentials,
            minimum_write_interval,
            timeout: crate::state::http::REQUEST_TIMEOUT,
        })?;
        Ok(Self {
            client,
            root: configuration.root,
            package: configuration.package,
            history: configuration.history,
            probe_tenant: configuration.probe_tenant,
            probed: tokio::sync::OnceCell::new(),
            migration_probed: tokio::sync::OnceCell::new(),
            checked_public: tokio::sync::OnceCell::new(),
            write_gate: tokio::sync::Mutex::new(()),
            acquired_at_serial: Mutex::new(HashMap::new()),
        })
    }

    pub(super) fn path(&self, tenant: &TenantKey) -> Result<TenantPath> {
        TenantPath::try_new(self.root.as_deref(), &self.package, tenant)
    }

    fn object_location(&self, key: &str) -> String {
        format!("s3://{}/{key}", self.client.bucket())
    }

    /// A fresh writer nonce.
    fn nonce() -> String {
        uuid::Uuid::new_v4().simple().to_string()
    }

    // -- reading ------------------------------------------------------------

    /// GET with retries for transient failures and rate limits.
    pub(super) async fn get(&self, key: &str) -> Result<Option<StoredObject>> {
        let mut last = None;
        for attempt in 0..READ_ATTEMPTS {
            match self.client.get(key).await? {
                Ok(found) => return Ok(found),
                Err(S3Failure::RateLimited(wait)) => {
                    tokio::time::sleep(wait.unwrap_or(RATE_LIMIT_WAIT)).await;
                    last = Some(S3Failure::RateLimited(wait));
                }
                Err(S3Failure::Ambiguous(message)) => {
                    tokio::time::sleep(backoff(attempt)).await;
                    last = Some(S3Failure::Ambiguous(message));
                }
                Err(failure) => return Err(failure.into_error(&format!("reading {key}"))),
            }
        }
        Err(last
            .unwrap_or_else(|| S3Failure::Permanent("no attempt was made".into()))
            .into_error(&format!("reading {key}")))
    }

    /// List with retries.
    pub(super) async fn list_keys(&self, prefix: &str) -> Result<Vec<String>> {
        let mut last = None;
        for attempt in 0..READ_ATTEMPTS {
            match self.client.list(prefix).await? {
                Ok(keys) => return Ok(keys),
                Err(S3Failure::RateLimited(wait)) => {
                    tokio::time::sleep(wait.unwrap_or(RATE_LIMIT_WAIT)).await;
                    last = Some(S3Failure::RateLimited(wait));
                }
                Err(S3Failure::Ambiguous(message)) => {
                    tokio::time::sleep(backoff(attempt)).await;
                    last = Some(S3Failure::Ambiguous(message));
                }
                Err(S3Failure::Permanent(message)) if message.contains("HTTP 403") => {
                    return Err(InfrastructureError::state(format!(
                        "object storage refused to list {prefix}: the credential may be scoped \
                         to a prefix that does not include it ({message})"
                    )));
                }
                Err(failure) => return Err(failure.into_error(&format!("listing {prefix}"))),
            }
        }
        Err(last
            .unwrap_or_else(|| S3Failure::Permanent("no attempt was made".into()))
            .into_error(&format!("listing {prefix}")))
    }

    /// Read and decode the tenant's document, with its ETag.
    pub(super) async fn read(&self, path: &TenantPath) -> Result<Option<(StateDocument, String)>> {
        let key = path.state_key();
        self.get(&key)
            .await?
            .map(|object| {
                StateDocument::decode(&object.body, path, &key)
                    .map(|document| (document, object.etag))
            })
            .transpose()
    }

    /// Read the document, refusing a tombstone (the state moved).
    async fn read_live(&self, path: &TenantPath) -> Result<Option<(StateDocument, String)>> {
        let found = self.read(path).await?;
        if let Some((document, _)) = &found {
            self.refuse_tombstone(path, document)?;
        }
        Ok(found)
    }

    fn refuse_tombstone(&self, path: &TenantPath, document: &StateDocument) -> Result<()> {
        let Some(tombstone) = &document.tombstone else {
            return Ok(());
        };
        // The target is untrusted text: shown only when it is one of ours.
        let target = super::super::key::parse_state_key(self.root.as_deref(), &tombstone.moved_to)
            .map_or_else(
                || "an unrecognised location".to_string(),
                |target| self.object_location(&target.state_key()),
            );
        Err(InfrastructureError::state(format!(
            "the state of {} moved to {target}; select that project, package or environment \
             instead",
            self.object_location(&path.state_key())
        )))
    }

    fn vanished(&self, path: &TenantPath) -> InfrastructureError {
        InfrastructureError::state(format!(
            "the state object {} vanished while its lock was held; nothing was recreated. \
             Restore it from history (`state restore`) or from bucket versioning",
            self.object_location(&path.state_key())
        ))
    }

    // -- writing ------------------------------------------------------------

    /// Apply `change` to the tenant's document under `lock`, as described in
    /// the module documentation.
    async fn mutate<Change>(&self, context: MutationContext<'_>, mut change: Change) -> Result<()>
    where
        Change: FnMut(&mut StateDocument) -> Result<Mutation> + Send,
    {
        let MutationContext { path, tenant, lock } = context;
        let path = path.clone();
        let key = path.state_key();
        let _gate = self.write_gate.lock().await;
        let mut last_failure = String::new();
        for _ in 0..MUTATION_ATTEMPTS {
            let (document, etag) = self
                .read(&path)
                .await?
                .ok_or_else(|| self.vanished(&path))?;
            self.refuse_tombstone(&path, &document)?;
            if document
                .lock
                .as_ref()
                .is_none_or(|held| held.lock_identifier != lock.lock_identifier)
            {
                return Err(lock_lost(tenant));
            }
            if let Some(held) = &document.lock {
                require_ordinary_lock(held, document.format.write)?;
            }
            document.require_writable(&key)?;
            let mut next = document.clone();
            if change(&mut next)? == Mutation::Unchanged {
                return Ok(());
            }
            // Leave one further serial for releasing the held lock.
            require_serial_headroom(&next, 2)?;
            let writer = Self::nonce();
            next.advance(&writer)?;
            let body = encode_bounded(&next, &key)?;
            match self
                .client
                .put(&key, body, &Condition::Matches(etag))
                .await?
            {
                Ok(_) => return Ok(()),
                Err(S3Failure::PreconditionFailed) => {
                    last_failure = "the object changed concurrently".to_string();
                }
                Err(S3Failure::Ambiguous(_) | S3Failure::RateLimited(_)) => {
                    match self.read(&path).await? {
                        Some((stored, _)) if stored.writer == writer => return Ok(()),
                        Some(_) => {
                            return Err(InfrastructureError::state(
                                "the state write outcome could not be confirmed; inspect the state before retrying",
                            ));
                        }
                        None => return Err(self.vanished(&path)),
                    }
                }
                Err(S3Failure::Missing) => return Err(self.vanished(&path)),
                Err(S3Failure::Permanent(message)) => {
                    return Err(InfrastructureError::state(format!(
                        "object storage refused writing {key}: {message}"
                    )));
                }
            }
        }
        Err(InfrastructureError::state(format!(
            "could not write {key} after {MUTATION_ATTEMPTS} attempts: {last_failure}"
        )))
    }

    /// Create the tenant's document with `If-None-Match: *`. Returns `false`
    /// when another writer created it first. A lost response is resolved by
    /// reading back the `writer` nonce.
    async fn create(&self, path: &TenantPath, mut document: StateDocument) -> Result<bool> {
        let key = path.state_key();
        let writer = Self::nonce();
        document.writer.clone_from(&writer);
        document.written_by = super::super::document::written_by();
        let body = encode_bounded(&document, &key)?;
        let mut last_failure = String::new();
        for attempt in 0..MUTATION_ATTEMPTS {
            match self
                .client
                .put(&key, body.clone(), &Condition::Absent)
                .await?
            {
                Ok(_) => return Ok(true),
                Err(S3Failure::PreconditionFailed) => {
                    return Ok(self
                        .read(path)
                        .await?
                        .is_some_and(|(stored, _)| stored.writer == writer));
                }
                Err(S3Failure::Ambiguous(_) | S3Failure::RateLimited(_)) => {
                    if let Some((stored, _)) = self.read(path).await? {
                        return Ok(stored.writer == writer);
                    }
                    tokio::time::sleep(backoff(attempt)).await;
                    last_failure = "the create outcome could not be confirmed".to_string();
                }
                Err(failure) => return Err(failure.into_error(&format!("creating {key}"))),
            }
        }
        Err(InfrastructureError::state(format!(
            "could not create {key} after {MUTATION_ATTEMPTS} attempts: {last_failure}"
        )))
    }

    /// Write a marker for `lock` before it is acquired. Markers are hints;
    /// a marker key is unique per lock identifier.
    pub(super) async fn write_marker(
        &self,
        path: &TenantPath,
        lock: &StateLock,
        holder: &str,
    ) -> Result<()> {
        let key = path.marker_key(&lock.lock_identifier);
        let body = serde_json::to_vec(&json!({
            "holder": holder,
            "acquiredAt": chrono::Utc::now().to_rfc3339(),
        }))
        .map_err(|error| InfrastructureError::state(format!("failed to encode marker: {error}")))?;
        for attempt in 0..MUTATION_ATTEMPTS {
            let result = self
                .client
                .put(&key, body.clone(), &Condition::Absent)
                .await
                .map_err(|_| InfrastructureError::state("could not write the state lock marker"))?;
            match result {
                // A 412 is this lock's own marker from a retry: it exists.
                Ok(_) | Err(S3Failure::PreconditionFailed) => return Ok(()),
                Err(S3Failure::Ambiguous(_) | S3Failure::RateLimited(_)) => {
                    let marker = self.get(&key).await.map_err(|_| {
                        InfrastructureError::state("could not confirm the state lock marker")
                    })?;
                    if marker.is_some() {
                        return Ok(());
                    }
                    tokio::time::sleep(backoff(attempt)).await;
                }
                Err(_) => {
                    return Err(InfrastructureError::state(
                        "could not write the state lock marker",
                    ));
                }
            }
        }
        Err(InfrastructureError::state(
            "could not write the state lock marker",
        ))
    }

    /// Delete the marker of `lock_identifier`, best effort: a stale marker
    /// is ignored by `locks()`.
    pub(super) async fn delete_marker(&self, path: &TenantPath, lock_identifier: &str) {
        let key = path.marker_key(lock_identifier);
        if !matches!(self.client.delete(&key).await, Ok(Ok(()))) {
            tracing::warn!("could not delete a state lock marker; it is ignored");
        }
    }

    /// Whether state vanished: `state.json` is absent but history exists or
    /// the migration manifest lists the tenant. A failed check refuses.
    async fn require_not_vanished(&self, path: &TenantPath) -> Result<()> {
        let newest = self.history_serials(path).await?.last().copied();
        if let Some(serial) = newest {
            return Err(InfrastructureError::state(format!(
                "the state object {} is missing but history exists (newest snapshot: serial \
                 {serial}); the state vanished, so nothing was created. Restore it with `state \
                 restore`",
                self.object_location(&path.state_key())
            )));
        }
        if let Some(manifest) = self.read_manifest(path).await?
            && manifest
                .tenants
                .iter()
                .any(|entry| entry.environment.as_deref() == path.environment())
        {
            return Err(InfrastructureError::state(format!(
                "{} is listed in the migration manifest {} but its state is missing; inspect \
                 and reconcile the migration before creating state",
                self.object_location(&path.state_key()),
                self.object_location(&path.migration_manifest_key())
            )));
        }
        Ok(())
    }

    // -- history ------------------------------------------------------------

    async fn history_serials(&self, path: &TenantPath) -> Result<Vec<u64>> {
        let prefix = path.history_prefix();
        let mut serials: Vec<u64> = self
            .list_keys(&prefix)
            .await?
            .iter()
            .map(|key| {
                super::super::key::parse_history_serial(&prefix, key).ok_or_else(|| {
                    InfrastructureError::state(
                        "object storage history contains an invalid snapshot key",
                    )
                })
            })
            .collect::<Result<_>>()?;
        serials.sort_unstable();
        Ok(serials)
    }

    /// Write the snapshot of `document` and prune. Failures only warn.
    async fn snapshot(&self, path: &TenantPath, document: &StateDocument) {
        if self.snapshot_and_prune(path, document).await.is_err() {
            tracing::warn!("could not write or prune a history snapshot");
            cuenv_events::emit_stderr!(format!(
                "warning: could not write or prune the history snapshot of {}",
                self.object_location(&path.state_key())
            ));
        }
    }

    pub(in crate::state::object) async fn snapshot_and_prune(
        &self,
        path: &TenantPath,
        document: &StateDocument,
    ) -> Result<()> {
        if self.history > 0 {
            let mut snapshot = document.clone();
            snapshot.lock = None;
            let key = path.history_key(document.serial);
            let body = snapshot.encode()?;
            match self
                .client
                .put(&key, body.clone(), &Condition::Absent)
                .await?
            {
                Ok(_) => {}
                Err(
                    S3Failure::PreconditionFailed
                    | S3Failure::Ambiguous(_)
                    | S3Failure::RateLimited(_),
                ) => {
                    let ours = self
                        .get(&key)
                        .await?
                        .is_some_and(|object| object.body == body);
                    if !ours {
                        return Err(InfrastructureError::state(format!(
                            "history snapshot {key} already holds another body"
                        )));
                    }
                }
                Err(failure) => return Err(failure.into_error(&format!("writing {key}"))),
            }
        }
        let serials = self.history_serials(path).await?;
        let keep = usize::try_from(self.history).unwrap_or(usize::MAX);
        let excess = serials.len().saturating_sub(keep);
        for serial in serials.iter().take(excess) {
            if *serial == document.serial && self.history > 0 {
                continue;
            }
            if let Err(failure) = self.client.delete(&path.history_key(*serial)).await? {
                return Err(failure.into_error("pruning history"));
            }
        }
        Ok(())
    }

    async fn read_snapshot(&self, path: &TenantPath, serial: u64) -> Result<StateDocument> {
        let key = path.history_key(serial);
        let object = self.get(&key).await?.ok_or_else(|| {
            InfrastructureError::configuration(format!(
                "no history snapshot at serial {serial} for {}",
                self.object_location(&path.state_key())
            ))
        })?;
        let snapshot = StateDocument::decode(&object.body, path, &key)?;
        if snapshot.serial != serial {
            return Err(InfrastructureError::state(
                "history snapshot serial does not match its key",
            ));
        }
        Ok(snapshot)
    }

    // -- probing ------------------------------------------------------------

    /// Check, once per process, that the store honours conditional writes,
    /// returns strong ETags and does not serve the bucket to unsigned
    /// requests. Every probe key is random and inside the tenant prefix.
    async fn probe(&self) -> Result<()> {
        let Some(tenant) = &self.probe_tenant else {
            return Ok(());
        };
        self.probe_at(tenant).await
    }

    /// Migration needs a probe even when the store has no configured probe tenant.
    pub(super) async fn ensure_migration_capability(&self, tenant: &TenantKey) -> Result<()> {
        self.migration_probed
            .get_or_try_init(|| self.probe_at(tenant))
            .await
            .map_err(|_| InfrastructureError::state("migration storage capability check failed"))?;
        Ok(())
    }

    async fn probe_at(&self, tenant: &TenantKey) -> Result<()> {
        let path = self.path(tenant)?;
        let refuse = |problem: &str| {
            InfrastructureError::state(format!(
                "the object store failed its capability check: {problem}; cuenv needs \
                 conditional writes (If-None-Match and If-Match) and strong ETags"
            ))
        };
        let created = path.probe_key(&Self::nonce());
        let body = br#"{"probe":true}"#.to_vec();
        let first = self
            .retry_rate_limited(|| self.client.put(&created, body.clone(), &Condition::Absent))
            .await?;
        if let Err(failure) = first {
            return Err(failure.into_error("probing conditional writes"));
        }
        let second = self
            .retry_rate_limited(|| self.client.put(&created, body.clone(), &Condition::Absent))
            .await?;
        let outcome = match second {
            Err(S3Failure::PreconditionFailed) => Ok(()),
            Ok(_) => Err(refuse("a second create with If-None-Match: * succeeded")),
            Err(failure) => Err(failure.into_error("probing conditional writes")),
        };
        let outcome = match outcome {
            Ok(()) => {
                let mismatch = Condition::Matches("\"cuenv-probe-mismatch\"".to_string());
                let matched = self
                    .retry_rate_limited(|| self.client.put(&created, body.clone(), &mismatch))
                    .await?;
                match matched {
                    Err(S3Failure::PreconditionFailed) => Ok(()),
                    Ok(_) => Err(refuse("a write with a wrong If-Match succeeded")),
                    Err(failure) => Err(failure.into_error("probing conditional writes")),
                }
            }
            failed => failed,
        };
        let public = match &outcome {
            Ok(()) => self.client.readable_without_credentials(&created).await?,
            Err(_) => Ok(false),
        };
        if self.client.delete(&created).await?.is_err() {
            tracing::warn!("could not delete a probe object");
        }
        outcome?;
        if public.unwrap_or(false) {
            return Err(public_bucket());
        }
        Ok(())
    }

    /// Repeat a request while it is rate-limited.
    async fn retry_rate_limited<Request, Response, Value>(
        &self,
        request: Request,
    ) -> Result<Attempt<Value>>
    where
        Request: Fn() -> Response,
        Response: std::future::Future<Output = Result<Attempt<Value>>>,
    {
        let mut attempts = 0;
        loop {
            match request().await? {
                Err(S3Failure::RateLimited(wait)) if attempts < READ_ATTEMPTS => {
                    tokio::time::sleep(wait.unwrap_or(RATE_LIMIT_WAIT)).await;
                    attempts += 1;
                }
                other => return Ok(other),
            }
        }
    }

    /// For read-only commands: refuse a bucket that serves the tenant's
    /// state to unsigned requests. Once per process.
    pub(super) async fn ensure_not_public(&self) -> Result<()> {
        let Some(tenant) = &self.probe_tenant else {
            return Ok(());
        };
        let path = self.path(tenant)?;
        self.checked_public
            .get_or_try_init(|| async {
                match self
                    .client
                    .readable_without_credentials(&path.state_key())
                    .await?
                {
                    Ok(true) => Err(public_bucket()),
                    // A failed check is not a finding.
                    Ok(false) | Err(_) => Ok(()),
                }
            })
            .await?;
        Ok(())
    }

    // -- migration manifest -------------------------------------------------

    /// Read the project's migration manifest.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest cannot be read or decoded.
    pub async fn read_manifest(&self, path: &TenantPath) -> Result<Option<MigrationManifest>> {
        let key = path.migration_manifest_key();
        self.get(&key)
            .await
            .map_err(|_| InfrastructureError::state("cannot read migration manifest"))?
            .map(|object| {
                serde_json::from_slice::<MigrationManifest>(&object.body)
                    .map_err(|_| InfrastructureError::state("migration manifest does not decode"))
            })
            .transpose()
    }
}

/// What a mutation did to the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mutation {
    /// The document changed and must be written.
    Write,
    /// Nothing to write.
    Unchanged,
}

/// One tenant in a migration manifest.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    /// Named environment, `None` for the identity without `--env`.
    pub environment: Option<String>,
    /// `pending`, `migrated` or `rolled-back`.
    pub status: String,
    /// The migration identifier.
    pub migration: String,
    /// The target's serial when the migration committed: a rollback
    /// refuses when the target changed since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_serial: Option<u64>,
    /// Atomic commit evidence, required before a rollback can trust a baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_evidence: Option<legacy_migration::LegacyCommitEvidence>,
    /// Immutable physical backend and tenant binding for the migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<legacy_migration::LegacyMigrationBinding>,
}

impl std::fmt::Debug for ManifestEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ManifestEntry(<redacted>)")
    }
}

/// A project's migration manifest.
#[derive(Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationManifest {
    /// Tenants of the project and their status.
    pub tenants: Vec<ManifestEntry>,
    /// Conditional manifest revision, absent in older manifests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<uuid::Uuid>,
    /// Nonce of the write that produced this revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_nonce: Option<uuid::Uuid>,
    /// Monotonically increasing revision sequence for conditional writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<u64>,
}

impl std::fmt::Debug for MigrationManifest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MigrationManifest(<redacted>)")
    }
}

impl MigrationManifest {
    /// Whether the manifest lists `path`'s tenant as not yet migrated.
    #[must_use]
    pub fn lists(&self, path: &TenantPath) -> bool {
        self.tenants.iter().any(|entry| {
            entry.environment.as_deref() == path.environment() && entry.status != "migrated"
        })
    }
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(200 * u64::from(attempt.saturating_add(1)))
}

fn migration_bound(document: &StateDocument) -> bool {
    document.format.write >= 2
        || document
            .origin
            .as_ref()
            .is_some_and(|origin| origin.kind == "migration")
}

fn require_ordinary_lock(lock: &DocumentLock, write_format: u32) -> Result<()> {
    if !lock.is_ordinary(write_format) {
        return Err(InfrastructureError::state(
            "a migration-owned lock requires migration recovery",
        ));
    }
    Ok(())
}

fn require_serial_headroom(document: &StateDocument, increments: u64) -> Result<()> {
    document
        .serial
        .checked_add(increments)
        .ok_or_else(|| InfrastructureError::state("object storage state serial is exhausted"))?;
    Ok(())
}

fn lock_lost(tenant: &TenantKey) -> InfrastructureError {
    InfrastructureError::LockLost {
        tenant: tenant.to_string(),
    }
}

fn history_writer(value: &str) -> String {
    let Some(version) = value.strip_prefix("cuenv ") else {
        return "unknown".to_string();
    };
    let Ok(parsed) = semver::Version::parse(version) else {
        return "unknown".to_string();
    };
    if version.len() > 64 || !parsed.pre.is_empty() || !parsed.build.is_empty() {
        return "unknown".to_string();
    }
    value.to_string()
}

fn public_bucket() -> InfrastructureError {
    InfrastructureError::state(
        "the bucket serves state objects to unsigned requests: it is public, and state holds \
         plaintext provider secrets; make the bucket private before using it for state",
    )
}

pub(super) fn encode_bounded(document: &StateDocument, key: &str) -> Result<Vec<u8>> {
    let body = document.encode()?;
    if body.len() > MAXIMUM_STATE_BYTES {
        return Err(InfrastructureError::state(format!(
            "the state of {key} would be {} bytes, above the {MAXIMUM_STATE_BYTES}-byte cap; \
             split the project or remove records",
            body.len()
        )));
    }
    Ok(body)
}

pub(super) fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Record `resource` in `document` with the serial and generation the store
/// assigns, as the SQL and memory stores do.
fn require_restorable_writer(document: &StateDocument) -> Result<()> {
    if document.format.write >= 2
        && document
            .last_writer
            .as_ref()
            .is_some_and(|writer| !writer.extra.is_empty())
    {
        return Err(InfrastructureError::state(
            "cannot replace extended migration last-writer metadata",
        ));
    }
    Ok(())
}

fn record(
    document: &mut StateDocument,
    resource: &ManagedResource,
    insertion: uuid::Uuid,
) -> Result<()> {
    let previous = document.resource(&resource.address)?;
    if let Some(raw) = document.records.get(&resource.address.to_string())
        && (raw.as_object().is_some_and(|fields| {
            fields.keys().any(|key| {
                ![
                    "address",
                    "provider",
                    "provider_source",
                    "schema_version",
                    "state",
                    "private",
                    "dependencies",
                    "sensitive_attributes",
                    "tainted",
                    "identity",
                    "serial",
                    "generation",
                ]
                .contains(&key.as_str())
            })
        }) || raw
            .get("address")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|fields| {
                fields
                    .keys()
                    .any(|key| !["resource_type", "name"].contains(&key.as_str()))
            }))
    {
        return Err(InfrastructureError::state(
            "cannot overwrite an extended resource record",
        ));
    }
    let serial = match &previous {
        Some(stored) if stored.serial <= 0 || stored.generation.is_nil() => {
            return Err(InfrastructureError::state(
                "stored resource has an invalid version",
            ));
        }
        Some(stored) => stored
            .serial
            .checked_add(1)
            .ok_or_else(|| InfrastructureError::state("stored resource version is exhausted"))?,
        None => 1,
    };
    let generation = previous
        .as_ref()
        .map_or(insertion, |stored| stored.generation);
    document.store(&ManagedResource {
        serial,
        generation,
        ..resource.clone()
    })
}

fn note_change(document: &mut StateDocument, address: &ResourceAddress, action: &str) {
    if let Some(lock) = &mut document.lock {
        lock.last_change = Some(LastChange {
            address: address.to_string(),
            action: action.to_string(),
            at: now(),
            extra: lock
                .last_change
                .as_ref()
                .map_or_else(Default::default, |change| change.extra.clone()),
        });
    }
}

#[async_trait]
impl StateStore for ObjectStateStore {
    fn legacy_migration_source(&self) -> Option<LegacyMigrationSource> {
        Some(LegacyMigrationSource::Object {
            connection: self.legacy_connection_identity().ok()?,
            root: self.root.clone(),
            package: self.package.clone(),
        })
    }

    fn recovery_identity(&self) -> Option<String> {
        let endpoint = self.client.endpoint();
        let mut digest = sha2::Sha256::new();
        digest.update(b"cuenv-infrastructure-s3-recovery-v1\0");
        digest.update(endpoint.host_str().unwrap_or_default().to_ascii_lowercase());
        if let Some(port) = endpoint.port() {
            digest.update(format!(":{port}"));
        }
        digest.update(b"/");
        digest.update(self.client.bucket());
        digest.update(b"/");
        digest.update(self.root.as_deref().unwrap_or_default());
        Some(hex::encode(digest.finalize()))
    }

    async fn check_legacy_migration_copy(
        &self,
        request: &LegacyMigrationCopy<'_>,
    ) -> Result<MigrationMutation> {
        self.check_legacy_source_copy(request).await
    }

    async fn read_legacy_migration_snapshot(
        &self,
        access: &LegacyMigrationAccess<'_>,
    ) -> Result<LegacyMigrationSnapshotRead> {
        self.read_legacy_source_snapshot(access).await
    }

    async fn replace_legacy_migration_record(
        &self,
        request: &LegacyMigrationRecordRequest<'_>,
    ) -> Result<MigrationMutation> {
        self.replace_legacy_source_record(request).await
    }

    async fn delete_legacy_migration_record(
        &self,
        request: &LegacyMigrationDeleteRequest<'_>,
    ) -> Result<MigrationMutation> {
        self.delete_legacy_source_record(request).await
    }

    async fn set_legacy_migration_writer(
        &self,
        request: &LegacyMigrationWriterRequest<'_>,
    ) -> Result<MigrationMutation> {
        self.set_legacy_source_writer(request).await
    }

    fn recovery_scope(&self) -> Option<String> {
        Some(self.package.clone())
    }

    fn location(&self, tenant: &TenantKey) -> Option<String> {
        self.path(tenant)
            .ok()
            .map(|path| self.object_location(&path.state_key()))
    }

    fn object_features(&self) -> Option<&dyn ObjectStateFeatures> {
        Some(self)
    }

    async fn migrate(&self) -> Result<()> {
        self.probed.get_or_try_init(|| self.probe()).await?;
        Ok(())
    }

    async fn list(&self, tenant: &TenantKey) -> Result<Vec<ManagedResource>> {
        self.ensure_not_public().await?;
        let path = self.path(tenant)?;
        match self.read_live(&path).await? {
            Some((document, _)) => document.resources(),
            None => Ok(Vec::new()),
        }
    }

    async fn addresses(&self, tenant: &TenantKey) -> Result<Vec<ResourceAddress>> {
        let path = self.path(tenant)?;
        match self.read_live(&path).await? {
            Some((document, _)) => document.addresses(),
            None => Ok(Vec::new()),
        }
    }

    async fn put(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        resource: &ManagedResource,
    ) -> Result<()> {
        let insertion = uuid::Uuid::new_v4();
        self.mutate(
            MutationContext {
                path: &self.path(tenant)?,
                tenant,
                lock,
            },
            |document| {
                record(document, resource, insertion)?;
                note_change(document, &resource.address, "put");
                Ok(Mutation::Write)
            },
        )
        .await
    }

    async fn put_many(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        resources: &[ManagedResource],
    ) -> Result<()> {
        if resources.is_empty() {
            return Ok(());
        }
        let insertions: Vec<uuid::Uuid> = resources.iter().map(|_| uuid::Uuid::new_v4()).collect();
        self.mutate(
            MutationContext {
                path: &self.path(tenant)?,
                tenant,
                lock,
            },
            |document| {
                for (resource, insertion) in resources.iter().zip(&insertions) {
                    record(document, resource, *insertion)?;
                }
                if let Some(last) = resources.last() {
                    note_change(document, &last.address, "refresh");
                }
                Ok(Mutation::Write)
            },
        )
        .await
    }

    async fn put_if_unchanged(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        put: &ConditionalPut<'_>,
    ) -> Result<()> {
        let insertion = if put.resource.generation.is_nil() {
            uuid::Uuid::new_v4()
        } else {
            put.resource.generation
        };
        self.mutate(
            MutationContext {
                path: &self.path(tenant)?,
                tenant,
                lock,
            },
            |document| {
                let stored = document.resource(&put.resource.address)?;
                let found = RecordVersion::of(stored.as_ref());
                if found != put.expected {
                    if stored
                        .as_ref()
                        .is_some_and(|stored| put.is_recorded(stored))
                    {
                        return Ok(Mutation::Unchanged);
                    }
                    return Err(InfrastructureError::StateChanged {
                        address: put.resource.address.to_string(),
                        expected: put.expected.to_string(),
                        found: found.to_string(),
                        file: None,
                    });
                }
                record(document, put.resource, insertion)?;
                note_change(document, &put.resource.address, "put");
                Ok(Mutation::Write)
            },
        )
        .await
    }

    async fn delete(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        address: &ResourceAddress,
    ) -> Result<()> {
        self.mutate(
            MutationContext {
                path: &self.path(tenant)?,
                tenant,
                lock,
            },
            |document| {
                if document.records.remove(&address.to_string()).is_none() {
                    return Ok(Mutation::Unchanged);
                }
                note_change(document, address, "delete");
                Ok(Mutation::Write)
            },
        )
        .await
    }

    async fn acquire_lock(
        &self,
        tenant: &TenantKey,
        request: &LockRequest<'_>,
    ) -> Result<StateLock> {
        request.lock.validate()?;
        let path = self.path(tenant)?;
        if let Some((document, _)) = self.read(&path).await? {
            if let Some(held) = &document.lock {
                require_ordinary_lock(held, document.format.write)?;
            } else {
                require_serial_headroom(&document, 2)?;
            }
        } else {
            self.require_not_vanished(&path).await?;
        }
        self.write_marker(&path, request.lock, request.holder)
            .await?;
        let acquired = self
            .acquire_in_document(DocumentAcquisition {
                path: &path,
                tenant,
                request,
                missing: MissingDocument::Create,
            })
            .await;
        if let Ok(serial) = &acquired
            && let Ok(mut serials) = self.acquired_at_serial.lock()
        {
            serials.insert(request.lock.lock_identifier.clone(), *serial);
        }
        acquired.map(|_| request.lock.clone())
    }

    async fn unlock(&self, tenant: &TenantKey, lock: &StateLock) -> Result<bool> {
        let path = self.path(tenant)?;
        let acquired_serial = self
            .acquired_at_serial
            .lock()
            .ok()
            .and_then(|serials| serials.get(&lock.lock_identifier).copied());
        let released = self.release(&path, tenant, &lock.lock_identifier).await?;
        if let Some(document) = &released
            && acquired_serial.is_none_or(|serial| document.serial > serial.saturating_add(1))
        {
            self.snapshot(&path, document).await;
        }
        if let Ok(mut serials) = self.acquired_at_serial.lock() {
            serials.remove(&lock.lock_identifier);
        }
        self.delete_marker(&path, &lock.lock_identifier).await;
        Ok(released.is_some())
    }

    async fn current_lock(&self, tenant: &TenantKey) -> Result<Option<LockInformation>> {
        self.ensure_not_public().await?;
        let path = self.path(tenant)?;
        Ok(self.read(&path).await?.and_then(|(document, _)| {
            document
                .lock
                .map(|lock| lock.information(document.format.write))
        }))
    }

    async fn locks(&self) -> Result<Vec<TenantLock>> {
        self.locks_under(None).await
    }

    async fn environments(&self, module_path: &str, project: &str) -> Result<Vec<Option<String>>> {
        let project = TenantKey::try_new(module_path, project)?;
        Ok(self
            .legacy_project_inventory(&project)
            .await?
            .into_iter()
            .map(|entry| entry.environment)
            .collect())
    }

    async fn force_unlock(&self, tenant: &TenantKey, lock_identifier: &str) -> Result<bool> {
        let path = self.path(tenant)?;
        let released = self.release(&path, tenant, lock_identifier).await?;
        if released.is_some() {
            self.delete_marker(&path, lock_identifier).await;
        }
        Ok(released.is_some())
    }

    async fn last_writer(&self, tenant: &TenantKey) -> Result<Option<LastWriter>> {
        let path = self.path(tenant)?;
        Ok(self.read(&path).await?.and_then(|(document, _)| {
            document
                .last_writer
                .as_ref()
                .map(DocumentWriter::last_writer)
        }))
    }
}

impl ObjectStateStore {
    /// Take the lock in `state.json`, creating it only when permitted.
    /// Returns the serial of the document holding the lock.
    async fn acquire_in_document(&self, acquisition: DocumentAcquisition<'_>) -> Result<u64> {
        let DocumentAcquisition {
            path,
            tenant,
            request,
            missing,
        } = acquisition;
        let identifier = &request.lock.lock_identifier;
        let new_lock = || DocumentLock {
            lock_identifier: identifier.clone(),
            holder: request.holder.to_string(),
            acquired_at: now(),
            cuenv_version: env!("CARGO_PKG_VERSION").to_string(),
            last_change: None,
            extra: serde_json::Map::new(),
        };
        let writer_record = request
            .instance
            .map(|instance: &ProjectInstance| DocumentWriter {
                instance: instance.as_str().to_string(),
                at: now(),
                extra: serde_json::Map::new(),
            });
        let _gate = self.write_gate.lock().await;
        let mut last_failure = String::new();
        for _ in 0..MUTATION_ATTEMPTS {
            let Some((document, etag)) = self.read(path).await? else {
                if matches!(missing, MissingDocument::Refuse) {
                    return Err(self.vanished(path));
                }
                self.require_not_vanished(path).await?;
                let mut document = StateDocument::new(path, 1);
                document.lock = Some(new_lock());
                document.last_writer.clone_from(&writer_record);
                if self.create(path, document).await? {
                    return Ok(1);
                }
                continue;
            };
            self.refuse_tombstone(path, &document)?;
            if let Some(held) = &document.lock {
                require_ordinary_lock(held, document.format.write)?;
                if &held.lock_identifier == identifier {
                    // An earlier attempt whose response was lost did commit.
                    return Ok(document.serial);
                }
                return Err(InfrastructureError::Locked {
                    tenant: tenant.to_string(),
                    lock: held.information(document.format.write),
                });
            }
            let key = path.state_key();
            document.require_writable(&key)?;
            // A lock must remain releasable even if no further mutation fits.
            require_serial_headroom(&document, 2)?;
            let mut next = document.clone();
            let mut ordinary = new_lock();
            if document.format.write >= 2 {
                ordinary.extra.insert(LOCK_KIND.into(), json!("ordinary"));
            }
            next.lock = Some(ordinary);
            if writer_record.is_some() {
                next.last_writer.clone_from(&writer_record);
                if let Some(next_writer) = &mut next.last_writer
                    && let Some(previous) = &document.last_writer
                {
                    next_writer.extra.clone_from(&previous.extra);
                }
            }
            let writer = Self::nonce();
            next.advance(&writer)?;
            let body = encode_bounded(&next, &key)?;
            match self
                .client
                .put(&key, body, &Condition::Matches(etag))
                .await?
            {
                Ok(_) => return Ok(next.serial),
                Err(S3Failure::PreconditionFailed) => {
                    last_failure = "the object changed concurrently".to_string();
                }
                Err(S3Failure::Ambiguous(_) | S3Failure::RateLimited(_)) => {
                    if let Some((stored, _)) = self.read(path).await?
                        && stored.writer == writer
                        && stored
                            .lock
                            .as_ref()
                            .is_some_and(|held| &held.lock_identifier == identifier)
                    {
                        return Ok(stored.serial);
                    }
                    return Err(InfrastructureError::state(
                        "the state lock acquisition could not be confirmed; inspect the lock before retrying",
                    ));
                }
                Err(S3Failure::Missing) => {
                    last_failure = "the object vanished".to_string();
                }
                Err(S3Failure::Permanent(message)) => {
                    return Err(InfrastructureError::state(format!(
                        "object storage refused writing {key}: {message}"
                    )));
                }
            }
        }
        Err(InfrastructureError::state(format!(
            "could not acquire the state lock for {tenant}: {last_failure}; the lock may have \
             been acquired as {identifier}; if so, release that lock by its identifier"
        )))
    }

    /// Clear the lock `lock_identifier` and record it as released. Returns
    /// the committed document, or `None` when that lock was not held.
    async fn release(
        &self,
        path: &TenantPath,
        tenant: &TenantKey,
        lock_identifier: &str,
    ) -> Result<Option<StateDocument>> {
        let key = path.state_key();
        let _gate = self.write_gate.lock().await;
        let mut last_failure = String::new();
        for _ in 0..MUTATION_ATTEMPTS {
            let Some((document, etag)) = self.read(path).await? else {
                return Ok(None);
            };
            if document
                .lock
                .as_ref()
                .is_none_or(|held| held.lock_identifier != lock_identifier)
            {
                return Ok(None);
            }
            if let Some(held) = &document.lock {
                require_ordinary_lock(held, document.format.write)?;
            }
            document.require_writable(&key)?;
            require_serial_headroom(&document, 1)?;
            let mut next = document.clone();
            next.lock = None;
            next.last_released = Some(lock_identifier.to_string());
            let writer = Self::nonce();
            next.advance(&writer)?;
            let body = encode_bounded(&next, &key)?;
            match self
                .client
                .put(&key, body, &Condition::Matches(etag))
                .await?
            {
                Ok(_) => return Ok(Some(next)),
                Err(S3Failure::PreconditionFailed) => {
                    last_failure = "the object changed concurrently".to_string();
                }
                Err(S3Failure::Ambiguous(_) | S3Failure::RateLimited(_)) => {
                    if let Some((stored, _)) = self.read(path).await?
                        && stored.writer == writer
                        && stored.last_released.as_deref() == Some(lock_identifier)
                        && stored.lock.is_none()
                    {
                        return Ok(Some(stored));
                    }
                    return Err(InfrastructureError::state(
                        "the state lock release could not be confirmed; inspect the lock before retrying",
                    ));
                }
                Err(S3Failure::Missing) => return Ok(None),
                Err(S3Failure::Permanent(message)) => {
                    return Err(InfrastructureError::state(format!(
                        "object storage refused writing {key}: {message}"
                    )));
                }
            }
        }
        Err(InfrastructureError::state(format!(
            "could not release the state lock {lock_identifier} of {tenant}: {last_failure}"
        )))
    }

    /// The locks held under `module` (every lock when `None`), found
    /// through their markers and confirmed in `state.json`. A module's
    /// listing stays inside `_locks/<module>/_`, so prefix-scoped
    /// credentials can list it.
    async fn locks_under(&self, module: Option<&str>) -> Result<Vec<TenantLock>> {
        self.ensure_not_public().await?;
        let prefix = super::super::key::markers_prefix(self.root.as_deref(), module)?;
        let mut paths: Vec<(TenantPath, String)> = self
            .list_keys(&prefix)
            .await?
            .iter()
            .map(|key| {
                let (path, identifier) =
                    super::super::key::parse_marker_key(self.root.as_deref(), key).ok_or_else(
                        || {
                            InfrastructureError::state(
                                "object storage lock listing contains an invalid marker",
                            )
                        },
                    )?;
                if path.marker_key(&identifier) != *key {
                    return Err(InfrastructureError::state(
                        "object storage lock listing contains an invalid marker",
                    ));
                }
                Ok((path, identifier))
            })
            .collect::<Result<_>>()?;
        paths.sort_by_key(|path| path.0.state_key());
        paths.dedup_by(|left, right| left.0 == right.0);
        let mut locks = Vec::new();
        for (path, _) in paths {
            // The marker only says where to look; `state.json` is the
            // authority, and holder details come from it alone.
            let Some((document, _)) = self.read(&path).await.map_err(|_| {
                InfrastructureError::state("cannot inspect a listed object storage lock")
            })?
            else {
                continue;
            };
            let Some(lock) = document.lock else {
                continue;
            };
            locks.push(TenantLock {
                module_path: path.module_path().to_string(),
                package: Some(path.package().to_string()),
                project: path.project().to_string(),
                environment: path.environment().map(ToString::to_string),
                lock: lock.information(document.format.write),
            });
        }
        locks.sort_by(|left, right| {
            (&left.lock.acquired_at, left.label()).cmp(&(&right.lock.acquired_at, right.label()))
        });
        Ok(locks)
    }

    /// The size of the tenant's `state.json` in bytes, `None` when absent.
    ///
    /// # Errors
    ///
    /// Returns an error when the object cannot be read.
    pub async fn state_size(&self, tenant: &TenantKey) -> Result<Option<usize>> {
        let path = self.path(tenant)?;
        Ok(self
            .get(&path.state_key())
            .await?
            .map(|object| object.body.len()))
    }

    /// Create the tenant's state from records copied out of another
    /// backend, holding `lock` in the same create. Used by `state migrate`.
    /// Returns `false` when the tenant already exists.
    ///
    /// # Errors
    ///
    /// Returns an error when the create fails or the records do not encode.
    pub async fn import(&self, tenant: &TenantKey, import: &TenantImport<'_>) -> Result<bool> {
        let path = self.path(tenant)?;
        if self.read(&path).await?.is_none() && !self.history_serials(&path).await?.is_empty() {
            return Err(InfrastructureError::state(
                "the migration target has history but no current state; inspect and reconcile it before importing",
            ));
        }
        self.write_marker(&path, import.lock, import.holder).await?;
        let mut document = StateDocument::new(&path, 1);
        document.lock = Some(DocumentLock {
            lock_identifier: import.lock.lock_identifier.clone(),
            holder: import.holder.to_string(),
            acquired_at: now(),
            cuenv_version: env!("CARGO_PKG_VERSION").to_string(),
            last_change: None,
            extra: serde_json::Map::new(),
        });
        document.last_writer = import.last_writer.map(|writer| DocumentWriter {
            instance: writer.instance.as_str().to_string(),
            at: writer.recorded_at.clone(),
            extra: serde_json::Map::new(),
        });
        document.origin = Some(Origin {
            kind: "migration".to_string(),
            id: import.migration.to_string(),
            source: import.source.to_string(),
            extra: serde_json::Map::new(),
        });
        for resource in import.resources {
            document.store(resource)?;
        }
        let _gate = self.write_gate.lock().await;
        let created = self.create(&path, document).await?;
        if created && let Ok(mut serials) = self.acquired_at_serial.lock() {
            // A snapshot of the imported state is written on release.
            serials.insert(import.lock.lock_identifier.clone(), 0);
        }
        if !created {
            self.delete_marker(&path, &import.lock.lock_identifier)
                .await;
        }
        Ok(created)
    }

    /// The migration that created the tenant, if any: (id, source).
    ///
    /// # Errors
    ///
    /// Returns an error when the object cannot be read.
    pub async fn origin(&self, tenant: &TenantKey) -> Result<Option<(String, String, u64)>> {
        let path = self.path(tenant)?;
        Ok(self.read(&path).await?.and_then(|(document, _)| {
            document
                .origin
                .map(|origin| (origin.id, origin.source, document.serial))
        }))
    }

    /// Read the project's migration manifest for `tenant`'s project.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest cannot be read.
    pub async fn manifest(&self, tenant: &TenantKey) -> Result<Option<MigrationManifest>> {
        let path = self.path(tenant)?;
        self.read_manifest(&path).await
    }

    /// Tombstone the tenant's state after a rollback copied it back, under
    /// `lock`.
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub async fn tombstone(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        moved_to: &str,
    ) -> Result<()> {
        let moved_to = moved_to.to_string();
        self.mutate(
            MutationContext {
                path: &self.path(tenant)?,
                tenant,
                lock,
            },
            |document| {
                document.tombstone = Some(Tombstone {
                    moved_to: moved_to.clone(),
                    extra: serde_json::Map::new(),
                });
                Ok(Mutation::Write)
            },
        )
        .await
    }
}

/// What [`ObjectStateStore::import`] creates.
#[derive(Clone, Copy)]
pub struct TenantImport<'import> {
    /// The lock held by the created document.
    pub lock: &'import StateLock,
    /// Its holder description.
    pub holder: &'import str,
    /// The records.
    pub resources: &'import [ManagedResource],
    /// The last writer recorded by the source.
    pub last_writer: Option<&'import LastWriter>,
    /// The migration identifier.
    pub migration: &'import str,
    /// Where the records came from (for display).
    pub source: &'import str,
}

#[async_trait]
impl ObjectStateFeatures for ObjectStateStore {
    async fn tenants(&self, module_path: &str) -> Result<Vec<StoredTenant>> {
        let prefix = super::super::key::module_prefix(self.root.as_deref(), module_path)?;
        let mut tenants = Vec::new();
        for key in self.list_keys(&prefix).await? {
            if !key.ends_with("/state.json") {
                continue;
            }
            let path = super::super::key::parse_state_key(self.root.as_deref(), &key)
                .filter(|path| path.state_key() == key)
                .ok_or_else(|| {
                    InfrastructureError::state(
                        "object storage listing contains an invalid state key",
                    )
                })?;
            // A tenant that cannot be read fails the listing: rename
            // detection never treats an unreadable tenant as absent.
            let (document, _) = self
                .read(&path)
                .await
                .map_err(|_| {
                    InfrastructureError::state("cannot inspect listed object storage state")
                })?
                .ok_or_else(|| {
                    InfrastructureError::state("listed object storage state vanished")
                })?;
            if document.tombstone.is_some() {
                continue;
            }
            tenants.push(StoredTenant {
                package: path.package().to_string(),
                project: path.project().to_string(),
                environment: path.environment().map(ToString::to_string),
                last_writer: document
                    .last_writer
                    .as_ref()
                    .map(DocumentWriter::last_writer),
                records: document.records.len(),
                locked: document.lock.is_some(),
                location: self.object_location(&path.state_key()),
            });
        }
        Ok(tenants)
    }

    async fn size(&self, tenant: &TenantKey) -> Result<Option<StateSize>> {
        Ok(self.state_size(tenant).await?.map(|bytes| StateSize {
            bytes,
            limit: MAXIMUM_STATE_BYTES,
        }))
    }

    async fn lock_details(&self, tenant: &TenantKey) -> Result<Option<LockDetails>> {
        let path = self.path(tenant)?;
        Ok(self
            .read(&path)
            .await?
            .and_then(|(document, _)| document.lock)
            .map(|lock| LockDetails {
                cuenv_version: crate::error::strip_control_characters(&lock.cuenv_version),
                last_change: lock.last_change.map(|change| {
                    crate::error::strip_control_characters(&format!(
                        "{} {} at {}",
                        change.action, change.address, change.at
                    ))
                }),
            }))
    }

    async fn module_locks(&self, module_path: &str) -> Result<Vec<TenantLock>> {
        self.locks_under(Some(module_path)).await
    }

    async fn awaiting_migration(&self, tenant: &TenantKey) -> Result<bool> {
        let path = self.path(tenant)?;
        Ok(self
            .read_manifest(&path)
            .await?
            .is_some_and(|manifest| manifest.lists(&path)))
    }

    async fn history(&self, tenant: &TenantKey) -> Result<Vec<HistoryEntry>> {
        let path = self.path(tenant)?;
        let mut entries = Vec::new();
        for serial in self.history_serials(&path).await? {
            let snapshot = self.read_snapshot(&path, serial).await?;
            entries.push(HistoryEntry {
                serial,
                written_by: history_writer(&snapshot.written_by),
                addresses: snapshot
                    .addresses()?
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            });
        }
        Ok(entries)
    }

    async fn restore(
        &self,
        tenant: &TenantKey,
        request: &RestoreRequest<'_>,
    ) -> Result<HistoryEntry> {
        let path = self.path(tenant)?;
        let serials = self.history_serials(&path).await?;
        let serial = match request.serial {
            Some(serial) if serials.contains(&serial) => serial,
            Some(serial) => {
                return Err(InfrastructureError::configuration(format!(
                    "no history snapshot at serial {serial}; available: {}",
                    serials
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            None => *serials.last().ok_or_else(|| {
                InfrastructureError::configuration(format!(
                    "{} has no history snapshots to restore",
                    self.object_location(&path.state_key())
                ))
            })?,
        };
        let snapshot = self.read_snapshot(&path, serial).await?;
        snapshot.require_writable(&path.history_key(serial))?;
        // A damaged snapshot must be rejected before creating a marker or
        // acquiring a lock. Listing keys alone still permits removing a
        // damaged record whose address is valid. A snapshot's generation may
        // be nil because restore assigns a fresh generation to every record.
        let snapshot_addresses = snapshot.addresses()?;
        for resource in snapshot.resources()? {
            if resource.serial <= 0 {
                return Err(InfrastructureError::state(
                    "history contains a resource with an invalid version",
                ));
            }
        }
        let existing = self.read(&path).await?;
        if let Some((document, _)) = &existing
            && let Some(held) = &document.lock
        {
            require_ordinary_lock(held, document.format.write)?;
        }
        if existing.is_none() {
            let manifest_binds_tenant = self.read_manifest(&path).await?.is_some_and(|manifest| {
                manifest
                    .tenants
                    .iter()
                    .any(|entry| entry.environment.as_deref() == path.environment())
            });
            if migration_bound(&snapshot) || manifest_binds_tenant {
                return Err(InfrastructureError::state(
                    "a missing migration-bound state cannot be restored without reconciling its migration",
                ));
            }
        }
        let highest = serials.last().copied().unwrap_or(0);
        highest.checked_add(2).ok_or_else(|| {
            InfrastructureError::state("object storage state serial is exhausted")
        })?;
        if let Some((document, _)) = &existing {
            require_restorable_writer(document)?;
            // Acquiring the lock, restoring the snapshot and releasing the lock
            // each advance the current document's serial.
            require_serial_headroom(document, 3)?;
        }
        let restored_records = |document: &mut StateDocument| -> Result<()> {
            require_restorable_writer(document)?;
            // Changes in flight now stay in flight: a restore rewinds what is
            // recorded, never the fence on an outcome nobody recorded. Markers
            // in the snapshot were cleared since and are not brought back.
            let mut records: std::collections::BTreeMap<String, serde_json::Value> = document
                .resources()?
                .into_iter()
                .filter(|resource| crate::in_flight::is_marker(&resource.address))
                .filter_map(|resource| {
                    let key = resource.address.to_string();
                    document.records.get(&key).map(|raw| (key, raw.clone()))
                })
                .collect();
            for (address, raw) in &snapshot.records {
                let resource: ManagedResource =
                    serde_json::from_value(raw.clone()).map_err(|_| {
                        InfrastructureError::state("history contains an invalid resource")
                    })?;
                if resource.address.to_string() != *address {
                    return Err(InfrastructureError::state(
                        "history resource address does not match its key",
                    ));
                }
                if crate::in_flight::is_marker(&resource.address) {
                    continue;
                }
                // A fresh generation, so a rewound record can never match a
                // version an older unrecorded file expects.
                let mut restored = raw.clone();
                let fields = restored.as_object_mut().ok_or_else(|| {
                    InfrastructureError::state("history contains an invalid resource")
                })?;
                fields.insert("generation".into(), json!(uuid::Uuid::new_v4()));
                records.insert(address.clone(), restored);
            }
            document.records = records;
            document.last_writer.clone_from(&snapshot.last_writer);
            document.tombstone = None;
            Ok(())
        };
        self.write_marker(&path, request.lock, request.holder)
            .await?;
        if existing.is_none() {
            // The only path that may recreate a vanished `state.json`: the
            // create carries the restorer's lock.
            let mut document = StateDocument::new(&path, highest + 1);
            document.format.clone_from(&snapshot.format);
            document.tenant.clone_from(&snapshot.tenant);
            document.origin.clone_from(&snapshot.origin);
            document.extra.clone_from(&snapshot.extra);
            restored_records(&mut document)?;
            document.lock = Some(DocumentLock {
                lock_identifier: request.lock.lock_identifier.clone(),
                holder: request.holder.to_string(),
                acquired_at: now(),
                cuenv_version: env!("CARGO_PKG_VERSION").to_string(),
                last_change: None,
                extra: serde_json::Map::new(),
            });
            let created = {
                let _gate = self.write_gate.lock().await;
                self.create(&path, document).await?
            };
            if !created {
                self.delete_marker(&path, &request.lock.lock_identifier)
                    .await;
                return Err(InfrastructureError::state(format!(
                    "{} was created concurrently; run `state restore` again",
                    self.object_location(&path.state_key())
                )));
            }
        } else {
            let lock = match self
                .acquire_in_document(DocumentAcquisition {
                    path: &path,
                    tenant,
                    request: &LockRequest {
                        lock: request.lock,
                        holder: request.holder,
                        instance: None,
                    },
                    missing: MissingDocument::Refuse,
                })
                .await
            {
                Ok(_) => request.lock.clone(),
                Err(error) => {
                    return Err(error);
                }
            };
            let restored = self
                .mutate(
                    MutationContext {
                        path: &path,
                        tenant,
                        lock: &lock,
                    },
                    |document| {
                        restored_records(document)?;
                        if document.serial <= highest {
                            // Keep serials above every snapshot.
                            document.serial = highest;
                        }
                        Ok(Mutation::Write)
                    },
                )
                .await;
            if let Err(error) = restored {
                let _ = self.unlock(tenant, &lock).await;
                return Err(error);
            }
        }
        if let Ok(mut serials) = self.acquired_at_serial.lock() {
            serials.insert(request.lock.lock_identifier.clone(), 0);
        }
        if !self.unlock(tenant, request.lock).await? {
            return Err(lock_lost(tenant));
        }
        Ok(HistoryEntry {
            serial,
            written_by: history_writer(&snapshot.written_by),
            addresses: snapshot_addresses.iter().map(ToString::to_string).collect(),
        })
    }

    async fn rename(&self, from: &ObjectTenant, to: &ObjectTenant, holder: &str) -> Result<()> {
        let source = TenantPath::try_new(self.root.as_deref(), &from.package, &from.tenant)?;
        let target = TenantPath::try_new(self.root.as_deref(), &to.package, &to.tenant)?;
        if source == target {
            return Err(InfrastructureError::configuration(
                "the rename's source and target are the same tenant",
            ));
        }
        let (document, _) = self
            .read(&source)
            .await?
            .ok_or_else(|| InfrastructureError::configuration("the rename source has no state"))?;
        self.require_rename_source(&source, &document)?;
        // Acquiring the source lock, tombstoning and releasing each use one
        // serial. The target needs only the latter two.
        require_serial_headroom(&document, 3)?;
        self.require_unused_rename_target(&target).await?;
        let lock = StateLock::generate();
        let identifier = lock.lock_identifier.clone();
        let rename_id = format!("rename-{}", Self::nonce());
        // Lock the source in its own document.
        self.write_marker(&source, &lock, holder).await?;
        let source_serial = match self
            .acquire_in_document(DocumentAcquisition {
                path: &source,
                tenant: &from.tenant,
                request: &LockRequest {
                    lock: &lock,
                    holder,
                    instance: None,
                },
                missing: MissingDocument::Refuse,
            })
            .await
        {
            Ok(serial) => serial,
            Err(error) => {
                // The marker remains discoverable if the acquisition committed
                // but its response could not be confirmed.
                if matches!(error, InfrastructureError::Locked { .. }) {
                    return Err(error);
                }
                return Err(InfrastructureError::state(
                    "the rename source lock could not be confirmed; inspect state locks before retrying",
                ));
            }
        };
        let step = RenameStep {
            source: &source,
            target: &target,
            from,
            to,
            lock: &lock,
            holder,
            rename_id: &rename_id,
            source_serial,
        };
        let locked_source = match self.preflight_locked_rename(&step).await {
            Ok(document) => document,
            Err(error) => {
                if matches!(
                    self.release(&source, &from.tenant, &identifier).await,
                    Ok(Some(_))
                ) {
                    self.delete_marker(&source, &identifier).await;
                    return Err(error);
                }
                return Err(InfrastructureError::state(
                    "rename stopped before copying, but the source lock release could not be confirmed; inspect state locks before retrying",
                ));
            }
        };
        self.rename_locked(&step, &locked_source)
            .await
            .map_err(|_| {
                InfrastructureError::state(
                    "rename may have copied the target or moved the source; source or target locks may remain. Inspect both state objects and state locks, then reconcile them manually before retrying",
                )
            })
    }
}

/// One rename in progress.
struct RenameStep<'step> {
    source: &'step TenantPath,
    target: &'step TenantPath,
    from: &'step ObjectTenant,
    to: &'step ObjectTenant,
    lock: &'step StateLock,
    holder: &'step str,
    rename_id: &'step str,
    source_serial: u64,
}

impl ObjectStateStore {
    fn require_rename_source(&self, path: &TenantPath, document: &StateDocument) -> Result<()> {
        self.refuse_tombstone(path, document)?;
        document.require_writable(&path.state_key())?;
        if migration_bound(document) {
            return Err(InfrastructureError::state(
                "a migration-bound state cannot be renamed without reconciling its migration",
            ));
        }
        if !document.extra.is_empty()
            || !document.format.extra.is_empty()
            || !document.tenant.extra.is_empty()
            || document
                .origin
                .as_ref()
                .is_some_and(|origin| origin.kind != "rename" || !origin.extra.is_empty())
        {
            return Err(InfrastructureError::state(
                "state has metadata that cannot safely be moved by rename",
            ));
        }
        for resource in document.resources()? {
            if resource.serial <= 0 || resource.generation.is_nil() {
                return Err(InfrastructureError::state(
                    "state contains a resource with an invalid version",
                ));
            }
        }
        Ok(())
    }

    async fn require_unused_rename_target(&self, target: &TenantPath) -> Result<()> {
        let manifest_binds_target = self.read_manifest(target).await?.is_some_and(|manifest| {
            manifest
                .tenants
                .iter()
                .any(|entry| entry.environment.as_deref() == target.environment())
        });
        if self.read(target).await?.is_some()
            || !self.history_serials(target).await?.is_empty()
            || manifest_binds_target
        {
            return Err(InfrastructureError::configuration(format!(
                "{} already holds state or migration evidence; a rename never overwrites it",
                self.object_location(&target.state_key())
            )));
        }
        Ok(())
    }

    async fn preflight_locked_rename(&self, step: &RenameStep<'_>) -> Result<StateDocument> {
        let (source_document, _) = self
            .read(step.source)
            .await?
            .ok_or_else(|| self.vanished(step.source))?;
        self.require_rename_source(step.source, &source_document)?;
        if source_document.serial != step.source_serial
            || source_document
                .lock
                .as_ref()
                .is_none_or(|held| held.lock_identifier != step.lock.lock_identifier)
        {
            return Err(lock_lost(&step.from.tenant));
        }
        require_serial_headroom(&source_document, 2)?;
        self.require_unused_rename_target(step.target).await?;
        Ok(source_document)
    }

    async fn rename_locked(
        &self,
        step: &RenameStep<'_>,
        source_document: &StateDocument,
    ) -> Result<()> {
        // Create the target, locked by the rename, continuing the serial.
        self.write_marker(step.target, step.lock, step.holder)
            .await?;
        let mut document = StateDocument::new(step.target, step.source_serial + 1);
        document.format.clone_from(&source_document.format);
        document.records.clone_from(&source_document.records);
        document
            .last_writer
            .clone_from(&source_document.last_writer);
        document.origin = Some(Origin {
            kind: "rename".to_string(),
            id: step.rename_id.to_string(),
            source: self.object_location(&step.source.state_key()),
            extra: serde_json::Map::new(),
        });
        document.lock = Some(DocumentLock {
            lock_identifier: step.lock.lock_identifier.clone(),
            holder: step.holder.to_string(),
            acquired_at: now(),
            cuenv_version: env!("CARGO_PKG_VERSION").to_string(),
            last_change: None,
            extra: serde_json::Map::new(),
        });
        let mut expected = document.clone();
        let created = {
            let _gate = self.write_gate.lock().await;
            self.create(step.target, document).await?
        };
        if !created {
            self.delete_marker(step.target, &step.lock.lock_identifier)
                .await;
            return Err(InfrastructureError::state(format!(
                "{} was created concurrently; nothing was moved",
                self.object_location(&step.target.state_key())
            )));
        }
        // Verify the copy.
        let (copied, _) = self
            .read(step.target)
            .await?
            .ok_or_else(|| self.vanished(step.target))?;
        // The create operation assigns its own fresh writer nonce.
        expected.writer.clone_from(&copied.writer);
        if copied != expected {
            return Err(InfrastructureError::state(format!(
                "the copy at {} does not match the source; the source was left unchanged",
                self.object_location(&step.target.state_key())
            )));
        }
        // Tombstone the source, then release both locks.
        let moved_to = step.target.state_key();
        self.mutate(
            MutationContext {
                path: step.source,
                tenant: &step.from.tenant,
                lock: step.lock,
            },
            |source| {
                if &*source != source_document {
                    return Err(InfrastructureError::state(
                        "the rename source changed after its copy was made",
                    ));
                }
                source.records.clear();
                source.tombstone = Some(Tombstone {
                    moved_to: moved_to.clone(),
                    extra: serde_json::Map::new(),
                });
                Ok(Mutation::Write)
            },
        )
        .await?;
        let source = self
            .release(step.source, &step.from.tenant, &step.lock.lock_identifier)
            .await?
            .ok_or_else(|| lock_lost(&step.from.tenant))?;
        if source.tombstone.as_ref().map(|value| &value.moved_to) != Some(&moved_to)
            || !source.records.is_empty()
        {
            return Err(InfrastructureError::state(
                "the rename source release does not confirm the tombstone",
            ));
        }
        self.delete_marker(step.source, &step.lock.lock_identifier)
            .await;
        let released = self
            .release(step.target, &step.to.tenant, &step.lock.lock_identifier)
            .await?
            .ok_or_else(|| lock_lost(&step.to.tenant))?;
        self.snapshot(step.target, &released).await;
        self.delete_marker(step.target, &step.lock.lock_identifier)
            .await;
        Ok(())
    }
}
