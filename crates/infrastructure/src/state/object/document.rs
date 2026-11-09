//! The `state.json` document: one tenant's whole state.
//!
//! Every write changes `serial` and `writer`, so no two bodies are ever the
//! same and an ETag can never repeat (no ABA, even where the ETag is a
//! content hash). Unknown fields are kept at every level a writer rebuilds,
//! so a writer that is allowed to write never drops a field a newer cuenv
//! added. A reader refuses a newer `format.read`, a writer a newer
//! `format.write`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::key::TenantPath;
use crate::error::{InfrastructureError, Result};
use crate::state::{LastWriter, LockIdentity, LockInformation, ManagedResource, ResourceAddress};
use crate::tenant::ProjectInstance;

/// The read format this build understands and writes.
pub const FORMAT_READ: u32 = 1;

/// The newest write format this build understands.
pub const FORMAT_WRITE: u32 = 2;
pub(super) const LOCK_KIND: &str = "cuenvLockKind";
pub(super) const MIGRATION_LOCK: &str = "cuenvMigrationLock";
const LEGACY_ROLLBACK_LOCK: &str = "cuenvLegacyRollbackLock";

/// New state remains writable by older clients until a migration binds it.
const INITIAL_WRITE_FORMAT: u32 = 1;

/// The cuenv version written into `writtenBy`.
#[must_use]
pub fn written_by() -> String {
    format!("cuenv {}", env!("CARGO_PKG_VERSION"))
}

/// Format versions of a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Format {
    /// Readers refuse a newer read version.
    pub read: u32,
    /// Writers refuse a newer write version.
    pub write: u32,
    /// Fields retained from compatible writers.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The tenant identity a document belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentTenant {
    /// CUE module path.
    pub module_path: String,
    /// CUE package.
    pub package: String,
    /// Project name.
    pub project: String,
    /// Named environment, `None` for the identity without `--env`.
    pub environment: Option<String>,
    /// Fields retained from compatible writers.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl DocumentTenant {
    /// The identity `path` holds.
    #[must_use]
    pub fn of(path: &TenantPath) -> Self {
        Self {
            module_path: path.module_path().to_string(),
            package: path.package().to_string(),
            project: path.project().to_string(),
            environment: path.environment().map(ToString::to_string),
            extra: Map::new(),
        }
    }
}

/// The last change a lock holder recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastChange {
    /// Resource address.
    pub address: String,
    /// What happened (`put` or `delete`).
    pub action: String,
    /// When (RFC 3339).
    pub at: String,
    /// Fields retained from compatible writers.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A held lock.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentLock {
    /// Lock identifier.
    pub lock_identifier: String,
    /// Holder description.
    pub holder: String,
    /// When it was acquired (RFC 3339).
    pub acquired_at: String,
    /// The cuenv version of the holder.
    #[serde(default)]
    pub cuenv_version: String,
    /// The last change the holder recorded.
    #[serde(default)]
    pub last_change: Option<LastChange>,
    /// Fields a newer cuenv added.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl std::fmt::Debug for DocumentLock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DocumentLock(<redacted>)")
    }
}

impl DocumentLock {
    /// The lock as the trait reports it; stored text is untrusted.
    #[must_use]
    pub fn information(&self, write_format: u32) -> LockInformation {
        if !self.is_ordinary(write_format) {
            let acquired_at = chrono::DateTime::parse_from_rfc3339(&self.acquired_at)
                .map_or_else(|_| "unknown".to_string(), |time| time.to_rfc3339());
            return LockInformation {
                identity: LockIdentity::Migration { operation_id: None },
                holder: "infrastructure migration".into(),
                acquired_at,
            };
        }
        LockInformation {
            identity: LockIdentity::Ordinary {
                identifier: crate::error::strip_control_characters(&self.lock_identifier),
            },
            holder: crate::error::strip_control_characters(&self.holder),
            acquired_at: crate::error::strip_control_characters(&self.acquired_at),
        }
    }

