//! Validated requests and identities for legacy state migration.

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::{LastWriter, MIGRATION_FENCE_PREFIX, ManagedResource, ResourceAddress, StateLock};
use crate::error::{InfrastructureError, Result};
use crate::tenant::TenantKey;

/// Opaque digest of a non-secret, backend-derived physical identity.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BackendIdentity([u8; 32]);

impl BackendIdentity {
    /// Build an identity from a digest of non-secret backend identity fields.
    #[must_use]
    pub const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    /// Return the opaque digest bytes for backend storage.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for BackendIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<backend identity>")
    }
}

/// The connected source backend and its effective state namespace.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum LegacyMigrationSource {
    /// An in-memory state store with an instance-scoped identity.
    Memory {
        /// Digest of the in-memory store instance identity.
        connection: BackendIdentity,
    },
    /// A Turso database reached through a non-secret connection identity.
    Turso {
        /// Digest of the validated effective connection.
        connection: BackendIdentity,
    },
    /// A D1 database reached through a non-secret connection identity.
    D1 {
        /// Digest of the validated effective connection.
        connection: BackendIdentity,
    },
    /// An object-store namespace, including its root and CUE package.
    Object {
        /// Digest of the validated effective connection.
        connection: BackendIdentity,
        /// Configured object-key prefix.
        root: Option<String>,
        /// CUE package in the object namespace.
        package: String,
    },
}

impl fmt::Debug for LegacyMigrationSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<legacy migration source>")
    }
}

/// Result of a conditional migration-only mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationMutation {
    /// The requested mutation is durably established.
    Applied,
    /// The expected legacy fence no longer matches.
    Stale,
    /// The outcome cannot be established from durable evidence.
    Indeterminate,
}

/// An exact legacy migration fence used during an externally frozen copy-back.
#[derive(Clone, Copy)]
pub struct LegacyMigrationAccess<'access> {
    /// Tenant protected by the fence.
    pub tenant: &'access TenantKey,
    /// The exact existing legacy migration fence.
    pub fence: &'access StateLock,
}

/// A complete representable source snapshot read under a legacy fence.
#[derive(Clone, PartialEq)]
pub struct LegacyMigrationSnapshot {
    /// Stored records with their generations and serials.
    pub resources: Vec<ManagedResource>,
    /// Stored last-writer metadata, if any.
    pub writer: Option<LastWriter>,
}

impl fmt::Debug for LegacyMigrationSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<legacy migration snapshot>")
    }
}

/// Outcome of reading a complete snapshot under a legacy fence.
#[derive(Debug, Clone, PartialEq)]
pub enum LegacyMigrationSnapshotRead {
    /// The exact fence was held and the complete snapshot was read.
    Available(Box<LegacyMigrationSnapshot>),
    /// The expected fence or schema is no longer current.
    Stale,
    /// The snapshot could not be established safely.
    Indeterminate,
}

impl fmt::Debug for LegacyMigrationAccess<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<legacy migration access>")
    }
}

impl LegacyMigrationAccess<'_> {
    /// Validate the fence syntax. Its origin and ownership must also be
    /// checked against the migration receipt by the caller and backend.
    ///
    /// # Errors
    ///
    /// Rejects malformed or non-migration fence identifiers.
    pub fn validate(&self) -> Result<()> {
        self.fence.validate()?;
        if self
            .fence
            .lock_identifier
            .strip_prefix(MIGRATION_FENCE_PREFIX)
            .is_none_or(str::is_empty)
        {
            return Err(InfrastructureError::configuration(
                "legacy migration copy requires an exact migration fence",
            ));
        }
        Ok(())
    }
}

/// The complete typed snapshot to validate before any legacy copy-back write.
#[derive(Clone, Copy)]
pub struct LegacyMigrationCopy<'copy> {
    /// Existing source fence.
    pub access: LegacyMigrationAccess<'copy>,
    /// Target records to copy with their original generations and serials.
    pub resources: &'copy [ManagedResource],
    /// Target last-writer metadata, or `None` to clear the source metadata.
    pub writer: Option<&'copy LastWriter>,
}

impl fmt::Debug for LegacyMigrationCopy<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<legacy migration copy>")
    }
}

