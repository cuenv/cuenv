//! Planning and applying managed resources.
//!
//! The engine mirrors Terraform core's managed-resource lifecycle, one
//! resource at a time:
//!
//! 1. **Refresh** stored state: `UpgradeResourceState` then `ReadResource`.
//!    State written with a newer resource schema version than the provider
//!    knows is refused before the provider sees it.
//! 2. **Plan**: `ValidateResourceConfiguration`, then `PlanResourceChange` with the
//!    proposed new state. A non-empty `requires_replace` turns an update into
//!    a destroy-then-create replacement. Deletes are planned too
//!    (`PlanResourceChange` with null configuration) for providers with the
//!    `plan_destroy` capability, which may refuse them.
//! 3. **Apply**: `ApplyResourceChange` with the planned state, persisting
//!    the provider's new state (or deleting the record) after every
//!    resource so a failed run never loses track of what exists. A create
//!    or update result must be a valid completion of its plan (Terraform's
//!    `AssertObjectCompatible`); an inconsistent create is recorded tainted.
//!
//! Values travel to providers as MessagePack. A provider's own MessagePack
//! bytes (refreshed and planned states) are passed back verbatim so
//! refinements of unknown values survive; values a provider sent as JSON
//! are decoded and re-encoded, which is lossless because JSON cannot carry
//! unknown values.
//!
//! What it deliberately does not do yet: references between resources
//! (values known only after apply), data sources, imports, saved plans,
//! or parallel applies. Ordering comes from explicit `dependsOn`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use cuenv_manifest::manifest::{
    Infrastructure, InfrastructureProvider, ManagedResourceDeclaration,
};
use sha2::{Digest, Sha256};

use crate::cancellation::Cancellation;
use crate::error::{
    InfrastructureError, Result, failure_category, json_error_category,
    strip_control_characters_except_newlines,
};
use crate::object_change::{PlanProblem, PlanValues, compatibility_problems, plan_problems};
use crate::plugin::{ApplyRequest, LaunchOptions, PlanRequest, ProviderClient};
use crate::protocol::{self, Diagnostic, Severity};
use crate::registry::{
    ProviderInstaller, ProviderSource, default_cache_directory, validate_version,
};
use crate::schema::{Block, ProviderSchema, Schema};
use crate::state::{
    ConditionalPut, ManagedResource, RecordVersion, ResourceAddress, StateLock, StateStore,
};
use crate::tenant::TenantKey;
use crate::type_system::{self, PathStep, Type, Value};
use crate::unrecorded::UnrecordedStore;

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
    /// Nothing changes in the real world, but the stored record is
    /// rewritten with refreshed state (computed values, private data,
    /// schema version or dependencies).
    Refresh,
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
            Self::NoOp | Self::Refresh => " ",
            Self::Create => "+",
            Self::Update => "~",
            Self::Replace => "-/+",
            Self::Delete => "-",
        }
    }

    /// Stable lowercase name, used in digests and machine-readable output.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NoOp => "no-op",
            Self::Refresh => "refresh",
            Self::Create => "create",
            Self::Update => "update",
            Self::Replace => "replace",
            Self::Delete => "delete",
        }
    }

    /// Whether the action changes real infrastructure.
    #[must_use]
    pub const fn changes_infrastructure(self) -> bool {
        !matches!(self, Self::NoOp | Self::Refresh)
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

impl StepKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

/// One provider call needed to realize a change. Every value is cty
/// MessagePack.
#[derive(Debug, Clone)]
struct ApplyStep {
    kind: StepKind,
    prior: Vec<u8>,
    planned: Vec<u8>,
    configuration: Vec<u8>,
    planned_private: Vec<u8>,
    /// The planned state `planned` encodes, which the provider's result
    /// must complete; null for a delete.
    planned_value: Value,
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
    /// For [`Action::Refresh`]: the refreshed record apply writes.
    refreshed_record: Option<ManagedResource>,
    /// The record stored when the plan was made.
    stored: Option<ManagedResource>,
}

impl ResourceChange {
    /// Whether applying only rewrites the stored record (refreshed computed
    /// values, private data, schema version or dependencies) without
    /// calling the provider.
    #[must_use]
    pub const fn refreshes_state(&self) -> bool {
        matches!(self.action, Action::Refresh)
    }

    /// Whether applying does anything at all for this resource.
    #[must_use]
    pub const fn has_work(&self) -> bool {
        !matches!(self.action, Action::NoOp)
    }
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
    /// Fingerprint of the Cuenv variables provided to provider processes.
    /// The values themselves are never stored in the plan.
    environment_identity: [u8; 32],
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
    /// Unchanged resources whose stored record will be rewritten with
    /// refreshed state.
    pub refresh: usize,
    /// Resources already up to date, with nothing to write.
    pub unchanged: usize,
}

/// A digest of everything a plan would do, for comparing the plan shown to
/// an operator with the plan made again under the lock.
///
/// Covers, for every change in order: the address, the action, the before
/// and after values (unknowns included), the replacement paths, the record
/// stored when the plan was made, and the refreshed record apply would
/// write. Any write to a stored record between two plans changes the
/// digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlanDigest(String);

impl PlanDigest {
    /// The digest as lowercase hexadecimal SHA-256.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PlanDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
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
                Action::Refresh => summary.refresh += 1,
                Action::NoOp => summary.unchanged += 1,
            }
        }
        summary
    }

    /// Whether applying the plan would change any real infrastructure.
    #[must_use]
    pub fn has_changes(&self) -> bool {
        self.changes
            .iter()
            .any(|change| change.action.changes_infrastructure())
    }

    /// Whether applying the plan would do anything: change infrastructure
    /// or rewrite stored records with refreshed state. Apply is needed (and
    /// takes the lock) exactly when this is true.
    #[must_use]
    pub fn has_work(&self) -> bool {
        self.changes.iter().any(ResourceChange::has_work)
    }

    /// Digest of everything this plan would do; see [`PlanDigest`].
    #[must_use]
    pub fn digest(&self) -> PlanDigest {
        let mut digest = DigestWriter::default();
        digest.field(b"cuenv infrastructure plan digest 1");
        digest.field(self.tenant.module_path().as_bytes());
        digest.field(self.tenant.project().as_bytes());
        match self.tenant.environment() {
            Some(environment) => {
                digest.tag(1);
                digest.field(environment.as_bytes());
            }
            None => digest.tag(0),
        }
        digest.field(&self.environment_identity);
        digest.count(self.changes.len());
        for change in &self.changes {
            digest.field(change.address.resource_type.as_bytes());
            digest.field(change.address.name.as_bytes());
            digest.field(change.action.name().as_bytes());
            digest.value(&change.before);
            digest.value(&change.after);
            digest.count(change.requires_replace.len());
            for path in &change.requires_replace {
                digest.field(path.as_bytes());
            }
            digest.record(change.stored.as_ref());
            digest.record(change.refreshed_record.as_ref());
        }
        PlanDigest(hex::encode(digest.hasher.finalize()))
    }
}

