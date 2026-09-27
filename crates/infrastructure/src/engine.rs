//! Planning and applying managed resources.
//!
//! The engine mirrors Terraform core's managed-resource lifecycle, one
//! resource at a time:
//!
//! 1. **Refresh** stored state: `UpgradeResourceState` then `ReadResource`.
//! 2. **Plan**: `ValidateResourceConfig`, then `PlanResourceChange` with the
//!    proposed new state. A non-empty `requires_replace` turns an update into
//!    a destroy-then-create replacement.
//! 3. **Apply**: `ApplyResourceChange` with the planned state, persisting
//!    the provider's new state (or deleting the record) after every
//!    resource so a failed run never loses track of what exists.
//!
//! What it deliberately does not do yet: references between resources
//! (values known only after apply), data sources, imports, saved plans,
//! or parallel applies. Ordering comes from explicit `dependsOn`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cuenv_manifest::manifest::{
    Infrastructure, InfrastructureProvider, ManagedResourceDeclaration,
};

use crate::error::{InfrastructureError, Result};
use crate::plugin::{ApplyRequest, PlanRequest, ProviderClient};
use crate::protocol::{self, Diagnostic, Severity};
use crate::registry::{ProviderInstaller, ProviderSource, default_cache_directory};
use crate::schema::{ProviderSchema, Schema};
use crate::state::{ManagedResource, ResourceAddress, StateStore};
use crate::tenant::TenantKey;
use crate::type_system::{self, Value};

/// MessagePack encoding of null, used for absent prior/planned states.
const NULL_MESSAGE_PACK: [u8; 1] = [0xc0];

/// What a plan intends to converge towards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanMode {
    /// Converge real infrastructure on the configuration.
    Apply,
    /// Delete every managed resource the tenant owns.
    Destroy,
}

/// Planned action for one resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing to do.
    NoOp,
    /// Create a new resource.
    Create,
    /// Update in place.
    Update,
    /// Destroy, then create.
    Replace,
    /// Destroy.
    Delete,
}

impl Action {
    /// Short symbol used when rendering plans.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::NoOp => " ",
            Self::Create => "+",
            Self::Update => "~",
            Self::Replace => "-/+",
            Self::Delete => "-",
        }
    }
}

/// One provider call needed to realize a change.
#[derive(Debug, Clone)]
struct ApplyStep {
    prior: Vec<u8>,
    planned: Vec<u8>,
    configuration: Vec<u8>,
    planned_private: Vec<u8>,
}

/// Planned change to one managed resource.
#[derive(Debug, Clone)]
pub struct ResourceChange {
    /// Resource address.
    pub address: ResourceAddress,
    /// Local provider name.
    pub provider: String,
    /// Planned action.
    pub action: Action,
    /// Refreshed prior state (null when absent).
    pub before: Value,
    /// Planned new state (null when deleted); may contain unknowns.
    pub after: Value,
    /// Top-level attributes to mask when rendering.
    pub sensitive: Vec<String>,
    /// Attribute paths forcing replacement.
    pub requires_replace: Vec<String>,
    dependencies: Vec<String>,
    steps: Vec<ApplyStep>,
}

/// A full plan for one tenant.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Tenant the plan belongs to.
    pub tenant: TenantKey,
    /// Changes in apply order.
    pub changes: Vec<ResourceChange>,
    /// Provider warnings collected while planning.
    pub warnings: Vec<String>,
}

/// Counts of planned actions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlanSummary {
    /// Resources to create.
    pub create: usize,
    /// Resources to update in place.
    pub update: usize,
    /// Resources to replace.
    pub replace: usize,
    /// Resources to delete.
    pub delete: usize,
    /// Resources already up to date.
    pub unchanged: usize,
}

impl Plan {
    /// Count changes by action.
    #[must_use]
    pub fn summary(&self) -> PlanSummary {
        let mut summary = PlanSummary::default();
        for change in &self.changes {
            match change.action {
                Action::Create => summary.create += 1,
                Action::Update => summary.update += 1,
                Action::Replace => summary.replace += 1,
                Action::Delete => summary.delete += 1,
                Action::NoOp => summary.unchanged += 1,
            }
        }
        summary
    }

