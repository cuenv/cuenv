//! Conditional commit and rollback evidence for legacy object migrations.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::super::client::{Addressing, Condition, S3Failure};
use super::super::document::{DocumentLock, DocumentWriter, LOCK_KIND, StateDocument, Tombstone};
use super::super::key::{self, TenantPath};
use super::{ObjectStateStore, object_state_store};
use crate::error::{InfrastructureError, Result};
use crate::state::{BackendIdentity, LegacyMigrationSource, StateLock};
use crate::tenant::TenantKey;

const COMMIT_RECEIPT: &str = "cuenvLegacyMigrationCommit";
const ROLLBACK_RECEIPT: &str = "cuenvLegacyRollbackLock";
const TOMBSTONE_RECEIPT: &str = "cuenvLegacyRollbackTombstone";
const RELEASE_RECEIPT: &str = "cuenvLegacyRollbackRelease";

fn private_failure(_: InfrastructureError) -> InfrastructureError {
    InfrastructureError::state("cannot access the legacy migration target")
}

/// The original scope of a migration's selected environment set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LegacyMigrationSelection {
    /// Every environment in the project was selected.
    All,
    /// One environment was selected.
    Selected,
}

/// Immutable physical and logical boundaries of a legacy migration.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyMigrationBinding {
    /// Operator-attested identity of the physical source store.
    pub source_physical_id: Uuid,
    /// Operator-attested identity of the physical target store.
    pub target_physical_id: Uuid,
    /// Source backend and effective namespace.
    pub source: LegacyMigrationSource,
    /// Non-secret identity of the effective target connection.
    pub target_connection: BackendIdentity,
    /// Target object root.
    pub target_root: Option<String>,
    /// Target CUE package.
    pub target_package: String,
    /// CUE module path.
    pub module_path: String,
    /// Project name.
    pub project: String,
    /// Whether the original operation selected the entire project.
    pub selection: LegacyMigrationSelection,
    /// Exact selected environment set, sorted and without duplicates.
    pub environments: Vec<Option<String>>,
}

impl fmt::Debug for LegacyMigrationBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LegacyMigrationBinding(<redacted>)")
    }
}

impl LegacyMigrationBinding {
    /// Validate the physical identities, configured namespaces and tenant set.
    ///
    /// # Errors
    ///
    /// Returns an error when the binding is ambiguous or does not include the tenant.
    pub fn validate_for(&self, tenant: &TenantKey) -> Result<()> {
        let invalid = || InfrastructureError::configuration("invalid legacy migration binding");
        let source_connection = match &self.source {
            LegacyMigrationSource::Turso { connection }
            | LegacyMigrationSource::D1 { connection }
            | LegacyMigrationSource::Memory { connection } => connection,
            LegacyMigrationSource::Object {
                connection,
                root,
                package,
            } => {
                TenantPath::try_new(root.as_deref(), package, tenant).map_err(|_| invalid())?;
                connection
            }
        };
        if self.source_physical_id.is_nil()
            || self.target_physical_id.is_nil()
            || self.source_physical_id == self.target_physical_id
            || source_connection.as_bytes() == &[0; 32]
            || self.target_connection.as_bytes() == &[0; 32]
            || matches!(&self.source, LegacyMigrationSource::Object { .. })
                && *source_connection == self.target_connection
            || self.module_path != tenant.module_path()
            || self.project != tenant.project()
            || self.environments.is_empty()
            || (self.selection == LegacyMigrationSelection::Selected
                && self.environments.len() != 1)
            || !self.environments.windows(2).all(|pair| pair[0] < pair[1])
            || !self
                .environments
                .iter()
                .any(|environment| environment.as_deref() == tenant.environment())
        {
            return Err(invalid());
        }
        key::validate_environment_case(self.environments.iter().filter_map(Option::as_deref))
            .map_err(|_| invalid())?;
        for environment in &self.environments {
            let selected = match environment {
                Some(environment) => {
                    TenantKey::with_environment(&self.module_path, &self.project, environment)
                }
                None => TenantKey::try_new(&self.module_path, &self.project),
            }
            .map_err(|_| invalid())?;
            TenantPath::try_new(self.target_root.as_deref(), &self.target_package, &selected)
                .map_err(|_| invalid())?;
            if let LegacyMigrationSource::Object { root, package, .. } = &self.source {
                TenantPath::try_new(root.as_deref(), package, &selected).map_err(|_| invalid())?;
            }
        }
        Ok(())
    }
}

/// Evidence written by the target commit that released the import lock.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyCommitEvidence {
    /// The preselected writer nonce of the committed document.
    pub writer_nonce: Uuid,
    /// The document serial committed in the same conditional write.
    pub serial: u64,
}

impl fmt::Debug for LegacyCommitEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LegacyCommitEvidence(<redacted>)")
    }
}

/// A legacy import lock and a preselected commit writer nonce.
#[derive(Clone, Copy)]
pub struct LegacyCommitRequest<'commit> {
    /// Target tenant.
    pub tenant: &'commit TenantKey,
    /// Exact physical and logical migration binding.
    pub binding: &'commit LegacyMigrationBinding,
    /// Identifier of the original migration.
    pub migration: &'commit str,
    /// Exact import lock held by this run.
    pub lock: &'commit StateLock,
    /// Fresh nonce selected before sending the conditional write.
    pub writer_nonce: Uuid,
}

impl fmt::Debug for LegacyCommitRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LegacyCommitRequest(<redacted>)")
    }
}

/// Request to lock an existing committed target before rollback.
#[derive(Clone, Copy)]
pub struct LegacyRollbackRequest<'rollback> {
    /// Target tenant.
    pub tenant: &'rollback TenantKey,
    /// Exact physical and logical migration binding.
    pub binding: &'rollback LegacyMigrationBinding,
    /// Identifier of the original migration.
    pub migration: &'rollback str,
    /// Exact durable commit evidence from the manifest.
    pub commit: &'rollback LegacyCommitEvidence,
    /// Fresh rollback lock selected before sending the request.
    pub lock: &'rollback StateLock,
    /// Fresh nonce selected before sending the conditional write.
    pub writer_nonce: Uuid,
    /// Description of the lock holder.
    pub holder: &'rollback str,
}

impl fmt::Debug for LegacyRollbackRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LegacyRollbackRequest(<redacted>)")
    }
}

/// The exact target version immediately before taking a rollback lock.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyRollbackEvidence {
    /// Target serial before the lock was acquired.
    pub pre_lock_serial: u64,
    /// The original commit evidence checked in the locking write.
    pub commit: LegacyCommitEvidence,
}

impl fmt::Debug for LegacyRollbackEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LegacyRollbackEvidence(<redacted>)")
    }
}

/// An exact rollback lock and the evidence recorded when it was taken.
#[derive(Clone, Copy)]
pub struct LegacyRollbackAction<'action> {
    /// The request that acquired the protected lock.
    pub rollback: &'action LegacyRollbackRequest<'action>,
    /// The evidence returned by that acquisition.
    pub evidence: &'action LegacyRollbackEvidence,
    /// A fresh nonce for this action, selected before the write.
    pub writer_nonce: Uuid,
}

impl fmt::Debug for LegacyRollbackAction<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LegacyRollbackAction(<redacted>)")
    }
}

/// Request to mark the target as moved while retaining its protected lock.
#[derive(Clone, Copy)]
pub struct LegacyTombstoneRequest<'tombstone> {
    /// Exact rollback lock, evidence and fresh action nonce.
    pub action: LegacyRollbackAction<'tombstone>,
    /// Destination to record in the tombstone.
    pub moved_to: &'tombstone str,
}

impl fmt::Debug for LegacyTombstoneRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LegacyTombstoneRequest(<redacted>)")
    }
}

/// Evidence needed to check whether a legacy rollback completed on the target.
#[derive(Clone, Copy)]
pub struct LegacyRollbackCompletionRequest<'completion> {
    /// Target tenant.
    pub tenant: &'completion TenantKey,
    /// Identifier of the original migration.
    pub migration: &'completion str,
    /// Exact physical and logical migration binding.
    pub binding: &'completion LegacyMigrationBinding,
    /// Commit receipt recorded in the manifest.
    pub commit: &'completion LegacyCommitEvidence,
}

impl fmt::Debug for LegacyRollbackCompletionRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LegacyRollbackCompletionRequest(<redacted>)")
    }
}

/// A guarded target snapshot for checking whether its contents can be copied.
///
/// Values can contain provider secrets. The consumer must preflight every
/// extension and preserve all representable fields before changing the source.
#[derive(Clone, PartialEq)]
pub struct LegacyRollbackSnapshot {
    /// Raw records, including resource version and unknown fields.
    pub records: BTreeMap<String, Value>,
    /// Raw last writer, including extensions.
    pub last_writer: Option<DocumentWriter>,
    /// Unknown document fields other than protocol-owned receipts.
    pub document_extensions: Map<String, Value>,
    /// Unknown format fields.
    pub format_extensions: Map<String, Value>,
    /// Unknown tenant identity fields.
    pub tenant_extensions: Map<String, Value>,
    /// Unknown migration origin fields.
    pub origin_extensions: Map<String, Value>,
    /// Unknown fields on the protected lock.
    pub lock_extensions: Map<String, Value>,
    /// Unknown fields on its last-change metadata.
    pub lock_change_extensions: Map<String, Value>,
    /// A tombstone, if the target has already been marked as moved.
    pub tombstone: Option<Tombstone>,
    /// Unknown fields on the tombstone, when one is present.
    pub tombstone_extensions: Map<String, Value>,
}

impl fmt::Debug for LegacyRollbackSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LegacyRollbackSnapshot(<redacted>)")
    }
}