/// Feeds an unambiguous, length-prefixed encoding into SHA-256.
#[derive(Default)]
struct DigestWriter {
    hasher: Sha256,
}

impl DigestWriter {
    fn count(&mut self, count: usize) {
        self.hasher
            .update(u64::try_from(count).unwrap_or(u64::MAX).to_be_bytes());
    }

    fn field(&mut self, bytes: &[u8]) {
        self.count(bytes.len());
        self.hasher.update(bytes);
    }

    fn tag(&mut self, tag: u8) {
        self.hasher.update([tag]);
    }

    fn value(&mut self, value: &Value) {
        match value {
            Value::Null => self.tag(0),
            Value::Unknown => self.tag(1),
            Value::Boolean(boolean) => {
                self.tag(2);
                self.tag(u8::from(*boolean));
            }
            Value::Number(number) => {
                self.tag(3);
                self.field(number.to_string().as_bytes());
            }
            Value::String(text) => {
                self.tag(4);
                self.field(text.as_bytes());
            }
            Value::List(elements) => {
                self.tag(5);
                self.count(elements.len());
                for element in elements {
                    self.value(element);
                }
            }
            Value::Object(attributes) => {
                self.tag(6);
                self.count(attributes.len());
                for (name, attribute) in attributes {
                    self.field(name.as_bytes());
                    self.value(attribute);
                }
            }
            Value::Typed(typed) => {
                self.tag(7);
                self.field(typed.value_type.to_json().to_string().as_bytes());
                self.value(&typed.value);
            }
        }
    }

    fn record(&mut self, record: Option<&ManagedResource>) {
        match record.map(serde_json::to_vec) {
            None => self.tag(0),
            Some(Ok(bytes)) => {
                self.tag(1);
                self.field(&bytes);
            }
            // Serializing a record cannot fail in practice; if it ever did,
            // make the digest unique so two such plans never compare equal.
            Some(Err(_)) => {
                self.tag(2);
                self.field(uuid::Uuid::new_v4().as_bytes());
            }
        }
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
    /// The stored record of an unchanged resource was rewritten with
    /// refreshed state.
    Refreshed {
        /// Resource address.
        address: ResourceAddress,
    },
    /// A provider warning.
    Warning(String),
}

/// What an apply needs besides the plan.
#[derive(Debug, Clone, Copy)]
pub struct ApplyContext<'apply> {
    /// The tenant's lock; every state write presents it.
    pub lock: &'apply StateLock,
}

/// Options for [`InfrastructureEngine::new`].
#[derive(Clone)]
pub struct EngineOptions {
    /// Directory relative provider `path`s resolve against.
    pub project_directory: PathBuf,
    /// Provider plugin cache; defaults to [`default_cache_directory`].
    pub plugin_cache_directory: Option<PathBuf>,
    /// Environment variables providers must not inherit, such as the state
    /// store's authentication token.
    pub withheld_environment_variables: Vec<String>,
    /// Resolved, policy-authorized Cuenv variables overlaid on the host
    /// environment for each provider process.
    pub provider_environment_variables: BTreeMap<String, String>,
    /// Where changes the state store could not record are saved; defaults
    /// to [`UnrecordedStore::default_location`].
    pub unrecorded_directory: Option<PathBuf>,
    /// Interruption shared with the command's signal handling. Every
    /// provider the engine launches, while planning or applying, is
    /// registered with it.
    pub cancellation: Cancellation,
}

