//! Durable, multi-tenant state for managed resources.
//!
//! Each managed resource is one record keyed by
//! `(module_path, project, resource_type, resource_name)`. Records hold the
//! provider's state in cty JSON (the format `UpgradeResourceState` expects),
//! the schema version it was written with, the provider's opaque private
//! bytes, and a serial the store increments on every write.
//!
//! Every tenant also has at most one owner record naming the CUE instance
//! that owns its state ([`TenantOwner`]), and at most one lock.

mod memory;
mod turso;

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{InfrastructureError, Result};
use crate::tenant::{ProjectInstance, TenantKey};

pub use memory::MemoryStateStore;
pub use turso::{TursoConfiguration, TursoStateStore};

/// Address of a managed resource within a tenant: `type.name`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ResourceAddress {
    /// Resource type, for example `random_pet`.
    pub resource_type: String,
    /// Resource name from the `infrastructure.resources` map.
    pub name: String,
}

impl ResourceAddress {
    /// Build an address.
    #[must_use]
    pub fn new(resource_type: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            resource_type: resource_type.into(),
            name: name.into(),
        }
    }
}

impl fmt::Display for ResourceAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.resource_type, self.name)
    }
}

/// A managed resource as persisted in state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManagedResource {
    /// Resource address.
    pub address: ResourceAddress,
    /// Local provider name from `infrastructure.providers`.
    pub provider: String,
    /// Provider source address, for example `registry.terraform.io/hashicorp/random`.
    pub provider_source: String,
    /// Resource schema version the state was written with.
    pub schema_version: i64,
    /// Resource state as cty JSON.
    pub state: serde_json::Value,
    /// Provider private data, opaque to cuenv.
    #[serde(with = "base64_bytes")]
    pub private: Vec<u8>,
    /// Names of resources this one depends on, used to order destroys.
    pub dependencies: Vec<String>,
    /// The resource exists but its creation failed part way; the next plan
    /// must replace it.
    #[serde(default)]
    pub tainted: bool,
    /// Resource identity data as cty JSON, for providers that declare an
    /// identity schema.
    #[serde(default)]
    pub identity: Option<serde_json::Value>,
    /// How many times the stored record was written: 1 after the first
    /// write, one more after every further write. Assigned by the store
    /// when the record is read; ignored when writing.
    #[serde(default)]
    pub serial: i64,
    /// Identity of this insertion, retained on updates and renewed after deletion.
    /// Nil identifies records read from a schema predating generations.
    #[serde(default)]
    pub generation: uuid::Uuid,
}

impl ManagedResource {
    /// Whether two records hold the same content, ignoring store metadata.
    #[must_use]
    pub fn same_content(&self, other: &Self) -> bool {
        Self {
            serial: other.serial,
            generation: other.generation,
            ..self.clone()
        } == *other
    }
}

/// The version of a stored record a conditional write expects to replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordVersion {
    /// No record is stored.
    Absent,
    /// The record with this serial is stored.
    Serial(i64),
    /// A particular insertion and its update serial. Unlike a serial alone,
    /// this version cannot match a record deleted and recreated at the same address.
    Generation {
        /// Identity assigned when the row was inserted.
        generation: uuid::Uuid,
        /// Number of writes to that insertion.
        serial: i64,
    },
}

impl RecordVersion {
    /// The version of `record` (absent when there is none).
    #[must_use]
    pub fn of(record: Option<&ManagedResource>) -> Self {
        record.map_or(Self::Absent, |record| Self::Generation {
            generation: record.generation,
            serial: record.serial,
        })
    }

    /// The version a successful write of the record leaves behind.
    #[must_use]
    pub const fn after_write(self) -> Self {
        match self {
            Self::Absent => Self::Serial(1),
            Self::Serial(serial) => Self::Serial(serial.saturating_add(1)),
            Self::Generation { generation, serial } => Self::Generation {
                generation,
                serial: serial.saturating_add(1),
            },
        }
    }
}

impl fmt::Display for RecordVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => formatter.write_str("no record"),
            Self::Serial(serial) => write!(formatter, "serial {serial}"),
            Self::Generation { generation, serial } => {
                write!(formatter, "generation {generation}, serial {serial}")
            }
        }
    }
}

/// A write that succeeds only while the stored record is still at
/// `expected`; see [`StateStore::put_if_unchanged`].
#[derive(Debug, Clone, Copy)]
pub struct ConditionalPut<'put> {
    /// The record to write.
    pub resource: &'put ManagedResource,
    /// The stored version it must replace.
    pub expected: RecordVersion,
}