/// Outcome of a conditional legacy migration operation.
#[derive(Clone, PartialEq, Eq)]
pub enum LegacyMutation<Value> {
    /// Exact evidence of the requested operation is durable.
    Applied(Value),
    /// A condition did not hold and no successful write was observed.
    Stale,
    /// A response was lost and exact durable evidence was not observed.
    Indeterminate,
}

impl<Value> fmt::Debug for LegacyMutation<Value> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Applied(_) => formatter.write_str("Applied(<redacted>)"),
            Self::Stale => formatter.write_str("Stale"),
            Self::Indeterminate => formatter.write_str("Indeterminate"),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredCommit {
    migration: String,
    binding: LegacyMigrationBinding,
    lock_identifier: String,
    evidence: LegacyCommitEvidence,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredRollbackLock {
    migration: String,
    writer_nonce: Uuid,
    evidence: LegacyRollbackEvidence,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredRollbackAction {
    migration: String,
    binding: LegacyMigrationBinding,
    lock_identifier: String,
    rollback_nonce: Uuid,
    writer_nonce: Uuid,
    serial: u64,
    evidence: LegacyRollbackEvidence,
    moved_to: Option<String>,
}

fn valid_migration(migration: &str) -> bool {
    migration.len() == 16
        && migration
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_evidence(evidence: &LegacyCommitEvidence) -> bool {
    !evidence.writer_nonce.is_nil() && evidence.serial > 0 && evidence.serial < u64::MAX
}

fn matching_origin(document: &StateDocument, migration: &str) -> bool {
    document.tombstone.is_none() && origin_is_migration(document, migration)
}

fn origin_is_migration(document: &StateDocument, migration: &str) -> bool {
    document
        .origin
        .as_ref()
        .is_some_and(|origin| origin.kind == "migration" && origin.id == migration)
}

fn commit_receipt(document: &StateDocument) -> Result<Option<StoredCommit>> {
    document
        .extra
        .get(COMMIT_RECEIPT)
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|_| InfrastructureError::state("invalid legacy migration commit evidence"))
        })
        .transpose()
}

fn rollback_receipt(lock: &DocumentLock) -> Result<Option<StoredRollbackLock>> {
    lock.extra
        .get(ROLLBACK_RECEIPT)
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|_| InfrastructureError::state("invalid legacy rollback lock evidence"))
        })
        .transpose()
}

fn action_receipt(document: &StateDocument, key: &str) -> Result<Option<StoredRollbackAction>> {
    document
        .extra
        .get(key)
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|_| InfrastructureError::state("invalid legacy rollback action evidence"))
        })
        .transpose()
}

fn valid_commit(
    receipt: &StoredCommit,
    binding: &LegacyMigrationBinding,
    document_serial: u64,
) -> bool {
    let lock = StateLock {
        lock_identifier: receipt.lock_identifier.clone(),
    };
    receipt.binding == *binding
        && valid_evidence(&receipt.evidence)
        && document_serial >= receipt.evidence.serial
        && check_request(&receipt.migration, &lock, receipt.evidence.writer_nonce).is_ok()
        && check_lock_binding(&lock, binding).is_ok()
}

fn valid_rollback_request(request: &LegacyRollbackRequest<'_>) -> Result<()> {
    check_request(request.migration, request.lock, request.writer_nonce)?;
    request.binding.validate_for(request.tenant)?;
    check_lock_binding(request.lock, request.binding)?;
    if !valid_evidence(request.commit)
        || request.writer_nonce == request.commit.writer_nonce
        || Uuid::parse_str(&request.lock.lock_identifier).ok() == Some(request.commit.writer_nonce)
    {
        return Err(InfrastructureError::configuration(
            "invalid legacy rollback evidence",
        ));
    }
    Ok(())
}

fn valid_rollback_evidence(
    request: &LegacyRollbackRequest<'_>,
    evidence: &LegacyRollbackEvidence,
) -> bool {
    evidence.commit == *request.commit
        && evidence.pre_lock_serial >= request.commit.serial
        && rollback_has_serial_headroom(evidence.pre_lock_serial)
}

fn rollback_has_serial_headroom(serial: u64) -> bool {
    serial
        .checked_add(3)
        .is_some_and(|last_serial| last_serial < u64::MAX)
}

fn held_rollback(
    document: &StateDocument,
    request: &LegacyRollbackRequest<'_>,
    evidence: &LegacyRollbackEvidence,
) -> bool {
    if document.format.write != 2
        || !origin_is_migration(document, request.migration)
        || !valid_rollback_evidence(request, evidence)
        || document.serial <= evidence.pre_lock_serial
    {
        return false;
    }
    let Some(commit) = commit_receipt(document).ok().flatten() else {
        return false;
    };
    if commit.migration != request.migration
        || !valid_commit(&commit, request.binding, document.serial)
    {
        return false;
    }
    if commit.evidence != *request.commit {
        return false;
    }
    document.lock.as_ref().is_some_and(|held| {
        held.lock_identifier == request.lock.lock_identifier
            && held.extra.get(LOCK_KIND) == Some(&serde_json::json!("migration"))
            && rollback_receipt(held)
                .ok()
                .flatten()
                .is_some_and(|receipt| {
                    receipt.migration == request.migration
                        && receipt.writer_nonce == request.writer_nonce
                        && receipt.evidence == *evidence
                })
    })
}

fn action_value(
    action: &LegacyRollbackAction<'_>,
    moved_to: Option<&str>,
    serial: u64,
) -> Result<StoredRollbackAction> {
    let request = action.rollback;
    valid_rollback_request(request)?;
    if !valid_rollback_evidence(request, action.evidence)
        || action.writer_nonce.is_nil()
        || [request.writer_nonce, request.commit.writer_nonce].contains(&action.writer_nonce)
        || Uuid::parse_str(&request.lock.lock_identifier).ok() == Some(action.writer_nonce)
    {
        return Err(InfrastructureError::configuration(
            "invalid legacy rollback action",
        ));
    }
    Ok(StoredRollbackAction {
        migration: request.migration.to_string(),
        binding: request.binding.clone(),
        lock_identifier: request.lock.lock_identifier.clone(),
        rollback_nonce: request.writer_nonce,
        writer_nonce: action.writer_nonce,
        serial,
        evidence: action.evidence.clone(),
        moved_to: moved_to.map(str::to_string),
    })
}

fn proves_action(document: &StateDocument, key: &str, expected: &StoredRollbackAction) -> bool {
    if document.format.write != 2
        || !origin_is_migration(document, &expected.migration)
        || expected.serial <= expected.evidence.pre_lock_serial
        || expected.serial == u64::MAX
        || document.serial < expected.serial
        || action_receipt(document, key).ok().flatten().as_ref() != Some(expected)
    {
        return false;
    }
    let Some(commit) = commit_receipt(document).ok().flatten() else {
        return false;
    };
    if commit.migration != expected.migration
        || !valid_commit(&commit, &expected.binding, document.serial)
    {
        return false;
    }
    if commit.evidence != expected.evidence.commit {
        return false;
    }
    if let Some(moved_to) = &expected.moved_to {
        document
            .tombstone
            .as_ref()
            .is_some_and(|tombstone| tombstone.moved_to == *moved_to)
    } else {
        document
            .lock
            .as_ref()
            .is_none_or(|held| held.lock_identifier != expected.lock_identifier)
    }
}

fn proves_completion(
    document: &StateDocument,
    request: &LegacyRollbackCompletionRequest<'_>,
) -> Result<bool> {
    if document.lock.is_some() {
        return Ok(false);
    }
    let Some(tombstone) = &document.tombstone else {
        return Ok(false);
    };
    let Some(marked) = action_receipt(document, TOMBSTONE_RECEIPT)? else {
        return Ok(false);
    };
    let Some(released) = action_receipt(document, RELEASE_RECEIPT)? else {
        return Ok(false);
    };
    if marked.serial >= released.serial || marked.writer_nonce == released.writer_nonce {
        return Ok(false);
    }
    let lock = StateLock {
        lock_identifier: marked.lock_identifier.clone(),
    };
    let rollback = LegacyRollbackRequest {
        tenant: request.tenant,
        migration: request.migration,
        binding: request.binding,
        commit: request.commit,
        lock: &lock,
        writer_nonce: marked.rollback_nonce,
        holder: "",
    };
    let action = LegacyRollbackAction {
        rollback: &rollback,
        evidence: &marked.evidence,
        writer_nonce: marked.writer_nonce,
    };
    let Ok(expected_tombstone) = action_value(&action, Some(&tombstone.moved_to), marked.serial)
    else {
        return Ok(false);
    };
    let release = LegacyRollbackAction {
        writer_nonce: released.writer_nonce,
        ..action
    };
    let Ok(expected_release) = action_value(&release, None, released.serial) else {
        return Ok(false);
    };
    Ok(
        proves_action(document, TOMBSTONE_RECEIPT, &expected_tombstone)
            && proves_action(document, RELEASE_RECEIPT, &expected_release),
    )
}

fn next_serial(document: &StateDocument) -> Result<u64> {
    document
        .serial
        .checked_add(1)
        .filter(|serial| *serial < u64::MAX)
        .ok_or_else(|| InfrastructureError::state("the target state serial is exhausted"))
}

fn check_request(migration: &str, lock: &StateLock, writer_nonce: Uuid) -> Result<()> {
    lock.validate()?;
    if !valid_migration(migration)
        || writer_nonce.is_nil()
        || lock.lock_identifier == migration
        || Uuid::parse_str(&lock.lock_identifier).ok() == Some(writer_nonce)
    {
        return Err(InfrastructureError::configuration(
            "invalid legacy migration request",
        ));
    }
    Ok(())
}