    /// Whether this lock has an unambiguous ordinary-lock representation.
    #[must_use]
    pub fn is_ordinary(&self, write_format: u32) -> bool {
        if self.extra.contains_key(MIGRATION_LOCK) || self.extra.contains_key(LEGACY_ROLLBACK_LOCK)
        {
            return false;
        }
        match self.extra.get(LOCK_KIND) {
            Some(Value::String(kind)) if kind == "ordinary" => true,
            None if write_format < 2 => true,
            _ => false,
        }
    }
}

/// The instance that last took the lock to change state.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentWriter {
    /// `<directory>:<package>`.
    pub instance: String,
    /// When (RFC 3339).
    pub at: String,
    /// Fields retained from compatible writers.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl std::fmt::Debug for DocumentWriter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DocumentWriter(<redacted>)")
    }
}

impl DocumentWriter {
    /// The writer as the trait reports it; stored text is untrusted.
    #[must_use]
    pub fn last_writer(&self) -> LastWriter {
        LastWriter {
            instance: ProjectInstance::from_stored(&self.instance),
            recorded_at: crate::error::strip_control_characters(&self.at),
        }
    }
}

/// Where a tenant came from, for a resumable migration or rename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// `migration` or `rename`.
    pub kind: String,
    /// The operation identifier.
    pub id: String,
    /// Where the state came from.
    pub source: String,
    /// Fields retained from compatible writers.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A tenant whose state moved elsewhere.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tombstone {
    /// The key the state moved to. Untrusted: re-validated before display.
    pub moved_to: String,
    /// Fields retained from compatible writers.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl std::fmt::Debug for Tombstone {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Tombstone(<redacted>)")
    }
}

/// One tenant's state.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateDocument {
    /// Format versions.
    pub format: Format,
    /// The cuenv version that wrote this body.
    pub written_by: String,
    /// Incremented by every write; never reset for a key.
    pub serial: u64,
    /// A fresh random value on every write.
    pub writer: String,
    /// The identity this document belongs to.
    pub tenant: DocumentTenant,
    /// The instance that last took the lock to change state.
    #[serde(default)]
    pub last_writer: Option<DocumentWriter>,
    /// The held lock.
    #[serde(default)]
    pub lock: Option<DocumentLock>,
    /// The identifier of the last lock released.
    #[serde(default)]
    pub last_released: Option<String>,
    /// Where this state came from, when migrated or renamed.
    #[serde(default)]
    pub origin: Option<Origin>,
    /// Set when the state moved to another key.
    #[serde(default)]
    pub tombstone: Option<Tombstone>,
    /// Records by address (`type.name`), as stored.
    #[serde(default, deserialize_with = "deserialize_records")]
    pub records: BTreeMap<String, Value>,
    /// Fields a newer cuenv added.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

struct UniqueRawObject(BTreeMap<String, Box<serde_json::value::RawValue>>);

impl<'de> Deserialize<'de> for UniqueRawObject {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ObjectVisitor;

        impl<'de> serde::de::Visitor<'de> for ObjectVisitor {
            type Value = UniqueRawObject;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an object with unique field names")
            }