impl ConditionalPut<'_> {
    /// Whether this exact write was already recorded after a lost response.
    /// A create's generation belongs to its write payload, so an independent
    /// insertion with identical content cannot acknowledge the pending write.
    #[must_use]
    pub fn is_recorded(&self, stored: &ManagedResource) -> bool {
        !self.resource.generation.is_nil()
            && stored.generation == self.resource.generation
            && stored.same_content(self.resource)
            && match self.expected {
                RecordVersion::Absent => stored.serial == 1,
                RecordVersion::Generation { .. } => {
                    RecordVersion::of(Some(stored)) == self.expected.after_write()
                }
                RecordVersion::Serial(_) => false,
            }
    }
}

/// An acquired state lock. Every write must present it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateLock {
    /// Lock identifier, needed to write and to release it.
    pub lock_identifier: String,
}

impl StateLock {
    /// A lock with a fresh, random identifier, not yet acquired.
    ///
    /// Generating the identifier before acquiring it lets a caller register
    /// the lock it is about to take (for example, to release it from an
    /// interrupt) before the acquisition could have committed.
    #[must_use]
    pub fn generate() -> Self {
        Self {
            lock_identifier: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// Check that the identifier is safe to store and display: 1 to 128
    /// letters, digits, `-` or `_`.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for any other
    /// identifier.
    pub fn validate(&self) -> Result<()> {
        let identifier = &self.lock_identifier;
        let valid = (1..=128).contains(&identifier.len())
            && identifier
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "-_".contains(character));
        if valid {
            Ok(())
        } else {
            Err(InfrastructureError::configuration(
                "a lock identifier must be 1 to 128 letters, digits, '-' or '_'",
            ))
        }
    }
}

/// What [`StateStore::acquire_lock`] needs.
#[derive(Debug, Clone, Copy)]
pub struct LockRequest<'request> {
    /// The lock to take, with its identifier already chosen
    /// ([`StateLock::generate`]).
    pub lock: &'request StateLock,
    /// Description of the holder (command, user, host, process).
    pub holder: &'request str,
}

/// Who holds a tenant's lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockInformation {
    /// Lock identifier.
    pub lock_identifier: String,
    /// Description of the holder (command, user, host, process).
    pub holder: String,
    /// When the lock was acquired (RFC 3339).
    pub acquired_at: String,
}

/// The CUE instance that owns a tenant's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantOwner {
    /// The owning instance.
    pub instance: ProjectInstance,
    /// When ownership was claimed or last transferred (RFC 3339).
    pub claimed_at: String,
}

impl TenantOwner {
    /// Refuse unless `instance` is this owner.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::OwnedByAnotherInstance`] when another
    /// instance owns the tenant.
    pub fn require(&self, tenant: &TenantKey, instance: &ProjectInstance) -> Result<()> {
        if self.instance == *instance {
            Ok(())
        } else {
            Err(InfrastructureError::OwnedByAnotherInstance {
                tenant: tenant.to_string(),
                owner: self.instance.to_string(),
                instance: instance.to_string(),
            })
        }
    }
}

/// How [`StateStore::claim_owner`] treats an existing owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerClaimMode {
    /// Record the instance only when the tenant has no owner yet; an
    /// existing owner is kept (and returned).
    IfUnowned,
    /// Make the instance the owner, replacing any existing owner (an
    /// explicit adoption).
    Transfer,
}

/// What [`StateStore::claim_owner`] records.
#[derive(Debug, Clone, Copy)]
pub struct OwnerClaim<'claim> {
    /// The instance claiming ownership.
    pub instance: &'claim ProjectInstance,
    /// Whether an existing owner is kept or replaced.
    pub mode: OwnerClaimMode,
}

/// Storage backend for managed resource state.
///
/// Every operation is scoped to a [`TenantKey`]; implementations must never
/// read or write rows belonging to another tenant.
#[async_trait]
pub trait StateStore: Send + Sync {
    /// Non-secret, stable backend identity for binding local recovery files.
    /// In-memory or custom stores can remain unbound.
    fn recovery_identity(&self) -> Option<String> {
        None
    }

    /// Create tables and indexes if they do not exist, and bring the schema
    /// up to date.
    ///
    /// Fails when the stored schema is newer than this build knows.
    async fn migrate(&self) -> Result<()>;

    /// List all managed resources of a tenant, with generation and serial.
    ///
    /// Never migrates: a store that was never migrated has no resources.
    async fn list(&self, tenant: &TenantKey) -> Result<Vec<ManagedResource>>;

    /// Insert or replace a managed resource.
    /// New insertions receive a fresh generation; updates retain it.
    ///
    /// Fails with [`crate::InfrastructureError::LockLost`] unless `lock` is
    /// still the tenant's current lock; the check and the write are atomic.
    async fn put(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        resource: &ManagedResource,
    ) -> Result<()>;

