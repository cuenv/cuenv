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

use cuenv_manifest::manifest::{Infra, InfraProvider, ManagedResourceSpec};

use crate::cty::{self, Value};
use crate::error::{InfraError, Result};
use crate::plugin::{ApplyRequest, PlanRequest, ProviderClient};
use crate::proto::{self, Diagnostic, Severity};
use crate::registry::{ProviderInstaller, ProviderSource, default_cache_dir};
use crate::schema::{ProviderSchema, Schema};
use crate::state::{ManagedResource, ResourceAddress, StateStore};
use crate::tenant::TenantKey;

/// msgpack encoding of null, used for absent prior/planned states.
const NULL_MSGPACK: [u8; 1] = [0xc0];

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
    config: Vec<u8>,
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
        let mut s = PlanSummary::default();
        for change in &self.changes {
            match change.action {
                Action::Create => s.create += 1,
                Action::Update => s.update += 1,
                Action::Replace => s.replace += 1,
                Action::Delete => s.delete += 1,
                Action::NoOp => s.unchanged += 1,
            }
        }
        s
    }

    /// Whether applying the plan would change anything.
    #[must_use]
    pub fn has_changes(&self) -> bool {
        self.changes.iter().any(|c| c.action != Action::NoOp)
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

/// Options for [`InfraEngine::new`].
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// Directory relative provider `path`s resolve against.
    pub project_dir: PathBuf,
    /// Provider plugin cache; defaults to [`default_cache_dir`].
    pub plugin_cache_dir: Option<PathBuf>,
}

struct LoadedProvider {
    client: ProviderClient,
    schema: ProviderSchema,
    source: String,
}

/// Plans and applies an `infra` configuration for one tenant.
pub struct InfraEngine {
    tenant: TenantKey,
    store: Arc<dyn StateStore>,
    infra: Infra,
    options: EngineOptions,
    providers: BTreeMap<String, LoadedProvider>,
}

