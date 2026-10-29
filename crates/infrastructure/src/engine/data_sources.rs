//! Invocation-local data reads and the mixed declaration graph.
//!
//! Read nodes are composed with the managed operation schedule. Every resource
//! carries the managed ancestors of its data dependencies, so a read is ready
//! before its consumer's apply node. No read enters a state-store write path.

use std::collections::{BTreeMap, BTreeSet};

use cuenv_manifest::manifest::{Infrastructure, ManagedResourceDeclaration};

use crate::error::{ChangeFailure, InfrastructureError, Result};
use crate::protocol;
use crate::references::{ConfigurationReferences, ResourceOutputReference, SourceAddress};
use crate::schema::Schema;
use crate::state::{ManagedResource, ResourceAddress};
use crate::type_system::{self, Type, Value};

use super::sensitivity::{SensitiveProviderValue, sensitive_provider_value};
use super::{
    Action, ApplyEvent, DataSourceReadContext, DigestWriter, InfrastructureEngine, LoadedProvider,
    OperationFailure, OperationRunner, Plan, ReferenceValueInput, check_diagnostics,
    declared_resource, masked_attributes, reference_error, register_value_strings,
    topological_order,
};

/// One data source read by this invocation, or deferred until apply.
#[derive(Debug, Clone)]
pub struct DataSourcePlan {
    /// Provider type and declaration name; render with a `data.` prefix.
    pub address: ResourceAddress,
    /// Local provider name.
    pub provider: String,
    /// Read result, or an object of unknown attributes for a deferred read.
    pub after: Value,
    /// Top-level attributes to mask in every presentation of the result.
    pub sensitive: Vec<String>,
    /// Whether the provider read must wait for apply-time prerequisites.
    pub deferred: bool,
    pub(super) configuration: Value,
    pub(super) references: ConfigurationReferences,
    pub(super) dependencies: Vec<SourceAddress>,
}

impl DataSourcePlan {
    pub(super) fn digest(&self, digest: &mut DigestWriter) {
        digest.field(self.address.to_string().as_bytes());
        digest.field(self.provider.as_bytes());
        digest.tag(u8::from(self.deferred));
        digest.value(&self.after);
        digest.value(&self.configuration);
        digest.count(self.sensitive.len());
        for name in &self.sensitive {
            digest.field(name.as_bytes());
        }
        digest.count(self.dependencies.len());
        for dependency in &self.dependencies {
            digest.field(dependency.to_string().as_bytes());
        }
        digest.count(self.references.references().len());
        for reference in self.references.references() {
            digest.field(reference.source.to_string().as_bytes());
            digest.field(reference.attribute.as_bytes());
            digest.field(reference.destination_attribute.as_bytes());
        }
    }
}

pub(super) struct DeclarationGraph {
    pub(super) references: BTreeMap<SourceAddress, ConfigurationReferences>,
    pub(super) dependencies: BTreeMap<SourceAddress, Vec<SourceAddress>>,
    pub(super) order: Vec<SourceAddress>,
}

pub(super) fn declaration<'declaration>(
    infrastructure: &'declaration Infrastructure,
    source: &SourceAddress,
) -> Option<&'declaration ManagedResourceDeclaration> {
    let declarations = match source {
        SourceAddress::Resource(_) => &infrastructure.resources,
        SourceAddress::DataSource(_) => &infrastructure.data,
    };
    declared_resource(declarations, source.address())
}