            fn visit_map<M>(self, mut access: M) -> std::result::Result<Self::Value, M::Error>
            where
                M: serde::de::MapAccess<'de>,
            {
                let mut fields = BTreeMap::new();
                while let Some((name, value)) =
                    access.next_entry::<String, Box<serde_json::value::RawValue>>()?
                {
                    if fields.insert(name, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate object field"));
                    }
                }
                Ok(UniqueRawObject(fields))
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

#[derive(Clone, Copy)]
enum ReservedKeys {
    Allow,
    Reject,
}

fn validate_raw_value(
    raw: &serde_json::value::RawValue,
    policy: ReservedKeys,
    depth: usize,
) -> Result<()> {
    if depth >= 128 {
        return Err(InfrastructureError::state(
            "object storage state nesting is too deep",
        ));
    }
    match raw.get().as_bytes().first() {
        Some(b'{') => {
            let fields: UniqueRawObject = serde_json::from_str(raw.get()).map_err(|_| {
                InfrastructureError::state("object storage state document does not decode")
            })?;
            for (name, child) in fields.0 {
                if matches!(policy, ReservedKeys::Reject)
                    && matches!(
                        name.as_str(),
                        "$serde_json::private::RawValue" | "$serde_json::private::Number"
                    )
                {
                    return Err(InfrastructureError::state(
                        "object storage state contains unsupported extension metadata",
                    ));
                }
                validate_raw_value(&child, policy, depth + 1)?;
            }
        }
        Some(b'[') => {
            let elements: Vec<Box<serde_json::value::RawValue>> = serde_json::from_str(raw.get())
                .map_err(|_| {
                InfrastructureError::state("object storage state document does not decode")
            })?;
            for element in elements {
                validate_raw_value(&element, policy, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_raw_document(raw: &serde_json::value::RawValue) -> Result<()> {
    let fields: UniqueRawObject = serde_json::from_str(raw.get())
        .map_err(|_| InfrastructureError::state("object storage state document does not decode"))?;
    for (name, child) in fields.0 {
        if matches!(
            name.as_str(),
            "$serde_json::private::RawValue" | "$serde_json::private::Number"
        ) {
            return Err(InfrastructureError::state(
                "object storage state contains unsupported extension metadata",
            ));
        }
        let policy = if name == "records" {
            ReservedKeys::Allow
        } else {
            ReservedKeys::Reject
        };
        validate_raw_value(&child, policy, 1)?;
    }
    Ok(())
}

fn deserialize_records<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let fields = UniqueRawObject::deserialize(deserializer)?;
    fields
        .0
        .into_iter()
        .map(|(name, raw)| {
            validate_raw_value(&raw, ReservedKeys::Allow, 1).map_err(serde::de::Error::custom)?;
            let value = crate::type_system::parse_json_preserving_numbers(&raw, &[])
                .map_err(serde::de::Error::custom)?;
            Ok((name, value))
        })
        .collect()
}

impl std::fmt::Debug for StateDocument {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("StateDocument(<redacted>)")
    }
}

impl StateDocument {
    /// A new, empty document for `path`.
    #[must_use]
    pub fn new(path: &TenantPath, serial: u64) -> Self {
        Self {
            format: Format {
                read: FORMAT_READ,
                write: INITIAL_WRITE_FORMAT,
                extra: Map::new(),
            },
            written_by: written_by(),
            serial,
            writer: String::new(),
            tenant: DocumentTenant::of(path),
            last_writer: None,
            lock: None,
            last_released: None,
            origin: None,
            tombstone: None,
            records: BTreeMap::new(),
            extra: Map::new(),
        }
    }

    /// Decode a body read from `key`, refusing a newer read format and a
    /// document that belongs to another tenant than `path`.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::State`] or
    /// [`InfrastructureError::StateSchemaNewer`].
    pub fn decode(bytes: &[u8], path: &TenantPath, _key: &str) -> Result<Self> {
        let raw: &serde_json::value::RawValue = serde_json::from_slice(bytes).map_err(|_| {
            InfrastructureError::state("object storage state document does not decode")
        })?;
        validate_raw_document(raw)?;
        let numbers = crate::type_system::scan_json_numbers(raw).map_err(|_| {
            InfrastructureError::state("object storage state document does not decode")
        })?;
        if numbers
            .iter()
            .any(|token| token.number.as_json_number().is_none())
        {
            return Err(InfrastructureError::state(
                "object storage state has a number that cannot be represented exactly",
            ));
        }
        let document: Self = serde_json::from_slice(bytes).map_err(|_| {
            InfrastructureError::state("object storage state document does not decode")
        })?;
        if document.format.read > FORMAT_READ {
            return Err(InfrastructureError::state(format!(
                "object storage state needs a newer cuenv to read (format {}; this build \
                 reads format {FORMAT_READ}); upgrade cuenv",
                document.format.read
            )));
        }
        if document.tenant.module_path != path.module_path()
            || document.tenant.package != path.package()
            || document.tenant.project != path.project()
            || document.tenant.environment.as_deref() != path.environment()
        {
            return Err(InfrastructureError::state(
                "object storage state belongs to another tenant; refusing a copied or planted state document",
            ));
        }
        Ok(document)
    }

    /// Refuse to write a document a newer cuenv wrote in a newer write format.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::State`] naming the version needed.
    pub fn require_writable(&self, _key: &str) -> Result<()> {
        if self.format.write > FORMAT_WRITE {
            return Err(InfrastructureError::state(format!(
                "object storage state needs a newer cuenv to write (write format {}; this \
                 build writes format {FORMAT_WRITE}); upgrade cuenv",
                self.format.write
            )));
        }
        Ok(())
    }

    /// Prepare the next version for writing: a new serial and writer, this
    /// build as `writtenBy`. The format numbers are kept: no ordinary write
    /// raises them.
    ///
    /// # Errors
    ///
    /// Returns an error if no later serial can be represented.
    pub(super) fn advance(&mut self, writer: &str) -> Result<()> {
        self.serial = self.serial.checked_add(1).ok_or_else(|| {
            InfrastructureError::state("object storage state serial is exhausted")
        })?;
        self.writer = writer.to_string();
        self.written_by = written_by();
        Ok(())
    }

    /// The body to store.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::State`] if serialisation fails.
    pub fn encode(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|error| {
            InfrastructureError::state(format!("failed to encode state document: {error}"))
        })
    }

    /// Decode every record.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::UndecodableRecord`] for the first record
    /// that does not decode.
    pub fn resources(&self) -> Result<Vec<ManagedResource>> {
        self.records
            .iter()
            .map(|(address, value)| decode_resource(address, value))
            .collect()
    }

    /// The record at `address`, decoded.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::UndecodableRecord`] if it does not
    /// decode.
    pub fn resource(&self, address: &ResourceAddress) -> Result<Option<ManagedResource>> {
        self.records
            .get(&address.to_string())
            .map(|value| decode_resource(&address.to_string(), value))
            .transpose()
    }

    /// Store `resource` as given (serial and generation included).
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::State`] if it does not serialise.
    pub fn store(&mut self, resource: &ManagedResource) -> Result<()> {
        let value = serde_json::to_value(resource).map_err(|error| {
            InfrastructureError::state(format!("failed to encode {}: {error}", resource.address))
        })?;
        self.records.insert(resource.address.to_string(), value);
        Ok(())
    }

    /// The addresses of every record, from the keys alone, so a record that
    /// does not decode can still be named.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::UndecodableRecord`] if a key is not a
    /// valid resource address.
    pub fn addresses(&self) -> Result<Vec<ResourceAddress>> {
        self.records
            .keys()
            .map(|address| parse_record_address(address))
            .collect()
    }
}

fn parse_record_address(address: &str) -> Result<ResourceAddress> {
    ResourceAddress::parse(address).ok_or_else(|| InfrastructureError::UndecodableRecord {
        address: "(an unknown resource)".to_string(),
        problem: "the record key is not a valid resource address".to_string(),
    })
}

fn decode_resource(address: &str, value: &Value) -> Result<ManagedResource> {
    let parsed_address = parse_record_address(address)?;
    let record: ManagedResource = serde_json::from_value(value.clone()).map_err(|_| {
        InfrastructureError::UndecodableRecord {
            address: "(an unknown resource)".to_string(),
            problem: "the record has missing or invalid fields".to_string(),
        }
    })?;
    if record.address != parsed_address {
        return Err(InfrastructureError::UndecodableRecord {
            address: "(an unknown resource)".to_string(),
            problem: "the record address does not match its key".to_string(),
        });
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tenant::TenantKey;

    fn path() -> TenantPath {
        TenantPath::try_new(
            None,
            "cuenv",
            &TenantKey::try_new("github.com/acme/infra", "web").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn unknown_fields_survive_a_round_trip() {
        let mut value = serde_json::to_value(StateDocument::new(&path(), 1)).unwrap();
        value["futureField"] = serde_json::json!({"kept": true});
        value["lock"] = serde_json::json!({
            "lockIdentifier": "l", "holder": "h", "acquiredAt": "t", "futureLockField": 1
        });
        let document: StateDocument = serde_json::from_value(value).unwrap();
        let again = serde_json::to_value(&document).unwrap();
        assert_eq!(again["futureField"], serde_json::json!({"kept": true}));
        assert_eq!(again["lock"]["futureLockField"], serde_json::json!(1));
    }

    #[test]
    fn document_and_lock_debug_redact_tokens_and_provider_state() {
        let mut value = serde_json::to_value(StateDocument::new(&path(), 1)).unwrap();
        value["lock"] = serde_json::json!({
            "lockIdentifier": "private-lock-sentinel", "holder": "holder", "acquiredAt": "now"
        });
        value["records"] = serde_json::json!({"pet.test": {"state": "private-state-sentinel"}});
        value["futureField"] = serde_json::json!("private-extension-sentinel");
        let document: StateDocument = serde_json::from_value(value).unwrap();
        let printed = format!("{document:?} {:?}", document.lock);
        for secret in [
            "private-lock-sentinel",
            "private-state-sentinel",
            "private-extension-sentinel",
        ] {
            assert!(!printed.contains(secret));
        }
    }

    #[test]
    fn migration_metadata_is_opaque_even_with_a_malformed_kind() {
        let private = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        let mut value = serde_json::to_value(StateDocument::new(&path(), 1)).unwrap();
        value["lock"] = serde_json::json!({
            "lockIdentifier": private.to_string(), "holder": private.to_string(), "acquiredAt": "now",
            "cuenvLockKind": "ordinary",
            "cuenvMigrationLock": {"owner": {"operationId": private, "ownerNonce": other},
                "invalid": "metadata"}
        });
        let mut document: StateDocument = serde_json::from_value(value).unwrap();
        let lock = document.lock.as_mut().unwrap();
        let information = lock.information(2);
        assert!(information.identifier().is_none());
        assert!(information.operation_id().is_none());
        assert!(!format!("{information:?}").contains(&private.to_string()));
        lock.extra.remove(MIGRATION_LOCK);
        lock.extra.insert(LEGACY_ROLLBACK_LOCK.into(), Value::Null);
        assert!(!lock.is_ordinary(2));
        assert!(lock.information(2).operation_id().is_none());
        assert!(lock.information(2).identifier().is_none());
        lock.extra.remove(LEGACY_ROLLBACK_LOCK);
        assert!(lock.is_ordinary(2));
    }

    #[test]
    fn invalid_document_diagnostics_do_not_echo_untrusted_values() {
        let secret = "private-token-sentinel";
        let mut raw = serde_json::to_value(StateDocument::new(&path(), 1)).unwrap();
        raw["format"]["read"] = serde_json::json!(secret);
        let error =
            StateDocument::decode(&serde_json::to_vec(&raw).unwrap(), &path(), secret).unwrap_err();
        assert!(!error.to_string().contains(secret));
        raw["format"]["read"] = serde_json::json!(99);
        raw["writtenBy"] = serde_json::json!(secret);
        let error =
            StateDocument::decode(&serde_json::to_vec(&raw).unwrap(), &path(), secret).unwrap_err();
        assert!(!error.to_string().contains(secret));
        raw["format"]["read"] = serde_json::json!(1);
        raw["tenant"]["modulePath"] = serde_json::json!(secret);
        let error =
            StateDocument::decode(&serde_json::to_vec(&raw).unwrap(), &path(), secret).unwrap_err();
        assert!(!error.to_string().contains(secret));
        let mut document = StateDocument::new(&path(), 1);
        document.format.write = FORMAT_WRITE + 1;
        document.written_by = secret.into();
        assert!(
            !document
                .require_writable(secret)
                .unwrap_err()
                .to_string()
                .contains(secret)
        );
    }

    #[test]
    fn malformed_resource_diagnostics_do_not_disclose_values_or_invalid_keys()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let secret = "STATE_SECRET_SENTINEL";
        let address = ResourceAddress::new("random_pet", "one");
        let resource = ManagedResource {
            address: address.clone(),
            provider: "random".into(),
            provider_source: "registry.example/random".into(),
            schema_version: 0,
            state: serde_json::json!({}),
            private: Vec::new(),
            dependencies: Vec::new(),
            sensitive_attributes: Vec::new(),
            tainted: false,
            identity: None,
            serial: 1,
            generation: uuid::Uuid::new_v4(),
        };
        let mut document = StateDocument::new(&path(), 1);
        for field in ["schema_version", "private", "dependencies", "generation"] {
            let mut value = serde_json::to_value(&resource)?;
            value[field] = serde_json::json!(secret);
            document.records.insert(address.to_string(), value);

            let Err(list_error) = document.resources() else {
                return Err(format!("invalid {field} decoded in the list path").into());
            };
            let Err(get_error) = document.resource(&address) else {
                return Err(format!("invalid {field} decoded in the single-record path").into());
            };
            for error in [list_error, get_error] {
                for rendered in [format!("{error}"), format!("{error:?}")] {
                    assert!(!rendered.contains(secret), "{rendered}");
                    assert!(rendered.contains("unknown resource"), "{rendered}");
                }
            }
            assert_eq!(document.addresses()?, vec![address.clone()]);
        }

        let invalid_address = ResourceAddress::new("invalid", secret);
        document.records.clear();
        document.records.insert(
            invalid_address.to_string(),
            serde_json::to_value(&resource)?,
        );
        let Err(list_error) = document.resources() else {
            return Err("invalid stored address decoded in the list path".into());
        };
        let Err(get_error) = document.resource(&invalid_address) else {
            return Err("invalid stored address decoded in the single-record path".into());
        };
        let Err(addresses_error) = document.addresses() else {
            return Err("invalid stored address was silently omitted".into());
        };
        for error in [list_error, get_error, addresses_error] {
            for rendered in [format!("{error}"), format!("{error:?}")] {
                assert!(!rendered.contains(secret), "{rendered}");
                assert!(rendered.contains("unknown resource"), "{rendered}");
            }
        }

        document.records.clear();
        let mut value = serde_json::to_value(&resource)?;
        value["address"]["name"] = serde_json::json!(secret);
        document.records.insert(address.to_string(), value);
        assert_eq!(document.addresses()?, vec![address.clone()]);
        let Err(list_error) = document.resources() else {
            return Err("mismatched address decoded in the list path".into());
        };
        let Err(get_error) = document.resource(&address) else {
            return Err("mismatched address decoded in the single-record path".into());
        };
        for error in [list_error, get_error] {
            for rendered in [format!("{error}"), format!("{error:?}")] {
                assert!(!rendered.contains(secret), "{rendered}");
                assert!(rendered.contains("does not match its key"), "{rendered}");
            }
        }

        let secret_address = ResourceAddress::new("random_pet", secret);
        document.records.clear();
        document
            .records
            .insert(secret_address.to_string(), serde_json::json!({}));
        for error in [
            document.resources().unwrap_err(),
            document.resource(&secret_address).unwrap_err(),
        ] {
            for rendered in [format!("{error}"), format!("{error:?}")] {
                assert!(!rendered.contains(secret), "{rendered}");
                assert!(rendered.contains("unknown resource"), "{rendered}");
            }
        }
        Ok(())
    }

    #[test]
    fn state_decode_preserves_reserved_keys_in_records_and_refuses_lossy_extensions() {
        let mut document = StateDocument::new(&path(), 1);
        let resource = ManagedResource {
            address: ResourceAddress::new("random_pet", "one"),
            provider: "random".into(),
            provider_source: "registry.example/random".into(),
            schema_version: 0,
            state: serde_json::json!({"$serde_json::private::RawValue": "42", "nested": {"$serde_json::private::Number": "9"}}),
            private: Vec::new(),
            dependencies: Vec::new(),
            sensitive_attributes: Vec::new(),
            tainted: false,
            identity: None,
            serial: 1,
            generation: uuid::Uuid::new_v4(),
        };
        document.store(&resource).unwrap();
        let decoded = StateDocument::decode(&document.encode().unwrap(), &path(), "key").unwrap();
        assert_eq!(decoded.resource(&resource.address).unwrap(), Some(resource));
        document.extra.insert(
            "future".into(),
            serde_json::json!({"$serde_json::private::RawValue": "42"}),
        );
        assert!(StateDocument::decode(&document.encode().unwrap(), &path(), "key").is_err());
    }

    #[test]
    fn state_decode_refuses_rounding_and_duplicate_keys() {
        let mut document = StateDocument::new(&path(), 1);
        document
            .extra
            .insert("future".into(), serde_json::json!("NUMBER_PLACEHOLDER"));
        let encoded = String::from_utf8(document.encode().unwrap()).unwrap();
        let lossy = encoded.replace("\"NUMBER_PLACEHOLDER\"", "18446744073709551617");
        assert!(StateDocument::decode(lossy.as_bytes(), &path(), "key").is_err());
        let exact = encoded.replace("\"NUMBER_PLACEHOLDER\"", "18446744073709551615");
        assert!(StateDocument::decode(exact.as_bytes(), &path(), "key").is_ok());
        let duplicate = encoded.replacen("\"serial\":1", "\"serial\":1,\"serial\":2", 1);
        assert_ne!(duplicate, encoded);
        assert!(StateDocument::decode(duplicate.as_bytes(), &path(), "key").is_err());
    }

    #[test]
    fn newer_formats_and_other_tenants_are_refused() {
        let mut document = StateDocument::new(&path(), 1);
        document.format.read = FORMAT_READ + 1;
        let error = StateDocument::decode(&document.encode().unwrap(), &path(), "k").unwrap_err();
        assert!(error.to_string().contains("newer cuenv to read"), "{error}");

        let mut document = StateDocument::new(&path(), 1);
        document.format.write = FORMAT_WRITE + 1;
        let decoded = StateDocument::decode(&document.encode().unwrap(), &path(), "k").unwrap();
        let error = decoded.require_writable("k").unwrap_err();
        assert!(
            error.to_string().contains("newer cuenv to write"),
            "{error}"
        );

        let other = TenantPath::try_new(
            None,
            "cuenv",
            &TenantKey::try_new("github.com/acme/infra", "api").unwrap(),
        )
        .unwrap();
        let document = StateDocument::new(&other, 1);
        let error = StateDocument::decode(&document.encode().unwrap(), &path(), "k").unwrap_err();
        assert!(error.to_string().contains("another tenant"), "{error}");
    }

    #[test]
    fn advancing_changes_serial_and_writer_but_never_the_format() {
        let mut document = StateDocument::new(&path(), 41);
        document.format.write = 1;
        document.advance("nonce").expect("serial has headroom");
        assert_eq!(document.serial, 42);
        assert_eq!(document.writer, "nonce");
        assert_eq!(
            document.format,
            Format {
                read: FORMAT_READ,
                write: 1,
                extra: Map::new(),
            }
        );
    }

    #[test]
    fn advancing_an_exhausted_serial_leaves_the_document_unchanged() {
        let mut document = StateDocument::new(&path(), u64::MAX);
        let before = document.clone();
        assert!(document.advance("new writer").is_err());
        assert_eq!(document, before);
    }
}
