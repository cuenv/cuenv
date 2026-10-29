//! Sensitivity survives stored state and provider schema migrations.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{InfrastructureError, Result};
use crate::protocol::{ApplyResourceChangeResponse, DynamicValue};
use crate::schema::Schema;
use crate::state::{ConditionalPut, ManagedResource, RecordVersion, StateLock};
use crate::type_system::{Type, Value};

use super::{
    DeclarationGraph, InfrastructureEngine, ResourceAddress, ResourceChange, SourceAddress,
    applied_value, data_source_schema, data_sources, masked_attributes, provider_value,
    register_attributes, resource_schema,
};

pub(super) struct ImportSensitivity<'input> {
    pub(super) address: &'input ResourceAddress,
    pub(super) stored: &'input [ManagedResource],
}

pub(super) struct ApplyResponseValue<'response> {
    pub(super) response: &'response ApplyResourceChangeResponse,
    pub(super) value_type: &'response Type,
}

pub(super) struct RetainedSensitivity<'operation> {
    pub(super) change: &'operation ResourceChange,
    pub(super) lock: &'operation StateLock,
    pub(super) rows: &'operation mut BTreeMap<ResourceAddress, ManagedResource>,
    pub(super) expected: RecordVersion,
}

impl InfrastructureEngine {
    /// A malformed response cannot supply trustworthy provider state. Before
    /// aborting, preserve newly learned marks on the old record so a later
    /// refresh cannot expose values copied by this possibly successful update.
    pub(super) async fn decode_apply_response(
        &self,
        input: ApplyResponseValue<'_>,
        retained: RetainedSensitivity<'_>,
    ) -> Result<Value> {
        let sensitive = &retained.change.sensitive;
        let decoded = if sensitive.is_empty() {
            applied_value(
                input.response,
                input.value_type,
                &format!("apply {}", retained.change.address),
            )
        } else {
            sensitive_provider_value(SensitiveProviderValue {
                value: input.response.new_state.as_ref(),
                value_type: input.value_type,
                sensitive,
            })
            .map(|(value, _)| value)
        };
        if decoded.is_err() {
            self.retain_failed_sensitivity(retained).await?;
        }
        decoded
    }

    pub(super) async fn retain_failed_sensitivity(
        &self,
        input: RetainedSensitivity<'_>,
    ) -> Result<()> {
        if let Some(stored) = input.rows.get(&input.change.address)
            && let Some(record) = retained_sensitive_record(stored, &input.change.sensitive)
        {
            self.record_change(
                input.lock,
                &ConditionalPut {
                    resource: &record,
                    expected: input.expected,
                },
            )
            .await?;
            input.rows.insert(input.change.address.clone(), record);
        }
        Ok(())
    }