    /// Whether applying the plan would change anything.
    #[must_use]
    pub fn has_changes(&self) -> bool {
        self.changes
            .iter()
            .any(|change| change.action != Action::NoOp)
    }
}

/// Progress reported while applying a plan.
#[derive(Debug, Clone)]
pub enum ApplyEvent {
    /// A resource change started.
    Started {
        /// Resource address.
        address: ResourceAddress,
        /// Action being applied.
        action: Action,
    },
    /// A resource change finished and state was persisted.
    Finished {
        /// Resource address.
        address: ResourceAddress,
        /// Action applied.
        action: Action,
    },
    /// A provider warning.
    Warning(String),
}

/// Options for [`InfrastructureEngine::new`].
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// Directory relative provider `path`s resolve against.
    pub project_directory: PathBuf,
    /// Provider plugin cache; defaults to [`default_cache_directory`].
    pub plugin_cache_directory: Option<PathBuf>,
}

struct LoadedProvider {
    client: ProviderClient,
    schema: ProviderSchema,
    source: String,
}

/// Plans and applies an `infrastructure` configuration for one tenant.
pub struct InfrastructureEngine {
    tenant: TenantKey,
    store: Arc<dyn StateStore>,
    infrastructure: Infrastructure,
    options: EngineOptions,
    providers: BTreeMap<String, LoadedProvider>,
}

impl InfrastructureEngine {
    /// Create an engine. Providers are launched lazily while planning.
    #[must_use]
    pub fn new(
        tenant: TenantKey,
        store: Arc<dyn StateStore>,
        infrastructure: Infrastructure,
        options: EngineOptions,
    ) -> Self {
        Self {
            tenant,
            store,
            infrastructure,
            options,
            providers: BTreeMap::new(),
        }
    }

    /// Tenant this engine operates on.
    #[must_use]
    pub const fn tenant(&self) -> &TenantKey {
        &self.tenant
    }

    /// Compute a plan.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration, provider failures,
    /// provider error diagnostics, or state store failures.
    pub async fn plan(&mut self, mode: PlanMode) -> Result<Plan> {
        let stored: BTreeMap<ResourceAddress, ManagedResource> = self
            .store
            .list(&self.tenant)
            .await?
            .into_iter()
            .map(|resource| (resource.address.clone(), resource))
            .collect();
        let mut warnings = Vec::new();
        let mut changes = Vec::new();

        let declared: BTreeMap<String, ManagedResourceDeclaration> = match mode {
            PlanMode::Apply => self.infrastructure.resources.clone(),
            PlanMode::Destroy => BTreeMap::new(),
        };
        let order = topological_order(&dependency_graph(&declared))?;
        let declared_addresses: BTreeSet<ResourceAddress> = declared
            .iter()
            .map(|(name, declaration)| ResourceAddress::new(&declaration.resource_type, name))
            .collect();

        // Orphans (and everything, when destroying) are deleted first, in
        // reverse dependency order.
        let orphans: Vec<&ManagedResource> = stored
            .values()
            .filter(|resource| !declared_addresses.contains(&resource.address))
            .collect();
        let mut orphan_order = topological_order(&orphan_graph(&orphans))?;
        orphan_order.reverse();
        for address in orphan_order {
            if let Some(row) = orphans
                .iter()
                .find(|orphan| orphan.address.to_string() == address)
            {
                changes.push(self.plan_delete(row, &mut warnings).await?);
            }
        }

        for name in order {
            let Some(declaration) = declared.get(&name) else {
                continue;
            };
            let address = ResourceAddress::new(&declaration.resource_type, &name);
            let change = self
                .plan_resource(&name, declaration, stored.get(&address), &mut warnings)
                .await?;
            changes.push(change);
        }

        Ok(Plan {
            tenant: self.tenant.clone(),
            changes,
            warnings,
        })
    }