impl DeclarationGraph {
    pub(super) fn new(infrastructure: &Infrastructure) -> Result<Self> {
        let mut references = BTreeMap::new();
        let mut dependencies = BTreeMap::new();
        for (source, declared) in infrastructure
            .resources
            .iter()
            .map(|(name, declared)| {
                (
                    SourceAddress::Resource(ResourceAddress::new(&declared.resource_type, name)),
                    declared,
                )
            })
            .chain(infrastructure.data.iter().map(|(name, declared)| {
                (
                    SourceAddress::DataSource(ResourceAddress::new(&declared.resource_type, name)),
                    declared,
                )
            }))
        {
            let parsed = ConfigurationReferences::extract(&declared.configuration)?;
            let mut prerequisites = BTreeSet::new();
            for name in &declared.depends_on {
                let managed = infrastructure.resources.get(name).ok_or_else(|| {
                    InfrastructureError::configuration(format!(
                        "{source} depends on unknown managed resource '{name}'"
                    ))
                })?;
                prerequisites.insert(SourceAddress::Resource(ResourceAddress::new(
                    &managed.resource_type,
                    name,
                )));
            }
            for reference in parsed.references() {
                if declaration(infrastructure, &reference.source).is_none() {
                    return Err(InfrastructureError::configuration(format!(
                        "{source} configuration.{} references undeclared source {}",
                        reference.destination_attribute, reference.source
                    )));
                }
                prerequisites.insert(reference.source.clone());
            }
            references.insert(source.clone(), parsed);
            dependencies.insert(source, prerequisites.into_iter().collect::<Vec<_>>());
        }
        let names: BTreeMap<String, SourceAddress> = dependencies
            .keys()
            .map(|source| (source.to_string(), source.clone()))
            .collect();
        let graph = dependencies
            .iter()
            .map(|(source, dependencies)| {
                (
                    source.to_string(),
                    dependencies.iter().map(ToString::to_string).collect(),
                )
            })
            .collect();
        let order = topological_order(&graph)?
            .into_iter()
            .filter_map(|name| names.get(&name).cloned())
            .collect();
        Ok(Self {
            references,
            dependencies,
            order,
        })
    }

    /// Collapse data nodes for persisted dependencies and managed scheduling.
    pub(super) fn managed_dependencies(&self, source: &SourceAddress) -> Vec<String> {
        let mut managed = BTreeSet::new();
        for prerequisite in self.dependencies.get(source).into_iter().flatten() {
            match prerequisite {
                SourceAddress::Resource(address) => {
                    managed.insert(address.to_string());
                }
                SourceAddress::DataSource(_) => {
                    for ancestor in self.read_dependencies(prerequisite) {
                        if let SourceAddress::Resource(address) = ancestor {
                            managed.insert(address.to_string());
                        }
                    }
                }
            }
        }
        managed.into_iter().collect()
    }

    /// A no-op intermediate resource still orders a read after its changing
    /// ancestors. Include those ancestors explicitly in read readiness.
    fn read_dependencies(&self, source: &SourceAddress) -> Vec<SourceAddress> {
        let mut visited = BTreeSet::new();
        let mut pending = self.dependencies.get(source).cloned().unwrap_or_default();
        while let Some(prerequisite) = pending.pop() {
            if visited.insert(prerequisite.clone()) {
                pending.extend(
                    self.dependencies
                        .get(&prerequisite)
                        .into_iter()
                        .flatten()
                        .cloned(),
                );
            }
        }
        visited.into_iter().collect()
    }
}

/// Results are scoped to one apply, and are never inserted into managed rows.
#[derive(Default)]
pub(super) struct DataSourceExecution {
    pub(super) values: BTreeMap<ResourceAddress, Value>,
    pub(super) failures: BTreeMap<ResourceAddress, String>,
    /// Only provider reads actually attempted and failed, in execution order.
    /// Entries in `failures` also include reads skipped by prerequisites.
    read_failures: Vec<ChangeFailure>,
    completed_resources: BTreeSet<ResourceAddress>,
    failed_resources: BTreeSet<ResourceAddress>,
}

impl DataSourceExecution {
    pub(super) fn new(plan: &Plan) -> Self {
        Self {
            values: plan
                .data_sources
                .iter()
                .filter(|read| !read.deferred)
                .map(|read| (read.address.clone(), read.after.clone()))
                .collect(),
            completed_resources: plan
                .changes
                .iter()
                .filter(|change| change.action == Action::NoOp)
                .map(|change| change.address.clone())
                .collect(),
            ..Self::default()
        }
    }