impl fmt::Debug for EngineOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EngineOptions")
            .field("project_directory", &self.project_directory)
            .field("plugin_cache_directory", &self.plugin_cache_directory)
            .field(
                "withheld_environment_variables",
                &self.withheld_environment_variables,
            )
            .field(
                "provider_environment_variable_names",
                &self.provider_environment_variables.keys(),
            )
            .field("unrecorded_directory", &self.unrecorded_directory)
            .field("cancellation", &self.cancellation)
            .finish()
    }
}

/// Salt environment fingerprints so a visible plan digest cannot be used to
/// guess a low-entropy secret from a short list of candidates.
fn environment_identity(variables: &BTreeMap<String, String>) -> [u8; 32] {
    static SALT: OnceLock<[u8; 16]> = OnceLock::new();
    let salt = SALT.get_or_init(|| *uuid::Uuid::new_v4().as_bytes());
    let mut digest = DigestWriter::default();
    digest.field(b"cuenv infrastructure environment identity 1");
    digest.field(salt);
    digest.count(variables.len());
    for (name, value) in variables {
        digest.field(name.as_bytes());
        digest.field(value.as_bytes());
    }
    digest.hasher.finalize().into()
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

impl fmt::Debug for EngineSetup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EngineSetup")
            .field("tenant", &self.tenant)
            .field("providers", &self.infrastructure.providers.keys())
            .field("resources", &self.infrastructure.resources.keys())
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
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

impl fmt::Debug for InfrastructureEngine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
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

    /// The interruption this engine's providers are registered with.
    #[must_use]
    pub const fn cancellation(&self) -> &Cancellation {
        &self.options.cancellation
    }

    /// Where this engine saves changes the state store could not record.
    ///
    /// # Errors
    ///
    /// Returns an error when no location is configured and the default
    /// cannot be determined.
    pub fn unrecorded_store(&self) -> Result<UnrecordedStore> {
        self.options
            .unrecorded_directory
            .as_ref()
            .map_or_else(UnrecordedStore::default_location, |directory| {
                Ok(UnrecordedStore::at(directory))
            })
    }

    /// Compute a plan.
    ///
    /// Refuses to plan while the tenant has unrecorded changes (see
    /// [`crate::unrecorded`]). Honours [`Cancellation::stop`] between
    /// resources.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration, provider failures,
    /// provider error diagnostics, invalid provider plans, state store
    /// failures, [`InfrastructureError::UnrecordedChangesPending`], or
    /// [`InfrastructureError::InterruptedWhilePlanning`].
    #[tracing::instrument(skip(self), fields(tenant = %self.tenant))]
    pub async fn plan(&mut self, mode: PlanMode) -> Result<Plan> {
        self.ensure_not_stopped()?;
        validate_configuration(&self.infrastructure)?;
        let unrecorded = self.unrecorded_store()?;
        let pending = unrecorded.list(&self.tenant)?;
        if !pending.is_empty() {
            return Err(InfrastructureError::UnrecordedChangesPending {
                tenant: self.tenant.to_string(),
                count: pending.len(),
                directory: unrecorded
                    .tenant_directory(&self.tenant)
                    .display()
                    .to_string(),
            });
        }
        let result = self.plan_resources(mode).await;
        match result {
            Err(error) if self.cancellation().is_stop_requested() => {
                tracing::debug!(%error, "planning ended by an interrupt");
                Err(InfrastructureError::InterruptedWhilePlanning)
            }
            other => other,
        }
    }

    fn ensure_not_stopped(&self) -> Result<()> {
        if self.cancellation().is_stop_requested() {
            Err(InfrastructureError::InterruptedWhilePlanning)
        } else {
            Ok(())
        }
    }

    async fn plan_resources(&mut self, mode: PlanMode) -> Result<Plan> {
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
                self.ensure_not_stopped()?;
                changes.push(self.plan_delete(row, &mut warnings).await?);
            }
        }

        for name in order {
            let Some(declaration) = declared.get(&name) else {
                continue;
            };
            self.ensure_not_stopped()?;
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
            environment_identity: environment_identity(
                &self.options.provider_environment_variables,
            ),
        })
    }

    /// Apply a plan produced by [`InfrastructureEngine::plan`] on this engine.
    ///
    /// The plan must still describe the store: every record it was made
    /// from must be stored unchanged (serial included), and no other record
    /// may have appeared, or the plan is refused as out of date. The caller
    /// may hold the lock from before planning until after applying (so the
    /// plan cannot go stale), or plan first and lock only to apply.
    ///
    /// State is persisted after every resource and every write is fenced by
    /// the lock. Refreshed records of unchanged resources are written too.
    /// [`Cancellation::stop`] is honoured between resources and between the
    /// delete and the create of a replacement; an operation in flight when
    /// it arrives is still recorded.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::PlanOutdated`] for a stale plan, the
    /// first provider or state store failure,
    /// [`InfrastructureError::UnrecordedChange`] or
    /// [`InfrastructureError::UnrecordedChangeLost`] when a provider change
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
        self.require_current(plan).await?;
        for warning in &plan.warnings {
            on_event(ApplyEvent::Warning(warning.clone()));
        }
        let mut progress = Progress {
            completed: 0,
            total: plan
                .changes
                .iter()
                .filter(|change| change.action.changes_infrastructure())
                .count(),
        };
        for change in plan.changes.iter().filter(|change| change.has_work()) {
            if self.cancellation().is_stop_requested() {
                return Err(progress.interrupted());
            }
            if let Some(record) = &change.refreshed_record
                && change.action == Action::Refresh
            {
                // Nothing changed in the real world, so a failed write is a
                // plain state error, not an unrecorded change.
                self.store
                    .put(&self.tenant, context.lock, record)
                    .await
                    .map_err(|error| naming_address(error, &change.address))?;
                on_event(ApplyEvent::Refreshed {
                    address: change.address.clone(),
                });
                continue;
            }
            on_event(ApplyEvent::Started {
                address: change.address.clone(),
                action: change.action,
            });
            match self.apply_change(change, context.lock, on_event).await {
                Ok(ChangeOutcome::Completed) => {}
                Ok(ChangeOutcome::StoppedAfterDelete) => {
                    on_event(ApplyEvent::Warning(format!(
                        "{} was deleted but its replacement was not created because the run \
                         was interrupted; the next apply creates it",
                        change.address
                    )));
                    return Err(progress.interrupted());
                }
                Err(error) => return Err(self.interrupted_or(error, progress, on_event)),
            }
            progress.completed += 1;
            on_event(ApplyEvent::Finished {
                address: change.address.clone(),
                action: change.action,
            });
        }
        Ok(plan.summary())
    }

    /// Refuse a plan whose view of stored state no longer matches the store.
    async fn require_current(&self, plan: &Plan) -> Result<()> {
        let mut stored: BTreeMap<ResourceAddress, ManagedResource> = self
            .store
            .list(&self.tenant)
            .await?
            .into_iter()
            .map(|resource| (resource.address.clone(), resource))
            .collect();
        for change in &plan.changes {
            let current = stored.remove(&change.address);
            if current != change.stored {
                return Err(InfrastructureError::PlanOutdated {
                    address: change.address.to_string(),
                });
            }
        }
        // A record the plan never saw appeared since.
        stored.into_keys().next().map_or(Ok(()), |address| {
            Err(InfrastructureError::PlanOutdated {
                address: address.to_string(),
            })
        })
    }

    /// After a stop request, a provider failure is most likely the stopped
    /// operation returning: report the interrupt, keeping the provider's
    /// message as a warning. Recording failures are never masked.
    fn interrupted_or(
        &self,
        error: InfrastructureError,
        progress: Progress,
        on_event: &mut (dyn FnMut(ApplyEvent) + Send),
    ) -> InfrastructureError {
        let provider_failure = matches!(
            error,
            InfrastructureError::Diagnostics { .. }
                | InfrastructureError::RemoteProcedure { .. }
                | InfrastructureError::Plugin(_)
        );
        if provider_failure && self.cancellation().is_stop_requested() {
            on_event(ApplyEvent::Warning(format!(
                "the operation in flight when the run was interrupted ended with: {error}"
            )));
            progress.interrupted()
        } else {
            error
        }
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
    ) -> Result<ChangeOutcome> {
        let provider = self.loaded(&change.provider)?;
        let schema = resource_schema(provider, &change.provider, &change.address.resource_type)?;
        let value_type = schema.block.implied_type();
        // The stored version every write replaces, for saving a record the
        // store cannot take.
        let mut version = RecordVersion::of(change.stored.as_ref());

        for (index, step) in change.steps.iter().enumerate() {
            // The delete half of a replacement is recorded; stop before the
            // create half like before any other new work.
            if index > 0 && self.cancellation().is_stop_requested() {
                return Ok(ChangeOutcome::StoppedAfterDelete);
            }
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
            // From here on the change happened: a forced exit waits for it
            // to be recorded (or saved locally).
            let _recording = self.cancellation().begin_recording();
            let context = format!("apply {}", change.address);
            let failed = response.diagnostics.iter().any(is_error);
            let returned = applied_value(&response, &value_type, &context)?;
            let problem = apply_result_problem(&ApplyResult {
                address: &change.address,
                kind: step.kind,
                returned: &returned,
                failed,
            })
            .or_else(|| {
                (!failed && !response.legacy_type_system)
                    .then(|| {
                        inconsistent_result(&InconsistentResultCheck {
                            address: &change.address,
                            block: &schema.block,
                            step,
                            returned: &returned,
                        })
                    })
                    .flatten()
            });
            let has_errors = failed || problem.is_some();
            // Terraform saves whatever the provider returns, with unknown
            // values turned into nulls, so partial results stay tracked.
            let new_state = returned.unknown_as_null();

            if new_state.is_null() {
                // A null result alongside errors means the change did not
                // happen; keep whatever was recorded.
                if !has_errors {
                    self.forget(lock, &change.address).await?;
                    version = RecordVersion::Absent;
                }
            } else {
                let stored = change.stored.as_ref();
                let record = ManagedResource {
                    address: change.address.clone(),
                    provider: change.provider.clone(),
                    provider_source: provider.source.clone(),
                    schema_version: schema.version,
                    state: new_state.to_state_json(&value_type)?,
                    private: response.private.clone(),
                    dependencies: match (step.kind, stored) {
                        // A failed delete leaves the old object, and its
                        // dependencies, as they were.
                        (StepKind::Delete, Some(stored)) if has_errors => {
                            stored.dependencies.clone()
                        }
                        _ => change.dependencies.clone(),
                    },
                    // A create that failed part way (or returned a result
                    // inconsistent with its plan) left something behind
                    // that must be replaced, not trusted. Any other failed
                    // step keeps the object's recorded status, so a
                    // tainted object stays tainted when its delete fails.
                    tainted: match step.kind {
                        StepKind::Create => has_errors,
                        StepKind::Update | StepKind::Delete => {
                            has_errors && stored.is_some_and(|stored| stored.tainted)
                        }
                    },
                    identity: None,
                    serial: 0,
                };
                self.record_change(
                    lock,
                    &ConditionalPut {
                        resource: &record,
                        expected: version,
                    },
                )
                .await?;
                version = version.after_write();
            }

            let mut warnings = Vec::new();
            check_diagnostics(&context, &response.diagnostics, &mut warnings)?;
            for warning in warnings {
                on_event(ApplyEvent::Warning(warning));
            }
            if let Some(problem) = problem {
                return Err(InfrastructureError::plugin(problem));
            }
        }

        if change.action == Action::Delete {
            // Also covers resources already gone during refresh (no steps).
            let _recording = self.cancellation().begin_recording();
            self.forget(lock, &change.address).await?;
        }
        Ok(ChangeOutcome::Completed)
    }

    /// Write a record the provider's change produced. If the write fails,
    /// save it locally, with the stored version it replaces, so a resource
    /// the provider already changed is never silently lost.
    async fn record_change(&self, lock: &StateLock, put: &ConditionalPut<'_>) -> Result<()> {
        match self.store.put(&self.tenant, lock, put.resource).await {
            Ok(()) => Ok(()),
            Err(error) => Err(self.save_unrecorded(put, &error)),
        }
    }

    async fn forget(&self, lock: &StateLock, address: &ResourceAddress) -> Result<()> {
        self.store.delete(&self.tenant, lock, address).await
    }

    fn save_unrecorded(
        &self,
        put: &ConditionalPut<'_>,
        error: &InfrastructureError,
    ) -> InfrastructureError {
        let address = &put.resource.address;
        match self
            .unrecorded_store()
            .and_then(|unrecorded| unrecorded.save(&self.tenant, put))
        {
            Ok(file) => InfrastructureError::UnrecordedChange {
                address: address.to_string(),
                reason: error.to_string(),
                saved_to: file.display().to_string(),
            },
            Err(save_error) => {
                tracing::error!(
                    address = %address,
                    store_error = %error,
                    %save_error,
                    "a provider change could be neither recorded nor saved locally"
                );
                InfrastructureError::UnrecordedChangeLost {
                    address: address.to_string(),
                    save_failure: failure_category(&save_error),
                }
            }
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
            stored: input.stored.cloned(),
        };
        let create = CreatePlanInput {
            provider,
            address: &address,
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
        let (planned, planned_bytes) =
            provider_value(response.planned_state.as_ref(), &value_type)?;
        PlanValidity {
            address: &address,
            block,
            prior: &prior.value,
            configuration: &configuration,
            planned: &planned,
            legacy_type_system: response.legacy_type_system,
        }
        .check()?;

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
                    serial: row.serial,
                };
                if record != *row {
                    change.action = Action::Refresh;
                    change.refreshed_record = Some(record);
                }
            }
            return Ok(change);
        }

        let requires_replace = replacement_paths(&ReplacementCheck {
            address: &address,
            paths: &response.requires_replace,
            prior: &prior.value,
            planned: &planned,
            value_type: &value_type,
        })?;

        if requires_replace.is_empty() {
            change.action = Action::Update;
            change.steps.push(ApplyStep {
                kind: StepKind::Update,
                prior: prior.bytes,
                planned: planned_bytes,
                configuration: configuration_bytes,
                planned_private: response.planned_private,
                planned_value: planned.clone(),
            });
            change.after = planned;
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
            stored: Some(row.clone()),
        };
        if let Some(prior) = prior {
            let mut step = delete_step(&prior);
            if provider.schema.capabilities.plan_destroy {
                step.planned_private = plan_destroy(
                    &DestroyPlanInput {
                        provider,
                        address: &row.address,
                        value_type: &schema.block.implied_type(),
                        prior: &prior,
                    },
                    warnings,
                )
                .await?;
            }
            change.steps.push(step);
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
            provider_environment_variables: &self.options.provider_environment_variables,
            cancellation: &self.options.cancellation,
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

/// How far an apply got.
#[derive(Debug, Clone, Copy)]
struct Progress {
    /// Changes applied and recorded.
    completed: usize,
    /// Changes to real infrastructure the plan contains.
    total: usize,
}

impl Progress {
    const fn interrupted(self) -> InfrastructureError {
        InfrastructureError::Interrupted {
            completed: self.completed,
            total: self.total,
        }
    }
}

/// How applying one change ended, when no error did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeOutcome {
    /// Every step was applied and recorded.
    Completed,
    /// A stop request arrived between the recorded delete of a replacement
    /// and its create, which was not started.
    StoppedAfterDelete,
}

