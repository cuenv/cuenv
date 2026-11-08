//! Resource identity: the provider's versioned, permanent description of
//! which remote object a record is.
//!
//! Providers that declare an identity schema (`GetResourceIdentitySchemas`)
//! return identity data from read, plan, apply and import, and expect the
//! stored identity back on later calls. cuenv persists it in each record as
//! `{"version": <identity schema version>, "data": <cty JSON>}`, sends it back
//! as MessagePack against the identity schema, and upgrades data written
//! against an older identity schema version with `UpgradeResourceIdentity`.
//!
//! Identity is never a secret and never shapes a plan. Identity data for a
//! type without an identity schema is refused as a provider defect (after an
//! apply, the change is still recorded and a warning keeps the earlier
//! identity).

use std::collections::BTreeMap;

use crate::error::{InfrastructureError, Result};
use crate::plugin::ProviderClient;
use crate::protocol::{self, ResourceIdentityData};
use crate::type_system::{self, Type};

/// One resource type's identity schema.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct IdentitySchema {
    /// The identity schema version, independent of the resource schema's.
    pub(super) version: i64,
    /// The object type identity data is encoded against.
    pub(super) value_type: Type,
}

/// Identity schemas of a provider, by resource type.
pub(super) type IdentitySchemas = BTreeMap<String, IdentitySchema>;

/// Fetch and decode a provider's identity schemas.
///
/// # Errors
///
/// Returns an error when the RPC fails, the provider reports errors, or a
/// schema names a malformed type.
pub(super) async fn fetch(client: &ProviderClient) -> Result<IdentitySchemas> {
    let response = client.resource_identity_schemas().await?;
    if let Some(error) = response
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.severity != protocol::Severity::Warning as i32)
    {
        return Err(InfrastructureError::plugin(format!(
            "the provider's identity schemas could not be read: {}",
            error.summary
        )));
    }
    tracing::debug!(
        types = ?response.identity_schemas.keys().collect::<Vec<_>>(),
        "provider identity schemas"
    );
    response
        .identity_schemas
        .into_iter()
        .map(|(type_name, schema)| {
            let attributes = schema
                .identity_attributes
                .iter()
                .map(|attribute| {
                    Type::from_json_bytes(&attribute.r#type)
                        .map(|attribute_type| (attribute.name.clone(), attribute_type))
                        .map_err(|error| {
                            InfrastructureError::plugin(format!(
                                "the identity schema of {type_name} has an invalid type for \
                                 {}: {error}",
                                attribute.name
                            ))
                        })
                })
                .collect::<Result<BTreeMap<_, _>>>()?;
            Ok((
                type_name,
                IdentitySchema {
                    version: schema.version,
                    value_type: Type::Object(attributes),
                },
            ))
        })
        .collect()
}

/// Identity data a provider returned, checked against `schema` and
/// re-encoded as MessagePack for later calls. `None` when there is none.
///
/// # Errors
///
/// Returns [`InfrastructureError::Plugin`] when the data does not match the
/// identity schema, or arrives for a type without one.
pub(super) fn received(
    schema: Option<&IdentitySchema>,
    data: Option<&ResourceIdentityData>,
    context: &str,
) -> Result<Option<Vec<u8>>> {
    let Some(value) = data.and_then(|data| data.identity_data.as_ref()) else {
        return Ok(None);
    };
    if value.message_pack.is_empty() && value.json.is_empty() {
        return Ok(None);
    }
    // Identity data is decoded against its schema; data for a type without
    // one is a provider defect, not something to store or drop silently.
    let Some(schema) = schema else {
        return Err(InfrastructureError::plugin(format!(
            "{context}: the provider returned identity data without declaring an identity \
             schema for the resource type"
        )));
    };
    let decoded = if value.message_pack.is_empty() {
        type_system::from_json_bytes(&value.json, &schema.value_type)
    } else {
        type_system::from_message_pack(&value.message_pack, &schema.value_type)
    }
    .map_err(|error| {
        InfrastructureError::plugin(format!(
            "{context}: the provider returned identity data that does not match its identity \
             schema: {error}"
        ))
    })?;
    if decoded.is_null() {
        return Ok(None);
    }
    type_system::to_message_pack(&decoded, &schema.value_type).map(Some)
}

/// Planned identity a provider returned, passed back verbatim on apply: it
/// may hold unknown values the apply resolves.
pub(super) fn planned(
    schema: Option<&IdentitySchema>,
    data: Option<&ResourceIdentityData>,
) -> Option<Vec<u8>> {
    schema?;
    let value = data?.identity_data.as_ref()?;
    (!value.message_pack.is_empty()).then(|| value.message_pack.clone())
}