    /// Apply a plan produced by [`InfrastructureEngine::plan`] on this engine.
    ///
    /// State is persisted after every resource. On failure, resources
    /// already applied stay recorded and the error is returned.
    ///
    /// # Errors
    ///
    /// Returns the first provider or state store failure.
    pub async fn apply(
        &mut self,
        plan: &Plan,
        on_event: &mut (dyn FnMut(ApplyEvent) + Send),
    ) -> Result<PlanSummary> {
        if plan.tenant != self.tenant {
            return Err(InfrastructureError::configuration(
                "plan belongs to a different tenant",
            ));
        }
        for warning in &plan.warnings {
            on_event(ApplyEvent::Warning(warning.clone()));
        }
        for change in plan
            .changes
            .iter()
            .filter(|change| change.action != Action::NoOp)
        {
            on_event(ApplyEvent::Started {
                address: change.address.clone(),
                action: change.action,
            });
            self.apply_change(change, on_event).await?;
            on_event(ApplyEvent::Finished {
                address: change.address.clone(),
                action: change.action,
            });
        }
        Ok(plan.summary())
    }

    /// Stop every launched provider.
    pub async fn shutdown(self) {
        for (_, provider) in self.providers {
            provider.client.shutdown().await;
        }
    }

    async fn apply_change(
        &self,
        change: &ResourceChange,
        on_event: &mut (dyn FnMut(ApplyEvent) + Send),
    ) -> Result<()> {
        let provider = self.loaded(&change.provider)?;
        let schema = resource_schema(provider, &change.provider, &change.address.resource_type)?;
        let value_type = schema.block.implied_type();

        for step in &change.steps {
            let response = provider
                .client
                .apply_resource_change(ApplyRequest {
                    type_name: &change.address.resource_type,
                    prior_state: step.prior.clone(),
                    planned_state: step.planned.clone(),
                    configuration: step.configuration.clone(),
                    planned_private: step.planned_private.clone(),
                })
                .await?;
            let new_state = type_system::from_message_pack(
                response
                    .new_state
                    .as_ref()
                    .map_or(&[][..], |dynamic_value| &dynamic_value.message_pack),
                &value_type,
            )?;

            // Persist what the provider reports before surfacing errors so a
            // partially-created resource stays tracked. A null (or still
            // unknown) result alongside errors means the change did not
            // happen: keep whatever was recorded, as Terraform does.
            let failed = response.diagnostics.iter().any(is_error);
            if new_state.is_null() {
                if !failed {
                    self.store.delete(&self.tenant, &change.address).await?;
                }
            } else if !(failed && new_state.contains_unknown()) {
                self.store
                    .put(
                        &self.tenant,
                        &ManagedResource {
                            address: change.address.clone(),
                            provider: change.provider.clone(),
                            provider_source: provider.source.clone(),
                            schema_version: schema.version,
                            state: new_state.to_state_json(&value_type)?,
                            private: response.private.clone(),
                            dependencies: change.dependencies.clone(),
                        },
                    )
                    .await?;
            }

            let mut warnings = Vec::new();
            check_diagnostics(
                &format!("apply {}", change.address),
                &response.diagnostics,
                &mut warnings,
            )?;
            for warning in warnings {
                on_event(ApplyEvent::Warning(warning));
            }
        }

        if change.action == Action::Delete {
            // Also covers resources already gone during refresh (no steps).
            self.store.delete(&self.tenant, &change.address).await?;
        }
        Ok(())
    }

