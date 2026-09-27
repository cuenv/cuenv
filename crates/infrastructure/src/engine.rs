//! Planning and applying managed resources.
//!
//! The engine mirrors Terraform core's managed-resource lifecycle, one
//! resource at a time:
//!
//! 1. **Refresh** stored state: `UpgradeResourceState` then `ReadResource`.
//! 2. **Plan**: `ValidateResourceConfiguration`, then `PlanResourceChange` with the
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
use std::sync::atomic::{AtomicBool, Ordering};

use cuenv_manifest::manifest::{
    Infrastructure, InfrastructureProvider, ManagedResourceDeclaration,
};

use crate::error::{InfrastructureError, Result};
use crate::plugin::{ApplyRequest, LaunchOptions, PlanRequest, ProviderClient};
use crate::protocol::{self, Diagnostic, Severity};
use crate::registry::{ProviderInstaller, ProviderSource, default_cache_directory};
use crate::schema::{Block, ProviderSchema, Schema};
use crate::state::{ManagedResource, ResourceAddress, StateLock, StateStore};
use crate::tenant::TenantKey;
use crate::type_system::{self, PathStep, Value};

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

/// What a provider call does to the resource, which decides how failures
/// are recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepKind {
    Create,
    Update,
    Delete,
}

/// One provider call needed to realize a change.
#[derive(Debug, Clone)]
struct ApplyStep {
    kind: StepKind,
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
    /// Top-level attributes whose values contain anything sensitive; they
    /// are masked when rendering.
    pub sensitive: Vec<String>,
    /// Attribute paths forcing replacement.
    pub requires_replace: Vec<String>,
    dependencies: Vec<String>,
    steps: Vec<ApplyStep>,
    /// For unchanged resources: the refreshed record, when it differs from
    /// what is stored, so apply keeps state current.
    refreshed_record: Option<ManagedResource>,
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

    /// The addresses and actions of every change, for comparing a plan
    /// shown to an operator with a plan made again under the lock.
    #[must_use]
    pub fn intent(&self) -> Vec<(ResourceAddress, Action)> {
        self.changes
            .iter()
            .filter(|change| change.action != Action::NoOp)
            .map(|change| (change.address.clone(), change.action))
            .collect()
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

/// A request to stop between resources, shared with signal handlers.
#[derive(Debug, Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    /// Ask the run to stop after the resource in flight.
    pub fn request(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether a stop was requested.
    #[must_use]
    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// What an apply needs besides the plan.
#[derive(Debug, Clone, Copy)]
pub struct ApplyContext<'apply> {
    /// The tenant's lock; every state write presents it.
    pub lock: &'apply StateLock,
    /// Checked between resources.
    pub cancellation: &'apply Cancellation,
}

/// Options for [`InfrastructureEngine::new`].
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// Directory relative provider `path`s resolve against, and where state
    /// that could not be recorded is saved.
    pub project_directory: PathBuf,
    /// Provider plugin cache; defaults to [`default_cache_directory`].
    pub plugin_cache_directory: Option<PathBuf>,
    /// Environment variables providers must not inherit, such as the state
    /// store's authentication token.
    pub withheld_environment_variables: Vec<String>,
}

/// Everything an engine needs.
pub struct EngineSetup {
    /// Tenant whose state the engine reads and writes.
    pub tenant: TenantKey,
    /// State store.
    pub store: Arc<dyn StateStore>,
    /// Evaluated `infrastructure` block.
    pub infrastructure: Infrastructure,
    /// Engine options.
    pub options: EngineOptions,
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

impl std::fmt::Debug for InfrastructureEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InfrastructureEngine")
            .field("tenant", &self.tenant)
            .field(
                "loaded_providers",
                &self.providers.keys().collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// Inputs for planning one declared resource.
struct ResourcePlanInput<'input> {
    name: &'input str,
    declaration: &'input ManagedResourceDeclaration,
    stored: Option<&'input ManagedResource>,
}

impl InfrastructureEngine {
    /// Create an engine. Providers are launched lazily while planning.
    #[must_use]
    pub fn new(setup: EngineSetup) -> Self {
        Self {
            tenant: setup.tenant,
            store: setup.store,
            infrastructure: setup.infrastructure,
            options: setup.options,
            providers: BTreeMap::new(),
        }
    }