    /// Insert or replace a managed resource only while the stored record is
    /// still at `put.expected` (compare and swap). Fenced by `lock` like
    /// [`StateStore::put`]; the lock check, the version check and the write
    /// are atomic.
    /// A create payload with a non-nil generation preserves that generation
    /// on insertion, so an exact retry can acknowledge its own earlier write.
    ///
    /// Fails with [`crate::InfrastructureError::StateChanged`] naming the
    /// address when the stored version differs.
    async fn put_if_unchanged(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        put: &ConditionalPut<'_>,
    ) -> Result<()>;

    /// Remove a managed resource. Fenced by `lock` like [`StateStore::put`].
    async fn delete(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        address: &ResourceAddress,
    ) -> Result<()>;

    /// Acquire the tenant's exclusive lock under the identifier the caller
    /// chose.
    ///
    /// Fails with [`crate::InfrastructureError::Locked`] if another run holds
    /// it, and fails unless [`StateStore::migrate`] brought the store to the
    /// schema this build writes. When the outcome is uncertain (the
    /// response was lost), the error names the identifier so the caller can
    /// release it.
    async fn acquire_lock(
        &self,
        tenant: &TenantKey,
        request: &LockRequest<'_>,
    ) -> Result<StateLock>;

    /// Acquire the tenant's exclusive lock under a fresh identifier; see
    /// [`StateStore::acquire_lock`].
    async fn lock(&self, tenant: &TenantKey, holder: &str) -> Result<StateLock> {
        let lock = StateLock::generate();
        self.acquire_lock(
            tenant,
            &LockRequest {
                lock: &lock,
                holder,
            },
        )
        .await
    }

    /// Release a lock acquired with [`StateStore::lock`].
    async fn unlock(&self, tenant: &TenantKey, lock: &StateLock) -> Result<()>;

    /// Describe the tenant's current lock, if any. Never migrates.
    async fn current_lock(&self, tenant: &TenantKey) -> Result<Option<LockInformation>>;

    /// Release the tenant's lock only if its identifier is `lock_identifier`.
    ///
    /// Returns `false` when no lock with that identifier is held.
    async fn force_unlock(&self, tenant: &TenantKey, lock_identifier: &str) -> Result<bool>;

    /// The instance that owns the tenant's state, if one is recorded.
    ///
    /// Never migrates: a store without the owner table (never migrated, or
    /// migrated by an older cuenv) has no owner recorded.
    async fn owner(&self, tenant: &TenantKey) -> Result<Option<TenantOwner>>;

    /// Record `claim.instance` as the tenant's owner, as `claim.mode`
    /// allows, and return the owner recorded afterwards (the existing one,
    /// when [`OwnerClaimMode::IfUnowned`] finds one). Fenced by `lock` like
    /// [`StateStore::put`].
    async fn claim_owner(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        claim: &OwnerClaim<'_>,
    ) -> Result<TenantOwner>;
}

/// Serde helpers storing bytes as standard base64 text.
pub(crate) mod base64_bytes {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<Output: Serializer>(
        bytes: &[u8],
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'input, Input: Deserializer<'input>>(
        deserializer: Input,
    ) -> Result<Vec<u8>, Input::Error> {
        let encoded = String::deserialize(deserializer)?;
        STANDARD.decode(encoded).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_versions_advance_with_every_write() {
        assert_eq!(
            RecordVersion::Absent.after_write(),
            RecordVersion::Serial(1)
        );
        assert_eq!(
            RecordVersion::Serial(4).after_write(),
            RecordVersion::Serial(5)
        );
        assert_eq!(RecordVersion::of(None), RecordVersion::Absent);
        assert_eq!(RecordVersion::Serial(3).to_string(), "serial 3");
        assert_eq!(
            serde_json::to_value(RecordVersion::Serial(3)).unwrap(),
            serde_json::json!({"serial": 3})
        );
        assert_eq!(
            serde_json::to_value(RecordVersion::Absent).unwrap(),
            serde_json::json!("absent")
        );
    }

    #[test]
    fn generated_lock_identifiers_are_valid_and_others_are_checked() {
        let lock = StateLock::generate();
        lock.validate().unwrap();
        assert_ne!(lock, StateLock::generate());
        for invalid in ["", "has space", "semi;colon", &"x".repeat(129)] {
            assert!(
                StateLock {
                    lock_identifier: invalid.to_string()
                }
                .validate()
                .is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn owners_refuse_other_instances() {
        let tenant = TenantKey::new("example.com/app", "web").unwrap();
        let owner = TenantOwner {
            instance: ProjectInstance::new(".", "web").unwrap(),
            claimed_at: "2026-01-01T00:00:00Z".into(),
        };
        owner
            .require(&tenant, &ProjectInstance::new("", "web").unwrap())
            .unwrap();
        let error = owner
            .require(&tenant, &ProjectInstance::new("_copy", "web").unwrap())
            .unwrap_err();
        assert!(
            matches!(error, InfrastructureError::OwnedByAnotherInstance { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("_copy:web"), "{error}");
        assert!(error.to_string().contains("state adopt"), "{error}");
    }
}