fn check_lock_binding(lock: &StateLock, binding: &LegacyMigrationBinding) -> Result<()> {
    if Uuid::parse_str(&lock.lock_identifier).is_ok_and(|identifier| {
        identifier == binding.source_physical_id || identifier == binding.target_physical_id
    }) {
        return Err(InfrastructureError::configuration(
            "invalid legacy migration request",
        ));
    }
    Ok(())
}

impl ObjectStateStore {
    /// Fingerprint the effective, credential-free object connection configuration.
    ///
    /// This binds a configuration; it does not prove that different endpoints
    /// address different physical stores.
    ///
    /// # Errors
    ///
    /// Returns an error if the validated endpoint has no host or port.
    pub fn legacy_connection_identity(&self) -> Result<BackendIdentity> {
        let endpoint = self.client.endpoint();
        let host = endpoint
            .host_str()
            .ok_or_else(|| InfrastructureError::configuration("invalid object connection"))?;
        let port = endpoint
            .port_or_known_default()
            .ok_or_else(|| InfrastructureError::configuration("invalid object connection"))?
            .to_string();
        let addressing = match self.client.addressing() {
            Addressing::Path => "path",
            Addressing::VirtualHosted => "virtual-hosted",
        };
        let mut digest = Sha256::new();
        digest.update(b"cuenv-legacy-object-connection-v1\0");
        for field in [
            endpoint.scheme(),
            host,
            &port,
            self.client.bucket(),
            self.client.region(),
            addressing,
        ] {
            digest.update(field.len().to_string().as_bytes());
            digest.update(b":");
            digest.update(field.as_bytes());
        }
        Ok(BackendIdentity::from_digest(digest.finalize().into()))
    }

    /// Check a binding against this store's actual configured target.
    ///
    /// # Errors
    ///
    /// Returns an error if any identity or namespace does not match.
    pub fn validate_legacy_target_binding(
        &self,
        tenant: &TenantKey,
        binding: &LegacyMigrationBinding,
    ) -> Result<()> {
        binding.validate_for(tenant)?;
        if binding.target_root != self.root
            || binding.target_package != self.package
            || binding.target_connection != self.legacy_connection_identity()?
        {
            return Err(InfrastructureError::configuration(
                "legacy migration target binding does not match",
            ));
        }
        Ok(())
    }

    /// Verify the durable evidence of a completed legacy rollback.
    ///
    /// This checks the target only. The caller must separately verify that
    /// the source fence was released and keep the maintenance freeze active.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid request or an unreadable target.
    pub async fn legacy_rollback_completed(
        &self,
        request: &LegacyRollbackCompletionRequest<'_>,
    ) -> Result<bool> {
        if !valid_migration(request.migration) || !valid_evidence(request.commit) {
            return Err(InfrastructureError::configuration(
                "invalid legacy rollback completion request",
            ));
        }
        self.validate_legacy_target_binding(request.tenant, request.binding)?;
        self.ensure_not_public().await.map_err(private_failure)?;
        let path = self.path(request.tenant).map_err(private_failure)?;
        let Some((document, _)) = self.read(&path).await.map_err(private_failure)? else {
            return Ok(false);
        };
        proves_completion(&document, request)
    }
    /// Read a target's exact committed legacy migration evidence.
    ///
    /// # Errors
    ///
    /// Returns an error when the request or stored evidence is malformed or unreadable.
    pub async fn legacy_commit_evidence(
        &self,
        tenant: &TenantKey,
        migration: &str,
        binding: &LegacyMigrationBinding,
    ) -> Result<Option<LegacyCommitEvidence>> {
        if !valid_migration(migration) {
            return Err(InfrastructureError::configuration(
                "invalid legacy migration identifier",
            ));
        }
        self.validate_legacy_target_binding(tenant, binding)?;
        self.ensure_not_public().await.map_err(private_failure)?;
        let path = self.path(tenant).map_err(private_failure)?;
        let Some((document, _)) = self.read(&path).await.map_err(private_failure)? else {
            return Ok(None);
        };
        if !matching_origin(&document, migration) || document.format.write != 2 {
            return Ok(None);
        }
        let Some(receipt) = commit_receipt(&document)? else {
            return Ok(None);
        };
        if receipt.migration != migration || !valid_commit(&receipt, binding, document.serial) {
            return Err(InfrastructureError::state(
                "invalid legacy migration commit evidence",
            ));
        }
        Ok(Some(receipt.evidence))
    }

    /// Commit an existing imported target and release its exact lock in one CAS.
    ///
    /// This is an interim operation: callers must maintain the external freeze.
    /// A lost response is resolved only by the exact stored commit receipt.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, exhausted versions or definite I/O failure.
    pub async fn commit_legacy_migration(
        &self,
        request: &LegacyCommitRequest<'_>,
    ) -> Result<LegacyMutation<LegacyCommitEvidence>> {
        check_request(request.migration, request.lock, request.writer_nonce)?;
        self.validate_legacy_target_binding(request.tenant, request.binding)?;
        check_lock_binding(request.lock, request.binding)?;
        let path = self.path(request.tenant).map_err(private_failure)?;
        let key = path.state_key();
        let _gate = self.write_gate.lock().await;
        let Some((mut document, etag)) = self.read(&path).await.map_err(private_failure)? else {
            return Ok(LegacyMutation::Stale);
        };
        if !matching_origin(&document, request.migration) {
            return Ok(LegacyMutation::Stale);
        }
        if let Some(receipt) = commit_receipt(&document)? {
            return Ok(
                if document.format.write == 2
                    && receipt.migration == request.migration
                    && receipt.binding == *request.binding
                    && receipt.lock_identifier == request.lock.lock_identifier
                    && receipt.evidence.writer_nonce == request.writer_nonce
                    && valid_evidence(&receipt.evidence)
                    && document.serial >= receipt.evidence.serial
                {
                    LegacyMutation::Applied(receipt.evidence)
                } else {
                    LegacyMutation::Stale
                },
            );
        }
        let Some(held) = &document.lock else {
            return Ok(LegacyMutation::Stale);
        };
        if document.format.write != 1
            || !held.is_ordinary(document.format.write)
            || held.lock_identifier != request.lock.lock_identifier
            || Uuid::parse_str(&document.writer).ok() == Some(request.writer_nonce)
        {
            return Ok(LegacyMutation::Stale);
        }
        let serial = next_serial(&document)?;
        self.ensure_migration_capability(request.tenant)
            .await
            .map_err(private_failure)?;
        let evidence = LegacyCommitEvidence {
            writer_nonce: request.writer_nonce,
            serial,
        };
        let receipt = StoredCommit {
            migration: request.migration.to_string(),
            binding: request.binding.clone(),
            lock_identifier: request.lock.lock_identifier.clone(),
            evidence,
        };
        document.extra.insert(
            COMMIT_RECEIPT.into(),
            serde_json::to_value(&receipt)
                .map_err(|_| InfrastructureError::state("cannot encode legacy commit evidence"))?,
        );
        document.format.write = 2;
        document.lock = None;
        document.last_released = Some(request.lock.lock_identifier.clone());
        document.advance(&request.writer_nonce.to_string())?;
        let outcome = self
            .client
            .put(
                &key,
                object_state_store::encode_bounded(&document, &key).map_err(private_failure)?,
                &Condition::Matches(etag),
            )
            .await
            .map_err(|_| InfrastructureError::state("cannot commit legacy migration"))?;
        match outcome {
            Ok(_) => {
                if self.snapshot_and_prune(&path, &document).await.is_err() {
                    tracing::warn!("could not write or prune a legacy migration history snapshot");
                    cuenv_events::emit_stderr!(
                        "warning: could not write the legacy migration history snapshot"
                            .to_string()
                    );
                }
                self.delete_marker(&path, &request.lock.lock_identifier)
                    .await;
                Ok(LegacyMutation::Applied(evidence))
            }
            Err(S3Failure::PreconditionFailed | S3Failure::Missing) => {
                let found = self.read(&path).await.map_err(private_failure)?;
                Ok(match found {
                    Some((found, _))
                        if matching_origin(&found, request.migration)
                            && found.format.write == 2
                            && commit_receipt(&found)?.as_ref() == Some(&receipt) =>
                    {
                        LegacyMutation::Applied(evidence)
                    }
                    _ => LegacyMutation::Stale,
                })
            }
            Err(S3Failure::Ambiguous(_) | S3Failure::RateLimited(_)) => {
                Ok(match self.read(&path).await {
                    Ok(Some((found, _)))
                        if matching_origin(&found, request.migration)
                            && found.format.write == 2
                            && commit_receipt(&found).ok().flatten().as_ref() == Some(&receipt) =>
                    {
                        LegacyMutation::Applied(evidence)
                    }
                    _ => LegacyMutation::Indeterminate,
                })
            }
            Err(S3Failure::Permanent(_)) => {
                Err(InfrastructureError::state("cannot commit legacy migration"))
            }
        }
    }