/// Decode the state `ApplyResourceChange` returned. When it cannot be
/// decoded, the provider's error diagnostics (which usually explain why)
/// are reported rather than hidden behind the decoding failure.
fn applied_value(
    response: &protocol::ApplyResourceChangeResponse,
    value_type: &Type,
    context: &str,
) -> Result<Value> {
    match provider_value(response.new_state.as_ref(), value_type) {
        Ok((returned, _)) => Ok(returned),
        Err(decode_error) => {
            check_diagnostics(context, &response.diagnostics, &mut Vec::new())?;
            Err(decode_error)
        }
    }
}

/// Terraform's checks of an upgraded state: it must exist, and nothing in
/// it may be unknown.
fn check_upgraded(address: &ResourceAddress, upgraded: &Value) -> Result<()> {
    if upgraded.is_null() {
        // Reading a null state would report the resource gone and plan to
        // create it again; refuse instead.
        return Err(InfrastructureError::plugin(format!(
            "provider returned no upgraded state for {address}"
        )));
    }
    if upgraded.contains_unknown() {
        return Err(InfrastructureError::plugin(format!(
            "provider returned unknown values while upgrading the state of {address}, which is \
             a provider bug"
        )));
    }
    Ok(())
}

/// A state store failure writing the record of `address`, naming it.
fn naming_address(error: InfrastructureError, address: &ResourceAddress) -> InfrastructureError {
    match error {
        InfrastructureError::State(message) => InfrastructureError::State(format!(
            "recording the refreshed state of {address}: {message}"
        )),
        other => other,
    }
}