    /// Import does not execute lookups, but its record still needs the same
    /// transitive sensitivity as a normal plan, including data-to-data chains.
    pub(super) async fn import_sensitivity(
        &mut self,
        input: ImportSensitivity<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<String>> {
        self.output_sensitivity.clear();
        let graph = DeclarationGraph::new(&self.infrastructure)?;
        let target = SourceAddress::Resource(input.address.clone());
        let mut needed = BTreeSet::new();
        let mut pending = vec![target.clone()];
        while let Some(source) = pending.pop() {
            if needed.insert(source.clone())
                && let Some(references) = graph.references.get(&source)
            {
                pending.extend(
                    references
                        .references()
                        .iter()
                        .map(|reference| reference.source.clone()),
                );
            }
        }
        for source in graph.order.iter().filter(|source| needed.contains(*source)) {
            let declaration =
                data_sources::declaration(&self.infrastructure, source).ok_or_else(|| {
                    InfrastructureError::configuration(format!(
                        "import source {source} is not declared"
                    ))
                })?;
            let provider_name = declaration.provider_name().to_string();
            self.ensure_provider(&provider_name, warnings).await?;
            let provider = self.loaded(&provider_name)?;
            let schema = match source {
                SourceAddress::Resource(address) => {
                    resource_schema(provider, &provider_name, &address.resource_type)?
                }
                SourceAddress::DataSource(address) => {
                    data_source_schema(provider, &address.resource_type)?
                }
            };
            let references = graph.references.get(source).cloned().unwrap_or_default();
            let mut sensitive = masked_attributes(
                &schema.block,
                self.validate_output_references(source.address(), &references, &schema.block)?,
            );
            if let SourceAddress::Resource(address) = source
                && let Some(record) = input
                    .stored
                    .iter()
                    .find(|record| &record.address == address)
            {
                sensitive.extend(resource_sensitivity(record, schema));
                sensitive.sort();
                sensitive.dedup();
                register_stored_values(record, &sensitive);
            }
            self.output_sensitivity.insert(source.clone(), sensitive);
        }
        self.output_sensitivity
            .get(&target)
            .cloned()
            .ok_or_else(|| {
                InfrastructureError::configuration(format!(
                    "import target {} is not declared",
                    input.address
                ))
            })
    }
}

/// Current schema flags plus historical marks. A schema migration supplies no
/// attribute rename map, so every attribute of its result inherits the marks.
/// Missing historical names need the same protection even if a provider kept
/// its schema version unchanged when renaming an attribute.
pub(super) fn resource_sensitivity(row: &ManagedResource, schema: &Schema) -> Vec<String> {
    let mut names: BTreeSet<String> = schema
        .block
        .sensitive_attributes()
        .into_iter()
        .chain(row.sensitive_attributes.iter().cloned())
        .collect();
    let unmapped = row.sensitive_attributes.iter().any(|name| {
        !schema.block.attributes.contains_key(name) && !schema.block.blocks.contains_key(name)
    });
    if !row.sensitive_attributes.is_empty() && (row.schema_version != schema.version || unmapped) {
        names.extend(
            schema
                .block
                .attributes
                .keys()
                .chain(schema.block.blocks.keys())
                .cloned(),
        );
    }
    names.into_iter().collect()
}

/// A provider returned no usable state on failure. Keep the old schema and
/// bytes, adding only effective marks if they have not yet been persisted.
pub(super) fn retained_sensitive_record(
    row: &ManagedResource,
    sensitive: &[String],
) -> Option<ManagedResource> {
    let sensitive = super::remembered_sensitive_attributes(sensitive, Some(row));
    if sensitive == row.sensitive_attributes {
        return None;
    }
    let mut retained = row.clone();
    retained.sensitive_attributes = sensitive;
    Some(retained)
}

/// Register raw JSON before attempting a decode against a potentially newer
/// schema. Historical names must still protect values the new schema cannot see.
pub(super) fn register_stored_values(row: &ManagedResource, names: &[String]) {
    for name in names.iter().chain(&row.sensitive_attributes) {
        if let Some(value) = row.state.get(name) {
            register_json_strings(value);
        }
    }
}

fn register_json_strings(value: &serde_json::Value) {
    match value {
        serde_json::Value::String(text) => cuenv_events::register_secret(text.clone()),
        serde_json::Value::Array(values) => values.iter().for_each(register_json_strings),
        serde_json::Value::Object(values) => {
            for (name, value) in values {
                cuenv_events::register_secret(name.clone());
                register_json_strings(value);
            }
        }
        _ => {}
    }
}

#[derive(Clone, Copy)]
pub(super) struct SensitiveProviderValue<'value> {
    pub(super) value: Option<&'value DynamicValue>,
    pub(super) value_type: &'value Type,
    pub(super) sensitive: &'value [String],
}

/// Decode and register before diagnostics are rendered. If any field fails to
/// decode, even an unrelated nonsensitive one, no unrestricted diagnostic text
/// is safe: it can echo a sensitive sibling that was never registered.
pub(super) fn sensitive_provider_value(
    input: SensitiveProviderValue<'_>,
) -> Result<(Value, Vec<u8>)> {
    let decoded = provider_value(input.value, input.value_type).map_err(|_| {
        // A schema mismatch in an unrelated field need not stop us protecting
        // well-formed sensitive siblings. This is best effort only: malformed
        // wire data can still prevent extraction, so diagnostics stay withheld.
        register_raw_provider_values(input);
        InfrastructureError::plugin("provider returned an undecodable value; diagnostics withheld because response values could be sensitive")
    })?;
    register_attributes(&decoded.0, input.sensitive);
    Ok(decoded)
}

fn register_raw_provider_values(input: SensitiveProviderValue<'_>) {
    let Some(value) = input.value else { return };
    if !value.message_pack.is_empty() {
        if let Ok(rmpv::Value::Map(attributes)) =
            rmpv::decode::read_value(&mut value.message_pack.as_slice())
        {
            for (name, value) in attributes {
                if name
                    .as_str()
                    .is_some_and(|name| input.sensitive.iter().any(|marked| marked == name))
                {
                    register_message_pack_strings(&value);
                }
            }
        }
    } else if let Ok(serde_json::Value::Object(attributes)) = serde_json::from_slice(&value.json) {
        for name in input.sensitive {
            if let Some(value) = attributes.get(name) {
                register_json_strings(value);
            }
        }
    }
}

fn register_message_pack_strings(value: &rmpv::Value) {
    match value {
        rmpv::Value::String(text) => {
            if let Some(text) = text.as_str() {
                cuenv_events::register_secret(text.to_string());
            }
        }
        rmpv::Value::Array(values) => values.iter().for_each(register_message_pack_strings),
        rmpv::Value::Map(values) => {
            for (name, value) in values {
                register_message_pack_strings(name);
                register_message_pack_strings(value);
            }
        }
        _ => {}
    }
}