    async fn plan_resource(
        &mut self,
        name: &str,
        declaration: &ManagedResourceDeclaration,
        stored: Option<&ManagedResource>,
        warnings: &mut Vec<String>,
    ) -> Result<ResourceChange> {
        let provider_name = declaration.provider_name().to_string();
        let address = ResourceAddress::new(&declaration.resource_type, name);
        self.ensure_provider(&provider_name, warnings).await?;
        let provider = self.loaded(&provider_name)?;
        let schema = resource_schema(provider, &provider_name, &declaration.resource_type)?;
        let block = &schema.block;
        let value_type = block.implied_type();

        let configuration = block.normalize_configuration(
            Value::from_configuration_json(
                &serde_json::Value::Object(declaration.configuration.clone()),
                &value_type,
            )
            .map_err(|error| InfrastructureError::configuration(format!("{address}: {error}")))?,
        );
        let configuration_bytes = type_system::to_message_pack(&configuration, &value_type)?;
        let diagnostics = provider
            .client
            .validate_resource_configuration(
                &declaration.resource_type,
                configuration_bytes.clone(),
            )
            .await?;
        check_diagnostics(&format!("validate {address}"), &diagnostics, warnings)?;

        let prior = match stored {
            Some(row) => refresh(provider, schema, row, warnings).await?,
            None => None,
        };

        let mut change = ResourceChange {
            address: address.clone(),
            provider: provider_name,
            action: Action::NoOp,
            before: Value::Null,
            after: Value::Null,
            sensitive: block.sensitive_attributes(),
            requires_replace: Vec::new(),
            dependencies: declaration.depends_on.clone(),
            steps: Vec::new(),
        };

        let Some(prior) = prior else {
            let create = plan_create(
                provider,
                declaration,
                schema,
                &configuration,
                &configuration_bytes,
                warnings,
            )
            .await?;
            change.action = Action::Create;
            change.after = create.0;
            change.steps.push(create.1);
            return Ok(change);
        };

        let proposed = block.proposed_new(&prior.value, &configuration);
        let response = provider
            .client
            .plan_resource_change(PlanRequest {
                type_name: &declaration.resource_type,
                prior_state: prior.bytes.clone(),
                proposed_new_state: type_system::to_message_pack(&proposed, &value_type)?,
                configuration: configuration_bytes.clone(),
                prior_private: prior.private.clone(),
            })
            .await?;
        check_diagnostics(&format!("plan {address}"), &response.diagnostics, warnings)?;
        let planned_bytes = dynamic_bytes(response.planned_state.as_ref());
        let planned = type_system::from_message_pack(&planned_bytes, &value_type)?;
        change.before = prior.value.clone();

        if type_system::semantically_equal(&planned, &prior.value, &value_type) {
            change.after = planned;
            return Ok(change);
        }

        if response.requires_replace.is_empty() {
            change.action = Action::Update;
            change.after = planned;
            change.steps.push(ApplyStep {
                prior: prior.bytes,
                planned: planned_bytes,
                configuration: configuration_bytes,
                planned_private: response.planned_private,
            });
            return Ok(change);
        }

        change.action = Action::Replace;
        change.requires_replace = response.requires_replace.iter().map(render_path).collect();
        change.steps.push(ApplyStep {
            prior: prior.bytes,
            planned: NULL_MESSAGE_PACK.to_vec(),
            configuration: NULL_MESSAGE_PACK.to_vec(),
            planned_private: prior.private,
        });
        let create = plan_create(
            provider,
            declaration,
            schema,
            &configuration,
            &configuration_bytes,
            warnings,
        )
        .await?;
        change.after = create.0;
        change.steps.push(create.1);
        Ok(change)
    }

    async fn plan_delete(
        &mut self,
        row: &ManagedResource,
        warnings: &mut Vec<String>,
    ) -> Result<ResourceChange> {
        self.ensure_provider(&row.provider, warnings)
            .await
            .map_err(|error| {
                InfrastructureError::configuration(format!(
                    "cannot delete {}: provider '{}' is unavailable ({error}); keep it in infrastructure.providers until its resources are gone",
                    row.address, row.provider
                ))
            })?;
        let provider = self.loaded(&row.provider)?;
        let schema = resource_schema(provider, &row.provider, &row.address.resource_type)?;
        let prior = refresh(provider, schema, row, warnings).await?;

        let mut change = ResourceChange {
            address: row.address.clone(),
            provider: row.provider.clone(),
            action: Action::Delete,
            before: Value::Null,
            after: Value::Null,
            sensitive: schema.block.sensitive_attributes(),
            requires_replace: Vec::new(),
            dependencies: row.dependencies.clone(),
            steps: Vec::new(),
        };
        if let Some(prior) = prior {
            change.before = prior.value;
            change.steps.push(ApplyStep {
                prior: prior.bytes,
                planned: NULL_MESSAGE_PACK.to_vec(),
                configuration: NULL_MESSAGE_PACK.to_vec(),
                planned_private: prior.private,
            });
        }
        Ok(change)
    }

    fn loaded(&self, name: &str) -> Result<&LoadedProvider> {
        self.providers.get(name).ok_or_else(|| {
            InfrastructureError::configuration(format!("provider '{name}' is not loaded"))
        })
    }