/// Inputs for [`inconsistent_result`].
struct InconsistentResultCheck<'check> {
    address: &'check ResourceAddress,
    block: &'check Block,
    step: &'check ApplyStep,
    returned: &'check Value,
}

/// Terraform's `AssertObjectCompatible` check of a create or update
/// result: every known planned value must have been kept.
fn inconsistent_result(check: &InconsistentResultCheck<'_>) -> Option<String> {
    if check.step.kind == StepKind::Delete {
        return None;
    }
    let problems = compatibility_problems(check.block, &check.step.planned_value, check.returned);
    if problems.is_empty() {
        return None;
    }
    let rendered = problems
        .iter()
        .map(PlanProblem::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    Some(format!(
        "provider produced an inconsistent result after the {} of {}, which is a provider bug: \
         {rendered}",
        check.step.kind.name(),
        check.address
    ))
}

/// What `ApplyResourceChange` returned for one step.
struct ApplyResult<'result> {
    address: &'result ResourceAddress,
    kind: StepKind,
    returned: &'result Value,
    failed: bool,
}

/// Terraform's checks of an apply result: nothing may be unknown after
/// apply, a delete must return null and anything else must not. Returns the
/// problem, if any; the object is still recorded as Terraform does.
fn apply_result_problem(result: &ApplyResult<'_>) -> Option<String> {
    let address = result.address;
    if result.returned.contains_unknown() {
        return Some(format!(
            "provider returned unknown values for {address} after apply; they were recorded as \
             null"
        ));
    }
    if result.failed {
        return None;
    }
    match (result.kind, result.returned.is_null()) {
        (StepKind::Delete, false) => Some(format!(
            "provider returned an object after deleting {address}, which is a provider bug; the \
             object was kept in state for recovery"
        )),
        (StepKind::Create | StepKind::Update, true) => Some(format!(
            "provider returned no object after the {} of {address}, which is a provider bug; \
             state was left as it was",
            result.kind.name()
        )),
        _ => None,
    }
}