    /// Compute a plan.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration, provider failures,
    /// provider error diagnostics, invalid provider plans, or state store
    /// failures.
    #[tracing::instrument(skip(self), fields(tenant = %self.tenant))]
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
            let input = ResourcePlanInput {
                name: &name,
                declaration,
                stored: stored.get(&address),
            };
            changes.push(self.plan_resource(&input, &mut warnings).await?);
        }

        Ok(Plan {
            tenant: self.tenant.clone(),
            changes,
            warnings,
        })
    }

    /// Apply a plan produced by [`InfrastructureEngine::plan`] on this engine.
    ///
    /// State is persisted after every resource and every write is fenced by
    /// the lock. Cancellation is honoured between resources.
    ///
    /// # Errors
    ///
    /// Returns the first provider or state store failure,
    /// [`InfrastructureError::UnrecordedChange`] when a provider change
    /// could not be recorded, or [`InfrastructureError::Interrupted`].
    #[tracing::instrument(skip_all, fields(tenant = %self.tenant))]
    pub async fn apply(
        &mut self,
        plan: &Plan,
        context: ApplyContext<'_>,
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
        let total = plan.intent().len();
        let mut completed = 0;
        for change in &plan.changes {
            if change.action == Action::NoOp {
                if let Some(record) = &change.refreshed_record {
                    self.record(context.lock, record).await?;
                }
                continue;
            }
            if context.cancellation.is_requested() {
                return Err(InfrastructureError::Interrupted { completed, total });
            }
            on_event(ApplyEvent::Started {
                address: change.address.clone(),
                action: change.action,
            });
            self.apply_change(change, context.lock, on_event).await?;
            completed += 1;
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
        lock: &StateLock,
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
            let failed = response.diagnostics.iter().any(is_error);
            let returned = decode_dynamic(response.new_state.as_ref(), &value_type)?;
            let unknown_after_apply = !failed && returned.contains_unknown();
            // Terraform saves whatever the provider returns, with unknown
            // values turned into nulls, so partial results stay tracked.
            let new_state = returned.unknown_as_null();

            if new_state.is_null() {
                // A null result alongside errors means the change did not
                // happen; keep whatever was recorded.
                if !failed {
                    self.forget(lock, &change.address).await?;
                }
            } else {
                let record = ManagedResource {
                    address: change.address.clone(),
                    provider: change.provider.clone(),
                    provider_source: provider.source.clone(),
                    schema_version: schema.version,
                    state: new_state.to_state_json(&value_type)?,
                    private: response.private.clone(),
                    dependencies: change.dependencies.clone(),
                    // A create that failed part way left something behind
                    // that must be replaced, not trusted.
                    tainted: failed && step.kind == StepKind::Create,
                    identity: None,
                };
                self.record(lock, &record).await?;
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
            if unknown_after_apply {
                return Err(InfrastructureError::plugin(format!(
                    "provider returned unknown values for {} after apply; they were recorded as null",
                    change.address
                )));
            }
        }

        if change.action == Action::Delete {
            // Also covers resources already gone during refresh (no steps).
            self.forget(lock, &change.address).await?;
        }
        Ok(())
    }

    /// Write a record. If the write fails, save it locally so a resource the
    /// provider already changed is never silently lost.
    async fn record(&self, lock: &StateLock, record: &ManagedResource) -> Result<()> {
        match self.store.put(&self.tenant, lock, record).await {
            Ok(()) => Ok(()),
            Err(error) => Err(self.save_unrecorded(record, &error)),
        }
    }

    async fn forget(&self, lock: &StateLock, address: &ResourceAddress) -> Result<()> {
        self.store.delete(&self.tenant, lock, address).await
    }

    fn save_unrecorded(
        &self,
        record: &ManagedResource,
        error: &InfrastructureError,
    ) -> InfrastructureError {
        let directory = self
            .options
            .project_directory
            .join(".cuenv")
            .join("infrastructure");
        let file = directory.join(format!(
            "unrecorded-{}-{}.json",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
            record.address
        ));
        let document = serde_json::json!({
            "tenant": self.tenant.to_string(),
            "resource": record,
        });
        let saved = std::fs::create_dir_all(&directory)
            .and_then(|()| {
                serde_json::to_vec_pretty(&document)
                    .map_err(|serialize| std::io::Error::other(serialize.to_string()))
            })
            .and_then(|bytes| write_private_file(&file, &bytes));
        let saved_to = match saved {
            Ok(()) => file.display().to_string(),
            Err(write_error) => format!("nowhere ({write_error}); state JSON: {}", record.state),
        };
        InfrastructureError::UnrecordedChange {
            address: record.address.to_string(),
            reason: error.to_string(),
            saved_to,
        }
    }

    async fn plan_resource(
        &mut self,
        input: &ResourcePlanInput<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<ResourceChange> {
        let declaration = input.declaration;
        let provider_name = declaration.provider_name().to_string();
        let address = ResourceAddress::new(&declaration.resource_type, input.name);
        self.ensure_provider(&provider_name, warnings).await?;
        let provider = self.loaded(&provider_name)?;
        if let Some(row) = input.stored {
            require_same_source(row, provider)?;
        }
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

        let prior = match input.stored {
            Some(row) => refresh(provider, row, warnings).await?,
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
            refreshed_record: None,
        };
        let create = CreatePlanInput {
            provider,
            resource_type: &declaration.resource_type,
            schema,
            configuration: &configuration,
            configuration_bytes: &configuration_bytes,
        };

        let Some(prior) = prior else {
            let (planned, step) = plan_create(&create, Vec::new(), warnings).await?;
            change.action = Action::Create;
            change.after = planned;
            change.steps.push(step);
            return Ok(change);
        };
        change.before = prior.value.clone();

        let tainted = input.stored.is_some_and(|row| row.tainted);
        if tainted {
            warnings.push(format!(
                "{address} is tainted by an earlier failed create and will be replaced"
            ));
            change.action = Action::Replace;
            change.requires_replace.push("(tainted)".to_string());
            change.steps.push(delete_step(&prior));
            let (planned, step) = plan_create(&create, Vec::new(), warnings).await?;
            change.after = planned;
            change.steps.push(step);
            return Ok(change);
        }

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
        reject_deferral(&address, response.deferred.as_ref())?;
        let planned_bytes = dynamic_bytes(response.planned_state.as_ref());
        let planned = decode_dynamic(response.planned_state.as_ref(), &value_type)?;
        let validity = PlanValidity {
            address: &address,
            block,
            configuration: &configuration,
            planned: &planned,
            legacy_type_system: response.legacy_type_system,
        };
        validity.check(warnings)?;

        if type_system::semantically_equal(&planned, &prior.value, &value_type) {
            change.after = planned;
            // Keep state current even when nothing changes: refreshed computed
            // values, provider private data, schema version and dependencies.
            if let Some(row) = input.stored {
                let record = ManagedResource {
                    address: row.address.clone(),
                    provider: change.provider.clone(),
                    provider_source: row.provider_source.clone(),
                    schema_version: schema.version,
                    state: prior.value.to_state_json(&value_type)?,
                    private: prior.private.clone(),
                    dependencies: change.dependencies.clone(),
                    tainted: false,
                    identity: row.identity.clone(),
                };
                if record != *row {
                    change.refreshed_record = Some(record);
                }
            }
            return Ok(change);
        }

        // Terraform only honours replacement paths whose value actually
        // changes (or is not yet known); SDKv2 providers report spurious ones.
        let requires_replace: Vec<String> = response
            .requires_replace
            .iter()
            .filter(|path| path_changes(path, &prior.value, &planned))
            .map(render_path)
            .collect();

        if requires_replace.is_empty() {
            change.action = Action::Update;
            change.after = planned;
            change.steps.push(ApplyStep {
                kind: StepKind::Update,
                prior: prior.bytes,
                planned: planned_bytes,
                configuration: configuration_bytes,
                planned_private: response.planned_private,
            });
            return Ok(change);
        }

        change.action = Action::Replace;
        change.requires_replace = requires_replace;
        change.steps.push(delete_step(&prior));
        let (planned, step) = plan_create(&create, response.planned_private, warnings).await?;
        change.after = planned;
        change.steps.push(step);
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
        require_same_source(row, provider)?;
        let schema = resource_schema(provider, &row.provider, &row.address.resource_type)?;
        let prior = refresh(provider, row, warnings).await?;

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
            refreshed_record: None,
        };
        if let Some(prior) = prior {
            change.steps.push(delete_step(&prior));
            change.before = prior.value;
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

        let client = ProviderClient::launch(&LaunchOptions {
            binary: &binary,
            withheld_environment_variables: &self.options.withheld_environment_variables,
        })
        .await?;
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
            if declaration.version.is_some() {
                return Err(InfrastructureError::configuration(format!(
                    "provider '{name}' sets both `path` and `version`; choose one"
                )));
            }
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

/// Stored state must only ever be handed back to the provider that wrote it.
fn require_same_source(row: &ManagedResource, provider: &LoadedProvider) -> Result<()> {
    if row.provider_source == provider.source {
        Ok(())
    } else {
        Err(InfrastructureError::configuration(format!(
            "{} was created by {} but provider '{}' is now {}; refusing to hand its state to a \
             different provider",
            row.address, row.provider_source, row.provider, provider.source
        )))
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    std::io::Write::write_all(&mut file, bytes)
}

struct Refreshed {
    value: Value,
    bytes: Vec<u8>,
    private: Vec<u8>,
}

fn delete_step(prior: &Refreshed) -> ApplyStep {
    ApplyStep {
        kind: StepKind::Delete,
        prior: prior.bytes.clone(),
        planned: NULL_MESSAGE_PACK.to_vec(),
        configuration: NULL_MESSAGE_PACK.to_vec(),
        planned_private: prior.private.clone(),
    }
}

async fn refresh(
    provider: &LoadedProvider,
    row: &ManagedResource,
    warnings: &mut Vec<String>,
) -> Result<Option<Refreshed>> {
    let type_name = &row.address.resource_type;
    let schema = resource_schema(provider, &row.provider, type_name)?;
    let value_type = schema.block.implied_type();
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
    reject_deferral(&row.address, response.deferred.as_ref())?;
    let value = decode_dynamic(response.new_state.as_ref(), &value_type)?;
    if value.is_null() {
        return Ok(None);
    }
    if value.contains_unknown() {
        return Err(InfrastructureError::plugin(format!(
            "provider returned unknown values while refreshing {}",
            row.address
        )));
    }
    Ok(Some(Refreshed {
        bytes: type_system::to_message_pack(&value, &value_type)?,
        value,
        private: response.private,
    }))
}

/// Inputs for planning a create.
struct CreatePlanInput<'input> {
    provider: &'input LoadedProvider,
    resource_type: &'input str,
    schema: &'input Schema,
    configuration: &'input Value,
    configuration_bytes: &'input [u8],
}

async fn plan_create(
    input: &CreatePlanInput<'_>,
    prior_private: Vec<u8>,
    warnings: &mut Vec<String>,
) -> Result<(Value, ApplyStep)> {
    let value_type = input.schema.block.implied_type();
    let proposed = input
        .schema
        .block
        .proposed_new(&Value::Null, input.configuration);
    let response = input
        .provider
        .client
        .plan_resource_change(PlanRequest {
            type_name: input.resource_type,
            prior_state: NULL_MESSAGE_PACK.to_vec(),
            proposed_new_state: type_system::to_message_pack(&proposed, &value_type)?,
            configuration: input.configuration_bytes.to_vec(),
            prior_private,
        })
        .await?;
    let context = format!("plan create of {}", input.resource_type);
    check_diagnostics(&context, &response.diagnostics, warnings)?;
    let address = ResourceAddress::new(input.resource_type, "(new)");
    reject_deferral(&address, response.deferred.as_ref())?;
    let planned = decode_dynamic(response.planned_state.as_ref(), &value_type)?;
    PlanValidity {
        address: &address,
        block: &input.schema.block,
        configuration: input.configuration,
        planned: &planned,
        legacy_type_system: response.legacy_type_system,
    }
    .check(warnings)?;
    Ok((
        planned,
        ApplyStep {
            kind: StepKind::Create,
            prior: NULL_MESSAGE_PACK.to_vec(),
            planned: dynamic_bytes(response.planned_state.as_ref()),
            configuration: input.configuration_bytes.to_vec(),
            planned_private: response.planned_private,
        },
    ))
}

/// The subset of Terraform's `AssertPlanValid` that catches plans that
/// would silently do something other than what the configuration says.
struct PlanValidity<'check> {
    address: &'check ResourceAddress,
    block: &'check Block,
    configuration: &'check Value,
    planned: &'check Value,
    legacy_type_system: bool,
}

impl PlanValidity<'_> {
    fn check(&self, warnings: &mut Vec<String>) -> Result<()> {
        if self.planned.is_null() {
            return Err(InfrastructureError::plugin(format!(
                "provider produced an invalid plan for {}: planned state is null although the \
                 resource is configured",
                self.address
            )));
        }
        let mut problems = Vec::new();
        for (name, attribute) in &self.block.attributes {
            if attribute.presence.is_computed() {
                continue;
            }
            let configured = self.configuration.attribute(name).unwrap_or(&Value::Null);
            let planned = self.planned.attribute(name).unwrap_or(&Value::Null);
            if !type_system::semantically_equal(configured, planned, &attribute.value_type) {
                problems.push(format!(
                    "planned value for non-computed attribute `{name}` does not match the \
                     configuration"
                ));
            }
        }
        if problems.is_empty() {
            return Ok(());
        }
        if self.legacy_type_system {
            // SDKv2 providers are allowed these inconsistencies, as in Terraform.
            warnings.extend(
                problems
                    .into_iter()
                    .map(|problem| format!("{}: {problem}", self.address)),
            );
            return Ok(());
        }
        Err(InfrastructureError::plugin(format!(
            "provider produced an invalid plan for {}: {}",
            self.address,
            problems.join("; ")
        )))
    }
}

fn reject_deferral(address: &ResourceAddress, deferred: Option<&protocol::Deferred>) -> Result<()> {
    deferred.map_or(Ok(()), |deferred| {
        Err(InfrastructureError::plugin(format!(
            "provider deferred {address} (reason {}); cuenv does not support deferred changes",
            deferred.reason
        )))
    })
}

/// Whether the value at a requires-replace path differs between prior and
/// planned state (or is not yet known).
fn path_changes(path: &protocol::AttributePath, prior: &Value, planned: &Value) -> bool {
    let steps: Vec<PathStep> = path
        .steps
        .iter()
        .filter_map(|step| match &step.selector {
            Some(protocol::Selector::AttributeName(name)) => {
                Some(PathStep::Attribute(name.clone()))
            }
            Some(protocol::Selector::ElementKeyString(key)) => Some(PathStep::Key(key.clone())),
            Some(protocol::Selector::ElementKeyInt(index)) => Some(PathStep::Index(*index)),
            None => None,
        })
        .collect();
    let before = type_system::value_at_path(prior, &steps);
    let after = type_system::value_at_path(planned, &steps);
    matches!(after, Some(Value::Unknown)) || before != after
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

/// Decode a provider value from MessagePack or, when a provider sends it
/// that way, JSON. An empty value is null.
fn decode_dynamic(
    value: Option<&protocol::DynamicValue>,
    value_type: &type_system::Type,
) -> Result<Value> {
    match value {
        Some(dynamic_value) if !dynamic_value.message_pack.is_empty() => {
            type_system::from_message_pack(&dynamic_value.message_pack, value_type)
        }
        Some(dynamic_value) if !dynamic_value.json.is_empty() => {
            type_system::from_json_bytes(&dynamic_value.json, value_type)
        }
        _ => Ok(Value::Null),
    }
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
            tainted: false,
            identity: None,
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
                refreshed_record: None,
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