    pub(super) fn finished(&mut self, address: &ResourceAddress) {
        self.completed_resources.insert(address.clone());
    }

    pub(super) fn failed(&mut self, address: &ResourceAddress) {
        self.failed_resources.insert(address.clone());
    }

    fn dependency_failed(&self, source: &SourceAddress) -> bool {
        match source {
            SourceAddress::Resource(address) => self.failed_resources.contains(address),
            SourceAddress::DataSource(address) => self.failures.contains_key(address),
        }
    }

    fn dependency_ready(&self, source: &SourceAddress) -> bool {
        match source {
            SourceAddress::Resource(address) => self.completed_resources.contains(address),
            SourceAddress::DataSource(address) => self.values.contains_key(address),
        }
    }

    pub(super) fn consumer_blocked(&self, references: &ConfigurationReferences) -> bool {
        references.references().iter().any(|reference| {
            let SourceAddress::DataSource(address) = &reference.source else {
                return false;
            };
            self.failures.contains_key(address)
        })
    }

    pub(super) fn take_read_failures(&mut self) -> Vec<ChangeFailure> {
        std::mem::take(&mut self.read_failures)
    }
}

impl InfrastructureEngine {
    pub(super) async fn plan_data_source(
        &mut self,
        input: DataSourcePlanning<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<DataSourcePlan> {
        let declared = declaration(&self.infrastructure, input.source)
            .cloned()
            .ok_or_else(|| {
                InfrastructureError::configuration(format!("{} is not declared", input.source))
            })?;
        let provider_name = declared.provider_name().to_string();
        self.ensure_provider(&provider_name, warnings).await?;
        let provider = self.loaded(&provider_name)?;
        let schema = data_source_schema(provider, &input.source.address().resource_type)?;
        let references = input
            .graph
            .references
            .get(input.source)
            .cloned()
            .unwrap_or_default();
        let value_type = schema.block.implied_type();
        let configuration = references.prepare(&declared.configuration, &value_type)?;
        let sensitive_destinations =
            self.validate_output_references(input.source.address(), &references, &schema.block)?;
        let configuration = references.resolve_planned(&configuration, |reference| {
            planned_output(input.outputs, reference)
        })?;
        let configuration = schema.block.normalize_configuration(configuration);
        let sensitive = masked_attributes(&schema.block, sensitive_destinations);
        register_attributes(&configuration, &sensitive);
        self.ensure_not_stopped()?;
        let diagnostics = provider
            .client
            .validate_data_source_configuration(
                &declared.resource_type,
                type_system::to_message_pack(&configuration, &value_type)?,
            )
            .await?;
        check_diagnostics(
            &format!("validate {}", input.source),
            &diagnostics,
            warnings,
        )?;
        let dependencies = input.graph.read_dependencies(input.source);
        let deferred = read_is_deferred(&configuration, &dependencies, input.pending);
        let mut read = DataSourcePlan {
            address: input.source.address().clone(),
            provider: provider_name,
            after: unknown_outputs(&value_type),
            sensitive,
            deferred,
            configuration,
            references,
            dependencies,
        };
        if !deferred {
            read.after = self
                .read_data_source(&read, &read.configuration, warnings)
                .await?;
        }
        Ok(read)
    }

    async fn read_data_source(
        &self,
        read: &DataSourcePlan,
        configuration: &Value,
        warnings: &mut Vec<String>,
    ) -> Result<Value> {
        let provider = self.loaded(&read.provider)?;
        let schema = data_source_schema(provider, &read.address.resource_type)?;
        let value_type = schema.block.implied_type();
        register_attributes(configuration, &read.sensitive);
        self.ensure_not_stopped()?;
        let response = provider
            .client
            .read_data_source(
                &read.address.resource_type,
                type_system::to_message_pack(configuration, &value_type)?,
            )
            .await?;
        data_source_result(
            DataSourceResult {
                read,
                value_type: &value_type,
                response: &response,
            },
            warnings,
        )
    }

    /// Read every ready node in topological order. Failures remain local to
    /// dependents; managed operation failures continue through ScheduleRun.
    pub(super) async fn read_ready_data_sources(
        &self,
        runner: &(impl OperationRunner + Sync),
        input: ReadyDataSources<'_>,
    ) -> Result<()> {
        for read in input.plans.iter().filter(|read| read.deferred) {
            if input.execution.values.contains_key(&read.address)
                || input.execution.failures.contains_key(&read.address)
            {
                continue;
            }
            if read
                .dependencies
                .iter()
                .any(|source| input.execution.dependency_failed(source))
            {
                input.execution.failures.insert(
                    read.address.clone(),
                    "a prerequisite failed or was skipped".to_string(),
                );
                continue;
            }
            if !read
                .dependencies
                .iter()
                .all(|source| input.execution.dependency_ready(source))
            {
                continue;
            }
            self.ensure_not_stopped()?;
            let result = runner
                .run_data_source(
                    read,
                    DataSourceReadContext {
                        rows: input.rows,
                        values: &input.execution.values,
                    },
                )
                .await;
            match result {
                Ok((value, warnings)) => {
                    input.execution.values.insert(read.address.clone(), value);
                    for warning in warnings {
                        (input.on_event)(ApplyEvent::Warning(warning));
                    }
                }
                Err(error) => {
                    if self.cancellation().is_stop_requested() {
                        return Err(error);
                    }
                    let failure = OperationFailure::from_reference_replan(error);
                    if !failure.continues() {
                        return Err(failure.into_error());
                    }
                    let error = failure.into_error();
                    let message = error.to_string();
                    (input.on_event)(ApplyEvent::Warning(format!(
                        "read data.{} failed: {message}",
                        read.address
                    )));
                    input
                        .execution
                        .failures
                        .insert(read.address.clone(), message);
                    input.execution.read_failures.push(ChangeFailure {
                        // ChangeFailure predates source kinds. This address is
                        // display-only error metadata, never a graph identity
                        // or a managed-state key. Keep the namespace visible.
                        address: ResourceAddress::new(
                            format!("data.{}", read.address.resource_type),
                            &read.address.name,
                        ),
                        error,
                    });
                }
            }
        }
        Ok(())
    }

    pub(super) async fn resolve_and_read_data_source(
        &self,
        read: &DataSourcePlan,
        rows: &BTreeMap<ResourceAddress, ManagedResource>,
        values: &BTreeMap<ResourceAddress, Value>,
    ) -> Result<(Value, Vec<String>)> {
        let mut warnings = Vec::new();
        let mut outputs = BTreeMap::new();
        for reference in read.references.references() {
            let value = self
                .reference_output_value(
                    ReferenceValueInput {
                        destination: &read.address,
                        reference,
                        rows,
                        data_sources: values,
                    },
                    &mut warnings,
                )
                .await?;
            outputs.insert(reference.destination_attribute.clone(), value);
        }
        let configuration = read.references.resolve(&read.configuration, |reference| {
            outputs
                .get(&reference.destination_attribute)
                .cloned()
                .ok_or_else(|| reference_error(&read.address, reference, "was not resolved"))
        })?;
        let provider = self.loaded(&read.provider)?;
        let schema = data_source_schema(provider, &read.address.resource_type)?;
        let configuration = schema.block.normalize_configuration(configuration);
        register_attributes(&configuration, &read.sensitive);
        self.ensure_not_stopped()?;
        let diagnostics = provider
            .client
            .validate_data_source_configuration(
                &read.address.resource_type,
                type_system::to_message_pack(&configuration, &schema.block.implied_type())?,
            )
            .await?;
        check_diagnostics(
            &format!("validate data.{}", read.address),
            &diagnostics,
            &mut warnings,
        )?;
        let value = self
            .read_data_source(read, &configuration, &mut warnings)
            .await?;
        Ok((value, warnings))
    }
}

pub(super) struct DataSourcePlanning<'input> {
    pub(super) source: &'input SourceAddress,
    pub(super) graph: &'input DeclarationGraph,
    pub(super) outputs: &'input BTreeMap<SourceAddress, Value>,
    pub(super) pending: &'input BTreeSet<SourceAddress>,
}