/// A resource's refreshed prior state.
struct Refreshed {
    value: Value,
    /// The provider's own MessagePack for `value`, passed back verbatim.
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
        planned_value: Value::Null,
    }
}

/// Inputs for [`plan_destroy`].
struct DestroyPlanInput<'input> {
    provider: &'input LoadedProvider,
    address: &'input ResourceAddress,
    value_type: &'input Type,
    prior: &'input Refreshed,
}

/// Terraform's destroy plan, for providers with the `plan_destroy`
/// capability: `PlanResourceChange` with null configuration and proposed
/// state, which the provider may refuse (deletion protection) or answer
/// with private data for the delete. Returns that private data.
async fn plan_destroy(input: &DestroyPlanInput<'_>, warnings: &mut Vec<String>) -> Result<Vec<u8>> {
    let address = input.address;
    let response = input
        .provider
        .client
        .plan_resource_change(PlanRequest {
            type_name: &address.resource_type,
            prior_state: input.prior.bytes.clone(),
            proposed_new_state: NULL_MESSAGE_PACK.to_vec(),
            configuration: NULL_MESSAGE_PACK.to_vec(),
            prior_private: input.prior.private.clone(),
        })
        .await?;
    check_diagnostics(
        &format!("plan destroy of {address}"),
        &response.diagnostics,
        warnings,
    )?;
    reject_deferral(address, response.deferred.as_ref())?;
    let (planned, _) = provider_value(response.planned_state.as_ref(), input.value_type)?;
    if !planned.is_null() {
        return Err(InfrastructureError::plugin(format!(
            "provider planned a non-null destroy value for {address}, which is a provider bug"
        )));
    }
    Ok(response.planned_private)
}