    /// Lock an existing committed target and record its pre-lock serial in one CAS.
    ///
    /// The external maintenance freeze remains required. An ambiguous response
    /// is resolved only by the exact lock and its stored pre-lock evidence.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, exhausted versions or definite I/O failure.
    pub async fn lock_legacy_rollback(
        &self,
        request: &LegacyRollbackRequest<'_>,
    ) -> Result<LegacyMutation<LegacyRollbackEvidence>> {
        valid_rollback_request(request)?;
        self.validate_legacy_target_binding(request.tenant, request.binding)?;
        let path = self.path(request.tenant).map_err(private_failure)?;
        let key = path.state_key();
        let _gate = self.write_gate.lock().await;
        let Some((mut document, etag)) = self.read(&path).await.map_err(private_failure)? else {
            return Ok(LegacyMutation::Stale);
        };
        if !matching_origin(&document, request.migration) || document.format.write != 2 {
            return Ok(LegacyMutation::Stale);
        }
        let Some(commit) = commit_receipt(&document)? else {
            return Ok(LegacyMutation::Stale);
        };
        if commit.migration != request.migration
            || !valid_commit(&commit, request.binding, document.serial)
        {
            return Ok(LegacyMutation::Stale);
        }
        if commit.evidence != *request.commit {
            return Ok(LegacyMutation::Stale);
        }
        if let Some(held) = &document.lock {
            if held.lock_identifier == request.lock.lock_identifier
                && held.extra.get(LOCK_KIND) == Some(&serde_json::json!("migration"))
                && let Some(receipt) = rollback_receipt(held)?
                && receipt.migration == request.migration
                && receipt.writer_nonce == request.writer_nonce
                && valid_rollback_evidence(request, &receipt.evidence)
                && document.serial > receipt.evidence.pre_lock_serial
            {
                return Ok(LegacyMutation::Applied(receipt.evidence));
            }
            return Ok(LegacyMutation::Stale);
        }
        if Uuid::parse_str(&document.writer).ok() == Some(request.writer_nonce) {
            return Err(InfrastructureError::configuration(
                "rollback requires a fresh writer nonce",
            ));
        }
        for key in [TOMBSTONE_RECEIPT, RELEASE_RECEIPT] {
            if let Some(prior) = action_receipt(&document, key)? {
                let prior_nonces = [prior.writer_nonce, prior.rollback_nonce];
                if prior_nonces.contains(&request.writer_nonce)
                    || Uuid::parse_str(&request.lock.lock_identifier)
                        .is_ok_and(|identifier| prior_nonces.contains(&identifier))
                {
                    return Err(InfrastructureError::configuration(
                        "rollback requires a fresh writer nonce",
                    ));
                }
            }
        }
        if !rollback_has_serial_headroom(document.serial) {
            return Err(InfrastructureError::state(
                "the target state serial is exhausted",
            ));
        }
        self.ensure_migration_capability(request.tenant)
            .await
            .map_err(private_failure)?;
        let evidence = LegacyRollbackEvidence {
            pre_lock_serial: document.serial,
            commit: *request.commit,
        };
        let receipt = StoredRollbackLock {
            migration: request.migration.to_string(),
            writer_nonce: request.writer_nonce,
            evidence: evidence.clone(),
        };
        let mut extra = serde_json::Map::new();
        extra.insert(LOCK_KIND.into(), serde_json::json!("migration"));
        extra.insert(
            ROLLBACK_RECEIPT.into(),
            serde_json::to_value(&receipt).map_err(|_| {
                InfrastructureError::state("cannot encode legacy rollback evidence")
            })?,
        );
        document.lock = Some(DocumentLock {
            lock_identifier: request.lock.lock_identifier.clone(),
            holder: request.holder.to_string(),
            acquired_at: object_state_store::now(),
            cuenv_version: env!("CARGO_PKG_VERSION").to_string(),
            last_change: None,
            extra,
        });
        document.advance(&request.writer_nonce.to_string())?;
        let marker = StateLock {
            lock_identifier: request.writer_nonce.to_string(),
        };
        self.write_marker(&path, &marker, "infrastructure migration")
            .await
            .map_err(private_failure)?;
        let outcome = self
            .client
            .put(
                &key,
                object_state_store::encode_bounded(&document, &key).map_err(private_failure)?,
                &Condition::Matches(etag),
            )
            .await
            .map_err(|_| InfrastructureError::state("cannot lock legacy migration target"))?;
        let matches = |found: &StateDocument| -> bool {
            matching_origin(found, request.migration)
                && found.format.write == 2
                && commit_receipt(found)
                    .ok()
                    .flatten()
                    .is_some_and(|stored| stored == commit)
                && found.lock.as_ref().is_some_and(|held| {
                    held.lock_identifier == request.lock.lock_identifier
                        && held.extra.get(LOCK_KIND) == Some(&serde_json::json!("migration"))
                        && rollback_receipt(held).ok().flatten().as_ref() == Some(&receipt)
                })
        };
        match outcome {
            Ok(_) => Ok(LegacyMutation::Applied(evidence)),
            Err(S3Failure::PreconditionFailed | S3Failure::Missing) => {
                let found = self.read(&path).await.map_err(private_failure)?;
                Ok(match found {
                    Some((found, _)) if matches(&found) => LegacyMutation::Applied(evidence),
                    _ => LegacyMutation::Stale,
                })
            }
            Err(S3Failure::Ambiguous(_) | S3Failure::RateLimited(_)) => {
                Ok(match self.read(&path).await {
                    Ok(Some((found, _))) if matches(&found) => LegacyMutation::Applied(evidence),
                    _ => LegacyMutation::Indeterminate,
                })
            }
            Err(S3Failure::Permanent(_)) => Err(InfrastructureError::state(
                "cannot lock legacy migration target",
            )),
        }
    }

    /// Read the complete raw contents of a target held by this rollback.
    ///
    /// A caller must reject any fields its destination cannot represent before
    /// writing the source. The external maintenance freeze remains required.
    ///
    /// # Errors
    ///
    /// Returns an error when the request is invalid or the target cannot be read.
    pub async fn legacy_rollback_snapshot(
        &self,
        action: &LegacyRollbackAction<'_>,
    ) -> Result<LegacyMutation<LegacyRollbackSnapshot>> {
        let _ = action_value(action, None, 0)?;
        self.validate_legacy_target_binding(action.rollback.tenant, action.rollback.binding)?;
        self.ensure_not_public().await.map_err(private_failure)?;
        let path = self.path(action.rollback.tenant).map_err(private_failure)?;
        let Some((document, _)) = self.read(&path).await.map_err(private_failure)? else {
            return Ok(LegacyMutation::Stale);
        };
        if !held_rollback(&document, action.rollback, action.evidence) {
            return Ok(LegacyMutation::Stale);
        }
        let Some(held) = document.lock.as_ref() else {
            return Ok(LegacyMutation::Stale);
        };
        for key in [TOMBSTONE_RECEIPT, RELEASE_RECEIPT] {
            if let Some(prior) = action_receipt(&document, key)? {
                let prior_lock = StateLock {
                    lock_identifier: prior.lock_identifier.clone(),
                };
                if prior.migration != action.rollback.migration
                    || prior.binding != *action.rollback.binding
                    || prior.evidence.commit != *action.rollback.commit
                    || prior.writer_nonce.is_nil()
                    || prior.rollback_nonce.is_nil()
                    || prior.writer_nonce == prior.rollback_nonce
                    || check_request(&prior.migration, &prior_lock, prior.writer_nonce).is_err()
                    || Uuid::parse_str(&prior.lock_identifier).ok() == Some(prior.rollback_nonce)
                    || Uuid::parse_str(&prior.lock_identifier).ok()
                        == Some(prior.evidence.commit.writer_nonce)
                    || prior.serial <= prior.evidence.pre_lock_serial
                    || prior.serial == u64::MAX
                    || prior.serial > document.serial
                    || prior.moved_to.is_some() != (key == TOMBSTONE_RECEIPT)
                {
                    return Ok(LegacyMutation::Stale);
                }
            }
        }
        let mut document_extensions = document.extra.clone();
        for key in [COMMIT_RECEIPT, TOMBSTONE_RECEIPT, RELEASE_RECEIPT] {
            document_extensions.remove(key);
        }
        let mut lock_extensions = held.extra.clone();
        lock_extensions.remove(LOCK_KIND);
        lock_extensions.remove(ROLLBACK_RECEIPT);
        Ok(LegacyMutation::Applied(LegacyRollbackSnapshot {
            records: document.records.clone(),
            last_writer: document.last_writer.clone(),
            document_extensions,
            format_extensions: document.format.extra.clone(),
            tenant_extensions: document.tenant.extra.clone(),
            origin_extensions: document
                .origin
                .as_ref()
                .map_or_else(Map::new, |origin| origin.extra.clone()),
            lock_extensions,
            lock_change_extensions: held
                .last_change
                .as_ref()
                .map_or_else(Map::new, |change| change.extra.clone()),
            tombstone: document.tombstone.clone(),
            tombstone_extensions: document
                .tombstone
                .as_ref()
                .map_or_else(Map::new, |tombstone| tombstone.extra.clone()),
        }))
    }

    /// Mark the target moved while retaining its records and protected lock.
    ///
    /// The source fence must remain held until this operation is confirmed.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, an exhausted serial, or definite I/O failure.
    pub async fn tombstone_legacy_rollback(
        &self,
        request: &LegacyTombstoneRequest<'_>,
    ) -> Result<LegacyMutation<()>> {
        if request.moved_to.is_empty() {
            return Err(InfrastructureError::configuration(
                "the rollback destination is empty",
            ));
        }
        self.finish_legacy_rollback(&request.action, Some(request.moved_to))
            .await
    }

    /// Release an exact protected rollback lock, recording a durable receipt.
    ///
    /// For a tombstoned target the caller must first confirm source fence
    /// release. A failed or uncertain source release leaves this lock held.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, an exhausted serial, or definite I/O failure.
    pub async fn release_legacy_rollback(
        &self,
        action: &LegacyRollbackAction<'_>,
    ) -> Result<LegacyMutation<()>> {
        self.finish_legacy_rollback(action, None).await
    }