/// The stored form of identity bytes received for `schema`.
///
/// # Errors
///
/// Returns [`InfrastructureError::Codec`] when the bytes do not decode.
pub(super) fn to_stored(
    schema: Option<&IdentitySchema>,
    bytes: Option<&[u8]>,
) -> Result<Option<serde_json::Value>> {
    let (Some(schema), Some(bytes)) = (schema, bytes) else {
        return Ok(None);
    };
    let value = type_system::from_message_pack(bytes, &schema.value_type)?;
    Ok(Some(serde_json::json!({
        "version": schema.version,
        "data": value.to_state_json(&schema.value_type)?,
    })))
}

/// What [`from_stored`] needs.
pub(super) struct StoredIdentity<'input> {
    pub(super) client: &'input ProviderClient,
    pub(super) type_name: &'input str,
    pub(super) schema: Option<&'input IdentitySchema>,
    pub(super) stored: Option<&'input serde_json::Value>,
}

/// A stored identity as MessagePack against the current identity schema,
/// upgraded by the provider when it was written against another version.
/// `None` when nothing is stored or the type no longer has an identity.
///
/// # Errors
///
/// Returns an error when the stored identity is malformed, or the provider
/// cannot upgrade it.
pub(super) async fn from_stored(input: StoredIdentity<'_>) -> Result<Option<Vec<u8>>> {
    let StoredIdentity {
        client,
        type_name,
        schema,
        stored,
    } = input;
    let (Some(schema), Some(stored)) = (schema, stored) else {
        return Ok(None);
    };
    let malformed = || {
        InfrastructureError::codec(format!(
            "the stored identity of a {type_name} record is not a versioned identity object"
        ))
    };
    let version = stored
        .get("version")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(malformed)?;
    let data = stored.get("data").ok_or_else(malformed)?;
    let data_json = serde_json::to_vec(data)
        .map_err(|error| InfrastructureError::codec(format!("cannot encode identity: {error}")))?;
    if version == schema.version {
        let value = type_system::from_json_bytes(&data_json, &schema.value_type)?;
        return type_system::to_message_pack(&value, &schema.value_type).map(Some);
    }
    if version > schema.version {
        return Err(InfrastructureError::plugin(format!(
            "a {type_name} record holds identity version {version}, newer than the provider's \
             identity schema version {}; upgrade the provider",
            schema.version
        )));
    }
    let response = client
        .upgrade_resource_identity(type_name, version, data_json)
        .await?;
    if let Some(error) = response
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.severity != protocol::Severity::Warning as i32)
    {
        return Err(InfrastructureError::plugin(format!(
            "upgrade identity of {type_name} from version {version}: {}",
            error.summary
        )));
    }
    received(
        Some(schema),
        response.upgraded_identity.as_ref(),
        &format!("upgrade identity of {type_name}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> IdentitySchema {
        IdentitySchema {
            version: 2,
            value_type: Type::Object(BTreeMap::from([
                ("id".to_string(), Type::String),
                ("region".to_string(), Type::String),
            ])),
        }
    }

    fn identity(json: &str) -> ResourceIdentityData {
        ResourceIdentityData {
            identity_data: Some(protocol::DynamicValue {
                message_pack: Vec::new(),
                json: json.as_bytes().to_vec(),
            }),
        }
    }

    #[test]
    fn received_identity_round_trips_through_the_stored_form() {
        let schema = schema();
        let bytes = received(
            Some(&schema),
            Some(&identity(r#"{"id":"i-1","region":"eu-west-1"}"#)),
            "read",
        )
        .unwrap()
        .unwrap();
        let stored = to_stored(Some(&schema), Some(&bytes)).unwrap().unwrap();
        assert_eq!(
            stored,
            serde_json::json!({"version": 2, "data": {"id": "i-1", "region": "eu-west-1"}})
        );
    }

    #[test]
    fn identity_without_a_schema_and_mismatches_are_refused() {
        let data = identity(r#"{"id":"i-1","region":"eu-west-1"}"#);
        let error = received(None, Some(&data), "read x")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("without declaring an identity schema"),
            "{error}"
        );
        assert_eq!(received(None, None, "read").unwrap(), None);
        assert_eq!(to_stored(None, Some(b"\x80")).unwrap(), None);
        let error = received(
            Some(&schema()),
            Some(&identity(r#"{"id":["x"],"region":"r"}"#)),
            "apply x",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("apply x"), "{error}");
        assert!(error.contains("identity schema"), "{error}");
    }
}