async fn refresh(
    provider: &LoadedProvider,
    row: &ManagedResource,
    warnings: &mut Vec<String>,
) -> Result<Option<Refreshed>> {
    let type_name = &row.address.resource_type;
    let schema = resource_schema(provider, &row.provider, type_name)?;
    // Handing newer state to an older provider would silently drop what
    // it does not know; Terraform refuses too.
    if row.schema_version > schema.version {
        return Err(InfrastructureError::StateFromNewerProvider {
            address: row.address.to_string(),
            stored_version: row.schema_version,
            provider_version: schema.version,
        });
    }
    let value_type = schema.block.implied_type();
    let state_json = serde_json::to_vec(&row.state).map_err(|error| {
        InfrastructureError::state(format!(
            "serialize stored state of {} ({})",
            row.address,
            json_error_category(&error)
        ))
    })?;
    let upgrade = provider
        .client
        .upgrade_resource_state(type_name, row.schema_version, state_json)
        .await?;
    check_diagnostics(
        &format!("upgrade state of {}", row.address),
        &upgrade.diagnostics,
        warnings,
    )?;
    let (upgraded, upgraded_bytes) = provider_value(upgrade.upgraded_state.as_ref(), &value_type)?;
    check_upgraded(&row.address, &upgraded)?;

    let response = provider
        .client
        .read_resource(type_name, upgraded_bytes, row.private.clone())
        .await?;
    check_diagnostics(
        &format!("refresh {}", row.address),
        &response.diagnostics,
        warnings,
    )?;
    reject_deferral(&row.address, response.deferred.as_ref())?;
    let (value, bytes) = provider_value(response.new_state.as_ref(), &value_type)?;
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
        value,
        bytes,
        private: response.private,
    }))
}

/// Inputs for planning a create.
struct CreatePlanInput<'input> {
    provider: &'input LoadedProvider,
    address: &'input ResourceAddress,
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
    let address = input.address;
    let proposed = input
        .schema
        .block
        .proposed_new(&Value::Null, input.configuration);
    let response = input
        .provider
        .client
        .plan_resource_change(PlanRequest {
            type_name: &address.resource_type,
            prior_state: NULL_MESSAGE_PACK.to_vec(),
            proposed_new_state: type_system::to_message_pack(&proposed, &value_type)?,
            configuration: input.configuration_bytes.to_vec(),
            prior_private,
        })
        .await?;
    check_diagnostics(
        &format!("plan create of {address}"),
        &response.diagnostics,
        warnings,
    )?;
    reject_deferral(address, response.deferred.as_ref())?;
    let (planned, planned_bytes) = provider_value(response.planned_state.as_ref(), &value_type)?;
    PlanValidity {
        address,
        block: &input.schema.block,
        prior: &Value::Null,
        configuration: input.configuration,
        planned: &planned,
        legacy_type_system: response.legacy_type_system,
    }
    .check()?;
    Ok((
        planned.clone(),
        ApplyStep {
            kind: StepKind::Create,
            prior: NULL_MESSAGE_PACK.to_vec(),
            planned: planned_bytes,
            configuration: input.configuration_bytes.to_vec(),
            planned_private: response.planned_private,
            planned_value: planned,
        },
    ))
}

/// Terraform's `AssertPlanValid` applied to one planned state.
struct PlanValidity<'check> {
    address: &'check ResourceAddress,
    block: &'check Block,
    prior: &'check Value,
    configuration: &'check Value,
    planned: &'check Value,
    legacy_type_system: bool,
}

