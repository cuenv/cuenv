//! Lossless preflight for copying a protected legacy object snapshot to SQL.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::{Map, Value};

use super::object::document::DocumentWriter;
use super::object::store::legacy_migration::LegacyRollbackSnapshot;
use super::{LastWriter, ManagedResource, ResourceAddress};
use crate::error::{InfrastructureError, Result};

const RECORD_FIELDS: [&str; 12] = [
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
];

/// A completely validated legacy snapshot ready for exact copy-back.
#[derive(Clone, PartialEq)]
pub struct LegacyMigrationCopyPlan {
    resources: Vec<ManagedResource>,
    writer: Option<LastWriter>,
}

impl fmt::Debug for LegacyMigrationCopyPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LegacyMigrationCopyPlan(<redacted>)")
    }
}

impl LegacyMigrationCopyPlan {
    /// Validate all raw records and metadata before any source mutation.
    ///
    /// # Errors
    ///
    /// Rejects unknown fields, damaged records, inconsistent addresses, and
    /// versions SQL or D1 cannot represent without loss.
    pub fn try_from_snapshot(snapshot: &LegacyRollbackSnapshot) -> Result<Self> {
        if snapshot.tombstone.is_some() {
            return Err(unrepresentable("a tombstoned target"));
        }
        ensure_no_legacy_extensions(&[
            &snapshot.document_extensions,
            &snapshot.format_extensions,
            &snapshot.tenant_extensions,
            &snapshot.origin_extensions,
            &snapshot.lock_extensions,
            &snapshot.lock_change_extensions,
            &snapshot.tombstone_extensions,
        ])?;
        let writer = decode_legacy_writer(snapshot.last_writer.as_ref())?;
        let resources = decode_legacy_records(&snapshot.records)?;
        Ok(Self { resources, writer })
    }

    /// Exact target records, including their generations and serials.
    #[must_use]
    pub fn resources(&self) -> &[ManagedResource] {
        &self.resources
    }

    /// Exact last-writer metadata, or `None` when none is recorded.
    #[must_use]
    pub fn writer(&self) -> Option<&LastWriter> {
        self.writer.as_ref()
    }
}

pub(super) fn ensure_no_legacy_extensions(extensions: &[&Map<String, Value>]) -> Result<()> {
    if extensions.iter().any(|fields| !fields.is_empty()) {
        return Err(unrepresentable("migration document metadata"));
    }
    Ok(())
}

pub(super) fn decode_legacy_writer(writer: Option<&DocumentWriter>) -> Result<Option<LastWriter>> {
    writer
        .map(|writer| {
            if !writer.extra.is_empty() {
                return Err(unrepresentable("last-writer metadata"));
            }
            let decoded = writer.last_writer();
            if decoded.instance.as_str() != writer.instance || decoded.recorded_at != writer.at {
                return Err(unrepresentable("last-writer metadata"));
            }
            Ok(decoded)
        })
        .transpose()
}

pub(super) fn decode_legacy_records(
    records: &BTreeMap<String, Value>,
) -> Result<Vec<ManagedResource>> {
    records
        .iter()
        .map(|(key, value)| decode_record(key, value))
        .collect()
}

fn unrepresentable(kind: &str) -> InfrastructureError {
    InfrastructureError::configuration(format!("legacy migration cannot represent {kind}"))
}

fn fields_are_known(value: &Map<String, Value>, allowed: &[&str]) -> bool {
    value.keys().all(|key| allowed.contains(&key.as_str()))
}

fn decode_record(key: &str, value: &Value) -> Result<ManagedResource> {
    let object = value
        .as_object()
        .ok_or_else(|| unrepresentable("a migration record"))?;
    if !fields_are_known(object, &RECORD_FIELDS) {
        return Err(unrepresentable("a migration record extension"));
    }
    let address = object
        .get("address")
        .and_then(Value::as_object)
        .ok_or_else(|| unrepresentable("a migration record address"))?;
    if !fields_are_known(address, &["resource_type", "name"]) {
        return Err(unrepresentable("a migration address extension"));
    }
    let resource: ManagedResource =
        serde_json::from_value(value.clone()).map_err(|_| unrepresentable("a migration record"))?;
    if resource.address.to_string() != key
        || ResourceAddress::parse(key).as_ref() != Some(&resource.address)
        || resource.generation.is_nil()
        || !(1..(1_i64 << 53)).contains(&resource.serial)
        || resource.schema_version.unsigned_abs() >= (1_u64 << 53)
    {
        return Err(unrepresentable("a migration record address or version"));
    }
    Ok(resource)
}

#[cfg(test)]
#[path = "legacy_copy_tests.rs"]
mod tests;