impl InfraEngine {
    /// Create an engine. Providers are launched lazily while planning.
    #[must_use]
    pub fn new(
        tenant: TenantKey,
        store: Arc<dyn StateStore>,
        infra: Infra,
        options: EngineOptions,
    ) -> Self {
        Self {
            tenant,
            store,
            infra,
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
            .map(|r| (r.address.clone(), r))
            .collect();
        let mut warnings = Vec::new();
        let mut changes = Vec::new();

        let declared: BTreeMap<String, ManagedResourceSpec> = match mode {
            PlanMode::Apply => self.infra.resources.clone(),
            PlanMode::Destroy => BTreeMap::new(),
        };
        let order = topo_order(&dependency_graph(&declared))?;
        let declared_addresses: BTreeSet<ResourceAddress> = declared
            .iter()
            .map(|(name, spec)| ResourceAddress::new(&spec.resource_type, name))
            .collect();

        // Orphans (and everything, when destroying) are deleted first, in
        // reverse dependency order.
        let orphans: Vec<&ManagedResource> = stored
            .values()
            .filter(|r| !declared_addresses.contains(&r.address))
            .collect();
        let mut orphan_order = topo_order(&orphan_graph(&orphans))?;
        orphan_order.reverse();
        for address in orphan_order {
            if let Some(row) = orphans.iter().find(|r| r.address.to_string() == address) {
                changes.push(self.plan_delete(row, &mut warnings).await?);
            }
        }

        for name in order {
            let Some(spec) = declared.get(&name) else {
                continue;
            };
            let address = ResourceAddress::new(&spec.resource_type, &name);
            let change = self
                .plan_resource(&name, spec, stored.get(&address), &mut warnings)
                .await?;
            changes.push(change);
        }

        Ok(Plan {
            tenant: self.tenant.clone(),
            changes,
            warnings,
        })
    }

    /// Apply a plan produced by [`InfraEngine::plan`] on this engine.
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
            return Err(InfraError::config("plan belongs to a different tenant"));
        }
        for warning in &plan.warnings {
            on_event(ApplyEvent::Warning(warning.clone()));
        }
        for change in plan.changes.iter().filter(|c| c.action != Action::NoOp) {
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
        let ty = schema.block.implied_type();

        for step in &change.steps {
            let resp = provider
                .client
                .apply_resource_change(ApplyRequest {
                    type_name: &change.address.resource_type,
                    prior_state: step.prior.clone(),
                    planned_state: step.planned.clone(),
                    config: step.config.clone(),
                    planned_private: step.planned_private.clone(),
                })
                .await?;
            let new_state =
                cty::from_msgpack(resp.new_state.as_ref().map_or(&[][..], |v| &v.msgpack), &ty)?;

            // Persist what the provider reports before surfacing errors so a
            // partially-created resource stays tracked. A null (or still
            // unknown) result alongside errors means the change did not
            // happen: keep whatever was recorded, as Terraform does.
            let failed = resp.diagnostics.iter().any(is_error);
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
                            state: new_state.to_state_json(&ty)?,
                            private: resp.private.clone(),
                            dependencies: change.dependencies.clone(),
                        },
                    )
                    .await?;
            }

            let mut warnings = Vec::new();
            check_diagnostics(
                &format!("apply {}", change.address),
                &resp.diagnostics,
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
        spec: &ManagedResourceSpec,
        stored: Option<&ManagedResource>,
        warnings: &mut Vec<String>,
    ) -> Result<ResourceChange> {
        let provider_name = spec.provider_name().to_string();
        let address = ResourceAddress::new(&spec.resource_type, name);
        self.ensure_provider(&provider_name, warnings).await?;
        let provider = self.loaded(&provider_name)?;
        let schema = resource_schema(provider, &provider_name, &spec.resource_type)?;
        let block = &schema.block;
        let ty = block.implied_type();

        let config = block.normalize_config(
            Value::from_config_json(&serde_json::Value::Object(spec.config.clone()), &ty)
                .map_err(|e| InfraError::config(format!("{address}: {e}")))?,
        );
        let config_bytes = cty::to_msgpack(&config, &ty)?;
        let diags = provider
            .client
            .validate_resource_config(&spec.resource_type, config_bytes.clone())
            .await?;
        check_diagnostics(&format!("validate {address}"), &diags, warnings)?;

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
            dependencies: spec.depends_on.clone(),
            steps: Vec::new(),
        };

        let Some(prior) = prior else {
            let create =
                plan_create(provider, spec, schema, &config, &config_bytes, warnings).await?;
            change.action = Action::Create;
            change.after = create.0;
            change.steps.push(create.1);
            return Ok(change);
        };

        let proposed = block.proposed_new(&prior.value, &config);
        let resp = provider
            .client
            .plan_resource_change(PlanRequest {
                type_name: &spec.resource_type,
                prior_state: prior.bytes.clone(),
                proposed_new_state: cty::to_msgpack(&proposed, &ty)?,
                config: config_bytes.clone(),
                prior_private: prior.private.clone(),
            })
            .await?;
        check_diagnostics(&format!("plan {address}"), &resp.diagnostics, warnings)?;
        let planned_bytes = dynamic_bytes(resp.planned_state.as_ref());
        let planned = cty::from_msgpack(&planned_bytes, &ty)?;
        change.before = prior.value.clone();

        if cty::semantically_equal(&planned, &prior.value, &ty) {
            change.after = planned;
            return Ok(change);
        }

        if resp.requires_replace.is_empty() {
            change.action = Action::Update;
            change.after = planned;
            change.steps.push(ApplyStep {
                prior: prior.bytes,
                planned: planned_bytes,
                config: config_bytes,
                planned_private: resp.planned_private,
            });
            return Ok(change);
        }

        change.action = Action::Replace;
        change.requires_replace = resp.requires_replace.iter().map(render_path).collect();
        change.steps.push(ApplyStep {
            prior: prior.bytes,
            planned: NULL_MSGPACK.to_vec(),
            config: NULL_MSGPACK.to_vec(),
            planned_private: prior.private,
        });
        let create = plan_create(provider, spec, schema, &config, &config_bytes, warnings).await?;
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
            .map_err(|e| {
                InfraError::config(format!(
                    "cannot delete {}: provider '{}' is unavailable ({e}); keep it in infra.providers until its resources are gone",
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
                planned: NULL_MSGPACK.to_vec(),
                config: NULL_MSGPACK.to_vec(),
                planned_private: prior.private,
            });
        }
        Ok(change)
    }

    fn loaded(&self, name: &str) -> Result<&LoadedProvider> {
        self.providers
            .get(name)
            .ok_or_else(|| InfraError::config(format!("provider '{name}' is not loaded")))
    }

    async fn ensure_provider(&mut self, name: &str, warnings: &mut Vec<String>) -> Result<()> {
        if self.providers.contains_key(name) {
            return Ok(());
        }
        let spec = self.infra.providers.get(name).cloned().ok_or_else(|| {
            InfraError::config(format!(
                "provider '{name}' is not declared in infra.providers"
            ))
        })?;
        let source = ProviderSource::parse(&spec.source)?;
        let binary = self.resolve_binary(name, &spec, &source).await?;

        let client = ProviderClient::launch(&binary).await?;
        let (schema, diags) = client.schema().await?;
        check_diagnostics(&format!("load provider '{name}' schema"), &diags, warnings)?;

        let ty = schema.provider.block.implied_type();
        let config = schema.provider.block.normalize_config(
            Value::from_config_json(&serde_json::Value::Object(spec.config.clone()), &ty)
                .map_err(|e| InfraError::config(format!("provider '{name}': {e}")))?,
        );
        let config_bytes = cty::to_msgpack(&config, &ty)?;
        let diags = client
            .validate_provider_config(config_bytes.clone())
            .await?;
        check_diagnostics(&format!("validate provider '{name}'"), &diags, warnings)?;
        let diags = client.configure(config_bytes).await?;
        check_diagnostics(&format!("configure provider '{name}'"), &diags, warnings)?;

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
        spec: &InfraProvider,
        source: &ProviderSource,
    ) -> Result<PathBuf> {
        if let Some(path) = &spec.path {
            let path = Path::new(path);
            return Ok(if path.is_absolute() {
                path.to_path_buf()
            } else {
                self.options.project_dir.join(path)
            });
        }
        let version = spec.version.as_deref().ok_or_else(|| {
            InfraError::config(format!(
                "provider '{name}' needs an exact `version` (or a local `path`)"
            ))
        })?;
        let cache = self
            .options
            .plugin_cache_dir
            .clone()
            .unwrap_or_else(default_cache_dir);
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
        .map_err(|e| InfraError::state(format!("serialize stored state: {e}")))?;
    let (upgraded, diags) = provider
        .client
        .upgrade_resource_state(type_name, row.schema_version, state_json)
        .await?;
    check_diagnostics(
        &format!("upgrade state of {}", row.address),
        &diags,
        warnings,
    )?;

    let resp = provider
        .client
        .read_resource(type_name, upgraded, row.private.clone())
        .await?;
    check_diagnostics(
        &format!("refresh {}", row.address),
        &resp.diagnostics,
        warnings,
    )?;
    let bytes = dynamic_bytes(resp.new_state.as_ref());
    let value = cty::from_msgpack(&bytes, &schema.block.implied_type())?;
    if value.is_null() {
        return Ok(None);
    }
    Ok(Some(Refreshed {
        value,
        bytes,
        private: resp.private,
    }))
}

