//! Managed resource records: their addresses, versions and conditional writes.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::base64_bytes;

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

    /// Parse a `type.name` address. The type must be a lowercase,
    /// provider-prefixed resource type such as `cloudflare_zone`, and the
    /// name must start with a letter and use only letters, digits, `-` and
    /// `_`. Returns `None` for anything else.
    #[must_use]
    pub fn parse(address: &str) -> Option<Self> {
        let (resource_type, name) = address.split_once('.')?;
        let (prefix, _) = resource_type.split_once('_')?;
        if prefix.is_empty()
            || !resource_type.starts_with(|character: char| character.is_ascii_lowercase())
            || resource_type.split('_').any(|part| {
                part.is_empty()
                    || !part.chars().all(|character| {
                        character.is_ascii_lowercase() || character.is_ascii_digit()
                    })
            })
            || !name.starts_with(|character: char| character.is_ascii_alphabetic())
            || !name.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
        {
            return None;
        }
        Some(Self::new(resource_type, name))
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
    /// Top-level attribute names whose values are effectively sensitive,
    /// including sensitivity inherited through output references. Never values.
    #[serde(default)]
    pub sensitive_attributes: Vec<String>,
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
    /// Identity of this insertion, retained on updates and renewed after
    /// deletion. Nil on a record that has not been stored yet; the store
    /// assigns one on insertion.
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
}

impl fmt::Display for RecordVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => formatter.write_str("no record"),
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
                RecordVersion::Generation { generation, serial } => {
                    stored.generation == generation && stored.serial == serial.saturating_add(1)
                }
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_provider_prefixed_type_name_addresses() {
        assert_eq!(
            ResourceAddress::parse("cloudflare_dns_record.www-1"),
            Some(ResourceAddress::new("cloudflare_dns_record", "www-1"))
        );
        for address in [
            "a.b.c",
            "zone.primary",
            "cloudflare_zone",
            "cloudflare_zone.primary.extra",
            "Cloudflare_zone.primary",
        ] {
            assert_eq!(ResourceAddress::parse(address), None, "{address}");
        }
    }

    #[test]
    fn record_versions_advance_with_every_write() {
        let generation = uuid::Uuid::new_v4();
        let stored = ManagedResource {
            address: ResourceAddress::new("random_pet", "pet"),
            provider: "random".into(),
            provider_source: "registry.terraform.io/hashicorp/random".into(),
            schema_version: 0,
            state: serde_json::json!({}),
            private: Vec::new(),
            dependencies: Vec::new(),
            sensitive_attributes: Vec::new(),
            tainted: false,
            identity: None,
            serial: 4,
            generation,
        };
        assert_eq!(RecordVersion::of(None), RecordVersion::Absent);
        let version = RecordVersion::of(Some(&stored));
        assert_eq!(
            version,
            RecordVersion::Generation {
                generation,
                serial: 4
            }
        );
        assert_eq!(
            version.to_string(),
            format!("generation {generation}, serial 4")
        );
        // A retry finds its own earlier write only at the next serial of the
        // same insertion.
        let next = ManagedResource {
            serial: 5,
            ..stored.clone()
        };
        let put = ConditionalPut {
            resource: &stored,
            expected: version,
        };
        assert!(put.is_recorded(&next));
        assert!(!put.is_recorded(&stored));
        assert_eq!(
            serde_json::to_value(RecordVersion::Absent).unwrap(),
            serde_json::json!("absent")
        );
    }
}