pub(super) struct ReadyDataSources<'run> {
    pub(super) plans: &'run [DataSourcePlan],
    pub(super) execution: &'run mut DataSourceExecution,
    pub(super) rows: &'run BTreeMap<ResourceAddress, ManagedResource>,
    pub(super) on_event: &'run mut (dyn FnMut(ApplyEvent) + Send),
}

pub(super) fn data_source_schema<'provider>(
    provider: &'provider LoadedProvider,
    type_name: &str,
) -> Result<&'provider Schema> {
    provider.schema.data_sources.get(type_name).ok_or_else(|| {
        InfrastructureError::configuration(format!(
            "provider has no data source type '{type_name}'"
        ))
    })
}

pub(super) fn planned_output(
    outputs: &BTreeMap<SourceAddress, Value>,
    reference: &ResourceOutputReference,
) -> Result<Value> {
    outputs
        .get(&reference.source)
        .and_then(|value| value.attribute(&reference.attribute))
        .cloned()
        .ok_or_else(|| {
            InfrastructureError::configuration(format!(
                "{} output {} is absent from its planned result",
                reference.source, reference.attribute
            ))
        })
}

pub(super) fn register_attributes(value: &Value, sensitive: &[String]) {
    for name in sensitive {
        if let Some(value) = value.attribute(name) {
            register_value_strings(value);
        }
    }
}