    async fn ensure_provider(&mut self, name: &str, warnings: &mut Vec<String>) -> Result<()> {
        if self.providers.contains_key(name) {
            return Ok(());
        }
        let declaration = self
            .infrastructure
            .providers
            .get(name)
            .cloned()
            .ok_or_else(|| {
                InfrastructureError::configuration(format!(
                    "provider '{name}' is not declared in infrastructure.providers"
                ))
            })?;
        let source = ProviderSource::parse(&declaration.source)?;
        let binary = self.resolve_binary(name, &declaration, &source).await?;

        let client = ProviderClient::launch(&binary).await?;
        let (schema, diagnostics) = client.schema().await?;
        check_diagnostics(
            &format!("load provider '{name}' schema"),
            &diagnostics,
            warnings,
        )?;

        let value_type = schema.provider.block.implied_type();
        let configuration = schema.provider.block.normalize_configuration(
            Value::from_configuration_json(
                &serde_json::Value::Object(declaration.configuration.clone()),
                &value_type,
            )
            .map_err(|error| {
                InfrastructureError::configuration(format!("provider '{name}': {error}"))
            })?,
        );
        let configuration_bytes = type_system::to_message_pack(&configuration, &value_type)?;
        let diagnostics = client
            .validate_provider_configuration(configuration_bytes.clone())
            .await?;
        check_diagnostics(
            &format!("validate provider '{name}'"),
            &diagnostics,
            warnings,
        )?;
        let diagnostics = client.configure(configuration_bytes).await?;
        check_diagnostics(
            &format!("configure provider '{name}'"),
            &diagnostics,
            warnings,
        )?;

        tracing::debug!(provider = name, protocol = ?client.protocol(), binary = %binary.display(), "provider ready");
        self.providers.insert(
            name.to_string(),
            LoadedProvider {
                client,
                schema,
                source: source.to_string(),
            },
        );
        Ok(())
    }

    async fn resolve_binary(
        &self,
        name: &str,
        declaration: &InfrastructureProvider,
        source: &ProviderSource,
    ) -> Result<PathBuf> {
        if let Some(path) = &declaration.path {
            let path = Path::new(path);
            return Ok(if path.is_absolute() {
                path.to_path_buf()
            } else {
                self.options.project_directory.join(path)
            });
        }
        let version = declaration.version.as_deref().ok_or_else(|| {
            InfrastructureError::configuration(format!(
                "provider '{name}' needs an exact `version` (or a local `path`)"
            ))
        })?;
        let cache = self
            .options
            .plugin_cache_directory
            .clone()
            .unwrap_or_else(default_cache_directory);
        ProviderInstaller::new(cache)?.ensure(source, version).await
    }
}

struct Refreshed {
    value: Value,
    bytes: Vec<u8>,
    private: Vec<u8>,
}

async fn refresh(
    provider: &LoadedProvider,
    schema: &Schema,
    row: &ManagedResource,
    warnings: &mut Vec<String>,
) -> Result<Option<Refreshed>> {
    let type_name = &row.address.resource_type;
    let state_json = serde_json::to_vec(&row.state)
        .map_err(|error| InfrastructureError::state(format!("serialize stored state: {error}")))?;
    let (upgraded, diagnostics) = provider
        .client
        .upgrade_resource_state(type_name, row.schema_version, state_json)
        .await?;
    check_diagnostics(
        &format!("upgrade state of {}", row.address),
        &diagnostics,
        warnings,
    )?;

    let response = provider
        .client
        .read_resource(type_name, upgraded, row.private.clone())
        .await?;
    check_diagnostics(
        &format!("refresh {}", row.address),
        &response.diagnostics,
        warnings,
    )?;
    let bytes = dynamic_bytes(response.new_state.as_ref());
    let value = type_system::from_message_pack(&bytes, &schema.block.implied_type())?;
    if value.is_null() {
        return Ok(None);
    }
    Ok(Some(Refreshed {
        value,
        bytes,
        private: response.private,
    }))
}