impl LegacyMigrationCopy<'_> {
    /// Validate every record before the first source mutation.
    ///
    /// The target's raw records and metadata must be checked separately for
    /// extensions the source cannot represent before constructing this copy.
    ///
    /// # Errors
    ///
    /// Rejects invalid fences, duplicate addresses, or unrepresentable versions.
    pub fn validate(&self) -> Result<()> {
        self.access.validate()?;
        let mut addresses = HashSet::new();
        for resource in self.resources {
            LegacyMigrationRecordRequest {
                access: self.access,
                resource,
            }
            .validate()?;
            if !addresses.insert(&resource.address) {
                return Err(InfrastructureError::configuration(
                    "a legacy migration copy cannot repeat a resource address",
                ));
            }
        }
        Ok(())
    }
}

/// An exact record replacement protected by a legacy migration fence.
#[derive(Clone, Copy)]
pub struct LegacyMigrationRecordRequest<'record> {
    /// Existing source fence.
    pub access: LegacyMigrationAccess<'record>,
    /// Complete stored record, including generation and serial.
    pub resource: &'record ManagedResource,
}

impl fmt::Debug for LegacyMigrationRecordRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<legacy migration record request>")
    }
}

impl LegacyMigrationRecordRequest<'_> {
    /// Validate a record's address and cross-backend version range.
    ///
    /// # Errors
    ///
    /// Rejects a malformed fence, address, generation or serial.
    pub fn validate(&self) -> Result<()> {
        self.access.validate()?;
        let resource = self.resource;
        if ResourceAddress::parse(&resource.address.to_string()).as_ref() != Some(&resource.address)
            || resource.generation.is_nil()
            || !(1..(1_i64 << 53)).contains(&resource.serial)
            || resource.schema_version.unsigned_abs() >= (1_u64 << 53)
            || resource
                .identity
                .as_ref()
                .is_some_and(serde_json::Value::is_null)
        {
            return Err(InfrastructureError::configuration(
                "a migrated record needs a valid address, version and representable identity",
            ));
        }
        Ok(())
    }
}

/// Delete a record under a legacy migration fence.
#[derive(Clone, Copy)]
pub struct LegacyMigrationDeleteRequest<'delete> {
    /// Existing source fence.
    pub access: LegacyMigrationAccess<'delete>,
    /// Address to remove.
    pub address: &'delete ResourceAddress,
}

impl fmt::Debug for LegacyMigrationDeleteRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<legacy migration delete request>")
    }
}

/// Replace or clear last-writer metadata under a legacy migration fence.
#[derive(Clone, Copy)]
pub struct LegacyMigrationWriterRequest<'writer> {
    /// Existing source fence.
    pub access: LegacyMigrationAccess<'writer>,
    /// Exact target metadata, or `None` to clear it.
    pub writer: Option<&'writer LastWriter>,
}

impl fmt::Debug for LegacyMigrationWriterRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<legacy migration writer request>")
    }
}

#[cfg(test)]
mod tests {
    use super::{LegacyMigrationAccess, LegacyMigrationRecordRequest};
    use crate::state::{ManagedResource, ResourceAddress, StateLock};
    use crate::tenant::TenantKey;

    #[test]
    fn records_refuse_null_identity_and_preserve_reserved_json_objects() {
        let tenant = TenantKey::try_new("example.com/module", "web").unwrap();
        let lock = StateLock {
            lock_identifier: "migrated-123".into(),
        };
        let access = LegacyMigrationAccess {
            tenant: &tenant,
            fence: &lock,
        };
        let mut resource = ManagedResource {
            address: ResourceAddress::new("random_pet", "pet"),
            provider: "random".into(),
            provider_source: "registry.terraform.io/hashicorp/random".into(),
            schema_version: 0,
            state: serde_json::json!({"$serde_json::private::RawValue": "42"}),
            private: Vec::new(),
            dependencies: Vec::new(),
            sensitive_attributes: Vec::new(),
            tainted: false,
            identity: Some(serde_json::Value::Null),
            serial: 1,
            generation: uuid::Uuid::new_v4(),
        };
        assert!(
            LegacyMigrationRecordRequest {
                access,
                resource: &resource,
            }
            .validate()
            .is_err()
        );
        resource.identity = Some(serde_json::json!({
            "$serde_json::private::RawValue": "42"
        }));
        LegacyMigrationRecordRequest {
            access,
            resource: &resource,
        }
        .validate()
        .unwrap();
        let encoded = serde_json::to_value(&resource).unwrap();
        assert_eq!(encoded["state"], resource.state);
        assert_eq!(encoded["identity"], resource.identity.clone().unwrap());
        assert_eq!(
            serde_json::from_value::<ManagedResource>(encoded).unwrap(),
            resource
        );
    }
}