    async fn finish_legacy_rollback(
        &self,
        action: &LegacyRollbackAction<'_>,
        moved_to: Option<&str>,
    ) -> Result<LegacyMutation<()>> {
        let _ = action_value(action, moved_to, 0)?;
        self.validate_legacy_target_binding(action.rollback.tenant, action.rollback.binding)?;
        let path = self.path(action.rollback.tenant).map_err(private_failure)?;
        let key = path.state_key();
        let receipt_key = if moved_to.is_some() {
            TOMBSTONE_RECEIPT
        } else {
            RELEASE_RECEIPT
        };
        let _gate = self.write_gate.lock().await;
        let Some((mut document, etag)) = self.read(&path).await.map_err(private_failure)? else {
            return Ok(LegacyMutation::Stale);
        };
        if let Some(prior) = action_receipt(&document, receipt_key)? {
            let expected = action_value(action, moved_to, prior.serial)?;
            if proves_action(&document, receipt_key, &expected) {
                if moved_to.is_none() {
                    self.delete_marker(&path, &action.rollback.writer_nonce.to_string())
                        .await;
                }
                return Ok(LegacyMutation::Applied(()));
            }
        }
        for key in [TOMBSTONE_RECEIPT, RELEASE_RECEIPT] {
            if let Some(prior) = action_receipt(&document, key)?
                && [prior.writer_nonce, prior.rollback_nonce].contains(&action.writer_nonce)
            {
                return Err(InfrastructureError::configuration(
                    "rollback requires a fresh action nonce",
                ));
            }
        }
        if !held_rollback(&document, action.rollback, action.evidence) {
            return Ok(LegacyMutation::Stale);
        }
        if moved_to.is_some() && document.tombstone.is_some() {
            return Ok(LegacyMutation::Stale);
        }
        if moved_to.is_none()
            && let Some(tombstone) = &document.tombstone
        {
            let Some(prior) = action_receipt(&document, TOMBSTONE_RECEIPT)? else {
                return Ok(LegacyMutation::Stale);
            };
            let tombstone_action = LegacyRollbackAction {
                writer_nonce: prior.writer_nonce,
                ..*action
            };
            let expected =
                action_value(&tombstone_action, Some(&tombstone.moved_to), prior.serial)?;
            if prior.writer_nonce == action.writer_nonce
                || !proves_action(&document, TOMBSTONE_RECEIPT, &expected)
            {
                return Ok(LegacyMutation::Stale);
            }
        }
        if Uuid::parse_str(&document.writer).ok() == Some(action.writer_nonce) {
            return Err(InfrastructureError::configuration(
                "rollback requires a fresh action nonce",
            ));
        }
        self.ensure_migration_capability(action.rollback.tenant)
            .await
            .map_err(private_failure)?;
        let serial = next_serial(&document)?;
        let expected = action_value(action, moved_to, serial)?;
        document.extra.insert(
            receipt_key.into(),
            serde_json::to_value(&expected).map_err(|_| {
                InfrastructureError::state("cannot encode legacy rollback action evidence")
            })?,
        );
        if let Some(destination) = moved_to {
            document.tombstone = Some(Tombstone {
                moved_to: destination.to_string(),
                extra: Map::new(),
            });
        } else {
            document.lock = None;
            document.last_released = None;
        }
        document.advance(&action.writer_nonce.to_string())?;
        let outcome = self
            .client
            .put(
                &key,
                object_state_store::encode_bounded(&document, &key).map_err(private_failure)?,
                &Condition::Matches(etag),
            )
            .await
            .map_err(|_| InfrastructureError::state("cannot complete legacy rollback action"))?;
        let confirmed = match outcome {
            Ok(_) => LegacyMutation::Applied(()),
            Err(S3Failure::PreconditionFailed | S3Failure::Missing) => {
                match self.read(&path).await.map_err(private_failure)? {
                    Some((found, _)) if proves_action(&found, receipt_key, &expected) => {
                        LegacyMutation::Applied(())
                    }
                    _ => LegacyMutation::Stale,
                }
            }
            Err(S3Failure::Ambiguous(_) | S3Failure::RateLimited(_)) => {
                match self.read(&path).await {
                    Ok(Some((found, _))) if proves_action(&found, receipt_key, &expected) => {
                        LegacyMutation::Applied(())
                    }
                    _ => LegacyMutation::Indeterminate,
                }
            }
            Err(S3Failure::Permanent(_)) => {
                return Err(InfrastructureError::state(
                    "cannot complete legacy rollback action",
                ));
            }
        };
        if moved_to.is_none() && matches!(confirmed, LegacyMutation::Applied(())) {
            self.delete_marker(&path, &action.rollback.writer_nonce.to_string())
                .await;
        }
        Ok(confirmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::object::Credentials;
    use crate::state::object::fake::{BarrierPosition, FakeS3, FaultAction};
    use crate::state::object::store::{AddressingChoice, ObjectStoreConfiguration, TenantImport};
    use crate::state::{ManagedResource, ResourceAddress, StateStore};
    use serde_json::{Value, json};
    use std::time::Duration;

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
    const MIGRATION: &str = "123456789abcdef0";

    fn store(server: &FakeS3) -> TestResult<ObjectStateStore> {
        Ok(ObjectStateStore::try_new(ObjectStoreConfiguration {
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
        })?)
    }

    struct Fixture {
        server: FakeS3,
        store: ObjectStateStore,
        tenant: TenantKey,
        binding: LegacyMigrationBinding,
        lock: StateLock,
        nonce: Uuid,
    }

    impl Fixture {
        async fn new() -> TestResult<Self> {
            let server = FakeS3::start().await?;
            let store = store(&server)?;
            let tenant = TenantKey::try_new("example.com/project", "app")?;
            let binding = LegacyMigrationBinding {
                source_physical_id: Uuid::new_v4(),
                target_physical_id: Uuid::new_v4(),
                source: LegacyMigrationSource::Turso {
                    connection: BackendIdentity::from_digest([1; 32]),
                },
                target_connection: store.legacy_connection_identity()?,
                target_root: None,
                target_package: "cuenv".into(),
                module_path: tenant.module_path().into(),
                project: tenant.project().into(),
                selection: LegacyMigrationSelection::Selected,
                environments: vec![None],
            };
            let lock = StateLock::generate();
            store.ensure_migration_capability(&tenant).await?;
            assert!(
                store
                    .import(
                        &tenant,
                        &TenantImport {
                            lock: &lock,
                            holder: "legacy migration",
                            resources: &[],
                            last_writer: None,
                            migration: MIGRATION,
                            source: "source",
                        }
                    )
                    .await?
            );
            Ok(Self {
                server,
                store,
                tenant,
                binding,
                lock,
                nonce: Uuid::new_v4(),
            })
        }

        fn commit(&self) -> LegacyCommitRequest<'_> {
            LegacyCommitRequest {
                tenant: &self.tenant,
                binding: &self.binding,
                migration: MIGRATION,
                lock: &self.lock,
                writer_nonce: self.nonce,
            }
        }

        async fn committed(&self) -> TestResult<LegacyCommitEvidence> {
            match self.store.commit_legacy_migration(&self.commit()).await? {
                LegacyMutation::Applied(evidence) => Ok(evidence),
                other => Err(format!("commit failed: {other:?}").into()),
            }
        }

        fn state_key(&self) -> TestResult<String> {
            Ok(self.store.path(&self.tenant)?.state_key())
        }

        fn state(&self) -> TestResult<Value> {
            let bytes = self
                .server
                .object("state", &self.state_key()?)
                .ok_or("missing state")?;
            Ok(serde_json::from_slice(&bytes)?)
        }

        fn set_state(&self, value: &Value) -> TestResult {
            self.server
                .put_object("state", &self.state_key()?, serde_json::to_vec(value)?);
            Ok(())
        }
    }

    fn record() -> ManagedResource {
        ManagedResource {
            address: ResourceAddress::new("random_pet", "concurrent"),
            provider: "random".into(),
            provider_source: "hashicorp/random".into(),
            schema_version: 0,
            state: json!({"id": "changed"}),
            private: Vec::new(),
            dependencies: Vec::new(),
            sensitive_attributes: Vec::new(),
            tainted: false,
            identity: None,
            serial: 0,
            generation: Uuid::nil(),
        }
    }

    async fn write_target(store: &ObjectStateStore, tenant: &TenantKey) -> TestResult {
        let lock = store.lock(tenant, "ordinary writer").await?;
        store.put(tenant, &lock, &record()).await?;
        assert!(store.unlock(tenant, &lock).await?);
        Ok(())
    }

    #[tokio::test]
    async fn commit_receipt_keeps_the_cas_serial_when_a_later_writer_wins() -> TestResult {
        let fixture = Fixture::new().await?;
        let other = store(&fixture.server)?;
        let mut barrier =
            fixture
                .server
                .pause_next("PUT", "state.json", BarrierPosition::AfterApply);
        let racing = async {
            barrier.reached(Duration::from_secs(10)).await?;
            write_target(&other, &fixture.tenant).await?;
            barrier.release(Duration::from_secs(10)).await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        };
        let request = fixture.commit();
        let (result, racing) =
            tokio::join!(fixture.store.commit_legacy_migration(&request), racing);
        racing?;
        let LegacyMutation::Applied(evidence) = result? else {
            return Err("commit not confirmed".into());
        };
        assert_eq!(evidence.serial, 2);
        assert_eq!(
            fixture
                .store
                .legacy_commit_evidence(&fixture.tenant, MIGRATION, &fixture.binding)
                .await?,
            Some(evidence)
        );
        let current = fixture
            .store
            .origin(&fixture.tenant)
            .await?
            .ok_or("origin missing")?
            .2;
        assert!(current > evidence.serial);
        Ok(())
    }

    #[tokio::test]
    async fn rollback_lock_rechecks_the_target_after_a_competing_write() -> TestResult {
        let fixture = Fixture::new().await?;
        let commit = fixture.committed().await?;
        let other = store(&fixture.server)?;
        let lock = StateLock::generate();
        let request = LegacyRollbackRequest {
            tenant: &fixture.tenant,
            binding: &fixture.binding,
            migration: MIGRATION,
            commit: &commit,
            lock: &lock,
            writer_nonce: Uuid::new_v4(),
            holder: "rollback",
        };
        let mut barrier =
            fixture
                .server
                .pause_next("PUT", "state.json", BarrierPosition::BeforeApply);
        let racing = async {
            barrier.reached(Duration::from_secs(10)).await?;
            write_target(&other, &fixture.tenant).await?;
            barrier.release(Duration::from_secs(10)).await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        };
        let (result, racing) = tokio::join!(fixture.store.lock_legacy_rollback(&request), racing);
        racing?;
        assert!(matches!(result?, LegacyMutation::Stale));
        let LegacyMutation::Applied(evidence) =
            fixture.store.lock_legacy_rollback(&request).await?
        else {
            return Err("rollback lock not confirmed".into());
        };
        assert!(evidence.pre_lock_serial > commit.serial);
        let info = fixture
            .store
            .current_lock(&fixture.tenant)
            .await?
            .ok_or("lock missing")?;
        assert!(info.identifier().is_none());
        assert!(fixture.store.unlock(&fixture.tenant, &lock).await.is_err());
        assert!(
            fixture
                .store
                .force_unlock(&fixture.tenant, &lock.lock_identifier)
                .await
                .is_err()
        );
        assert!(
            fixture
                .store
                .put(&fixture.tenant, &lock, &record())
                .await
                .is_err()
        );
        let action = LegacyRollbackAction {
            rollback: &request,
            evidence: &evidence,
            writer_nonce: Uuid::new_v4(),
        };
        assert!(matches!(
            fixture.store.release_legacy_rollback(&action).await?,
            LegacyMutation::Applied(())
        ));
        Ok(())
    }

    #[tokio::test]
    async fn lost_and_unusable_etag_responses_require_exact_readback() -> TestResult {
        for fault in [
            FaultAction::DropAfterApply,
            FaultAction::MissingEtagAfterApply,
            FaultAction::WeakEtagAfterApply,
        ] {
            let fixture = Fixture::new().await?;
            fixture.server.fault("PUT", "state.json", fault.clone());
            let evidence = fixture.committed().await?;
            assert!(
                matches!(fixture.store.commit_legacy_migration(&fixture.commit()).await?, LegacyMutation::Applied(found) if found == evidence)
            );
            let wrong_lock = StateLock::generate();
            let wrong = LegacyCommitRequest {
                lock: &wrong_lock,
                ..fixture.commit()
            };
            assert!(matches!(
                fixture.store.commit_legacy_migration(&wrong).await?,
                LegacyMutation::Stale
            ));

            let lock = StateLock::generate();
            let request = LegacyRollbackRequest {
                tenant: &fixture.tenant,
                binding: &fixture.binding,
                migration: MIGRATION,
                commit: &evidence,
                lock: &lock,
                writer_nonce: Uuid::new_v4(),
                holder: "rollback",
            };
            fixture.server.fault("PUT", "state.json", fault);
            let LegacyMutation::Applied(locked) =
                fixture.store.lock_legacy_rollback(&request).await?
            else {
                return Err("rollback lock not recovered".into());
            };
            assert_eq!(locked.pre_lock_serial, evidence.serial);
            assert!(
                matches!(fixture.store.lock_legacy_rollback(&request).await?, LegacyMutation::Applied(found) if found == locked)
            );
        }
        let fixture = Fixture::new().await?;
        fixture
            .server
            .fault("PUT", "state.json", FaultAction::DropBeforeApply);
        assert!(matches!(
            fixture
                .store
                .commit_legacy_migration(&fixture.commit())
                .await?,
            LegacyMutation::Indeterminate
        ));
        Ok(())
    }

    #[tokio::test]
    async fn rollback_never_creates_or_accepts_missing_foreign_or_legacy_targets() -> TestResult {
        for case in ["missing", "foreign", "tombstone", "legacy"] {
            let fixture = Fixture::new().await?;
            let commit = if case == "legacy" {
                LegacyCommitEvidence {
                    writer_nonce: Uuid::new_v4(),
                    serial: 2,
                }
            } else {
                fixture.committed().await?
            };
            match case {
                "missing" => fixture.server.remove_object("state", &fixture.state_key()?),
                "foreign" => {
                    let mut state = fixture.state()?;
                    state["origin"]["id"] = json!("ffffffffffffffff");
                    fixture.set_state(&state)?;
                }
                "tombstone" => {
                    let mut state = fixture.state()?;
                    state["tombstone"] = json!({"movedTo": "elsewhere"});
                    fixture.set_state(&state)?;
                }
                _ => {}
            }
            let before = fixture.server.object("state", &fixture.state_key()?);
            let lock = StateLock::generate();
            let request = LegacyRollbackRequest {
                tenant: &fixture.tenant,
                binding: &fixture.binding,
                migration: MIGRATION,
                commit: &commit,
                lock: &lock,
                writer_nonce: Uuid::new_v4(),
                holder: "rollback",
            };
            assert!(
                matches!(
                    fixture.store.lock_legacy_rollback(&request).await?,
                    LegacyMutation::Stale
                ),
                "{case}"
            );
            assert_eq!(
                fixture.server.object("state", &fixture.state_key()?),
                before,
                "{case}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn exhausted_serials_fail_before_commit_or_rollback_lock() -> TestResult {
        let fixture = Fixture::new().await?;
        let mut state = fixture.state()?;
        state["serial"] = json!(u64::MAX - 1);
        fixture.set_state(&state)?;
        assert!(
            fixture
                .store
                .commit_legacy_migration(&fixture.commit())
                .await
                .is_err()
        );
        assert_eq!(fixture.state()?, state);

        for remaining in 1..=3 {
            let fixture = Fixture::new().await?;
            let commit = fixture.committed().await?;
            let mut state = fixture.state()?;
            state["serial"] = json!(u64::MAX - remaining);
            fixture.set_state(&state)?;
            let keys_before = fixture.server.keys("state");
            let requests_before = fixture.server.requests().len();
            let lock = StateLock::generate();
            let request = LegacyRollbackRequest {
                tenant: &fixture.tenant,
                binding: &fixture.binding,
                migration: MIGRATION,
                commit: &commit,
                lock: &lock,
                writer_nonce: Uuid::new_v4(),
                holder: "rollback",
            };
            assert!(fixture.store.lock_legacy_rollback(&request).await.is_err());
            assert_eq!(fixture.state()?, state);
            assert_eq!(fixture.server.keys("state"), keys_before);
            assert!(
                fixture.server.requests()[requests_before..]
                    .iter()
                    .all(|request| request.method != "PUT")
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn rollback_completes_at_the_last_safe_serial() -> TestResult {
        let fixture = Fixture::new().await?;
        let commit = fixture.committed().await?;
        let mut state = fixture.state()?;
        state["serial"] = json!(u64::MAX - 4);
        fixture.set_state(&state)?;
        let lock = StateLock::generate();
        let request = LegacyRollbackRequest {
            tenant: &fixture.tenant,
            binding: &fixture.binding,
            migration: MIGRATION,
            commit: &commit,
            lock: &lock,
            writer_nonce: Uuid::new_v4(),
            holder: "rollback",
        };
        let LegacyMutation::Applied(evidence) =
            fixture.store.lock_legacy_rollback(&request).await?
        else {
            return Err("rollback lock not confirmed".into());
        };
        let action = LegacyRollbackAction {
            rollback: &request,
            evidence: &evidence,
            writer_nonce: Uuid::new_v4(),
        };
        assert!(matches!(
            fixture
                .store
                .tombstone_legacy_rollback(&LegacyTombstoneRequest {
                    action,
                    moved_to: "source",
                })
                .await?,
            LegacyMutation::Applied(())
        ));
        let release = LegacyRollbackAction {
            writer_nonce: Uuid::new_v4(),
            ..action
        };
        assert!(matches!(
            fixture.store.release_legacy_rollback(&release).await?,
            LegacyMutation::Applied(())
        ));
        assert_eq!(fixture.state()?["serial"], json!(u64::MAX - 1));
        assert!(
            fixture
                .store
                .legacy_rollback_completed(&LegacyRollbackCompletionRequest {
                    tenant: &fixture.tenant,
                    migration: MIGRATION,
                    binding: &fixture.binding,
                    commit: &commit,
                })
                .await?
        );
        Ok(())
    }

    #[tokio::test]
    async fn public_evidence_never_reuses_a_lock_token() -> TestResult {
        let fixture = Fixture::new().await?;
        let reused_nonce = StateLock {
            lock_identifier: fixture.nonce.to_string(),
        };
        let request = LegacyCommitRequest {
            lock: &reused_nonce,
            ..fixture.commit()
        };
        assert!(
            fixture
                .store
                .commit_legacy_migration(&request)
                .await
                .is_err()
        );
        let reused_migration = StateLock {
            lock_identifier: MIGRATION.to_string(),
        };
        let request = LegacyCommitRequest {
            lock: &reused_migration,
            ..fixture.commit()
        };
        assert!(
            fixture
                .store
                .commit_legacy_migration(&request)
                .await
                .is_err()
        );

        let commit = fixture.committed().await?;
        let lock = StateLock {
            lock_identifier: commit.writer_nonce.to_string(),
        };
        let request = LegacyRollbackRequest {
            tenant: &fixture.tenant,
            binding: &fixture.binding,
            migration: MIGRATION,
            commit: &commit,
            lock: &lock,
            writer_nonce: Uuid::new_v4(),
            holder: "rollback",
        };
        assert!(fixture.store.lock_legacy_rollback(&request).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn protected_snapshot_contains_raw_fields_only_for_the_exact_owner() -> TestResult {
        let fixture = Fixture::new().await?;
        let commit = fixture.committed().await?;
        write_target(&fixture.store, &fixture.tenant).await?;
        let mut state = fixture.state()?;
        state["futureDocument"] = json!({"secret": "document-private"});
        state["format"]["futureFormat"] = json!(1);
        state["tenant"]["futureTenant"] = json!(2);
        state["origin"]["futureOrigin"] = json!(3);
        state["lastWriter"] =
            json!({"instance": "project:package", "at": "2026-01-01T00:00:00Z", "futureWriter": 4});
        state["records"]["random_pet.concurrent"]["futureRecord"] = json!("record-private");
        fixture.set_state(&state)?;

        let lock = StateLock::generate();
        let request = LegacyRollbackRequest {
            tenant: &fixture.tenant,
            binding: &fixture.binding,
            migration: MIGRATION,
            commit: &commit,
            lock: &lock,
            writer_nonce: Uuid::new_v4(),
            holder: "rollback-private",
        };
        let LegacyMutation::Applied(evidence) =
            fixture.store.lock_legacy_rollback(&request).await?
        else {
            return Err("rollback lock not confirmed".into());
        };
        let mut state = fixture.state()?;
        state["lock"]["futureLock"] = json!(5);
        state["lock"]["lastChange"] = json!({"address": "random_pet.concurrent", "action": "put", "at": "2026-01-01T00:00:00Z", "futureChange": 6});
        fixture.set_state(&state)?;
        let action = LegacyRollbackAction {
            rollback: &request,
            evidence: &evidence,
            writer_nonce: Uuid::new_v4(),
        };
        let LegacyMutation::Applied(snapshot) =
            fixture.store.legacy_rollback_snapshot(&action).await?
        else {
            return Err("snapshot not confirmed".into());
        };
        assert_eq!(
            snapshot.records["random_pet.concurrent"]["futureRecord"],
            json!("record-private")
        );
        assert_eq!(
            snapshot.last_writer.as_ref().ok_or("writer missing")?.extra["futureWriter"],
            json!(4)
        );
        assert_eq!(
            snapshot.document_extensions["futureDocument"]["secret"],
            json!("document-private")
        );
        assert!(!snapshot.document_extensions.contains_key(COMMIT_RECEIPT));
        assert_eq!(snapshot.format_extensions["futureFormat"], json!(1));
        assert_eq!(snapshot.tenant_extensions["futureTenant"], json!(2));
        assert_eq!(snapshot.origin_extensions["futureOrigin"], json!(3));
        assert_eq!(snapshot.lock_extensions["futureLock"], json!(5));
        assert_eq!(snapshot.lock_change_extensions["futureChange"], json!(6));
        assert_eq!(
            format!("{snapshot:?}"),
            "LegacyRollbackSnapshot(<redacted>)"
        );

        let wrong_lock = StateLock::generate();
        let wrong = LegacyRollbackRequest {
            lock: &wrong_lock,
            ..request
        };
        let foreign_action = LegacyRollbackAction {
            rollback: &wrong,
            ..action
        };
        assert!(matches!(
            fixture
                .store
                .legacy_rollback_snapshot(&foreign_action)
                .await?,
            LegacyMutation::Stale
        ));
        assert!(matches!(
            fixture
                .store
                .release_legacy_rollback(&foreign_action)
                .await?,
            LegacyMutation::Stale
        ));
        let tombstone = LegacyTombstoneRequest {
            action: foreign_action,
            moved_to: "source",
        };
        assert!(matches!(
            fixture.store.tombstone_legacy_rollback(&tombstone).await?,
            LegacyMutation::Stale
        ));
        assert_eq!(fixture.state()?, state);

        let requests = fixture.server.requests();
        assert!(
            requests
                .iter()
                .all(|request| !request.key.contains(&lock.lock_identifier))
        );
        assert!(requests.iter().any(
            |record| record.key.contains(&request.writer_nonce.to_string())
                && record.key.contains("_locks")
        ));
        let locks = fixture.store.locks().await?;
        assert_eq!(locks.len(), 1);
        assert!(locks[0].lock.identifier().is_none());
        Ok(())
    }

    #[tokio::test]
    async fn refusal_release_readback_survives_a_later_ordinary_writer() -> TestResult {
        let fixture = Fixture::new().await?;
        let commit = fixture.committed().await?;
        let other = store(&fixture.server)?;
        let lock = StateLock::generate();
        let request = LegacyRollbackRequest {
            tenant: &fixture.tenant,
            binding: &fixture.binding,
            migration: MIGRATION,
            commit: &commit,
            lock: &lock,
            writer_nonce: Uuid::new_v4(),
            holder: "rollback",
        };
        let LegacyMutation::Applied(evidence) =
            fixture.store.lock_legacy_rollback(&request).await?
        else {
            return Err("rollback lock not confirmed".into());
        };
        let action = LegacyRollbackAction {
            rollback: &request,
            evidence: &evidence,
            writer_nonce: Uuid::new_v4(),
        };
        fixture
            .server
            .fault("PUT", "state.json", FaultAction::DropAfterApply);
        let mut barrier =
            fixture
                .server
                .pause_next("PUT", "state.json", BarrierPosition::AfterApply);
        let racing = async {
            barrier.reached(Duration::from_secs(10)).await?;
            write_target(&other, &fixture.tenant).await?;
            barrier.release(Duration::from_secs(10)).await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        };
        let (result, racing) = tokio::join!(fixture.store.release_legacy_rollback(&action), racing);
        racing?;
        assert!(matches!(result?, LegacyMutation::Applied(())));
        assert!(matches!(
            fixture.store.release_legacy_rollback(&action).await?,
            LegacyMutation::Applied(())
        ));
        assert_eq!(fixture.store.list(&fixture.tenant).await?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn tombstone_and_final_release_preserve_records_and_exact_receipts() -> TestResult {
        for fault in [
            FaultAction::DropAfterApply,
            FaultAction::MissingEtagAfterApply,
            FaultAction::WeakEtagAfterApply,
        ] {
            let fixture = Fixture::new().await?;
            let commit = fixture.committed().await?;
            write_target(&fixture.store, &fixture.tenant).await?;
            let lock = StateLock::generate();
            let request = LegacyRollbackRequest {
                tenant: &fixture.tenant,
                binding: &fixture.binding,
                migration: MIGRATION,
                commit: &commit,
                lock: &lock,
                writer_nonce: Uuid::new_v4(),
                holder: "rollback",
            };
            let LegacyMutation::Applied(evidence) =
                fixture.store.lock_legacy_rollback(&request).await?
            else {
                return Err("rollback lock not confirmed".into());
            };
            let before = fixture.state()?["records"].clone();
            let action = LegacyRollbackAction {
                rollback: &request,
                evidence: &evidence,
                writer_nonce: Uuid::new_v4(),
            };
            let tombstone = LegacyTombstoneRequest {
                action,
                moved_to: "source",
            };
            fixture.server.fault("PUT", "state.json", fault.clone());
            assert!(matches!(
                fixture.store.tombstone_legacy_rollback(&tombstone).await?,
                LegacyMutation::Applied(())
            ));
            assert!(matches!(
                fixture.store.tombstone_legacy_rollback(&tombstone).await?,
                LegacyMutation::Applied(())
            ));
            let state = fixture.state()?;
            assert_eq!(state["records"], before);
            assert_eq!(state["tombstone"]["movedTo"], json!("source"));
            assert_eq!(state["lock"][LOCK_KIND], json!("migration"));
            assert!(fixture.store.unlock(&fixture.tenant, &lock).await.is_err());

            let release = LegacyRollbackAction {
                writer_nonce: Uuid::new_v4(),
                ..action
            };
            fixture.server.fault("PUT", "state.json", fault);
            assert!(matches!(
                fixture.store.release_legacy_rollback(&release).await?,
                LegacyMutation::Applied(())
            ));
            let mut later = fixture.state()?;
            assert_eq!(later["records"], before);
            assert!(later["lock"].is_null());
            later["serial"] = json!(later["serial"].as_u64().ok_or("serial missing")? + 1);
            later["writer"] = json!(Uuid::new_v4().to_string());
            fixture.set_state(&later)?;
            assert!(matches!(
                fixture.store.tombstone_legacy_rollback(&tombstone).await?,
                LegacyMutation::Applied(())
            ));
            assert!(matches!(
                fixture.store.release_legacy_rollback(&release).await?,
                LegacyMutation::Applied(())
            ));
            assert_eq!(fixture.state()?["records"], before);
        }
        Ok(())
    }

    #[tokio::test]
    async fn unobserved_actions_leave_the_protected_lock_and_records_intact() -> TestResult {
        let fixture = Fixture::new().await?;
        let commit = fixture.committed().await?;
        write_target(&fixture.store, &fixture.tenant).await?;
        let lock = StateLock::generate();
        let request = LegacyRollbackRequest {
            tenant: &fixture.tenant,
            binding: &fixture.binding,
            migration: MIGRATION,
            commit: &commit,
            lock: &lock,
            writer_nonce: Uuid::new_v4(),
            holder: "rollback",
        };
        let LegacyMutation::Applied(evidence) =
            fixture.store.lock_legacy_rollback(&request).await?
        else {
            return Err("rollback lock not confirmed".into());
        };
        let action = LegacyRollbackAction {
            rollback: &request,
            evidence: &evidence,
            writer_nonce: Uuid::new_v4(),
        };
        let before = fixture.state()?;
        fixture
            .server
            .fault("PUT", "state.json", FaultAction::DropBeforeApply);
        let tombstone = LegacyTombstoneRequest {
            action,
            moved_to: "source",
        };
        assert!(matches!(
            fixture.store.tombstone_legacy_rollback(&tombstone).await?,
            LegacyMutation::Indeterminate
        ));
        assert_eq!(fixture.state()?, before);
        fixture
            .server
            .fault("PUT", "state.json", FaultAction::DropBeforeApply);
        assert!(matches!(
            fixture.store.release_legacy_rollback(&action).await?,
            LegacyMutation::Indeterminate
        ));
        assert_eq!(fixture.state()?, before);
        Ok(())
    }

    #[tokio::test]
    async fn completed_rollback_requires_exact_ordered_receipts_and_no_lock() -> TestResult {
        let fixture = Fixture::new().await?;
        let commit = fixture.committed().await?;
        let lock = StateLock::generate();
        let request = LegacyRollbackRequest {
            tenant: &fixture.tenant,
            binding: &fixture.binding,
            migration: MIGRATION,
            commit: &commit,
            lock: &lock,
            writer_nonce: Uuid::new_v4(),
            holder: "rollback",
        };
        let LegacyMutation::Applied(evidence) =
            fixture.store.lock_legacy_rollback(&request).await?
        else {
            return Err("rollback lock not confirmed".into());
        };
        let action = LegacyRollbackAction {
            rollback: &request,
            evidence: &evidence,
            writer_nonce: Uuid::new_v4(),
        };
        let tombstone = LegacyTombstoneRequest {
            action,
            moved_to: "source",
        };
        let completion_request = LegacyRollbackCompletionRequest {
            tenant: &fixture.tenant,
            migration: MIGRATION,
            binding: &fixture.binding,
            commit: &commit,
        };
        assert!(
            !fixture
                .store
                .legacy_rollback_completed(&completion_request)
                .await?
        );
        assert!(matches!(
            fixture.store.tombstone_legacy_rollback(&tombstone).await?,
            LegacyMutation::Applied(())
        ));
        let protected = fixture.state()?;
        assert!(
            !fixture
                .store
                .legacy_rollback_completed(&completion_request)
                .await?
        );
        let release = LegacyRollbackAction {
            writer_nonce: Uuid::new_v4(),
            ..action
        };
        assert!(matches!(
            fixture.store.release_legacy_rollback(&release).await?,
            LegacyMutation::Applied(())
        ));
        assert!(
            fixture
                .store
                .legacy_rollback_completed(&completion_request)
                .await?
        );

        let final_document = fixture.state()?;
        let mut changed = final_document.clone();
        changed["lock"] = protected["lock"].clone();
        fixture.set_state(&changed)?;
        assert!(
            !fixture
                .store
                .legacy_rollback_completed(&completion_request)
                .await?
        );
        changed = final_document.clone();
        changed[RELEASE_RECEIPT]["rollbackNonce"] = json!(Uuid::new_v4());
        fixture.set_state(&changed)?;
        assert!(
            !fixture
                .store
                .legacy_rollback_completed(&completion_request)
                .await?
        );
        changed = final_document.clone();
        changed[RELEASE_RECEIPT]["serial"] = changed[TOMBSTONE_RECEIPT]["serial"].clone();
        fixture.set_state(&changed)?;
        assert!(
            !fixture
                .store
                .legacy_rollback_completed(&completion_request)
                .await?
        );
        changed = final_document.clone();
        changed
            .as_object_mut()
            .ok_or("state not an object")?
            .remove(TOMBSTONE_RECEIPT);
        fixture.set_state(&changed)?;
        assert!(
            !fixture
                .store
                .legacy_rollback_completed(&completion_request)
                .await?
        );
        let mut foreign = fixture.binding.clone();
        foreign.source_physical_id = Uuid::new_v4();
        let foreign_completion = LegacyRollbackCompletionRequest {
            binding: &foreign,
            ..completion_request
        };
        fixture.set_state(&final_document)?;
        assert!(
            !fixture
                .store
                .legacy_rollback_completed(&foreign_completion)
                .await?
        );
        Ok(())
    }

    #[tokio::test]
    async fn malformed_documents_and_provider_errors_do_not_disclose_tokens() -> TestResult {
        let fixture = Fixture::new().await?;
        let commit = fixture.committed().await?;
        let lock = StateLock::generate();
        let request = LegacyRollbackRequest {
            tenant: &fixture.tenant,
            binding: &fixture.binding,
            migration: MIGRATION,
            commit: &commit,
            lock: &lock,
            writer_nonce: Uuid::new_v4(),
            holder: "rollback",
        };
        let secret = lock.lock_identifier.as_str();
        fixture.server.fault(
            "GET",
            "state.json",
            FaultAction::Status(
                403,
                format!("<Error><Code>Denied</Code><Message>{secret}</Message></Error>"),
            ),
        );
        let error = fixture
            .store
            .lock_legacy_rollback(&request)
            .await
            .unwrap_err();
        assert!(!error.to_string().contains(secret));
        assert!(!format!("{error:?}").contains(secret));

        let mut malformed = fixture.state()?;
        malformed["serial"] = json!(secret);
        fixture.set_state(&malformed)?;
        let error = fixture
            .store
            .lock_legacy_rollback(&request)
            .await
            .unwrap_err();
        assert!(!error.to_string().contains(secret));
        assert!(!format!("{error:?}").contains(secret));
        Ok(())
    }

    #[tokio::test]
    async fn physical_and_namespace_mismatches_fail_before_target_writes() -> TestResult {
        let fixture = Fixture::new().await?;
        let before = fixture.state()?;
        let mut bindings = Vec::new();
        let mut same_store = fixture.binding.clone();
        same_store.target_physical_id = same_store.source_physical_id;
        bindings.push(same_store);
        let mut target_connection = fixture.binding.clone();
        target_connection.target_connection = BackendIdentity::from_digest([2; 32]);
        bindings.push(target_connection);
        let mut target_root = fixture.binding.clone();
        target_root.target_root = Some("other".into());
        bindings.push(target_root);
        let mut target_package = fixture.binding.clone();
        target_package.target_package = "other".into();
        bindings.push(target_package);
        let mut same_object_connection = fixture.binding.clone();
        same_object_connection.source = LegacyMigrationSource::Object {
            connection: fixture.binding.target_connection.clone(),
            root: Some("separate-prefix".into()),
            package: "cuenv".into(),
        };
        bindings.push(same_object_connection);
        let mut duplicate_tenant = fixture.binding.clone();
        duplicate_tenant.selection = LegacyMigrationSelection::All;
        duplicate_tenant.environments.push(None);
        bindings.push(duplicate_tenant);
        let mut case_collision = fixture.binding.clone();
        case_collision.selection = LegacyMigrationSelection::All;
        case_collision
            .environments
            .extend([Some("Prod".into()), Some("prod".into())]);
        bindings.push(case_collision);
        let mut invalid_selection = fixture.binding.clone();
        invalid_selection.environments.push(Some("staging".into()));
        bindings.push(invalid_selection);
        for binding in &bindings {
            let request = LegacyCommitRequest {
                binding,
                ..fixture.commit()
            };
            let requests = fixture.server.requests().len();
            assert!(
                fixture
                    .store
                    .commit_legacy_migration(&request)
                    .await
                    .is_err()
            );
            assert_eq!(fixture.server.requests().len(), requests);
            assert_eq!(fixture.state()?, before);
        }
        for identifier in [
            fixture.binding.source_physical_id,
            fixture.binding.target_physical_id,
        ] {
            let lock = StateLock {
                lock_identifier: identifier.to_string(),
            };
            let request = LegacyCommitRequest {
                lock: &lock,
                ..fixture.commit()
            };
            assert!(
                fixture
                    .store
                    .commit_legacy_migration(&request)
                    .await
                    .is_err()
            );
            assert_eq!(fixture.state()?, before);
        }

        let commit = fixture.committed().await?;
        for identifier in [
            fixture.binding.source_physical_id,
            fixture.binding.target_physical_id,
        ] {
            let lock = StateLock {
                lock_identifier: identifier.to_string(),
            };
            let request = LegacyRollbackRequest {
                tenant: &fixture.tenant,
                binding: &fixture.binding,
                migration: MIGRATION,
                commit: &commit,
                lock: &lock,
                writer_nonce: Uuid::new_v4(),
                holder: "rollback",
            };
            assert!(fixture.store.lock_legacy_rollback(&request).await.is_err());
        }
        let mut foreign = fixture.binding.clone();
        foreign.source_physical_id = Uuid::new_v4();
        assert!(
            fixture
                .store
                .legacy_commit_evidence(&fixture.tenant, MIGRATION, &foreign)
                .await
                .is_err()
        );
        let lock = StateLock::generate();
        let request = LegacyRollbackRequest {
            tenant: &fixture.tenant,
            binding: &foreign,
            migration: MIGRATION,
            commit: &commit,
            lock: &lock,
            writer_nonce: Uuid::new_v4(),
            holder: "rollback",
        };
        let before = fixture.state()?;
        assert!(matches!(
            fixture.store.lock_legacy_rollback(&request).await?,
            LegacyMutation::Stale
        ));
        assert_eq!(fixture.state()?, before);
        Ok(())
    }

    #[tokio::test]
    async fn connection_identity_binds_routing_but_excludes_credentials() -> TestResult {
        let server = FakeS3::start().await?;
        let configuration = ObjectStoreConfiguration {
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
        };
        let original =
            ObjectStateStore::try_new(configuration.clone())?.legacy_connection_identity()?;
        let mut changed = configuration.clone();
        changed.region = "us-west-2".into();
        assert_ne!(
            original,
            ObjectStateStore::try_new(changed)?.legacy_connection_identity()?
        );
        let mut changed = configuration.clone();
        changed.addressing = AddressingChoice::VirtualHosted;
        assert_ne!(
            original,
            ObjectStateStore::try_new(changed)?.legacy_connection_identity()?
        );
        let mut changed = configuration;
        changed.credentials.secret_access_key = "another-secret".into();
        assert_eq!(
            original,
            ObjectStateStore::try_new(changed)?.legacy_connection_identity()?
        );
        Ok(())
    }
}