fn unknown_outputs(value_type: &Type) -> Value {
    match value_type {
        Type::Object(attributes) => Value::Object(
            attributes
                .keys()
                .map(|name| (name.clone(), Value::Unknown))
                .collect(),
        ),
        _ => Value::Unknown,
    }
}

pub(super) fn read_is_deferred(
    configuration: &Value,
    dependencies: &[SourceAddress],
    pending: &BTreeSet<SourceAddress>,
) -> bool {
    configuration.contains_unknown() || dependencies.iter().any(|source| pending.contains(source))
}

#[derive(Clone, Copy)]
pub(super) struct DataSourceResult<'result> {
    pub(super) read: &'result DataSourcePlan,
    pub(super) value_type: &'result Type,
    pub(super) response: &'result protocol::ReadDataSourceResponse,
}

pub(super) fn data_source_result(
    input: DataSourceResult<'_>,
    warnings: &mut Vec<String>,
) -> Result<Value> {
    let DataSourceResult {
        read,
        value_type,
        response,
    } = input;
    // Register returned secrets before either warnings or errors can echo them.
    let (value, _) = sensitive_provider_value(SensitiveProviderValue {
        value: response.state.as_ref(),
        value_type,
        sensitive: &read.sensitive,
    })
    .map_err(|error| InfrastructureError::plugin(format!("read data.{}: {error}", read.address)))?;
    check_diagnostics(
        &format!("read data.{}", read.address),
        &response.diagnostics,
        warnings,
    )?;
    if response.deferred.is_some() {
        return Err(InfrastructureError::plugin(format!(
            "provider deferred data.{}; provider-requested deferral is unsupported",
            read.address
        )));
    }
    if value.is_null() || value.contains_unknown() {
        return Err(InfrastructureError::plugin(format!(
            "data.{} returned a null or unknown result",
            read.address
        )));
    }
    Ok(value)
}