async fn plan_create(
    provider: &LoadedProvider,
    declaration: &ManagedResourceDeclaration,
    schema: &Schema,
    configuration: &Value,
    configuration_bytes: &[u8],
    warnings: &mut Vec<String>,
) -> Result<(Value, ApplyStep)> {
    let value_type = schema.block.implied_type();
    let proposed = schema.block.proposed_new(&Value::Null, configuration);
    let response = provider
        .client
        .plan_resource_change(PlanRequest {
            type_name: &declaration.resource_type,
            prior_state: NULL_MESSAGE_PACK.to_vec(),
            proposed_new_state: type_system::to_message_pack(&proposed, &value_type)?,
            configuration: configuration_bytes.to_vec(),
            prior_private: Vec::new(),
        })
        .await?;
    check_diagnostics(
        &format!("plan create of {}", declaration.resource_type),
        &response.diagnostics,
        warnings,
    )?;
    let planned_bytes = dynamic_bytes(response.planned_state.as_ref());
    let planned = type_system::from_message_pack(&planned_bytes, &value_type)?;
    Ok((
        planned,
        ApplyStep {
            prior: NULL_MESSAGE_PACK.to_vec(),
            planned: planned_bytes,
            configuration: configuration_bytes.to_vec(),
            planned_private: response.planned_private,
        },
    ))
}

fn resource_schema<'provider>(
    provider: &'provider LoadedProvider,
    provider_name: &str,
    resource_type: &str,
) -> Result<&'provider Schema> {
    provider.schema.resources.get(resource_type).ok_or_else(|| {
        InfrastructureError::configuration(format!(
            "provider '{provider_name}' has no managed resource type '{resource_type}'"
        ))
    })
}

fn dynamic_bytes(value: Option<&protocol::DynamicValue>) -> Vec<u8> {
    match value {
        Some(dynamic_value) if !dynamic_value.message_pack.is_empty() => {
            dynamic_value.message_pack.clone()
        }
        _ => NULL_MESSAGE_PACK.to_vec(),
    }
}

fn is_error(diagnostic: &Diagnostic) -> bool {
    Severity::try_from(diagnostic.severity).map_or(true, |severity| severity != Severity::Warning)
}

/// Dependency graph over orphaned records, keyed by address. Dependencies
/// on resources that remain declared don't constrain deletion order.
fn orphan_graph(orphans: &[&ManagedResource]) -> BTreeMap<String, Vec<String>> {
    orphans
        .iter()
        .map(|orphan| {
            let dependencies = orphans
                .iter()
                .filter(|other| {
                    other.address != orphan.address
                        && orphan.dependencies.contains(&other.address.name)
                })
                .map(|other| other.address.to_string())
                .collect();
            (orphan.address.to_string(), dependencies)
        })
        .collect()
}

/// Render a diagnostic as a single human-readable string.
fn render_diagnostic(diagnostic: &Diagnostic) -> String {
    let mut rendered = diagnostic.summary.clone();
    if !diagnostic.detail.is_empty() {
        rendered.push_str(": ");
        rendered.push_str(&diagnostic.detail);
    }
    if let Some(path) = &diagnostic.attribute
        && !path.steps.is_empty()
    {
        let _ = write!(rendered, " (at {})", render_path(path));
    }
    rendered
}

