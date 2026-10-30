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
use crate::error::{InfrastructureError, Result, strip_control_characters};
use crate::state::{LastWriter, LockInformation, ManagedResource, ResourceAddress};
use crate::tenant::ProjectInstance;

/// The read format this build understands and writes.
pub const FORMAT_READ: u32 = 1;

/// The write format this build understands and writes.
pub const FORMAT_WRITE: u32 = 1;

/// The cuenv version written into `writtenBy`.
#[must_use]
pub fn written_by() -> String {
    format!("cuenv {}", env!("CARGO_PKG_VERSION"))
}

/// Format versions of a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Format {
    /// Readers refuse a newer read version.
    pub read: u32,
    /// Writers refuse a newer write version.
    pub write: u32,
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
}

/// A held lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

impl DocumentLock {
    /// The lock as the trait reports it; stored text is untrusted.
    #[must_use]
    pub fn information(&self) -> LockInformation {
        LockInformation {
            lock_identifier: strip_control_characters(&self.lock_identifier),
            holder: strip_control_characters(&self.holder),
            acquired_at: strip_control_characters(&self.acquired_at),
        }
    }
}

/// The instance that last took the lock to change state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentWriter {
    /// `<directory>:<package>`.
    pub instance: String,
    /// When (RFC 3339).
    pub at: String,
}

impl DocumentWriter {
    /// The writer as the trait reports it; stored text is untrusted.
    #[must_use]
    pub fn last_writer(&self) -> LastWriter {
        LastWriter {
            instance: ProjectInstance::from_stored(&self.instance),
            recorded_at: strip_control_characters(&self.at),
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
}

/// A tenant whose state moved elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tombstone {
    /// The key the state moved to. Untrusted: re-validated before display.
    pub moved_to: String,
}

/// One tenant's state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    #[serde(default)]
    pub records: BTreeMap<String, Value>,
    /// Fields a newer cuenv added.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl StateDocument {
    /// A new, empty document for `path`.
    #[must_use]
    pub fn new(path: &TenantPath, serial: u64) -> Self {
        Self {
            format: Format {
                read: FORMAT_READ,
                write: FORMAT_WRITE,
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
    pub fn decode(bytes: &[u8], path: &TenantPath, key: &str) -> Result<Self> {
        let document: Self = serde_json::from_slice(bytes).map_err(|error| {
            InfrastructureError::state(format!(
                "object {key} is not a cuenv state document: {error}"
            ))
        })?;
        if document.format.read > FORMAT_READ {
            return Err(InfrastructureError::state(format!(
                "object {key} needs a newer cuenv to read (format {}, written by {}; this build \
                 reads format {FORMAT_READ}); upgrade cuenv",
                document.format.read,
                strip_control_characters(&document.written_by)
            )));
        }
        if document.tenant != DocumentTenant::of(path) {
            return Err(InfrastructureError::state(format!(
                "object {key} belongs to another tenant ({}#{}); refusing a copied or planted \
                 state document",
                strip_control_characters(&document.tenant.module_path),
                strip_control_characters(&document.tenant.project)
            )));
        }
        Ok(document)
    }

    /// Refuse to write a document a newer cuenv wrote in a newer write format.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::State`] naming the version needed.
    pub fn require_writable(&self, key: &str) -> Result<()> {
        if self.format.write > FORMAT_WRITE {
            return Err(InfrastructureError::state(format!(
                "object {key} needs a newer cuenv to write (write format {}, written by {}; this \
                 build writes format {FORMAT_WRITE}); upgrade cuenv",
                self.format.write,
                strip_control_characters(&self.written_by)
            )));
        }
        Ok(())
    }

    /// Prepare the next version for writing: a new serial and writer, this
    /// build as `writtenBy`. The format numbers are kept: no ordinary write
    /// raises them.
    pub fn advance(&mut self, writer: &str) {
        self.serial = self.serial.saturating_add(1);
        self.writer = writer.to_string();
        self.written_by = written_by();
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
            .map(|(address, value)| {
                serde_json::from_value::<ManagedResource>(value.clone()).map_err(|error| {
                    InfrastructureError::UndecodableRecord {
                        address: strip_control_characters(address),
                        problem: error.to_string(),
                    }
                })
            })
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
            .map(|value| {
                serde_json::from_value::<ManagedResource>(value.clone()).map_err(|error| {
                    InfrastructureError::UndecodableRecord {
                        address: address.to_string(),
                        problem: error.to_string(),
                    }
                })
            })
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
    #[must_use]
    pub fn addresses(&self) -> Vec<ResourceAddress> {
        self.records
            .keys()
            .filter_map(|address| ResourceAddress::parse(address))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tenant::TenantKey;

    fn path() -> TenantPath {
        TenantPath::new(
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

        let other = TenantPath::new(
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
        document.advance("nonce");
        assert_eq!(document.serial, 42);
        assert_eq!(document.writer, "nonce");
        assert_eq!(
            document.format,
            Format {
                read: FORMAT_READ,
                write: FORMAT_WRITE
            }
        );
    }
}
