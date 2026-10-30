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

use super::client::{
    Addressing, Attempt, BucketAccess, Condition, S3Client, S3Failure, StoredObject,
};
use super::document::{
    DocumentLock, DocumentWriter, LastChange, Origin, StateDocument, Tombstone, written_by,
};
use super::key::{
    TenantPath, markers_prefix, module_prefix, parse_history_serial, parse_marker_key,
    parse_state_key, validate_package, validate_root,
};
use super::sigv4::Credentials;
use super::{HistoryEntry, ObjectStateFeatures, ObjectTenant, RestoreRequest, StoredTenant};
use crate::error::{InfrastructureError, Result, strip_control_characters};
use crate::state::{
    ConditionalPut, LastWriter, LockInformation, LockRequest, ManagedResource, RecordVersion,
    ResourceAddress, StateLock, StateStore, TenantLock,
};
use crate::tenant::{ProjectInstance, TenantKey};

/// Largest `state.json` a writer produces (16 MiB).
pub const MAXIMUM_STATE_BYTES: usize = 16 * 1024 * 1024;

/// Attempts a mutation makes before giving up.
const MUTATION_ATTEMPTS: u32 = 8;

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
    client: S3Client,
    root: Option<String>,
    package: String,
    history: u32,
    probe_tenant: Option<TenantKey>,
    probed: tokio::sync::OnceCell<()>,
    checked_public: tokio::sync::OnceCell<()>,
    /// Serialises this process's mutations of one store.
    write_gate: tokio::sync::Mutex<()>,
    /// The serial each held lock was acquired at, to tell whether a run
    /// changed anything (and so needs a history snapshot).
    acquired_at_serial: Mutex<HashMap<String, u64>>,
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
            validate_root(root)?;
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
        validate_package(&configuration.package)?;
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
        let client = S3Client::new(BucketAccess {
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
            checked_public: tokio::sync::OnceCell::new(),
            write_gate: tokio::sync::Mutex::new(()),
            acquired_at_serial: Mutex::new(HashMap::new()),
        })
    }

    fn path(&self, tenant: &TenantKey) -> Result<TenantPath> {
        TenantPath::new(self.root.as_deref(), &self.package, tenant)
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
    async fn get(&self, key: &str) -> Result<Option<StoredObject>> {
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
    async fn list_keys(&self, prefix: &str) -> Result<Vec<String>> {
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
    async fn read(&self, path: &TenantPath) -> Result<Option<(StateDocument, String)>> {
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
        let target = parse_state_key(self.root.as_deref(), &tombstone.moved_to).map_or_else(
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
    async fn mutate<Change>(
        &self,
        path: &TenantPath,
        tenant: &TenantKey,
        lock: &StateLock,
        mut change: Change,
    ) -> Result<()>
    where
        Change: FnMut(&mut StateDocument) -> Result<Mutation> + Send,
    {
        let path = path.clone();
        let key = path.state_key();
        let _gate = self.write_gate.lock().await;
        let mut last_failure = String::new();
        for attempt in 0..MUTATION_ATTEMPTS {
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
                return Err(lock_lost(tenant, lock));
            }
            document.require_writable(&key)?;
            let mut next = document.clone();
            if change(&mut next)? == Mutation::Unchanged {
                return Ok(());
            }
            let writer = Self::nonce();
            next.advance(&writer);
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
                Err(S3Failure::RateLimited(wait)) => {
                    tokio::time::sleep(wait.unwrap_or(RATE_LIMIT_WAIT)).await;
                    last_failure = "rate limited".to_string();
                }
                Err(S3Failure::Ambiguous(message)) => {
                    match self.read(&path).await? {
                        Some((stored, _)) if stored.writer == writer => return Ok(()),
                        Some(_) => {}
                        None => return Err(self.vanished(&path)),
                    }
                    tokio::time::sleep(backoff(attempt)).await;
                    last_failure = message;
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
        document.writer = writer.clone();
        document.written_by = written_by();
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
                Err(S3Failure::RateLimited(wait)) => {
                    tokio::time::sleep(wait.unwrap_or(RATE_LIMIT_WAIT)).await;
                    last_failure = "rate limited".to_string();
                }
                Err(S3Failure::Ambiguous(message)) => {
                    if let Some((stored, _)) = self.read(path).await? {
                        return Ok(stored.writer == writer);
                    }
                    tokio::time::sleep(backoff(attempt)).await;
                    last_failure = message;
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
    async fn write_marker(&self, path: &TenantPath, lock: &StateLock, holder: &str) -> Result<()> {
        let key = path.marker_key(&lock.lock_identifier);
        let body = serde_json::to_vec(&json!({
            "holder": holder,
            "acquiredAt": chrono::Utc::now().to_rfc3339(),
        }))
        .map_err(|error| InfrastructureError::state(format!("failed to encode marker: {error}")))?;
        for attempt in 0..MUTATION_ATTEMPTS {
            match self
                .client
                .put(&key, body.clone(), &Condition::Absent)
                .await?
            {
                // A 412 is this lock's own marker from a retry: it exists.
                Ok(_) | Err(S3Failure::PreconditionFailed) => return Ok(()),
                Err(S3Failure::RateLimited(wait)) => {
                    tokio::time::sleep(wait.unwrap_or(RATE_LIMIT_WAIT)).await;
                }
                Err(S3Failure::Ambiguous(_)) => {
                    if self.get(&key).await?.is_some() {
                        return Ok(());
                    }
                    tokio::time::sleep(backoff(attempt)).await;
                }
                Err(failure) => return Err(failure.into_error(&format!("writing {key}"))),
            }
        }
        Err(InfrastructureError::state(format!(
            "could not write the lock marker {key}"
        )))
    }

    /// Delete the marker of `lock_identifier`, best effort: a stale marker
    /// is ignored by `locks()`.
    async fn delete_marker(&self, path: &TenantPath, lock_identifier: &str) {
        let key = path.marker_key(lock_identifier);
        if let Ok(Err(failure)) | Err(failure) = self
            .client
            .delete(&key)
            .await
            .map_err(|error| S3Failure::Permanent(error.to_string()))
        {
            tracing::warn!(
                key,
                ?failure,
                "could not delete a lock marker; it is ignored"
            );
        }
    }

    /// Whether state vanished: `state.json` is absent but history exists or
    /// the migration manifest lists the tenant. A failed check refuses.
    async fn require_not_vanished(&self, path: &TenantPath) -> Result<()> {
        let history = self.list_keys(&path.history_prefix()).await?;
        let newest = history
            .iter()
            .filter_map(|key| parse_history_serial(&path.history_prefix(), key))
            .max();
        if let Some(serial) = newest {
            return Err(InfrastructureError::state(format!(
                "the state object {} is missing but history exists (newest snapshot: serial \
                 {serial}); the state vanished, so nothing was created. Restore it with `state \
                 restore`",
                self.object_location(&path.state_key())
            )));
        }
        if let Some(manifest) = self.read_manifest(path).await?
            && manifest.lists(path)
        {
            return Err(InfrastructureError::state(format!(
                "{} is listed in the migration manifest {} but its state is missing; run `state \
                 migrate` for it before using the object-storage backend",
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
            .filter_map(|key| parse_history_serial(&prefix, key))
            .collect();
        serials.sort_unstable();
        Ok(serials)
    }

    /// Write the snapshot of `document` and prune. Failures only warn.
    async fn snapshot(&self, path: &TenantPath, document: &StateDocument) {
        if let Err(error) = self.snapshot_and_prune(path, document).await {
            tracing::warn!(%error, "could not write or prune a history snapshot");
            cuenv_events::emit_stderr!(format!(
                "warning: could not write the history snapshot of {}: {}",
                self.object_location(&path.state_key()),
                self.client.redact(&error.to_string())
            ));
        }
    }

    async fn snapshot_and_prune(&self, path: &TenantPath, document: &StateDocument) -> Result<()> {
        if self.history > 0 {
            let mut snapshot = document.clone();
            snapshot.lock = None;
            let key = path.history_key(document.serial);
            let body = snapshot.encode()?;
            match self.client.put(&key, body, &Condition::Absent).await? {
                Ok(_) => {}
                Err(S3Failure::PreconditionFailed | S3Failure::Ambiguous(_)) => {
                    let ours = self.get(&key).await?.is_some_and(|object| {
                        serde_json::from_slice::<StateDocument>(&object.body)
                            .is_ok_and(|stored| stored.writer == document.writer)
                    });
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
        StateDocument::decode(&object.body, path, &key)
    }

    // -- probing ------------------------------------------------------------

    /// Check, once per process, that the store honours conditional writes,
    /// returns strong ETags and does not serve the bucket to unsigned
    /// requests. Every probe key is random and inside the tenant prefix.
    async fn probe(&self) -> Result<()> {
        let Some(tenant) = &self.probe_tenant else {
            return Ok(());
        };
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
        if let Err(failure) = self.client.delete(&created).await? {
            tracing::warn!(?failure, "could not delete a probe object");
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
    async fn ensure_not_public(&self) -> Result<()> {
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
            .await?
            .map(|object| {
                serde_json::from_slice::<MigrationManifest>(&object.body).map_err(|error| {
                    InfrastructureError::state(format!(
                        "migration manifest {key} does not decode: {error}"
                    ))
                })
            })
            .transpose()
    }
}

/// What a mutation did to the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutation {
    /// The document changed and must be written.
    Write,
    /// Nothing to write.
    Unchanged,
}

/// One tenant in a migration manifest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ManifestEntry {
    /// Named environment, `None` for the identity without `--env`.
    pub environment: Option<String>,
    /// `pending`, `migrated` or `rolled-back`.
    pub status: String,
    /// The migration identifier.
    pub migration: String,
}

/// A project's migration manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MigrationManifest {
    /// Tenants of the project and their status.
    pub tenants: Vec<ManifestEntry>,
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

fn lock_lost(tenant: &TenantKey, lock: &StateLock) -> InfrastructureError {
    InfrastructureError::LockLost {
        tenant: tenant.to_string(),
        lock_identifier: lock.lock_identifier.clone(),
    }
}

fn public_bucket() -> InfrastructureError {
    InfrastructureError::state(
        "the bucket serves state objects to unsigned requests: it is public, and state holds \
         plaintext provider secrets; make the bucket private before using it for state",
    )
}

fn encode_bounded(document: &StateDocument, key: &str) -> Result<Vec<u8>> {
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

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Record `resource` in `document` with the serial and generation the store
/// assigns, as the SQL and memory stores do.
fn record(
    document: &mut StateDocument,
    resource: &ManagedResource,
    insertion: uuid::Uuid,
) -> Result<()> {
    let previous = document.resource(&resource.address).ok().flatten();
    let serial = previous
        .as_ref()
        .map_or(1, |stored| stored.serial.saturating_add(1));
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
        });
    }
}

#[async_trait]
impl StateStore for ObjectStateStore {
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
        Ok(self
            .read_live(&path)
            .await?
            .map(|(document, _)| document.addresses())
            .unwrap_or_default())
    }

    async fn put(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        resource: &ManagedResource,
    ) -> Result<()> {
        let insertion = uuid::Uuid::new_v4();
        self.mutate(&self.path(tenant)?, tenant, lock, |document| {
            record(document, resource, insertion)?;
            note_change(document, &resource.address, "put");
            Ok(Mutation::Write)
        })
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
        self.mutate(&self.path(tenant)?, tenant, lock, |document| {
            for (resource, insertion) in resources.iter().zip(&insertions) {
                record(document, resource, *insertion)?;
            }
            if let Some(last) = resources.last() {
                note_change(document, &last.address, "refresh");
            }
            Ok(Mutation::Write)
        })
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
        self.mutate(&self.path(tenant)?, tenant, lock, |document| {
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
        })
        .await
    }

    async fn delete(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        address: &ResourceAddress,
    ) -> Result<()> {
        self.mutate(&self.path(tenant)?, tenant, lock, |document| {
            if document.records.remove(&address.to_string()).is_none() {
                return Ok(Mutation::Unchanged);
            }
            note_change(document, address, "delete");
            Ok(Mutation::Write)
        })
        .await
    }

    async fn acquire_lock(
        &self,
        tenant: &TenantKey,
        request: &LockRequest<'_>,
    ) -> Result<StateLock> {
        request.lock.validate()?;
        let path = self.path(tenant)?;
        self.write_marker(&path, request.lock, request.holder)
            .await?;
        let acquired = self.acquire_in_document(&path, tenant, request).await;
        match &acquired {
            Ok(serial) => {
                if let Ok(mut serials) = self.acquired_at_serial.lock() {
                    serials.insert(request.lock.lock_identifier.clone(), *serial);
                }
            }
            Err(InfrastructureError::State(message))
                if message.contains(&request.lock.lock_identifier) =>
            {
                // The outcome is uncertain: keep the marker so the lock can
                // be found and released.
            }
            Err(_) => {
                self.delete_marker(&path, &request.lock.lock_identifier)
                    .await;
            }
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
        Ok(self
            .read(&path)
            .await?
            .and_then(|(document, _)| document.lock.map(|lock| lock.information())))
    }

    async fn locks(&self) -> Result<Vec<TenantLock>> {
        self.ensure_not_public().await?;
        let prefix = markers_prefix(self.root.as_deref(), None)?;
        let mut paths: Vec<(TenantPath, String)> = self
            .list_keys(&prefix)
            .await?
            .iter()
            .filter_map(|key| parse_marker_key(self.root.as_deref(), key))
            .collect();
        paths.sort_by_key(|path| path.0.state_key());
        paths.dedup_by(|left, right| left.0 == right.0);
        let mut locks = Vec::new();
        for (path, _) in paths {
            // The marker only says where to look; `state.json` is the
            // authority, and holder details come from it alone.
            let Ok(Some((document, _))) = self.read(&path).await else {
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
                lock: lock.information(),
            });
        }
        locks.sort_by(|left, right| {
            (&left.lock.acquired_at, left.label()).cmp(&(&right.lock.acquired_at, right.label()))
        });
        Ok(locks)
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
    /// Take the lock in `state.json` (creating the document for a new
    /// tenant). Returns the serial of the document holding the lock.
    async fn acquire_in_document(
        &self,
        path: &TenantPath,
        tenant: &TenantKey,
        request: &LockRequest<'_>,
    ) -> Result<u64> {
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
            });
        let _gate = self.write_gate.lock().await;
        let mut last_failure = String::new();
        for attempt in 0..MUTATION_ATTEMPTS {
            let Some((document, etag)) = self.read(path).await? else {
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
                if &held.lock_identifier == identifier {
                    // An earlier attempt whose response was lost did commit.
                    return Ok(document.serial);
                }
                let information = held.information();
                return Err(InfrastructureError::Locked {
                    tenant: tenant.to_string(),
                    lock_identifier: information.lock_identifier,
                    holder: information.holder,
                    acquired_at: information.acquired_at,
                });
            }
            let key = path.state_key();
            document.require_writable(&key)?;
            let mut next = document.clone();
            next.lock = Some(new_lock());
            if writer_record.is_some() {
                next.last_writer.clone_from(&writer_record);
            }
            let writer = Self::nonce();
            next.advance(&writer);
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
                Err(S3Failure::RateLimited(wait)) => {
                    tokio::time::sleep(wait.unwrap_or(RATE_LIMIT_WAIT)).await;
                    last_failure = "rate limited".to_string();
                }
                Err(S3Failure::Ambiguous(message)) => {
                    if let Some((stored, _)) = self.read(path).await?
                        && stored
                            .lock
                            .as_ref()
                            .is_some_and(|held| &held.lock_identifier == identifier)
                    {
                        return Ok(stored.serial);
                    }
                    tokio::time::sleep(backoff(attempt)).await;
                    last_failure = message;
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
        for attempt in 0..MUTATION_ATTEMPTS {
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
            document.require_writable(&key)?;
            let mut next = document.clone();
            next.lock = None;
            next.last_released = Some(lock_identifier.to_string());
            let writer = Self::nonce();
            next.advance(&writer);
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
                Err(S3Failure::RateLimited(wait)) => {
                    tokio::time::sleep(wait.unwrap_or(RATE_LIMIT_WAIT)).await;
                    last_failure = "rate limited".to_string();
                }
                Err(S3Failure::Ambiguous(message)) => {
                    if let Some((stored, _)) = self.read(path).await?
                        && stored.last_released.as_deref() == Some(lock_identifier)
                        && stored.lock.is_none()
                    {
                        return Ok(Some(stored));
                    }
                    tokio::time::sleep(backoff(attempt)).await;
                    last_failure = message;
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
        });
        document.origin = Some(Origin {
            kind: "migration".to_string(),
            id: import.migration.to_string(),
            source: import.source.to_string(),
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

    /// Write the project's migration manifest (no condition: one migrator
    /// holds every source lock while it runs).
    ///
    /// # Errors
    ///
    /// Returns an error when the write fails.
    pub async fn write_manifest(
        &self,
        tenant: &TenantKey,
        manifest: &MigrationManifest,
    ) -> Result<()> {
        let path = self.path(tenant)?;
        let key = path.migration_manifest_key();
        let body = serde_json::to_vec(manifest).map_err(|error| {
            InfrastructureError::state(format!("failed to encode migration manifest: {error}"))
        })?;
        let written = self
            .retry_rate_limited(|| self.client.put(&key, body.clone(), &Condition::None))
            .await?;
        written
            .map(|_| ())
            .map_err(|failure| failure.into_error(&format!("writing {key}")))
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
        self.mutate(&self.path(tenant)?, tenant, lock, |document| {
            document.records.clear();
            document.tombstone = Some(Tombstone {
                moved_to: moved_to.clone(),
            });
            Ok(Mutation::Write)
        })
        .await
    }
}

/// What [`ObjectStateStore::import`] creates.
#[derive(Debug, Clone, Copy)]
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
        let prefix = module_prefix(self.root.as_deref(), module_path)?;
        let mut tenants = Vec::new();
        for key in self.list_keys(&prefix).await? {
            let Some(path) = parse_state_key(self.root.as_deref(), &key) else {
                continue;
            };
            let Ok(Some((document, _))) = self.read(&path).await else {
                continue;
            };
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

    async fn history(&self, tenant: &TenantKey) -> Result<Vec<HistoryEntry>> {
        let path = self.path(tenant)?;
        let mut entries = Vec::new();
        for serial in self.history_serials(&path).await? {
            let snapshot = self.read_snapshot(&path, serial).await?;
            entries.push(HistoryEntry {
                serial,
                written_by: strip_control_characters(&snapshot.written_by),
                addresses: snapshot
                    .addresses()
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
        let restored_records = |document: &mut StateDocument| -> Result<()> {
            document.records.clear();
            for resource in snapshot.resources()? {
                // A fresh generation, so a rewound record can never match a
                // version an older unrecorded file expects.
                document.store(&ManagedResource {
                    generation: uuid::Uuid::new_v4(),
                    ..resource
                })?;
            }
            document.last_writer.clone_from(&snapshot.last_writer);
            document.tombstone = None;
            Ok(())
        };
        self.write_marker(&path, request.lock, request.holder)
            .await?;
        let highest = serials.last().copied().unwrap_or(0);
        let existing = self.read(&path).await?;
        if existing.is_none() {
            // The only path that may recreate a vanished `state.json`: the
            // create carries the restorer's lock.
            let mut document = StateDocument::new(&path, highest.saturating_add(1));
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
                .acquire_in_document(
                    &path,
                    tenant,
                    &LockRequest {
                        lock: request.lock,
                        holder: request.holder,
                        instance: None,
                    },
                )
                .await
            {
                Ok(_) => request.lock.clone(),
                Err(error) => {
                    self.delete_marker(&path, &request.lock.lock_identifier)
                        .await;
                    return Err(error);
                }
            };
            let restored = self
                .mutate(&path, tenant, &lock, |document| {
                    restored_records(document)?;
                    if document.serial <= highest {
                        // Keep serials above every snapshot.
                        document.serial = highest;
                    }
                    Ok(Mutation::Write)
                })
                .await;
            if let Err(error) = restored {
                let _ = self.unlock(tenant, &lock).await;
                return Err(error);
            }
        }
        if let Ok(mut serials) = self.acquired_at_serial.lock() {
            serials.insert(request.lock.lock_identifier.clone(), 0);
        }
        self.unlock(tenant, request.lock).await?;
        Ok(HistoryEntry {
            serial,
            written_by: strip_control_characters(&snapshot.written_by),
            addresses: snapshot
                .addresses()
                .iter()
                .map(ToString::to_string)
                .collect(),
        })
    }

    async fn rename(&self, from: &ObjectTenant, to: &ObjectTenant, holder: &str) -> Result<()> {
        let source = TenantPath::new(self.root.as_deref(), &from.package, &from.tenant)?;
        let target = TenantPath::new(self.root.as_deref(), &to.package, &to.tenant)?;
        if source == target {
            return Err(InfrastructureError::configuration(
                "the rename's source and target are the same tenant",
            ));
        }
        let lock = StateLock::generate();
        let identifier = lock.lock_identifier.clone();
        let rename_id = format!("rename-{}", Self::nonce());
        // Lock the source in its own document.
        self.write_marker(&source, &lock, holder).await?;
        let source_serial = match self
            .acquire_in_document(
                &source,
                &from.tenant,
                &LockRequest {
                    lock: &lock,
                    holder,
                    instance: None,
                },
            )
            .await
        {
            Ok(serial) => serial,
            Err(error) => {
                self.delete_marker(&source, &identifier).await;
                return Err(error);
            }
        };
        let outcome = self
            .rename_locked(&RenameStep {
                source: &source,
                target: &target,
                from,
                to,
                lock: &lock,
                holder,
                rename_id: &rename_id,
                source_serial,
            })
            .await;
        if outcome.is_err() {
            let _ = self.release(&source, &from.tenant, &identifier).await;
            self.delete_marker(&source, &identifier).await;
        }
        outcome
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
    async fn rename_locked(&self, step: &RenameStep<'_>) -> Result<()> {
        let (source_document, _) = self
            .read(step.source)
            .await?
            .ok_or_else(|| self.vanished(step.source))?;
        if self.read(step.target).await?.is_some()
            || !self.history_serials(step.target).await?.is_empty()
        {
            return Err(InfrastructureError::configuration(format!(
                "{} already holds state; a rename never overwrites",
                self.object_location(&step.target.state_key())
            )));
        }
        // Create the target, locked by the rename, continuing the serial.
        self.write_marker(step.target, step.lock, step.holder)
            .await?;
        let mut document = StateDocument::new(step.target, step.source_serial.saturating_add(1));
        document.records.clone_from(&source_document.records);
        document
            .last_writer
            .clone_from(&source_document.last_writer);
        document.origin = Some(Origin {
            kind: "rename".to_string(),
            id: step.rename_id.to_string(),
            source: self.object_location(&step.source.state_key()),
        });
        document.lock = Some(DocumentLock {
            lock_identifier: step.lock.lock_identifier.clone(),
            holder: step.holder.to_string(),
            acquired_at: now(),
            cuenv_version: env!("CARGO_PKG_VERSION").to_string(),
            last_change: None,
            extra: serde_json::Map::new(),
        });
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
        if copied.records != source_document.records {
            return Err(InfrastructureError::state(format!(
                "the copy at {} does not match the source; the source was left unchanged",
                self.object_location(&step.target.state_key())
            )));
        }
        // Tombstone the source, then release both locks.
        let moved_to = step.target.state_key();
        self.mutate(step.source, &step.from.tenant, step.lock, |source| {
            source.records.clear();
            source.tombstone = Some(Tombstone {
                moved_to: moved_to.clone(),
            });
            Ok(Mutation::Write)
        })
        .await?;
        self.release(step.source, &step.from.tenant, &step.lock.lock_identifier)
            .await?;
        self.delete_marker(step.source, &step.lock.lock_identifier)
            .await;
        if let Some(released) = self
            .release(step.target, &step.to.tenant, &step.lock.lock_identifier)
            .await?
        {
            self.snapshot(step.target, &released).await;
        }
        self.delete_marker(step.target, &step.lock.lock_identifier)
            .await;
        Ok(())
    }
}