async fn plan_create(
    provider: &LoadedProvider,
    spec: &ManagedResourceSpec,
    schema: &Schema,
    config: &Value,
    config_bytes: &[u8],
    warnings: &mut Vec<String>,
) -> Result<(Value, ApplyStep)> {
    let ty = schema.block.implied_type();
    let proposed = schema.block.proposed_new(&Value::Null, config);
    let resp = provider
        .client
        .plan_resource_change(PlanRequest {
            type_name: &spec.resource_type,
            prior_state: NULL_MSGPACK.to_vec(),
            proposed_new_state: cty::to_msgpack(&proposed, &ty)?,
            config: config_bytes.to_vec(),
            prior_private: Vec::new(),
        })
        .await?;
    check_diagnostics(
        &format!("plan create of {}", spec.resource_type),
        &resp.diagnostics,
        warnings,
    )?;
    let planned_bytes = dynamic_bytes(resp.planned_state.as_ref());
    let planned = cty::from_msgpack(&planned_bytes, &ty)?;
    Ok((
        planned,
        ApplyStep {
            prior: NULL_MSGPACK.to_vec(),
            planned: planned_bytes,
            config: config_bytes.to_vec(),
            planned_private: resp.planned_private,
        },
    ))
}

fn resource_schema<'a>(
    provider: &'a LoadedProvider,
    provider_name: &str,
    resource_type: &str,
) -> Result<&'a Schema> {
    provider.schema.resources.get(resource_type).ok_or_else(|| {
        InfraError::config(format!(
            "provider '{provider_name}' has no managed resource type '{resource_type}'"
        ))
    })
}

fn dynamic_bytes(value: Option<&proto::DynamicValue>) -> Vec<u8> {
    match value {
        Some(v) if !v.msgpack.is_empty() => v.msgpack.clone(),
        _ => NULL_MSGPACK.to_vec(),
    }
}

fn is_error(d: &Diagnostic) -> bool {
    Severity::try_from(d.severity).map_or(true, |s| s != Severity::Warning)
}