/// Fail on error diagnostics; collect warnings.
fn check_diagnostics(
    context: &str,
    diagnostics: &[Diagnostic],
    warnings: &mut Vec<String>,
) -> Result<()> {
    let mut errors = Vec::new();
    for diagnostic in diagnostics {
        match Severity::try_from(diagnostic.severity).unwrap_or(Severity::Invalid) {
            Severity::Warning => {
                warnings.push(format!("{context}: {}", render_diagnostic(diagnostic)));
            }
            Severity::Error | Severity::Invalid => errors.push(render_diagnostic(diagnostic)),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(InfrastructureError::Diagnostics {
            context: context.to_string(),
            errors,
        })
    }
}

/// Render an attribute path as `rules.match[0]["key"]`.
fn render_path(path: &protocol::AttributePath) -> String {
    let mut rendered = String::new();
    for step in &path.steps {
        match &step.selector {
            Some(protocol::Selector::AttributeName(name)) => {
                if !rendered.is_empty() {
                    rendered.push('.');
                }
                rendered.push_str(name);
            }
            Some(protocol::Selector::ElementKeyString(key)) => {
                let _ = write!(rendered, "[{key:?}]");
            }
            Some(protocol::Selector::ElementKeyInt(index)) => {
                let _ = write!(rendered, "[{index}]");
            }
            None => {}
        }
    }
    rendered
}

fn dependency_graph(
    resources: &BTreeMap<String, ManagedResourceDeclaration>,
) -> BTreeMap<String, Vec<String>> {
    resources
        .iter()
        .map(|(name, declaration)| (name.clone(), declaration.depends_on.clone()))
        .collect()
}

/// Order nodes so dependencies come first. Unknown dependencies are
/// errors; ties are broken by name for deterministic plans.
fn topological_order(graph: &BTreeMap<String, Vec<String>>) -> Result<Vec<String>> {
    for (name, dependencies) in graph {
        if let Some(missing) = dependencies
            .iter()
            .find(|dependency| !graph.contains_key(*dependency))
        {
            return Err(InfrastructureError::configuration(format!(
                "resource '{name}' depends on unknown resource '{missing}'"
            )));
        }
    }
    let mut remaining: BTreeMap<&str, BTreeSet<&str>> = graph
        .iter()
        .map(|(name, dependencies)| {
            (
                name.as_str(),
                dependencies.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    let mut order = Vec::with_capacity(graph.len());
    while !remaining.is_empty() {
        let ready: Vec<&str> = remaining
            .iter()
            .filter(|(_, dependencies)| dependencies.is_empty())
            .map(|(name, _)| *name)
            .collect();
        if ready.is_empty() {
            let cycle: Vec<&str> = remaining.keys().copied().collect();
            return Err(InfrastructureError::configuration(format!(
                "dependency cycle between resources: {}",
                cycle.join(", ")
            )));
        }
        for name in ready {
            remaining.remove(name);
            for dependencies in remaining.values_mut() {
                dependencies.remove(name);
            }
            order.push(name.to_string());
        }
    }
    Ok(order)
}

/// Render a plan as human-readable text.
#[must_use]
pub fn render_plan(plan: &Plan) -> String {
    let mut rendered = String::new();
    for change in &plan.changes {
        let label = match change.action {
            Action::NoOp => continue,
            Action::Create => "create",
            Action::Update => "update in-place",
            Action::Replace => "replace",
            Action::Delete => "delete",
        };
        let _ = writeln!(
            rendered,
            "  {} {} ({label})",
            change.action.symbol(),
            change.address
        );
        if !change.requires_replace.is_empty() {
            let _ = writeln!(
                rendered,
                "      # forced by: {}",
                change.requires_replace.join(", ")
            );
        }
        for line in attribute_lines(change) {
            rendered.push_str("      ");
            rendered.push_str(&line);
            rendered.push('\n');
        }
    }
    let summary = plan.summary();
    let _ = writeln!(
        rendered,
        "\nPlan: {} to create, {} to update, {} to replace, {} to delete, {} unchanged.",
        summary.create, summary.update, summary.replace, summary.delete, summary.unchanged
    );
    rendered
}

fn attribute_lines(change: &ResourceChange) -> Vec<String> {
    let empty = BTreeMap::new();
    let before = match &change.before {
        Value::Object(attributes) => attributes,
        _ => &empty,
    };
    let after = match &change.after {
        Value::Object(attributes) => attributes,
        _ => &empty,
    };
    let render = |name: &str, value: &Value| {
        if change
            .sensitive
            .iter()
            .any(|sensitive_name| sensitive_name == name)
            && !value.is_null()
        {
            "(sensitive)".to_string()
        } else {
            value.to_string()
        }
    };
    let names: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let mut lines = Vec::new();
    for name in names {
        let before_value = before.get(name).unwrap_or(&Value::Null);
        let after_value = after.get(name).unwrap_or(&Value::Null);
        match change.action {
            Action::Create if !after_value.is_null() => {
                lines.push(format!("+ {name} = {}", render(name, after_value)));
            }
            Action::Delete if !before_value.is_null() => {
                lines.push(format!("- {name} = {}", render(name, before_value)));
            }
            Action::Update | Action::Replace if after_value != before_value => lines.push(format!(
                "~ {name}: {} -> {}",
                render(name, before_value),
                render(name, after_value)
            )),
            _ => {}
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(edges: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        edges
            .iter()
            .map(|(name, dependencies)| {
                (
                    (*name).to_owned(),
                    dependencies
                        .iter()
                        .map(|dependency| (*dependency).to_owned())
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn topological_order_puts_dependencies_first() {
        let order = topological_order(&graph(&[
            ("network", &["subnet"]),
            ("subnet", &["account"]),
            ("account", &[]),
            ("unrelated", &[]),
        ]))
        .unwrap();
        let position = |name: &str| order.iter().position(|entry| entry == name).unwrap();
        assert!(position("account") < position("subnet"));
        assert!(position("subnet") < position("network"));
        assert_eq!(order.len(), 4);
    }

    fn orphan(resource_type: &str, name: &str, dependencies: &[&str]) -> ManagedResource {
        ManagedResource {
            address: ResourceAddress::new(resource_type, name),
            provider: "random".into(),
            provider_source: "registry.terraform.io/hashicorp/random".into(),
            schema_version: 0,
            state: serde_json::json!({}),
            private: Vec::new(),
            dependencies: dependencies
                .iter()
                .map(|dependency| (*dependency).to_owned())
                .collect(),
        }
    }

    #[test]
    fn orphan_graph_keys_by_address_and_ignores_declared_dependencies() {
        let pet = orphan("random_pet", "shared", &[]);
        let identifier = orphan("random_id", "shared", &["shared", "still_declared"]);
        let graph = orphan_graph(&[&pet, &identifier]);
        assert_eq!(graph.len(), 2);
        assert_eq!(
            graph["random_id.shared"],
            vec!["random_pet.shared".to_string()]
        );
        assert!(graph["random_pet.shared"].is_empty());
        assert!(topological_order(&graph).is_ok());
    }

    #[test]
    fn topological_order_rejects_cycles_and_unknown_dependencies() {
        assert!(
            topological_order(&graph(&[("first", &["second"]), ("second", &["first"])])).is_err()
        );
        assert!(topological_order(&graph(&[("first", &["missing"])])).is_err());
    }

    #[test]
    fn diagnostics_split_errors_from_warnings() {
        let mut warnings = Vec::new();
        let warning = Diagnostic {
            severity: Severity::Warning as i32,
            summary: "deprecated".into(),
            detail: String::new(),
            attribute: None,
        };
        check_diagnostics("context", std::slice::from_ref(&warning), &mut warnings).unwrap();
        assert_eq!(warnings, vec!["context: deprecated".to_string()]);

        let error = Diagnostic {
            severity: Severity::Error as i32,
            summary: "bad".into(),
            detail: "value too long".into(),
            attribute: Some(protocol::AttributePath {
                steps: vec![
                    protocol::AttributePathStep {
                        selector: Some(protocol::Selector::AttributeName("rules".into())),
                    },
                    protocol::AttributePathStep {
                        selector: Some(protocol::Selector::ElementKeyInt(0)),
                    },
                ],
            }),
        };
        let failure = check_diagnostics("context", &[warning, error], &mut warnings).unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("bad: value too long (at rules[0])"),
            "{failure}"
        );
    }

    #[test]
    fn render_plan_masks_sensitive_values() {
        let mut after = BTreeMap::new();
        after.insert("id".to_string(), Value::Unknown);
        after.insert("secret".to_string(), Value::String("hunter2".into()));
        let plan = Plan {
            tenant: TenantKey::new("example.com/app", "web").unwrap(),
            changes: vec![ResourceChange {
                address: ResourceAddress::new("random_password", "database"),
                provider: "random".into(),
                action: Action::Create,
                before: Value::Null,
                after: Value::Object(after),
                sensitive: vec!["secret".into()],
                requires_replace: Vec::new(),
                dependencies: Vec::new(),
                steps: Vec::new(),
            }],
            warnings: Vec::new(),
        };
        let text = render_plan(&plan);
        assert!(
            text.contains("+ random_password.database (create)"),
            "{text}"
        );
        assert!(text.contains("+ id = (known after apply)"), "{text}");
        assert!(text.contains("+ secret = (sensitive)"), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("Plan: 1 to create"), "{text}");
    }
}