impl PlanValidity<'_> {
    fn check(&self) -> Result<()> {
        // Applying a null planned state would delete the resource; never
        // tolerated, not even from legacy providers.
        if self.planned.is_null() {
            return Err(InfrastructureError::plugin(format!(
                "provider produced an invalid plan for {}: planned state is null although the \
                 resource is configured",
                self.address
            )));
        }
        let problems = plan_problems(
            self.block,
            PlanValues {
                prior: self.prior,
                configuration: self.configuration,
                planned: self.planned,
            },
        );
        if problems.is_empty() {
            return Ok(());
        }
        let rendered = problems
            .iter()
            .map(PlanProblem::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        if self.legacy_type_system {
            // SDKv2 cannot plan precisely enough to pass these checks;
            // Terraform tolerates it and only logs, and so does cuenv.
            tracing::debug!(
                address = %self.address,
                problems = %rendered,
                "tolerating an inexact plan from a legacy type system provider"
            );
            return Ok(());
        }
        Err(InfrastructureError::plugin(format!(
            "provider produced an invalid plan for {}: {rendered}",
            self.address
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

/// Inputs for filtering a provider's `requires_replace` paths.
struct ReplacementCheck<'check> {
    address: &'check ResourceAddress,
    paths: &'check [protocol::AttributePath],
    prior: &'check Value,
    planned: &'check Value,
    value_type: &'check Type,
}

/// The replacement paths whose value actually changes (or is not yet
/// known), as Terraform honours them; SDKv2 providers report spurious ones.
/// Values are compared with their schema type, so set order and number
/// representation do not count as changes.
fn replacement_paths(check: &ReplacementCheck<'_>) -> Result<Vec<String>> {
    let mut changed = Vec::new();
    for path in check.paths {
        let steps = path_steps(path);
        let before = type_system::value_at_path(check.prior, &steps);
        let after = type_system::value_at_path(check.planned, &steps);
        if before.is_none() && after.is_none() {
            return Err(InfrastructureError::plugin(format!(
                "provider reported that changing `{}` of {} requires replacement, but that \
                 attribute path exists in neither the prior nor the planned state",
                render_path(path),
                check.address
            )));
        }
        let value_type = check.value_type.at_path(&steps).unwrap_or(Type::Dynamic);
        let before = before.unwrap_or(&Value::Null);
        let after = after.unwrap_or(&Value::Null);
        if !type_system::semantically_equal(before, after, &value_type) {
            changed.push(render_path(path));
        }
    }
    Ok(changed)
}

fn path_steps(path: &protocol::AttributePath) -> Vec<PathStep> {
    path.steps
        .iter()
        .filter_map(|step| match &step.selector {
            Some(protocol::Selector::AttributeName(name)) => {
                Some(PathStep::Attribute(name.clone()))
            }
            Some(protocol::Selector::ElementKeyString(key)) => Some(PathStep::Key(key.clone())),
            Some(protocol::Selector::ElementKeyInt(index)) => Some(PathStep::Index(*index)),
            None => None,
        })
        .collect()
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

/// Decode a provider value and produce the MessagePack to send it back
/// with. MessagePack from the provider is kept verbatim; JSON (which
/// Terraform also accepts) is decoded and re-encoded. Absent or empty is
/// null.
fn provider_value(
    value: Option<&protocol::DynamicValue>,
    value_type: &Type,
) -> Result<(Value, Vec<u8>)> {
    match value {
        Some(dynamic_value) if !dynamic_value.message_pack.is_empty() => Ok((
            type_system::from_message_pack(&dynamic_value.message_pack, value_type)?,
            dynamic_value.message_pack.clone(),
        )),
        Some(dynamic_value) if !dynamic_value.json.is_empty() => {
            let decoded = type_system::from_json_bytes(&dynamic_value.json, value_type)?;
            let bytes = type_system::to_message_pack(&decoded, value_type)?;
            Ok((decoded, bytes))
        }
        _ => Ok((Value::Null, NULL_MESSAGE_PACK.to_vec())),
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

/// Render a diagnostic as a single human-readable string, without the
/// control characters a provider could use to drive the terminal.
fn render_diagnostic(diagnostic: &Diagnostic) -> String {
    let mut rendered = strip_control_characters_except_newlines(&diagnostic.summary);
    if !diagnostic.detail.is_empty() {
        rendered.push_str(": ");
        rendered.push_str(&strip_control_characters_except_newlines(
            &diagnostic.detail,
        ));
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
    crate::object_change::render_steps(&path_steps(path))
}

fn dependency_graph(
    resources: &BTreeMap<String, ManagedResourceDeclaration>,
) -> BTreeMap<String, Vec<String>> {
    resources
        .iter()
        .map(|(name, declaration)| (name.clone(), declaration.depends_on.clone()))
        .collect()
}

/// Validate every provider and resource declaration in the selected
/// configuration.
///
/// CUE language v0.9 cannot express these cross-reference checks with the
/// `error()` builtin, so the selected concrete configuration is checked here.
/// Call this before resolving provider secrets or opening a state backend. The
/// engine repeats the validation as defense in depth before planning.
///
/// # Errors
///
/// Returns an error for invalid provider sources or versions, resource
/// references to undeclared providers, unknown dependencies, or dependency
/// cycles.
pub fn validate_configuration(infrastructure: &Infrastructure) -> Result<()> {
    for (name, declaration) in &infrastructure.providers {
        ProviderSource::parse(&declaration.source).map_err(|error| {
            InfrastructureError::configuration(format!("provider '{name}': {error}"))
        })?;

        match (declaration.version.as_deref(), declaration.path.as_deref()) {
            (Some(_), Some(_)) => {
                return Err(InfrastructureError::configuration(format!(
                    "provider '{name}' sets both `path` and `version`; choose one"
                )));
            }
            (None, None) => {
                return Err(InfrastructureError::configuration(format!(
                    "provider '{name}' needs an exact `version` (or a local `path`)"
                )));
            }
            (Some(version), None) => validate_version(version).map_err(|error| {
                InfrastructureError::configuration(format!("provider '{name}': {error}"))
            })?,
            (None, Some(_)) => {}
        }
    }

    for (name, declaration) in &infrastructure.resources {
        let provider_name = declaration.provider_name();
        if !infrastructure.providers.contains_key(provider_name) {
            let detail = if declaration.provider.is_some() {
                format!("no provider named '{provider_name}' in infrastructure.providers")
            } else {
                format!(
                    "no provider named '{provider_name}' (the prefix of type '{}')",
                    declaration.resource_type
                )
            };
            return Err(InfrastructureError::configuration(format!(
                "resource '{name}': {detail}"
            )));
        }
    }

    topological_order(&dependency_graph(&infrastructure.resources))?;
    Ok(())
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
            Action::Refresh => {
                let _ = writeln!(
                    rendered,
                    "    {} (refresh stored state; no change)",
                    change.address
                );
                continue;
            }
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
        "\nPlan: {} to create, {} to update, {} to replace, {} to delete, {} to refresh, {} \
         unchanged.",
        summary.create,
        summary.update,
        summary.replace,
        summary.delete,
        summary.refresh,
        summary.unchanged
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
mod tests;