/// Dependency graph over orphaned records, keyed by address. Dependencies
/// on resources that remain declared don't constrain deletion order.
fn orphan_graph(orphans: &[&ManagedResource]) -> BTreeMap<String, Vec<String>> {
    orphans
        .iter()
        .map(|r| {
            let deps = orphans
                .iter()
                .filter(|other| {
                    other.address != r.address && r.dependencies.contains(&other.address.name)
                })
                .map(|other| other.address.to_string())
                .collect();
            (r.address.to_string(), deps)
        })
        .collect()
}

/// Render a diagnostic as a single human-readable string.
fn render_diagnostic(d: &Diagnostic) -> String {
    let mut out = d.summary.clone();
    if !d.detail.is_empty() {
        out.push_str(": ");
        out.push_str(&d.detail);
    }
    if let Some(path) = &d.attribute
        && !path.steps.is_empty()
    {
        let _ = write!(out, " (at {})", render_path(path));
    }
    out
}

/// Fail on error diagnostics; collect warnings.
fn check_diagnostics(
    context: &str,
    diags: &[Diagnostic],
    warnings: &mut Vec<String>,
) -> Result<()> {
    let mut errors = Vec::new();
    for d in diags {
        match Severity::try_from(d.severity).unwrap_or(Severity::Invalid) {
            Severity::Warning => warnings.push(format!("{context}: {}", render_diagnostic(d))),
            Severity::Error | Severity::Invalid => errors.push(render_diagnostic(d)),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(InfraError::Diagnostics {
            context: context.to_string(),
            errors,
        })
    }
}

/// Render an attribute path as `a.b[0]["k"]`.
fn render_path(path: &proto::AttributePath) -> String {
    let mut out = String::new();
    for step in &path.steps {
        match &step.selector {
            Some(proto::Selector::AttributeName(name)) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(name);
            }
            Some(proto::Selector::ElementKeyString(key)) => {
                let _ = write!(out, "[{key:?}]");
            }
            Some(proto::Selector::ElementKeyInt(i)) => {
                let _ = write!(out, "[{i}]");
            }
            None => {}
        }
    }
    out
}

fn dependency_graph(
    resources: &BTreeMap<String, ManagedResourceSpec>,
) -> BTreeMap<String, Vec<String>> {
    resources
        .iter()
        .map(|(name, spec)| (name.clone(), spec.depends_on.clone()))
        .collect()
}

/// Order nodes so dependencies come first. Unknown dependencies are
/// errors; ties are broken by name for deterministic plans.
fn topo_order(graph: &BTreeMap<String, Vec<String>>) -> Result<Vec<String>> {
    for (name, deps) in graph {
        if let Some(missing) = deps.iter().find(|d| !graph.contains_key(*d)) {
            return Err(InfraError::config(format!(
                "resource '{name}' depends on unknown resource '{missing}'"
            )));
        }
    }
    let mut remaining: BTreeMap<&str, BTreeSet<&str>> = graph
        .iter()
        .map(|(n, deps)| (n.as_str(), deps.iter().map(String::as_str).collect()))
        .collect();
    let mut order = Vec::with_capacity(graph.len());
    while !remaining.is_empty() {
        let ready: Vec<&str> = remaining
            .iter()
            .filter(|(_, deps)| deps.is_empty())
            .map(|(n, _)| *n)
            .collect();
        if ready.is_empty() {
            let cycle: Vec<&str> = remaining.keys().copied().collect();
            return Err(InfraError::config(format!(
                "dependency cycle between resources: {}",
                cycle.join(", ")
            )));
        }
        for name in ready {
            remaining.remove(name);
            for deps in remaining.values_mut() {
                deps.remove(name);
            }
            order.push(name.to_string());
        }
    }
    Ok(order)
}

/// Render a plan as human-readable text.
#[must_use]
pub fn render_plan(plan: &Plan) -> String {
    let mut out = String::new();
    for change in &plan.changes {
        let label = match change.action {
            Action::NoOp => continue,
            Action::Create => "create",
            Action::Update => "update in-place",
            Action::Replace => "replace",
            Action::Delete => "delete",
        };
        let _ = writeln!(
            out,
            "  {} {} ({label})",
            change.action.symbol(),
            change.address
        );
        if !change.requires_replace.is_empty() {
            let _ = writeln!(
                out,
                "      # forced by: {}",
                change.requires_replace.join(", ")
            );
        }
        for line in attribute_lines(change) {
            out.push_str("      ");
            out.push_str(&line);
            out.push('\n');
        }
    }
    let s = plan.summary();
    let _ = writeln!(
        out,
        "\nPlan: {} to create, {} to update, {} to replace, {} to delete, {} unchanged.",
        s.create, s.update, s.replace, s.delete, s.unchanged
    );
    out
}

fn attribute_lines(change: &ResourceChange) -> Vec<String> {
    let empty = BTreeMap::new();
    let before = match &change.before {
        Value::Object(a) => a,
        _ => &empty,
    };
    let after = match &change.after {
        Value::Object(a) => a,
        _ => &empty,
    };
    let render = |name: &str, v: &Value| {
        if change.sensitive.iter().any(|s| s == name) && !v.is_null() {
            "(sensitive)".to_string()
        } else {
            v.to_string()
        }
    };
    let names: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let mut lines = Vec::new();
    for name in names {
        let b = before.get(name).unwrap_or(&Value::Null);
        let a = after.get(name).unwrap_or(&Value::Null);
        match change.action {
            Action::Create if !a.is_null() => lines.push(format!("+ {name} = {}", render(name, a))),
            Action::Delete if !b.is_null() => lines.push(format!("- {name} = {}", render(name, b))),
            Action::Update | Action::Replace if a != b => lines.push(format!(
                "~ {name}: {} -> {}",
                render(name, b),
                render(name, a)
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
            .map(|(n, deps)| ((*n).to_owned(), deps.iter().map(|d| (*d).to_owned()).collect()))
            .collect()
    }

    #[test]
    fn topo_order_puts_dependencies_first() {
        let order = topo_order(&graph(&[
            ("c", &["b"]),
            ("b", &["a"]),
            ("a", &[]),
            ("z", &[]),
        ]))
        .unwrap();
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();
        assert!(pos("a") < pos("b"));
        assert!(pos("b") < pos("c"));
        assert_eq!(order.len(), 4);
    }

    fn orphan(resource_type: &str, name: &str, deps: &[&str]) -> ManagedResource {
        ManagedResource {
            address: ResourceAddress::new(resource_type, name),
            provider: "random".into(),
            provider_source: "registry.terraform.io/hashicorp/random".into(),
            schema_version: 0,
            state: serde_json::json!({}),
            private: Vec::new(),
            dependencies: deps.iter().map(|d| (*d).to_owned()).collect(),
        }
    }

    #[test]
    fn orphan_graph_keys_by_address_and_ignores_declared_dependencies() {
        let a = orphan("random_pet", "x", &[]);
        let b = orphan("random_id", "x", &["x", "still_declared"]);
        let graph = orphan_graph(&[&a, &b]);
        assert_eq!(graph.len(), 2);
        assert_eq!(graph["random_id.x"], vec!["random_pet.x".to_string()]);
        assert!(graph["random_pet.x"].is_empty());
        assert!(topo_order(&graph).is_ok());
    }

    #[test]
    fn topo_order_rejects_cycles_and_unknown_dependencies() {
        assert!(topo_order(&graph(&[("a", &["b"]), ("b", &["a"])])).is_err());
        assert!(topo_order(&graph(&[("a", &["missing"])])).is_err());
    }

    #[test]
    fn diagnostics_split_errors_from_warnings() {
        let mut warnings = Vec::new();
        let warn = Diagnostic {
            severity: Severity::Warning as i32,
            summary: "deprecated".into(),
            detail: String::new(),
            attribute: None,
        };
        check_diagnostics("ctx", std::slice::from_ref(&warn), &mut warnings).unwrap();
        assert_eq!(warnings, vec!["ctx: deprecated".to_string()]);

        let err = Diagnostic {
            severity: Severity::Error as i32,
            summary: "bad".into(),
            detail: "value too long".into(),
            attribute: Some(proto::AttributePath {
                steps: vec![
                    proto::AttributePathStep {
                        selector: Some(proto::Selector::AttributeName("rules".into())),
                    },
                    proto::AttributePathStep {
                        selector: Some(proto::Selector::ElementKeyInt(0)),
                    },
                ],
            }),
        };
        let e = check_diagnostics("ctx", &[warn, err], &mut warnings).unwrap_err();
        assert!(
            e.to_string().contains("bad: value too long (at rules[0])"),
            "{e}"
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
                address: ResourceAddress::new("random_password", "db"),
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
        assert!(text.contains("+ random_password.db (create)"), "{text}");
        assert!(text.contains("+ id = (known after apply)"), "{text}");
        assert!(text.contains("+ secret = (sensitive)"), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("Plan: 1 to create"), "{text}");
    }
}
