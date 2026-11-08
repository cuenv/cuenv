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
//! 3. **Order**: the plan's changes become one dependency graph of
//!    operations (see the `schedule` module): a replacement is a delete and
//!    a create, with the edges of Terraform's `DestroyEdgeTransformer`. The
//!    graph is built, and refused when it has a cycle, while planning, and
//!    the plan lists its changes in the order apply runs them.
//! 4. **Apply**: `ApplyResourceChange` with the planned state, persisting
//!    the provider's new state (or deleting the record) after every
//!    operation so a failed run never loses track of what exists. A create
//!    or update result must be a valid completion of its plan (Terraform's
//!    `AssertObjectCompatible`); an inconsistent create is recorded tainted.
//!    When a provider fails an operation, the operations that depend on it
//!    are skipped and the others still run; replacements whose old object
//!    was deleted but whose new object was not created are reported.
//!
//! Values travel to providers as MessagePack. A provider's own MessagePack
//! bytes (refreshed and planned states) are passed back verbatim so
//! refinements of unknown values survive; values a provider sent as JSON
//! are decoded and re-encoded, which is lossless because JSON cannot carry
//! unknown values.
//!
//! Scalar arguments, including nested object and ordered collection elements,
//! may reference declared scalar outputs of managed resources or data sources.
//! Known outputs participate in the initial plan; values produced by a create
//! are resolved after their dependencies apply, and a newly discovered
//! replacement is refused until a fresh plan can show it. Imports refresh one
//! provider result before recording it. Saved plans and parallel applies are
//! not supported. `create_before_destroy` is not supported: a replacement is
//! always destroy, then create.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use cuenv_manifest::lockfile::LockedInfrastructureProvider;
use cuenv_manifest::manifest::{
    Infrastructure, InfrastructureOutputType, InfrastructureProvider, ManagedResourceDeclaration,
};
use sha2::{Digest, Sha256};

use crate::cancellation::Cancellation;
use crate::error::{
    ChangeFailure, IncompleteApply, InfrastructureError, Result, failure_category,
    json_error_category, strip_control_characters_except_newlines,
};
use crate::in_flight;
use crate::object_change::{PlanProblem, PlanValues, compatibility_problems, plan_problems};
use crate::plugin::{
    ApplyRequest, LaunchOptions, PlanRequest, ProviderClient, ReadRequest, redact_provider_text,
};
use crate::protocol::{self, Diagnostic, Severity};
#[cfg(test)]
use crate::references::ConfigurationSpecialization;
use crate::references::{
    ConfigurationEvaluator, ConfigurationReferences, OutputIdentity, ResourceOutputReference,
    SourceAddress,
};
use crate::registry::{
    Installation, ProviderInstaller, ProviderSource, current_platform, default_cache_directory,
};
use crate::schema::{Block, ProviderSchema, Schema};
use crate::secrets::Secrets;
use crate::state::{
    ConditionalPut, ManagedResource, RecordVersion, ResourceAddress, StateLock, StateStore,
};
use crate::tenant::TenantKey;
use crate::type_system::{self, PathStep, Type, Value};
use crate::unrecorded::UnrecordedStore;

mod data_sources;
mod evaluation;
mod identity;
mod schedule;
mod sensitivity;
mod validation;

use data_sources::{
    DataSourceExecution, DataSourcePlanning, DeclarationGraph, ReadyDataSources,
    data_source_schema, planned_output, register_attributes,
};
use schedule::{Aftermath, ApplyOperation, OperationKind, Schedule, ScheduleRun};
use sensitivity::{
    SensitiveProviderValue, register_stored_values, register_typed_attributes,
    resource_sensitivity, sensitive_provider_value,
};
use validation::ConfigurationScope;

pub use data_sources::DataSourcePlan;

pub use validation::validate_configuration;

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
#[derive(Clone)]
struct ApplyStep {
    kind: StepKind,
    prior: Vec<u8>,
    planned: Vec<u8>,
    configuration: Vec<u8>,
    planned_private: Vec<u8>,
    /// Private data supplied with the prior state for a final plan after
    /// output references become concrete.
    prior_private: Vec<u8>,
    /// The planned state `planned` encodes, which the provider's result
    /// must complete; null for a delete.
    planned_value: Value,
    /// The prior object's identity, MessagePack against the identity schema.
    prior_identity: Option<Vec<u8>>,
    /// The identity the plan returned, passed back verbatim on apply.
    planned_identity: Option<Vec<u8>>,
}

impl fmt::Debug for ApplyStep {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplyStep")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
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
    /// Hashes of the original structured paths, whose map keys are hidden in
    /// `requires_replace`. This keeps the plan digest bound to exact paths.
    replacement_path_identities: Vec<[u8; 32]>,
    /// The resources this one is configured to depend on, as full
    /// `type.name` addresses; recorded with the resource.
    dependencies: Vec<String>,
    /// Resource outputs used by its configuration.
    references: ConfigurationReferences,
    /// For a resource with references: the planned configuration. Outputs
    /// known while planning are filled in; the rest are typed unknowns that
    /// apply resolves once their producers have been applied.
    reference_configuration: Option<Value>,
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

    /// Whether apply must resolve output references and ask the provider
    /// for a final plan: only when an output was unknown while planning.
    /// Known outputs are already in the confirmed plan.
    fn needs_final_plan(&self) -> bool {
        self.reference_configuration
            .as_ref()
            .is_some_and(|configuration| self.references.has_unknown_destination(configuration))
    }
}

/// A full plan for one tenant.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Tenant the plan belongs to.
    pub tenant: TenantKey,
    /// Changes in the order apply runs them: the order in which each change
    /// first appears in the apply schedule, so a preview is the order of
    /// events. A replacement appears where its delete runs and finishes
    /// where its create runs, which can be later; unchanged resources come
    /// last. The schedule is computed, and refused when no safe order
    /// exists, while planning.
    pub changes: Vec<ResourceChange>,
    /// Invocation-local data results, in dependency order. Never persisted.
    pub data_sources: Vec<DataSourcePlan>,
    /// Warnings collected while planning: the providers', and what ordering
    /// the changes had to overlook (stored dependencies that form a cycle).
    pub warnings: Vec<String>,
    /// Fingerprint of the Cuenv variables provided to provider processes.
    /// The values themselves are never stored in the plan. It is part of the
    /// plan's digest, so two plans made with other provider variables differ.
    environment_identity: [u8; 32],
    /// Immutable evaluator identity used to prepare this plan.
    configuration_evaluation: Option<ConfigurationEvaluation>,
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
/// digest. Native CUE plans also bind the evaluator instance: two plans
/// from the same binding can be compared, while independently bound CUE
/// snapshots intentionally produce different digests.
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

/// A digest of what a reviewer sees of a plan: the redacted rendering of
/// every change to real infrastructure in its tenant, as `sha256:<hex>`.
///
/// Unlike [`PlanDigest`], it holds across runs: it covers no record
/// versions, evaluator bindings or refresh-only changes, so a plan made
/// again from the same configuration against the same infrastructure has
/// the same reviewed digest, and an apply can require the plan a reviewer
/// approved. It is computed from redacted text, so it never fingerprints a
/// secret's value, and a change to a secret alone does not change it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReviewedPlanDigest(String);

impl ReviewedPlanDigest {
    /// Parse `sha256:<64 lowercase hex>`.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for any other text.
    pub fn parse(text: &str) -> Result<Self> {
        let valid = text.strip_prefix("sha256:").is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if valid {
            Ok(Self(text.to_string()))
        } else {
            Err(InfrastructureError::configuration(
                "a reviewed plan digest is `sha256:` followed by 64 lowercase hexadecimal digits",
            ))
        }
    }

    /// The digest as `sha256:<hex>`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ReviewedPlanDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Plan {
    /// The digest of what a reviewer sees of this plan; see
    /// [`ReviewedPlanDigest`].
    #[must_use]
    pub fn reviewed_digest(&self) -> ReviewedPlanDigest {
        let reviewed = Self {
            changes: self
                .changes
                .iter()
                .filter(|change| change.action.changes_infrastructure())
                .cloned()
                .collect(),
            data_sources: Vec::new(),
            warnings: Vec::new(),
            ..self.clone()
        };
        let mut digest = DigestWriter::default();
        digest.field(b"cuenv infrastructure reviewed plan 1");
        digest.field(self.tenant.module_path().as_bytes());
        digest.field(self.tenant.project().as_bytes());
        match self.tenant.environment() {
            Some(environment) => {
                digest.tag(1);
                digest.field(environment.as_bytes());
            }
            None => digest.tag(0),
        }
        digest.field(cuenv_events::redact(&render_plan(&reviewed)).as_bytes());
        ReviewedPlanDigest(format!("sha256:{}", hex::encode(digest.hasher.finalize())))
    }

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
            || self.data_sources.iter().any(|read| read.deferred)
    }

    /// Digest of everything this plan would do; see [`PlanDigest`].
    #[must_use]
    pub fn digest(&self) -> PlanDigest {
        let mut digest = DigestWriter::default();
        digest.field(b"cuenv infrastructure plan digest 5");
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
        match &self.configuration_evaluation {
            // A snapshot held only to verify outputs does not shape the plan.
            Some(binding) if !binding.evaluator.references().is_empty() => {
                digest.tag(1);
                digest.field(&binding.identity);
            }
            _ => digest.tag(0),
        }
        digest.count(self.changes.len());
        for change in &self.changes {
            digest.field(change.address.resource_type.as_bytes());
            digest.field(change.address.name.as_bytes());
            digest.field(change.provider.as_bytes());
            digest.field(change.action.name().as_bytes());
            digest.value(&change.before);
            digest.value(&change.after);
            digest.count(change.dependencies.len());
            for dependency in &change.dependencies {
                digest.field(dependency.as_bytes());
            }
            digest.count(change.sensitive.len());
            for attribute in &change.sensitive {
                digest.field(attribute.as_bytes());
            }
            digest.count(change.requires_replace.len());
            for path in &change.requires_replace {
                digest.field(path.as_bytes());
            }
            digest.count(change.replacement_path_identities.len());
            for identity in &change.replacement_path_identities {
                digest.field(identity);
            }
            let write_only: Vec<_> = change
                .references
                .secrets()
                .iter()
                .filter(|argument| argument.write_only.is_some())
                .collect();
            digest.count(write_only.len());
            for argument in write_only {
                digest.path(argument.destination.steps());
                if let Some(destination) = &argument.write_only {
                    digest.path(destination.steps());
                }
            }
            digest.count(change.references.references().len());
            for reference in change.references.references() {
                digest.reference(reference);
            }
            digest.record(change.stored.as_ref());
            digest.record(change.refreshed_record.as_ref());
        }
        digest.count(self.data_sources.len());
        for read in &self.data_sources {
            read.digest(&mut digest);
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
    fn reference(&mut self, reference: &ResourceOutputReference) {
        self.field(reference.source.to_string().as_bytes());
        self.field(reference.attribute.as_bytes());
        self.tag(if reference.native.computed { 2 } else { 1 });
        self.path(reference.destination.steps());
    }

    fn path(&mut self, steps: &[PathStep]) {
        self.count(steps.len());
        for step in steps {
            match step {
                PathStep::Attribute(name) => {
                    self.tag(0);
                    self.field(name.as_bytes());
                }
                PathStep::Key(key) => {
                    self.tag(1);
                    self.field(key.as_bytes());
                }
                PathStep::Index(index) => {
                    self.tag(2);
                    self.field(&index.to_be_bytes());
                }
            }
        }
    }

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
    /// A warning: from a provider, about how the apply was ordered, or about
    /// a replacement whose new object exists but could not be recorded.
    Warning(String),
    /// A change failed. Changes that do not depend on it still run; the
    /// ones that do are reported as [`ApplyEvent::Skipped`].
    Failed {
        /// Resource address.
        address: ResourceAddress,
        /// Action that failed.
        action: Action,
    },
    /// A change was not attempted because a change it depends on failed.
    Skipped {
        /// Resource address.
        address: ResourceAddress,
        /// Action that was not attempted.
        action: Action,
    },
    /// A replacement's old object was deleted, and its new object was not
    /// created, because the create failed or could not run, or the run
    /// stopped between the two, or recording the deletion failed. The next
    /// apply creates it. Emitted on every way an apply can end early.
    DeletedNotRecreated {
        /// Resource address.
        address: ResourceAddress,
    },
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
    /// Provider releases `cuenv.lock` pins, keyed by fully qualified source
    /// address. A registry provider with a pin must match its version,
    /// archive SHA-256 and schema digest.
    pub locked_providers: BTreeMap<String, LockedInfrastructureProvider>,
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
            .field("locked_providers", &self.locked_providers)
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
    /// Identity schemas by resource type; empty for a provider without
    /// resource identity.
    identity_schemas: identity::IdentitySchemas,
    source: String,
    configured: bool,
}

#[derive(Clone)]
struct ConfigurationEvaluation {
    evaluator: Arc<dyn ConfigurationEvaluator>,
    identity: [u8; 16],
}

impl fmt::Debug for ConfigurationEvaluation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigurationEvaluation")
            .finish_non_exhaustive()
    }
}

/// Plans and applies an `infrastructure` configuration for one tenant.
pub struct InfrastructureEngine {
    tenant: TenantKey,
    store: Arc<dyn StateStore>,
    infrastructure: Infrastructure,
    options: EngineOptions,
    providers: BTreeMap<String, LoadedProvider>,
    /// Effective sensitivity for this plan, including reference propagation.
    output_sensitivity: BTreeMap<SourceAddress, Vec<String>>,
    configuration_evaluation: Option<ConfigurationEvaluation>,
    /// Resolves secrets in resource and data-source configuration; without
    /// one, a configuration that holds a secret cannot reach a provider.
    secrets: Option<Arc<Secrets>>,
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
    /// The resources it depends on, as full `type.name` addresses.
    dependencies: Vec<String>,
    references: ConfigurationReferences,
    planned_outputs: &'input BTreeMap<SourceAddress, Value>,
    stored: Option<&'input ManagedResource>,
}

/// The inputs needed to replan one operation after resolving its references.
struct ReferencedReplan<'step> {
    change: &'step ResourceChange,
    step: &'step ApplyStep,
    schema: &'step Schema,
    lock: &'step StateLock,
    rows: &'step mut BTreeMap<ResourceAddress, ManagedResource>,
    data_sources: &'step BTreeMap<ResourceAddress, Value>,
    on_event: &'step mut (dyn FnMut(ApplyEvent) + Send),
}

#[derive(Clone, Copy)]
struct ResolveReferenceConfiguration<'step> {
    change: &'step ResourceChange,
    destination_block: &'step Block,
    rows: &'step BTreeMap<ResourceAddress, ManagedResource>,
    data_sources: &'step BTreeMap<ResourceAddress, Value>,
}

struct OperationRunContext<'run> {
    lock: &'run StateLock,
    rows: &'run mut BTreeMap<ResourceAddress, ManagedResource>,
    data_sources: &'run BTreeMap<ResourceAddress, Value>,
    on_event: &'run mut (dyn FnMut(ApplyEvent) + Send),
}

/// Outputs known at one point of a run. A pending resource's recorded outputs are
/// about to be replaced, so they say nothing about what will be applied.
struct OutputVerification<'read> {
    rows: &'read BTreeMap<ResourceAddress, ManagedResource>,
    data_sources: &'read BTreeMap<ResourceAddress, Value>,
    pending: &'read BTreeSet<ResourceAddress>,
}

struct SensitiveOutput<'read> {
    source: &'read SourceAddress,
    attribute: &'read String,
    row: &'read ManagedResource,
    schema: &'read Schema,
    value: &'read Value,
}

struct ReferenceValueInput<'read> {
    destination: &'read ResourceAddress,
    reference: &'read ResourceOutputReference,
    rows: &'read BTreeMap<ResourceAddress, ManagedResource>,
    data_sources: &'read BTreeMap<ResourceAddress, Value>,
}

struct ConfigurationInput<'input> {
    consumer: &'input SourceAddress,
    declaration: &'input ManagedResourceDeclaration,
    references: &'input ConfigurationReferences,
    block: &'input Block,
    phase: ConfigurationPhase,
    secrets: SecretUse,
}

/// Whether a prepared configuration carries its secrets' values.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SecretUse {
    /// Checking declarations only: each secret stays an unknown string, and
    /// nothing is resolved.
    Placeholder,
    /// The configuration is about to reach a provider: resolve each secret.
    Resolve,
}

#[derive(Clone, Copy)]
enum ConfigurationPhase {
    Planning,
    Apply,
}

struct DataSourceReadContext<'read> {
    rows: &'read BTreeMap<ResourceAddress, ManagedResource>,
    values: &'read BTreeMap<ResourceAddress, Value>,
}

/// The declaration at `address`: declared under its name and with its type.
#[must_use]
pub fn declared_resource<'declared>(
    declared: &'declared BTreeMap<String, ManagedResourceDeclaration>,
    address: &ResourceAddress,
) -> Option<&'declared ManagedResourceDeclaration> {
    declared
        .get(&address.name)
        .filter(|declaration| declaration.resource_type == address.resource_type)
}

/// Changes in the order apply runs them.
struct OrderedChanges {
    changes: Vec<ResourceChange>,
    /// What the ordering overlooked, for the user to see.
    warnings: Vec<String>,
}

/// Put the changes in the order apply runs them, refusing changes that no
/// order can apply.
fn in_schedule_order(changes: Vec<ResourceChange>) -> Result<OrderedChanges> {
    let schedule = Schedule::build(&changes)?;
    let order = schedule.change_order;
    let warnings = schedule.warnings;
    let mut slots: Vec<Option<ResourceChange>> = changes.into_iter().map(Some).collect();
    Ok(OrderedChanges {
        changes: order
            .into_iter()
            .filter_map(|index| slots[index].take())
            .collect(),
        warnings,
    })
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
            output_sensitivity: BTreeMap::new(),
            configuration_evaluation: None,
            secrets: None,
        }
    }

    /// Resolve the secrets of resource and data-source configuration through
    /// `secrets`. Share one [`Secrets`] between the engines of a command so
    /// each secret is resolved once.
    pub fn set_secrets(&mut self, secrets: Arc<Secrets>) {
        self.secrets = Some(secrets);
    }

    /// Bind one immutable native CUE evaluator to future plans. Replacing this
    /// binding invalidates plans produced with an earlier snapshot.
    pub fn set_configuration_evaluator(&mut self, evaluator: Arc<dyn ConfigurationEvaluator>) {
        self.configuration_evaluation = Some(ConfigurationEvaluation {
            evaluator,
            identity: *uuid::Uuid::new_v4().as_bytes(),
        });
    }

    fn declaration_block(&self, source: &SourceAddress) -> Result<&Block> {
        let declaration =
            data_sources::declaration(&self.infrastructure, source).ok_or_else(|| {
                InfrastructureError::configuration(format!("{source} is not declared"))
            })?;
        let provider = self.loaded(declaration.provider_name())?;
        Ok(&match source {
            SourceAddress::Resource(address) => resource_schema(
                provider,
                declaration.provider_name(),
                &address.resource_type,
            )?,
            SourceAddress::DataSource(address) => {
                data_source_schema(provider, &address.resource_type)?
            }
        }
        .block)
    }

    fn classified_graph(&self) -> Result<DeclarationGraph> {
        let mut classified = BTreeMap::new();
        let native = self
            .configuration_evaluation
            .as_ref()
            .map(|binding| binding.evaluator.references())
            .unwrap_or_default();
        for reference in native {
            if data_sources::declaration(&self.infrastructure, &reference.consumer).is_none()
                || data_sources::declaration(&self.infrastructure, &reference.source).is_none()
            {
                return Err(InfrastructureError::configuration(
                    "native CUE reference names an undeclared source or consumer",
                ));
            }
        }
        for source in self.declaration_sources() {
            let declaration = data_sources::declaration(&self.infrastructure, &source)
                .ok_or_else(|| InfrastructureError::configuration("declaration disappeared"))?;
            let references: Vec<_> = native
                .iter()
                .filter(|reference| reference.consumer == source)
                .cloned()
                .collect();
            let block = self.declaration_block(&source)?;
            let classified_references = ConfigurationReferences::extract_with_schema(
                &declaration.configuration,
                block,
                &references,
            )?;
            if matches!(source, SourceAddress::DataSource(_)) {
                classified_references.reject_write_only()?;
            }
            classified.insert(source.clone(), classified_references);
        }
        DeclarationGraph::with_references(&self.infrastructure, &classified)
    }

    fn declaration_sources(&self) -> Vec<SourceAddress> {
        self.infrastructure
            .resources
            .iter()
            .map(|(name, declaration)| {
                SourceAddress::Resource(ResourceAddress::new(&declaration.resource_type, name))
            })
            .chain(self.infrastructure.data.iter().map(|(name, declaration)| {
                SourceAddress::DataSource(ResourceAddress::new(&declaration.resource_type, name))
            }))
            .collect()
    }

    async fn prepare_declarations(
        &mut self,
        warnings: &mut Vec<String>,
    ) -> Result<DeclarationGraph> {
        let names: BTreeSet<_> = self
            .declaration_sources()
            .iter()
            .filter_map(|source| data_sources::declaration(&self.infrastructure, source))
            .map(|declaration| declaration.provider_name().to_string())
            .collect();
        for name in names {
            self.ensure_provider_schema(&name, warnings).await?;
        }
        let graph = self.classified_graph()?;
        for source in &graph.order {
            let declaration = data_sources::declaration(&self.infrastructure, source)
                .ok_or_else(|| InfrastructureError::configuration("declaration disappeared"))?;
            let block = self.declaration_block(source)?;
            let references = graph.references.get(source).ok_or_else(|| {
                InfrastructureError::configuration("reference graph is incomplete")
            })?;
            if let Some(binding) = &self.configuration_evaluation {
                validate_output_declarations(source, binding.evaluator.as_ref(), block)?;
            }
            self.validate_output_references(source.address(), references, block)?;
            self.prepare_configuration(
                ConfigurationInput {
                    consumer: source,
                    declaration,
                    references,
                    block,
                    phase: ConfigurationPhase::Planning,
                    secrets: SecretUse::Placeholder,
                },
                |_| Ok(Value::Unknown),
            )
            .await?;
        }
        Ok(graph)
    }

    async fn prepare_configuration(
        &self,
        input: ConfigurationInput<'_>,
        mut resolver: impl FnMut(&ResourceOutputReference) -> Result<Value>,
    ) -> Result<Value> {
        let mut values: BTreeMap<OutputIdentity, Value> = BTreeMap::new();
        let mut sources = BTreeMap::new();
        let mut deferred = Vec::new();
        for reference in input.references.references() {
            let output = OutputIdentity {
                source: reference.source.clone(),
                attribute: reference.attribute.clone(),
            };
            let value = match values.get(&output) {
                Some(value) => value.clone(),
                None => resolver(reference)?,
            };
            if !value.is_unknown()
                && !reference.native.computed
                && !input
                    .block
                    .reference_destination(&reference.destination)?
                    .accepts(&value)
            {
                return Err(reference_error(
                    input.consumer.address(),
                    reference,
                    "output must match the destination's concrete scalar type",
                ));
            }
            if value.is_unknown() {
                deferred.push(reference.native.clone());
            } else {
                sources.insert(output.clone(), value.clone());
            }
            values.insert(output, value);
        }
        let has_native = !input.references.references().is_empty();
        let configuration = match &self.configuration_evaluation {
            Some(binding) if has_native => {
                self.ensure_not_stopped()?;
                let evaluator = Arc::clone(&binding.evaluator);
                let expected_deferred = deferred.clone();
                let specialized = evaluation::specialize(
                    evaluation::Specialization {
                        evaluator,
                        consumer: input.consumer.clone(),
                        sources,
                        deferred,
                    },
                    self.cancellation(),
                )
                .await?;
                self.ensure_not_stopped()?;
                let expected: BTreeSet<_> = expected_deferred.iter().collect();
                let returned: BTreeSet<_> = specialized.unresolved.iter().collect();
                if expected != returned || specialized.unresolved.len() != expected_deferred.len() {
                    return Err(InfrastructureError::configuration(
                        "CUE specialization returned inconsistent unknown source provenance",
                    ));
                }
                specialized.configuration
            }
            None if has_native => {
                return Err(InfrastructureError::configuration(
                    "native references require an immutable CUE evaluator",
                ));
            }
            Some(_) | None => input.declaration.configuration.clone(),
        };
        let projected = serde_json::Value::Object(configuration.clone());
        let mut computed = BTreeMap::new();
        for reference in input.references.references() {
            let output = OutputIdentity {
                source: reference.source.clone(),
                attribute: reference.attribute.clone(),
            };
            let Some(value) = values.get(&output).filter(|value| !value.is_unknown()) else {
                continue;
            };
            let destination = input.block.reference_destination(&reference.destination)?;
            let projected_value =
                reference
                    .destination
                    .json_value(&projected)
                    .ok_or_else(|| {
                        reference_error(
                            input.consumer.address(),
                            reference,
                            "native projection is missing",
                        )
                    })?;
            if reference.native.computed {
                // Null until CUE knows every source this value is computed from.
                if !projected_value.is_null() {
                    let decoded =
                        Value::from_configuration_json(projected_value, destination.value_type())
                            .map_err(|_| {
                            reference_error(
                                input.consumer.address(),
                                reference,
                                "computes a value of the wrong type for its destination",
                            )
                        })?;
                    computed.insert(reference.destination.clone(), decoded);
                }
                continue;
            }
            let decoded =
                Value::from_configuration_json(projected_value, destination.value_type())?;
            if !type_system::semantically_equal(&decoded, value, destination.value_type()) {
                return Err(reference_error(
                    input.consumer.address(),
                    reference,
                    "native projection does not match its source",
                ));
            }
        }
        let prepared = input
            .references
            .prepare(&configuration, &input.block.implied_type())?;
        let resolve = |reference: &ResourceOutputReference| {
            if reference.native.computed {
                return Ok(computed
                    .get(&reference.destination)
                    .cloned()
                    .unwrap_or(Value::Unknown));
            }
            values
                .get(&OutputIdentity {
                    source: reference.source.clone(),
                    attribute: reference.attribute.clone(),
                })
                .cloned()
                .ok_or_else(|| {
                    reference_error(input.consumer.address(), reference, "was not resolved")
                })
        };
        let mut configuration = match input.phase {
            ConfigurationPhase::Planning => input.references.resolve_planned(&prepared, resolve),
            ConfigurationPhase::Apply => input.references.resolve(&prepared, resolve),
        }?;
        configuration = input.block.normalize_configuration(configuration);
        self.fill_secrets(&input, &projected, &mut configuration)
            .await?;
        Ok(configuration)
    }

    /// Put each secret's value at its position, for a configuration about to
    /// reach a provider. The value never passes through CUE: it replaces the
    /// unknown placeholder after evaluation, and was registered for
    /// redaction when it was resolved.
    async fn fill_secrets(
        &self,
        input: &ConfigurationInput<'_>,
        projected: &serde_json::Value,
        configuration: &mut Value,
    ) -> Result<()> {
        if input.secrets == SecretUse::Placeholder || input.references.secrets().is_empty() {
            return Ok(());
        }
        let secrets = self.secrets.as_ref().ok_or_else(|| {
            InfrastructureError::configuration(format!(
                "{}: configuration holds a secret, but no secret resolver is available",
                input.consumer
            ))
        })?;
        for argument in input.references.secrets() {
            let configured = serde_json::Value::Object(argument.secret.descriptor().clone());
            if argument.destination.json_value(projected) != Some(&configured) {
                return Err(InfrastructureError::configuration(format!(
                    "{} configuration.{}: the secret changed after it was classified",
                    input.consumer, argument.destination
                )));
            }
            self.ensure_not_stopped()?;
            let value = secrets.value(&argument.secret).await.map_err(|failure| {
                InfrastructureError::Secret {
                    consumer: input.consumer.to_string(),
                    destination: argument.destination.to_string(),
                    resolver: argument.secret.resolver().to_string(),
                    reason: failure.reason().to_string(),
                }
            })?;
            if argument.write_only.is_some() {
                self.loaded(input.declaration.provider_name())?
                    .client
                    .register_write_only_secret(&value);
            }
            argument
                .destination
                .replace_value(configuration, Value::String(value))?;
        }
        Ok(())
    }

    /// Where in the `infrastructure` block this engine's configuration
    /// lives, for naming the field a message is about.
    fn scope(&self) -> ConfigurationScope<'_> {
        ConfigurationScope::new(self.tenant.environment())
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
        let unrecorded = self
            .options
            .unrecorded_directory
            .as_ref()
            .map_or_else(UnrecordedStore::default_location, |directory| {
                Ok(UnrecordedStore::at(directory))
            })?;
        unrecorded.bound_to(self.store.as_ref())
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
        validate_configuration(&self.infrastructure, self.tenant.environment())?;
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
            _ if self.cancellation().is_stop_requested() => {
                tracing::debug!("planning ended by an interrupt");
                Err(InfrastructureError::InterruptedWhilePlanning)
            }
            other => other,
        }
    }

    /// Import an existing object into this tenant's managed state.
    ///
    /// The address must already be declared in the selected configuration.
    /// Provider import results are always refreshed before the absent record
    /// is inserted, and the write is fenced by the caller's lock.
    ///
    /// # Errors
    ///
    /// Returns an error for an undeclared or already recorded address,
    /// provider import or refresh failures, unsupported identity or deferred
    /// results, invalid imported state, pending recovery files, or a state
    /// write that loses its absent-record compare-and-swap.
    pub async fn import(
        &mut self,
        address: &ResourceAddress,
        identifier: &str,
        lock: &StateLock,
    ) -> Result<Vec<String>> {
        self.ensure_not_stopped()?;
        if identifier.is_empty() {
            return Err(InfrastructureError::configuration(
                "the provider import identifier must not be empty",
            ));
        }
        validate_configuration(&self.infrastructure, self.tenant.environment())?;
        let declaration = declared_resource(&self.infrastructure.resources, address)
            .cloned()
            .ok_or_else(|| {
                InfrastructureError::configuration(format!(
                    "cannot import undeclared resource {address}; declare it in infrastructure.resources first"
                ))
            })?;
        let pending = self.unrecorded_store()?.list(&self.tenant)?;
        if !pending.is_empty() {
            return Err(InfrastructureError::UnrecordedChangesPending {
                tenant: self.tenant.to_string(),
                count: pending.len(),
                directory: self
                    .unrecorded_store()?
                    .tenant_directory(&self.tenant)
                    .display()
                    .to_string(),
            });
        }
        let (stored, in_flight) = in_flight::split(self.store.list(&self.tenant).await?);
        // Importing the object records the outcome of its own change in
        // flight; any other change in flight still blocks.
        let (own_marker, others): (Vec<_>, Vec<_>) = in_flight
            .into_iter()
            .partition(|change| &change.address == address);
        in_flight::refuse(&self.tenant.to_string(), &others)?;
        for record in &stored {
            register_stored_values(record, &record.sensitive_attributes);
        }
        if stored.iter().any(|record| &record.address == address) {
            return Err(InfrastructureError::configuration(format!(
                "cannot import {address}: this tenant already records it; inspect `state list` and select the intended `--env`, or remove the record with `state remove` only if forgetting managed state is intended"
            )));
        }

        let mut warnings = Vec::new();
        let graph = self.prepare_declarations(&mut warnings).await?;
        let provider_name = declaration.provider_name().to_string();
        let sensitive = self
            .import_sensitivity(
                sensitivity::ImportSensitivity {
                    address,
                    stored: &stored,
                },
                &mut warnings,
            )
            .await?;
        let provider = self.loaded(&provider_name)?;
        let schema = resource_schema(provider, &provider_name, &address.resource_type)?;

        // The identifier may appear in provider diagnostics or logs. Register
        // it before the process can echo it, then let the usual redaction
        // path protect both.
        cuenv_events::register_secret(identifier.to_string());
        self.ensure_not_stopped()?;
        let imported = provider
            .client
            .import_resource_state(&address.resource_type, identifier)
            .await?;
        let value_type = schema.block.implied_type();
        // Inspect every returned object before any diagnostics can quote it.
        // Unknown resource types have no trustworthy sensitivity schema here.
        for resource in &imported.imported_resources {
            if resource.type_name != address.resource_type {
                return Err(InfrastructureError::plugin(format!(
                    "provider import for {address} returned a different resource type"
                )));
            }
            let (value, _) = sensitive_provider_value(SensitiveProviderValue {
                value: resource.state.as_ref(),
                value_type: &value_type,
                sensitive: &sensitive,
            })?;
            crate::write_only::check_state(&schema.block, &value, &format!("import {address}"))?;
        }
        check_diagnostics(
            &format!("import {address}"),
            &imported.diagnostics,
            &mut warnings,
        )?;
        reject_deferral(address, imported.deferred.as_ref())?;
        self.ensure_not_stopped()?;
        if imported.imported_resources.len() != 1 {
            return Err(InfrastructureError::plugin(format!(
                "provider import for {address} returned {} resources; exactly one is supported",
                imported.imported_resources.len()
            )));
        }
        let imported = &imported.imported_resources[0];
        if imported.type_name != address.resource_type {
            return Err(InfrastructureError::plugin(format!(
                "provider import for {address} returned a different resource type"
            )));
        }
        let identity_schema = provider.identity_schemas.get(&address.resource_type);
        let imported_identity = identity::received(
            identity_schema,
            imported.identity.as_ref(),
            &format!("import {address}"),
        )?;
        let (imported_state, imported_bytes) = sensitive_provider_value(SensitiveProviderValue {
            value: imported.state.as_ref(),
            value_type: &value_type,
            sensitive: &sensitive,
        })?;
        crate::write_only::check_state(
            &schema.block,
            &imported_state,
            &format!("import {address}"),
        )?;
        check_upgraded(address, &imported_state)?;

        self.ensure_not_stopped()?;
        let refreshed = provider
            .client
            .read_resource(ReadRequest {
                type_name: &address.resource_type,
                current_state: imported_bytes,
                private: imported.private.clone(),
                current_identity: imported_identity.clone(),
            })
            .await?;
        let (state, _) = sensitive_provider_value(SensitiveProviderValue {
            value: refreshed.new_state.as_ref(),
            value_type: &value_type,
            sensitive: &sensitive,
        })?;
        crate::write_only::check_state(
            &schema.block,
            &state,
            &format!("refresh imported {address}"),
        )?;
        check_diagnostics(
            &format!("refresh imported {address}"),
            &refreshed.diagnostics,
            &mut warnings,
        )?;
        reject_deferral(address, refreshed.deferred.as_ref())?;
        self.ensure_not_stopped()?;
        let identity = identity::received(
            identity_schema,
            refreshed.new_identity.as_ref(),
            &format!("refresh imported {address}"),
        )?
        .or(imported_identity);
        if state.is_null() {
            return Err(InfrastructureError::plugin(format!(
                "provider import for {address} found no existing object during refresh"
            )));
        }
        check_upgraded(address, &state)?;
        let dependencies = graph.managed_dependencies(&SourceAddress::Resource(address.clone()));
        let record = ManagedResource {
            address: address.clone(),
            provider: provider_name,
            provider_source: provider.source.clone(),
            schema_version: schema.version,
            state: state.to_state_json(&value_type)?,
            private: refreshed.private,
            dependencies,
            sensitive_attributes: sensitive,
            tainted: false,
            identity: identity::to_stored(identity_schema, identity.as_deref())?,
            serial: 0,
            generation: write_generation(RecordVersion::Absent),
        };
        self.ensure_not_stopped()?;
        let _recording = self.cancellation().begin_recording();
        self.record_change(
            lock,
            &ConditionalPut {
                resource: &record,
                expected: RecordVersion::Absent,
            },
        )
        .await?;
        for change in own_marker {
            self.store
                .delete(&self.tenant, lock, &change.marker)
                .await?;
        }
        Ok(warnings)
    }

    fn ensure_not_stopped(&self) -> Result<()> {
        if self.cancellation().is_stop_requested() {
            Err(InfrastructureError::InterruptedWhilePlanning)
        } else {
            Ok(())
        }
    }

    async fn plan_resources(&mut self, mode: PlanMode) -> Result<Plan> {
        self.output_sensitivity.clear();
        let (stored, in_flight) = in_flight::split(self.store.list(&self.tenant).await?);
        in_flight::refuse(&self.tenant.to_string(), &in_flight)?;
        let stored: BTreeMap<ResourceAddress, ManagedResource> = stored
            .into_iter()
            .map(|resource| (resource.address.clone(), resource))
            .collect();
        for row in stored.values() {
            register_stored_values(row, &row.sensitive_attributes);
        }
        let mut warnings = Vec::new();
        let mut changes = Vec::new();
        let mut data_sources = Vec::new();
        let mut planned_outputs = BTreeMap::new();
        let mut pending_sources = BTreeSet::new();

        let declared: BTreeMap<String, ManagedResourceDeclaration> = match mode {
            PlanMode::Apply => self.infrastructure.resources.clone(),
            PlanMode::Destroy => BTreeMap::new(),
        };
        let graph = match mode {
            PlanMode::Apply => self.prepare_declarations(&mut warnings).await?,
            // Destroy uses persisted dependencies and never reads current data
            // sources. An unrelated provider need not start to delete state.
            PlanMode::Destroy => {
                DeclarationGraph::with_references(&self.infrastructure, &BTreeMap::new())?
            }
        };
        let declared_addresses: BTreeSet<ResourceAddress> = declared
            .iter()
            .map(|(name, declaration)| ResourceAddress::new(&declaration.resource_type, name))
            .collect();

        // Orphans (and everything, when destroying) are deleted; the schedule
        // below puts every change in its place.
        for row in stored
            .values()
            .filter(|resource| !declared_addresses.contains(&resource.address))
        {
            self.ensure_not_stopped()?;
            let change = self.plan_delete(row, &mut warnings).await?;
            register_sensitive_values(&change);
            changes.push(change);
        }

        for source in graph.order.iter().filter(|_| mode == PlanMode::Apply) {
            self.ensure_not_stopped()?;
            if matches!(source, SourceAddress::DataSource(_)) {
                let read = self
                    .plan_data_source(
                        DataSourcePlanning {
                            source,
                            graph: &graph,
                            outputs: &planned_outputs,
                            pending: &pending_sources,
                        },
                        &mut warnings,
                    )
                    .await?;
                if read.deferred {
                    pending_sources.insert(source.clone());
                }
                self.output_sensitivity
                    .insert(source.clone(), read.sensitive.clone());
                planned_outputs.insert(source.clone(), read.after.clone());
                data_sources.push(read);
                continue;
            }
            let address = source.address();
            let name = &address.name;
            let declaration = declared.get(name).ok_or_else(|| {
                InfrastructureError::configuration(format!("{source} is not declared"))
            })?;
            let resource_references = graph.references.get(source).cloned().unwrap_or_default();
            let input = ResourcePlanInput {
                name,
                declaration,
                dependencies: graph.managed_dependencies(source),
                references: resource_references,
                planned_outputs: &planned_outputs,
                stored: stored.get(address),
            };
            let change = self.plan_resource(&input, &mut warnings).await?;
            // Before any consumer can copy them into its configuration.
            register_typed_sensitive_values(
                &change,
                &self.declaration_block(source)?.implied_type(),
            );
            self.output_sensitivity
                .insert(source.clone(), change.sensitive.clone());
            if change.action.changes_infrastructure()
                || graph
                    .dependencies
                    .get(source)
                    .into_iter()
                    .flatten()
                    .any(|dependency| pending_sources.contains(dependency))
            {
                pending_sources.insert(source.clone());
            }
            planned_outputs.insert(source.clone(), change.after.clone());
            changes.push(change);
        }

        // Order, and refuse an impossible order, now: before anyone is asked
        // to confirm the plan.
        let ordered = in_schedule_order(changes)?;
        let mut changes = ordered.changes;
        warnings.extend(ordered.warnings);
        if mode == PlanMode::Apply {
            let mut rows = stored;
            let mut pending = BTreeSet::new();
            for change in &changes {
                if change.action.changes_infrastructure() {
                    pending.insert(change.address.clone());
                } else if let Some(record) = &change.refreshed_record {
                    rows.insert(change.address.clone(), record.clone());
                }
            }
            let reads: BTreeMap<_, _> = data_sources
                .iter()
                .filter(|read: &&DataSourcePlan| !read.deferred)
                .map(|read| (read.address.clone(), read.after.clone()))
                .collect();
            self.verify_outputs(&OutputVerification {
                rows: &rows,
                data_sources: &reads,
                pending: &pending,
            })
            .await?;
        }
        for change in &mut changes {
            let provider = self.loaded(&change.provider)?;
            let schema =
                resource_schema(provider, &change.provider, &change.address.resource_type)?;
            let value_type = schema.block.implied_type();
            for step in &mut change.steps {
                if step.kind != StepKind::Delete
                    && change
                        .references
                        .secrets()
                        .iter()
                        .any(|argument| argument.write_only.is_some())
                {
                    let configuration =
                        type_system::from_message_pack(&step.configuration, &value_type)?;
                    let configuration = change
                        .references
                        .without_write_only_values(&configuration)?;
                    step.configuration = type_system::to_message_pack(&configuration, &value_type)?;
                }
            }
        }
        Ok(Plan {
            tenant: self.tenant.clone(),
            changes,
            data_sources,
            warnings,
            environment_identity: environment_identity(
                &self.options.provider_environment_variables,
            ),
            configuration_evaluation: self.configuration_evaluation.clone(),
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
    /// Operations run in the order of one dependency graph (see the
    /// `schedule` module): a replacement is a delete followed, as soon as
    /// its prerequisites allow, by a create. State is persisted after every
    /// operation and every write is fenced by the lock. Refreshed records of
    /// unchanged resources are written too. [`Cancellation::stop`] is
    /// honoured between operations, including between the delete and the
    /// create of a replacement; an operation in flight when it arrives is
    /// still recorded.
    ///
    /// When a provider fails an operation, everything that depends on it is
    /// skipped, a replacement whose create can no longer run is not started,
    /// and every other operation still runs. Every replacement whose old
    /// object was deleted but whose new object was not created is reported
    /// through [`ApplyEvent::DeletedNotRecreated`], on every way the run can
    /// end early.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::PlanOutdated`] for a stale plan,
    /// [`InfrastructureError::ApplyIncomplete`] when providers failed some
    /// changes (it lists the failures, the skipped changes and the
    /// replacements deleted but not recreated), the first state store
    /// failure, [`InfrastructureError::UnrecordedChange`] or
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
        if plan
            .configuration_evaluation
            .as_ref()
            .map(|binding| binding.identity)
            != self
                .configuration_evaluation
                .as_ref()
                .map(|binding| binding.identity)
        {
            return Err(InfrastructureError::configuration(
                "plan belongs to a different immutable CUE snapshot",
            ));
        }
        let rows = self.require_current(plan).await?;
        for warning in &plan.warnings {
            on_event(ApplyEvent::Warning(warning.clone()));
        }
        let this: &Self = self;
        this.converge(
            this,
            Convergence {
                plan,
                lock: context.lock,
                rows,
                on_event,
            },
        )
        .await
    }

    /// Run a plan's schedule with `runner` and sum up how it went.
    async fn converge(
        &self,
        runner: &(impl OperationRunner + Sync),
        convergence: Convergence<'_>,
    ) -> Result<PlanSummary> {
        let Convergence {
            plan,
            lock,
            rows,
            on_event,
        } = convergence;
        let schedule = Schedule::build(&plan.changes)?;
        let total = plan
            .changes
            .iter()
            .filter(|change| change.action.changes_infrastructure())
            .count();
        let mut execution = Execution {
            run: ScheduleRun::new(&schedule),
            progress: Progress {
                completed: 0,
                total,
            },
            lock,
            rows,
            data_sources: DataSourceExecution::new(plan),
            data_source_plans: &plan.data_sources,
            on_event,
        };
        if let Err(error) = self.execute(runner, &mut execution).await {
            execution.run.abandon(execution.on_event);
            return Err(error);
        }
        if execution.run.has_failures() {
            execution.run.report_uncreated(execution.on_event);
            return Err(InfrastructureError::ApplyIncomplete(Box::new(
                IncompleteApply {
                    deleted_not_recreated: execution.run.deleted_not_recreated(),
                    failures: execution.run.take_failures(),
                    skipped: execution.run.take_skipped(),
                    completed: execution.progress.completed,
                    total,
                },
            )));
        }
        if !execution.data_sources.failures.is_empty() {
            return Err(InfrastructureError::Diagnostics {
                context: "data source reads failed".to_string(),
                errors: execution
                    .data_sources
                    .failures
                    .iter()
                    .map(|(address, reason)| format!("data.{address}: {reason}"))
                    .collect(),
            });
        }
        Ok(plan.summary())
    }

    /// Run the operations of a schedule in order. A provider failure of one
    /// operation skips the operations that depend on it and the rest carry
    /// on; anything else (an interrupt, a state store failure, a lost
    /// provider connection) ends the run at once.
    async fn execute(
        &self,
        runner: &(impl OperationRunner + Sync),
        execution: &mut Execution<'_, '_, '_>,
    ) -> Result<()> {
        let schedule = execution.run.schedule();
        let mut refreshed: Vec<ManagedResource> = Vec::new();
        let mut remaining: BTreeMap<ResourceAddress, usize> = BTreeMap::new();
        for scheduled in &schedule.operations {
            *remaining
                .entry(scheduled.operation.change.address.clone())
                .or_default() += 1;
        }
        let mut verified_reads = execution.data_sources.values.len();
        for (index, scheduled) in schedule.operations.iter().enumerate() {
            if execution.run.is_skipped(index) {
                execution
                    .data_sources
                    .failed(&scheduled.operation.change.address);
                continue;
            }
            let operation = &scheduled.operation;
            let change = operation.change;
            if self.cancellation().is_stop_requested() {
                return Err(execution.progress.interrupted());
            }
            let reads = self
                .read_ready_data_sources(
                    runner,
                    ReadyDataSources {
                        plans: execution.data_source_plans,
                        execution: &mut execution.data_sources,
                        rows: &execution.rows,
                        on_event: execution.on_event,
                    },
                )
                .await;
            execution
                .run
                .record_read_failures(execution.data_sources.take_read_failures());
            if self.cancellation().is_stop_requested() {
                return Err(execution.progress.interrupted());
            }
            reads?;
            // A deferred read produced new outputs: check them before this change.
            if execution.data_sources.values.len() != verified_reads {
                verified_reads = execution.data_sources.values.len();
                if !self
                    .verify_or_halt(execution, &remaining, &change.address)
                    .await?
                {
                    continue;
                }
            }
            if operation.kind != OperationKind::Delete
                && execution.data_sources.consumer_blocked(&change.references)
            {
                execution.data_sources.failed(&change.address);
                execution.run.blocked(index, execution.on_event);
                continue;
            }
            if operation.kind == OperationKind::Refresh
                && let Some(record) = &change.refreshed_record
            {
                // Refreshed records are written together, before the next
                // operation that changes something, so an object store
                // writes its state document once for a run of refreshes.
                execution
                    .rows
                    .insert(change.address.clone(), record.clone());
                refreshed.push(record.clone());
                execution.run.finished(index);
                execution.data_sources.finished(&change.address);
                settle(&mut remaining, &change.address);
                continue;
            }
            self.record_refreshed(execution, &mut refreshed).await?;
            // A replacement starts with its delete and finishes with its
            // create, which may be some operations later.
            if operation.starts_change() {
                (execution.on_event)(ApplyEvent::Started {
                    address: change.address.clone(),
                    action: change.action,
                });
            }
            let outcome = runner
                .run_operation(
                    operation,
                    OperationRunContext {
                        lock: execution.lock,
                        rows: &mut execution.rows,
                        data_sources: &execution.data_sources.values,
                        on_event: execution.on_event,
                    },
                )
                .await;
            let Err(failure) = outcome else {
                execution.run.finished(index);
                settle(&mut remaining, &change.address);
                if operation.completes_change() {
                    execution.data_sources.finished(&change.address);
                    execution.progress.completed += 1;
                    (execution.on_event)(ApplyEvent::Finished {
                        address: change.address.clone(),
                        action: change.action,
                    });
                    // New outputs exist: check them before anything else changes.
                    if self.declares_outputs(&change.address) {
                        self.verify_or_halt(execution, &remaining, &change.address)
                            .await?;
                    }
                }
                continue;
            };
            let continues = failure.continues();
            execution.data_sources.failed(&change.address);
            execution
                .run
                .aborted(index, failure.aftermath(operation.kind));
            let error = self.interrupted_or(
                failure.into_error(),
                InterruptedOperation {
                    address: &change.address,
                    progress: execution.progress,
                    on_event: execution.on_event,
                },
            );
            if !continues || self.cancellation().is_stop_requested() {
                if operation.starts_change() || operation.completes_change() {
                    (execution.on_event)(ApplyEvent::Failed {
                        address: change.address.clone(),
                        action: change.action,
                    });
                }
                return Err(error);
            }
            execution.run.failed(index, error, execution.on_event);
        }
        self.record_refreshed(execution, &mut refreshed).await?;
        if execution.run.is_halted() {
            return Ok(());
        }
        let reads = self
            .read_ready_data_sources(
                runner,
                ReadyDataSources {
                    plans: execution.data_source_plans,
                    execution: &mut execution.data_sources,
                    rows: &execution.rows,
                    on_event: execution.on_event,
                },
            )
            .await;
        execution
            .run
            .record_read_failures(execution.data_sources.take_read_failures());
        if self.cancellation().is_stop_requested() {
            return Err(execution.progress.interrupted());
        }
        reads?;
        if let Some(last) = schedule.operations.last()
            && !execution.run.has_failures()
        {
            self.verify_or_halt(execution, &remaining, &last.operation.change.address)
                .await?;
        }
        for read in execution
            .data_source_plans
            .iter()
            .filter(|read| read.deferred)
        {
            if !execution.data_sources.values.contains_key(&read.address) {
                execution
                    .data_sources
                    .failures
                    .entry(read.address.clone())
                    .or_insert_with(|| "a prerequisite did not complete".to_string());
            }
        }
        Ok(())
    }

    fn declares_outputs(&self, address: &ResourceAddress) -> bool {
        self.configuration_evaluation
            .as_ref()
            .is_some_and(|binding| {
                binding
                    .evaluator
                    .declared_outputs()
                    .iter()
                    .any(|identity| match &identity.source {
                        SourceAddress::Resource(source) => source == address,
                        SourceAddress::DataSource(_) => false,
                    })
            })
    }

    /// Check the outputs known so far, and if they fail skip everything not yet started,
    /// recording the failure against the change at `address`. Returns whether they passed.
    async fn verify_or_halt(
        &self,
        execution: &mut Execution<'_, '_, '_>,
        remaining: &BTreeMap<ResourceAddress, usize>,
        address: &ResourceAddress,
    ) -> Result<bool> {
        let pending = pending_addresses(remaining);
        let verified = self
            .verify_outputs(&OutputVerification {
                rows: &execution.rows,
                data_sources: &execution.data_sources.values,
                pending: &pending,
            })
            .await;
        if self.cancellation().is_stop_requested() {
            return Err(execution.progress.interrupted());
        }
        let Err(error) = verified else {
            return Ok(true);
        };
        execution.run.halted(
            ChangeFailure {
                address: address.clone(),
                error,
            },
            execution.on_event,
        );
        Ok(false)
    }

    /// Check every output known so far against the whole CUE instance. Reference
    /// analysis cannot prove an unmodelled expression independent of every value,
    /// but this needs no such proof: it evaluates the values that actually exist.
    async fn verify_outputs(&self, input: &OutputVerification<'_>) -> Result<()> {
        let Some(binding) = &self.configuration_evaluation else {
            return Ok(());
        };
        let mut sources = BTreeMap::new();
        for identity in binding.evaluator.declared_outputs() {
            if let Some(value) = self.known_output(identity, input).await? {
                sources.insert(identity.clone(), value);
            }
        }
        if sources.is_empty() {
            return Ok(());
        }
        self.ensure_not_stopped()?;
        evaluation::verify(
            evaluation::Verification {
                evaluator: Arc::clone(&binding.evaluator),
                sources,
            },
            self.cancellation(),
        )
        .await?;
        self.ensure_not_stopped()
    }

    /// The scalar value of one declared output, if this point of the run knows it.
    async fn known_output(
        &self,
        identity: &OutputIdentity,
        input: &OutputVerification<'_>,
    ) -> Result<Option<Value>> {
        let value = match &identity.source {
            SourceAddress::DataSource(address) => input
                .data_sources
                .get(address)
                .and_then(|value| value.attribute(&identity.attribute))
                .cloned(),
            SourceAddress::Resource(address) => {
                let Some(row) = input.rows.get(address) else {
                    return Ok(None);
                };
                if row.tainted || input.pending.contains(address) {
                    return Ok(None);
                }
                let Some(declaration) = declared_resource(&self.infrastructure.resources, address)
                else {
                    return Ok(None);
                };
                let provider = self.loaded(declaration.provider_name())?;
                if require_same_source(row, provider).is_err() {
                    return Ok(None);
                }
                let schema = resource_schema(
                    provider,
                    declaration.provider_name(),
                    &address.resource_type,
                )?;
                let mut warnings = Vec::new();
                let state = upgraded_state(provider, row, schema, &mut warnings).await?;
                let value = state.attribute(&identity.attribute).cloned();
                if let Some(value) = &value {
                    self.register_sensitive_output(&SensitiveOutput {
                        source: &identity.source,
                        attribute: &identity.attribute,
                        row,
                        schema,
                        value,
                    });
                }
                value
            }
        };
        match value.as_ref().map(Value::untyped) {
            None | Some(Value::Null | Value::Unknown) => Ok(None),
            Some(Value::String(_) | Value::Number(_) | Value::Boolean(_)) => Ok(value),
            // Outputs are declared as scalars. CUE would never see this value, so
            // nothing downstream could notice that the declaration is wrong.
            Some(_) => Err(InfrastructureError::configuration(format!(
                "output {}.{} is declared as a scalar, but its real value is not one",
                identity.source, identity.attribute
            ))),
        }
    }

    /// A source created during this apply was never planned with a known value:
    /// register it before anything downstream sees it.
    fn register_sensitive_output(&self, output: &SensitiveOutput<'_>) {
        let SensitiveOutput {
            source,
            attribute,
            row,
            schema,
            value,
        } = output;
        if resource_sensitivity(row, schema).contains(attribute)
            || self
                .output_sensitivity
                .get(source)
                .is_some_and(|attributes| attributes.contains(attribute))
            || schema
                .block
                .attributes
                .get(*attribute)
                .is_some_and(|attribute| attribute.sensitive)
        {
            register_value_strings(value);
        }
    }

    /// Write the refreshed records gathered since the last write, in one
    /// call, then report each one as refreshed.
    async fn record_refreshed(
        &self,
        execution: &mut Execution<'_, '_, '_>,
        refreshed: &mut Vec<ManagedResource>,
    ) -> Result<()> {
        if refreshed.is_empty() {
            return Ok(());
        }
        let records = std::mem::take(refreshed);
        self.store
            .put_many(&self.tenant, execution.lock, &records)
            .await
            .map_err(|error| naming_addresses(error, &records))?;
        for record in records {
            (execution.on_event)(ApplyEvent::Refreshed {
                address: record.address,
            });
        }
        Ok(())
    }

    /// Refuse a plan whose view of stored state no longer matches the store.
    async fn require_current(
        &self,
        plan: &Plan,
    ) -> Result<BTreeMap<ResourceAddress, ManagedResource>> {
        let (stored, in_flight) = in_flight::split(self.store.list(&self.tenant).await?);
        in_flight::refuse(&self.tenant.to_string(), &in_flight)?;
        let stored: BTreeMap<ResourceAddress, ManagedResource> = stored
            .into_iter()
            .map(|resource| (resource.address.clone(), resource))
            .collect();
        for change in &plan.changes {
            if stored.get(&change.address) != change.stored.as_ref() {
                return Err(InfrastructureError::PlanOutdated {
                    address: change.address.to_string(),
                });
            }
        }
        // A record the plan never saw appeared since.
        let planned_addresses = plan
            .changes
            .iter()
            .map(|change| change.address.clone())
            .collect::<BTreeSet<_>>();
        if let Some(address) = stored
            .keys()
            .find(|address| !planned_addresses.contains(address))
        {
            return Err(InfrastructureError::PlanOutdated {
                address: address.to_string(),
            });
        }
        Ok(stored)
    }

    /// After a stop request, a provider failure is most likely the stopped
    /// operation returning: report the interrupt, keeping the provider's
    /// message as a warning. Recording failures are never masked.
    fn interrupted_or(
        &self,
        error: InfrastructureError,
        operation: InterruptedOperation<'_>,
    ) -> InfrastructureError {
        let InterruptedOperation {
            address,
            progress,
            on_event,
        } = operation;
        let apply_response_lost = matches!(
            &error,
            InfrastructureError::RemoteProcedure { method, .. }
                if method.ends_with("ApplyResourceChange")
        );
        if apply_response_lost && self.cancellation().is_stop_requested() {
            return InfrastructureError::InterruptedUnknownOutcome {
                address: address.to_string(),
                completed: progress.completed,
                total: progress.total,
            };
        }
        let provider_failure = matches!(
            error,
            InfrastructureError::Diagnostics { .. }
                | InfrastructureError::Plugin(_)
                | InfrastructureError::RemoteProcedure { .. }
                | InfrastructureError::InterruptedWhilePlanning
        );
        if provider_failure && self.cancellation().is_stop_requested() {
            if !matches!(error, InfrastructureError::InterruptedWhilePlanning) {
                on_event(ApplyEvent::Warning(format!(
                    "the operation in flight when the run was interrupted ended with: {error}"
                )));
            }
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

    async fn apply_operation(
        &self,
        operation: &ApplyOperation<'_>,
        context: OperationRunContext<'_>,
    ) -> std::result::Result<(), OperationFailure> {
        let OperationRunContext {
            lock,
            rows,
            data_sources,
            on_event,
        } = context;
        let change = operation.change;
        let provider = self.loaded(&change.provider)?;
        let schema = resource_schema(provider, &change.provider, &change.address.resource_type)?;
        let value_type = schema.block.implied_type();
        if let Some(row) = &change.stored {
            register_stored_values(row, &change.sensitive);
        }
        register_typed_sensitive_values(change, &value_type);
        // The stored version every write replaces, for saving a record the
        // store cannot take.
        let version = operation.expected;

        let final_plan = change.needs_final_plan();
        for planned_step in operation.steps {
            let step = if !final_plan || planned_step.kind == StepKind::Delete {
                planned_step.clone()
            } else {
                let replanned = self
                    .replan_referenced_step(ReferencedReplan {
                        change,
                        step: planned_step,
                        schema,
                        lock,
                        rows,
                        data_sources,
                        on_event,
                    })
                    .await;
                match replanned {
                    Ok(Some(step)) => step,
                    // A concrete output can make an update unnecessary. The
                    // final plan has refreshed and recorded its state.
                    Ok(None) => continue,
                    Err(error) => {
                        if operation.kind == OperationKind::ReplaceCreate {
                            // Its delete has already run and been recorded.
                            on_event(ApplyEvent::Warning(format!(
                                "{} was destroyed for its replacement but could not be \
                                 recreated once its output references were resolved; it is no \
                                 longer in state and the next apply will create it",
                                change.address
                            )));
                        }
                        return Err(OperationFailure::from_reference_replan(error));
                    }
                }
            };
            // Once an operation without a final plan has emitted `Started`, it
            // is already in flight and must be allowed to finish and record
            // its result. A final plan performs extra provider calls, so stop
            // requests must still be checked before its apply.
            if final_plan {
                self.ensure_not_stopped()
                    .map_err(OperationFailure::Continue)?;
            }
            let has_write_only = change
                .references
                .secrets()
                .iter()
                .any(|argument| argument.write_only.is_some());
            let configuration = if step.kind == StepKind::Delete || !has_write_only {
                step.configuration.clone()
            } else {
                let mut configuration =
                    type_system::from_message_pack(&step.configuration, &value_type)?;
                for argument in change.references.secrets() {
                    if let Some(destination) = &argument.write_only {
                        let secrets = self.secrets.as_ref().ok_or_else(|| {
                            InfrastructureError::configuration(
                                "write-only configuration requires a secret resolver",
                            )
                        })?;
                        let value = secrets.value(&argument.secret).await.map_err(|_| {
                            InfrastructureError::configuration(
                                "write-only secret could not be resolved",
                            )
                        })?;
                        provider.client.register_write_only_secret(&value);
                        destination.replace_value(&mut configuration, Value::String(value))?;
                    }
                }
                self.ensure_not_stopped()?;
                type_system::to_message_pack(&configuration, &value_type)?
            };
            // Fence the change in the shared backend before the provider can
            // act on it: until its outcome is recorded, no other run plans.
            let marker =
                in_flight::marker_record(&change.address, step.kind.name(), &lock.lock_identifier)?;
            self.store.put(&self.tenant, lock, &marker).await?;
            let response = provider
                .client
                .apply_resource_change(ApplyRequest {
                    type_name: &change.address.resource_type,
                    prior_state: step.prior.clone(),
                    planned_state: step.planned.clone(),
                    configuration,
                    planned_private: step.planned_private.clone(),
                    planned_identity: step.planned_identity.clone(),
                })
                .await?;
            // From here on the change happened: a forced exit waits for it
            // to be recorded (or saved locally).
            let _recording = self.cancellation().begin_recording();
            let context = format!("apply {}", change.address);
            let failed = response.diagnostics.iter().any(is_error);
            if schema.block.contains_write_only() {
                let checked = decode_provider_value(response.new_state.as_ref(), &value_type)
                    .map_err(|_| InfrastructureError::plugin(format!("{context}: provider returned undecodable state; no returned state was accepted; the remote object may have changed")))?;
                crate::write_only::check_state(&schema.block, &checked, &context)?;
            }
            let returned = self
                .decode_apply_response(
                    sensitivity::ApplyResponseValue {
                        response: &response,
                        value_type: &value_type,
                    },
                    sensitivity::RetainedSensitivity {
                        change,
                        lock,
                        rows,
                        expected: version,
                    },
                )
                .await?;
            crate::write_only::check_state(&schema.block, &returned, &context)?;
            register_typed_attributes(&returned, &change.sensitive, &value_type);
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
                            step: &step,
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
                if has_errors {
                    // A failed operation with no returned object retains its
                    // old state, but must not lose newly learned sensitivity.
                    self.retain_failed_sensitivity(sensitivity::RetainedSensitivity {
                        change,
                        lock,
                        rows,
                        expected: version,
                    })
                    .await?;
                } else {
                    // The provider has deleted the object; recording that
                    // can still fail.
                    self.forget(lock, &change.address)
                        .await
                        .map_err(|error| match step.kind {
                            StepKind::Delete => OperationFailure::AbortAfterDelete(error),
                            StepKind::Create | StepKind::Update => OperationFailure::Abort(error),
                        })?;
                    rows.remove(&change.address);
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
                    sensitive_attributes: remembered_sensitive_attributes(
                        &change.sensitive,
                        stored,
                    ),
                    dependencies: match (step.kind, stored) {
                        // A failed delete leaves the old object, and its
                        // dependencies, as they were.
                        (StepKind::Delete, Some(stored)) if has_errors => {
                            stored.dependencies.clone()
                        }
                        (StepKind::Update, Some(stored)) if has_errors => stored
                            .dependencies
                            .iter()
                            .chain(&change.dependencies)
                            .cloned()
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .collect(),
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
                    identity: {
                        let (identity, warning) =
                            applied_identity(provider, &change.address, &response, stored);
                        if let Some(warning) = warning {
                            on_event(ApplyEvent::Warning(warning));
                        }
                        identity
                    },
                    serial: 0,
                    generation: write_generation(version),
                };
                self.record_change(
                    lock,
                    &ConditionalPut {
                        resource: &record,
                        expected: version,
                    },
                )
                .await?;
                rows.insert(change.address.clone(), record);
            }
            // The outcome is recorded: the change is no longer in flight.
            self.store
                .delete(&self.tenant, lock, &marker.address)
                .await?;

            // The change is recorded, so a failure the provider reported
            // concerns this change alone and the run can go on without it.
            let mut warnings = Vec::new();
            check_diagnostics(&context, &response.diagnostics, &mut warnings)
                .map_err(OperationFailure::Continue)?;
            for warning in warnings {
                on_event(ApplyEvent::Warning(warning));
            }
            if let Some(problem) = problem {
                return Err(OperationFailure::Continue(InfrastructureError::plugin(
                    problem,
                )));
            }
        }

        if change.action == Action::Delete {
            // Also covers resources already gone during refresh (no steps).
            let _recording = self.cancellation().begin_recording();
            self.forget(lock, &change.address).await?;
            rows.remove(&change.address);
        }
        Ok(())
    }

    /// Resolve output references immediately before applying a consumer, then
    /// ask the provider for a final plan with concrete configuration. The
    /// final plan may refine unknown values, but it may not introduce a new
    /// replacement that was absent from the confirmed plan.
    async fn replan_referenced_step(
        &self,
        input: ReferencedReplan<'_>,
    ) -> Result<Option<ApplyStep>> {
        let ReferencedReplan {
            change,
            step,
            schema,
            lock,
            rows,
            data_sources,
            on_event,
        } = input;
        change.reference_configuration.as_ref().ok_or_else(|| {
            InfrastructureError::configuration(format!(
                "{} has output references but no prepared configuration",
                change.address
            ))
        })?;
        let mut warnings = Vec::new();
        let configuration = self
            .resolve_reference_configuration(
                ResolveReferenceConfiguration {
                    change,
                    destination_block: &schema.block,
                    rows,
                    data_sources,
                },
                &mut warnings,
            )
            .await?;
        for warning in warnings {
            on_event(ApplyEvent::Warning(warning));
        }
        let configuration = schema.block.normalize_configuration(configuration);
        let value_type = schema.block.implied_type();
        register_typed_attributes(&configuration, &change.sensitive, &value_type);
        let configuration_bytes = type_system::to_message_pack(&configuration, &value_type)?;
        let provider = self.loaded(&change.provider)?;
        self.ensure_not_stopped()?;
        let validation_diagnostics = provider
            .client
            .validate_resource_configuration(
                &change.address.resource_type,
                configuration_bytes.clone(),
            )
            .await?;
        let mut warnings = Vec::new();
        check_diagnostics(
            &format!("validate {} after resolving references", change.address),
            &validation_diagnostics,
            &mut warnings,
        )?;
        for warning in warnings {
            on_event(ApplyEvent::Warning(warning));
        }

        self.ensure_not_stopped()?;
        let prior_value = type_system::from_message_pack(&step.prior, &value_type)?;
        let proposed = schema.block.proposed_new(&prior_value, &configuration);
        let response = provider
            .client
            .plan_resource_change(PlanRequest {
                type_name: &change.address.resource_type,
                prior_state: step.prior.clone(),
                proposed_new_state: type_system::to_message_pack(&proposed, &value_type)?,
                configuration: configuration_bytes.clone(),
                prior_private: step.prior_private.clone(),
                prior_identity: step.prior_identity.clone(),
            })
            .await?;
        let mut warnings = Vec::new();
        let (planned, planned_bytes) = sensitive_provider_value(SensitiveProviderValue {
            value: response.planned_state.as_ref(),
            value_type: &value_type,
            sensitive: &change.sensitive,
        })?;
        crate::write_only::check_state(&schema.block, &planned, "plan resource")?;
        check_diagnostics(
            &format!("final plan for {}", change.address),
            &response.diagnostics,
            &mut warnings,
        )?;
        for warning in warnings {
            on_event(ApplyEvent::Warning(warning));
        }
        reject_deferral(&change.address, response.deferred.as_ref())?;
        PlanValidity {
            address: &change.address,
            block: &schema.block,
            prior: &prior_value,
            configuration: &configuration,
            planned: &planned,
            legacy_type_system: response.legacy_type_system,
        }
        .check()?;

        let consistency = compatibility_problems(&schema.block, &step.planned_value, &planned);
        if !consistency.is_empty() {
            let rendered = consistency
                .iter()
                .map(PlanProblem::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(InfrastructureError::plugin(format!(
                "provider produced an inconsistent final plan for {}: {rendered}; run plan again",
                change.address
            )));
        }

        match step.kind {
            StepKind::Update => {
                let replacements = replacement_paths(&ReplacementCheck {
                    address: &change.address,
                    paths: &response.requires_replace,
                    prior: &prior_value,
                    planned: &planned,
                    value_type: &value_type,
                    block: &schema.block,
                })?;
                if !replacements.is_empty() {
                    return Err(InfrastructureError::plugin(format!(
                        "final plan for {} requires a replacement that was not in the confirmed plan; run plan again",
                        change.address
                    )));
                }
                if type_system::semantically_equal(&planned, &prior_value, &value_type) {
                    let stored = change.stored.as_ref().ok_or_else(|| {
                        InfrastructureError::configuration(format!(
                            "update plan for {} has no stored record",
                            change.address
                        ))
                    })?;
                    let record = ManagedResource {
                        address: change.address.clone(),
                        provider: change.provider.clone(),
                        provider_source: provider.source.clone(),
                        schema_version: schema.version,
                        state: prior_value.to_state_json(&value_type)?,
                        private: step.prior_private.clone(),
                        dependencies: change.dependencies.clone(),
                        sensitive_attributes: remembered_sensitive_attributes(
                            &change.sensitive,
                            Some(stored),
                        ),
                        tainted: false,
                        identity: stored.identity.clone(),
                        serial: 0,
                        generation: write_generation(RecordVersion::of(Some(stored))),
                    };
                    self.ensure_not_stopped()?;
                    let _recording = self.cancellation().begin_recording();
                    self.record_change(
                        lock,
                        &ConditionalPut {
                            resource: &record,
                            expected: RecordVersion::of(Some(stored)),
                        },
                    )
                    .await?;
                    rows.insert(change.address.clone(), record);
                    return Ok(None);
                }
            }
            // As in Terraform, `requires_replace` is meaningless without a
            // prior object: SDKv2 providers report every `ForceNew`
            // attribute on creates too.
            StepKind::Create => {}
            StepKind::Delete => {
                return Err(InfrastructureError::configuration(
                    "output references cannot be resolved for a delete",
                ));
            }
        }

        Ok(Some(ApplyStep {
            kind: step.kind,
            prior: step.prior.clone(),
            planned: planned_bytes,
            configuration: configuration_bytes,
            planned_private: response.planned_private,
            prior_private: step.prior_private.clone(),
            planned_value: planned,
            prior_identity: step.prior_identity.clone(),
            planned_identity: identity::planned(
                provider.identity_schemas.get(&change.address.resource_type),
                response.planned_identity.as_ref(),
            ),
        }))
    }

    /// Resolve every reference of a consumer from the recorded state of its
    /// sources. Warnings from upgrading source state are added to `warnings`.
    async fn resolve_reference_configuration(
        &self,
        input: ResolveReferenceConfiguration<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<Value> {
        let ResolveReferenceConfiguration {
            change,
            destination_block,
            rows,
            data_sources,
        } = input;
        self.validate_output_references(&change.address, &change.references, destination_block)?;
        let mut outputs = BTreeMap::new();
        for reference in change.references.references() {
            let value = self
                .reference_output_value(
                    ReferenceValueInput {
                        destination: &change.address,
                        reference,
                        rows,
                        data_sources,
                    },
                    warnings,
                )
                .await?;
            outputs.insert(
                OutputIdentity {
                    source: reference.source.clone(),
                    attribute: reference.attribute.clone(),
                },
                value,
            );
        }
        let consumer = SourceAddress::Resource(change.address.clone());
        let declaration =
            data_sources::declaration(&self.infrastructure, &consumer).ok_or_else(|| {
                InfrastructureError::configuration("resource declaration disappeared")
            })?;
        self.prepare_configuration(
            ConfigurationInput {
                consumer: &consumer,
                declaration,
                references: &change.references,
                block: destination_block,
                phase: ConfigurationPhase::Apply,
                secrets: SecretUse::Resolve,
            },
            |reference| {
                outputs
                    .get(&OutputIdentity {
                        source: reference.source.clone(),
                        attribute: reference.attribute.clone(),
                    })
                    .cloned()
                    .ok_or_else(|| reference_error(&change.address, reference, "was not resolved"))
            },
        )
        .await
    }

    /// The recorded value of one source output, upgraded to the provider's
    /// current schema version first, as a refresh would. Schema
    /// compatibility is checked by [`Self::validate_output_references`];
    /// a sensitive value is registered for redaction before it is returned.
    async fn reference_output_value(
        &self,
        input: ReferenceValueInput<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<Value> {
        self.ensure_not_stopped()?;
        let ReferenceValueInput {
            destination,
            reference,
            rows,
            data_sources,
        } = input;
        if let SourceAddress::DataSource(address) = &reference.source {
            return data_sources
                .get(address)
                .and_then(|value| value.attribute(&reference.attribute))
                .cloned()
                .ok_or_else(|| {
                    reference_error(
                        destination,
                        reference,
                        "has no successful data source result",
                    )
                });
        }
        let row = rows.get(reference.source.address()).ok_or_else(|| {
            reference_error(
                destination,
                reference,
                "has no successfully recorded source state",
            )
        })?;
        if row.tainted {
            return Err(reference_error(
                destination,
                reference,
                "cannot read tainted source state",
            ));
        }
        let source_declaration =
            declared_resource(&self.infrastructure.resources, reference.source.address())
                .ok_or_else(|| {
                    reference_error(destination, reference, "source resource is not declared")
                })?;
        let source_provider = self.loaded(source_declaration.provider_name())?;
        require_same_source(row, source_provider)?;
        let source_schema = resource_schema(
            source_provider,
            source_declaration.provider_name(),
            &reference.source.address().resource_type,
        )?;
        let source_state = upgraded_state(source_provider, row, source_schema, warnings).await?;
        let value = source_state
            .attribute(&reference.attribute)
            .cloned()
            .ok_or_else(|| {
                reference_error(
                    destination,
                    reference,
                    "source attribute is absent from state",
                )
            })?;
        self.register_sensitive_output(&SensitiveOutput {
            source: &reference.source,
            attribute: &reference.attribute,
            row,
            schema: source_schema,
            value: &value,
        });
        Ok(value)
    }

    /// Write a record the provider's change produced. If the write fails,
    /// save it locally, with the stored version it replaces, so a resource
    /// the provider already changed is never silently lost.
    async fn record_change(&self, lock: &StateLock, put: &ConditionalPut<'_>) -> Result<()> {
        match self.store.put_if_unchanged(&self.tenant, lock, put).await {
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
        let declared_at = format!("{}.{}", self.scope().resources(), input.name);
        self.ensure_provider(&provider_name, warnings).await?;
        let provider = self.loaded(&provider_name)?;
        if let Some(row) = input.stored {
            require_same_source(row, provider)?;
        }
        let schema = resource_schema(provider, &provider_name, &declaration.resource_type)
            .map_err(|error| validation::at_path(error, &format!("{declared_at}.type")))?;
        let block = &schema.block;
        let value_type = block.implied_type();

        // Check schema compatibility before any source output is copied
        // into this configuration.
        let sensitive_destinations =
            self.validate_output_references(&address, &input.references, block)?;
        let consumer = SourceAddress::Resource(address.clone());
        let configuration = self
            .prepare_configuration(
                ConfigurationInput {
                    consumer: &consumer,
                    declaration,
                    references: &input.references,
                    block,
                    phase: ConfigurationPhase::Planning,
                    secrets: SecretUse::Resolve,
                },
                |reference| planned_output(input.planned_outputs, reference),
            )
            .await?;
        let configuration = block.normalize_configuration(configuration);
        let mut sensitive = masked_attributes(block, sensitive_destinations);
        if let Some(row) = input.stored {
            sensitive.extend(resource_sensitivity(row, schema));
            sensitive.sort();
            sensitive.dedup();
            register_stored_values(row, &sensitive);
        }
        register_typed_attributes(&configuration, &sensitive, &value_type);
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
            Some(row) => {
                refresh(
                    provider,
                    RefreshInput {
                        row,
                        sensitive: &sensitive,
                    },
                    warnings,
                )
                .await?
            }
            None => None,
        };

        let mut change = ResourceChange {
            address: address.clone(),
            provider: provider_name,
            action: Action::NoOp,
            before: Value::Null,
            after: Value::Null,
            sensitive,
            requires_replace: Vec::new(),
            replacement_path_identities: Vec::new(),
            dependencies: input.dependencies.clone(),
            references: input.references.clone(),
            reference_configuration: if input.references.references().is_empty() {
                None
            } else {
                Some(input.references.without_write_only_values(&configuration)?)
            },
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
            sensitive: &change.sensitive,
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
            // Terraform plans the create with the private data of the object
            // it replaces.
            let (planned, step) = plan_create(&create, prior.private.clone(), warnings).await?;
            change.after = planned;
            change.steps = replacement_steps(&prior, step);
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
                prior_identity: prior.identity.clone(),
            })
            .await?;
        let (planned, planned_bytes) = sensitive_provider_value(SensitiveProviderValue {
            value: response.planned_state.as_ref(),
            value_type: &value_type,
            sensitive: &change.sensitive,
        })?;
        crate::write_only::check_state(block, &planned, "plan resource")?;
        check_diagnostics(&format!("plan {address}"), &response.diagnostics, warnings)?;
        reject_deferral(&address, response.deferred.as_ref())?;
        PlanValidity {
            address: &address,
            block,
            prior: &prior.value,
            configuration: &configuration,
            planned: &planned,
            legacy_type_system: response.legacy_type_system,
        }
        .check()?;

        let requires_replace = replacement_paths(&ReplacementCheck {
            address: &address,
            paths: &response.requires_replace,
            prior: &prior.value,
            planned: &planned,
            value_type: &value_type,
            block,
        })?;
        let reference_output_unknown = input.references.has_unknown_destination(&configuration);

        if requires_replace.is_empty()
            && !reference_output_unknown
            && type_system::semantically_equal(&planned, &prior.value, &value_type)
        {
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
                    sensitive_attributes: remembered_sensitive_attributes(
                        &change.sensitive,
                        Some(row),
                    ),
                    tainted: false,
                    // The refreshed identity, so a record learns its identity
                    // (or an upgraded one) without a change to the object.
                    identity: identity::to_stored(
                        provider.identity_schemas.get(&declaration.resource_type),
                        prior.identity.as_deref(),
                    )?
                    .or_else(|| row.identity.clone()),
                    serial: row.serial,
                    generation: row.generation,
                };
                if record != *row {
                    change.action = Action::Refresh;
                    change.refreshed_record = Some(record);
                }
            }
            return Ok(change);
        }

        if requires_replace.rendered.is_empty() {
            change.action = Action::Update;
            change.steps.push(ApplyStep {
                kind: StepKind::Update,
                prior: prior.bytes,
                planned: planned_bytes,
                configuration: configuration_bytes,
                planned_private: response.planned_private,
                prior_private: prior.private,
                planned_value: planned.clone(),
                planned_identity: identity::planned(
                    provider.identity_schemas.get(&declaration.resource_type),
                    response.planned_identity.as_ref(),
                ),
                prior_identity: prior.identity,
            });
            change.after = planned;
            return Ok(change);
        }

        change.action = Action::Replace;
        change.requires_replace = requires_replace.rendered;
        change.replacement_path_identities = requires_replace.identities;
        let (planned, step) = plan_create(&create, response.planned_private, warnings).await?;
        change.after = planned;
        change.steps = replacement_steps(&prior, step);
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
                    "cannot delete {}: provider '{}' is unavailable ({error}); keep it in {} until its resources are gone",
                    row.address,
                    row.provider,
                    self.scope().providers()
                ))
            })?;
        let provider = self.loaded(&row.provider)?;
        require_same_source(row, provider)?;
        let schema = resource_schema(provider, &row.provider, &row.address.resource_type)?;
        let sensitive = resource_sensitivity(row, schema);
        let prior = refresh(
            provider,
            RefreshInput {
                row,
                sensitive: &sensitive,
            },
            warnings,
        )
        .await?;

        let mut change = ResourceChange {
            address: row.address.clone(),
            provider: row.provider.clone(),
            action: Action::Delete,
            before: Value::Null,
            after: Value::Null,
            sensitive,
            requires_replace: Vec::new(),
            replacement_path_identities: Vec::new(),
            dependencies: row.dependencies.clone(),
            references: ConfigurationReferences::default(),
            reference_configuration: None,
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
                        sensitive: &change.sensitive,
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

    /// Check that every reference joins compatible top-level scalar
    /// attributes. Provider schemas are authoritative; generated CUE types
    /// catch the common mistakes before the engine starts a provider.
    /// Returns the destination arguments set from a sensitive source output or
    /// from a secret: plans mask them as they mask the provider's own
    /// sensitive attributes, and state records them as sensitive.
    fn validate_output_references(
        &self,
        destination: &ResourceAddress,
        references: &ConfigurationReferences,
        destination_block: &Block,
    ) -> Result<Vec<String>> {
        let mut sensitive = Vec::new();
        for reference in references.references() {
            let source_declaration =
                data_sources::declaration(&self.infrastructure, &reference.source).ok_or_else(
                    || reference_error(destination, reference, "source resource is not declared"),
                )?;
            let source_provider = self.loaded(source_declaration.provider_name())?;
            let source_schema = match &reference.source {
                SourceAddress::Resource(address) => resource_schema(
                    source_provider,
                    source_declaration.provider_name(),
                    &address.resource_type,
                )?,
                SourceAddress::DataSource(address) => {
                    data_source_schema(source_provider, &address.resource_type)?
                }
            };
            let joined = OutputReferenceCheck {
                destination,
                reference,
                destination_block,
                source_block: &source_schema.block,
                declared_type: source_declaration.output_types.get(&reference.attribute),
            }
            .check()?;
            let inherited = self
                .output_sensitivity
                .get(&reference.source)
                .is_some_and(|attributes| attributes.contains(&reference.attribute));
            if (joined.sensitive || inherited)
                && !sensitive.contains(&reference.destination_attribute)
            {
                sensitive.push(reference.destination_attribute.clone());
            }
        }
        // An argument set from a secret is as sensitive as the secret, whatever
        // the provider's schema says about it.
        for attribute in references.secret_attributes() {
            if !sensitive.contains(&attribute) {
                sensitive.push(attribute);
            }
        }
        Ok(sensitive)
    }

    fn loaded(&self, name: &str) -> Result<&LoadedProvider> {
        self.providers.get(name).ok_or_else(|| {
            InfrastructureError::configuration(format!("provider '{name}' is not loaded"))
        })
    }

    async fn ensure_provider_schema(
        &mut self,
        name: &str,
        warnings: &mut Vec<String>,
    ) -> Result<()> {
        self.ensure_not_stopped()?;
        if self.providers.contains_key(name) {
            return Ok(());
        }
        let declaration = self
            .infrastructure
            .providers
            .get(name)
            .cloned()
            .ok_or_else(|| {
                let scope = self.scope();
                InfrastructureError::configuration(format!(
                    "provider '{name}' is not declared in {}{}",
                    scope.providers(),
                    scope.inheritance_note()
                ))
            })?;
        let source = ProviderSource::parse(&declaration.source)?;
        let binary = self.resolve_binary(name, &declaration, &source).await?;
        self.ensure_not_stopped()?;

        let client = ProviderClient::launch(&LaunchOptions {
            binary: &binary,
            withheld_environment_variables: &self.options.withheld_environment_variables,
            provider_environment_variables: &self.options.provider_environment_variables,
            cancellation: &self.options.cancellation,
        })
        .await?;
        let initialized = async {
            self.ensure_not_stopped()?;
            let (schema, diagnostics) = client.schema().await?;
            self.ensure_not_stopped()?;
            check_diagnostics(
                &format!("load provider '{name}' schema"),
                &diagnostics,
                warnings,
            )?;
            self.check_schema_digest(
                &DeclaredProvider {
                    name,
                    declaration: &declaration,
                    source: &source,
                },
                &schema,
            )?;
            self.ensure_not_stopped()?;
            let identity_schemas = identity::fetch(&client).await?;

            Ok::<_, InfrastructureError>((schema, identity_schemas))
        }
        .await;
        let (schema, identity_schemas) = match initialized {
            Ok(loaded) => loaded,
            Err(error) => {
                client.shutdown().await;
                return Err(error);
            }
        };

        tracing::debug!(provider = name, protocol = ?client.protocol(), binary = %binary.display(), "provider ready");
        self.providers.insert(
            name.to_string(),
            LoadedProvider {
                client,
                schema,
                identity_schemas,
                source: source.to_string(),
                configured: false,
            },
        );
        Ok(())
    }

    async fn ensure_provider(&mut self, name: &str, warnings: &mut Vec<String>) -> Result<()> {
        self.ensure_provider_schema(name, warnings).await?;
        if self.loaded(name)?.configured {
            return Ok(());
        }
        let initialized = async {
            let provider = self.loaded(name)?;
            let declaration = self.infrastructure.providers.get(name).ok_or_else(|| {
                InfrastructureError::configuration("provider declaration disappeared")
            })?;
            let block = &provider.schema.provider.block;
            ConfigurationReferences::extract_with_schema(&declaration.configuration, block, &[])?
                .reject_write_only()?;
            let value_type = block.implied_type();
            let raw_configuration = Value::from_configuration_json(
                &serde_json::Value::Object(declaration.configuration.clone()),
                &Type::Dynamic,
            )?;
            register_attributes(&raw_configuration, &block.sensitive_attributes());
            let configuration = block.normalize_configuration(
                Value::from_configuration_json(
                    &serde_json::Value::Object(declaration.configuration.clone()),
                    &value_type,
                )
                .map_err(|error| {
                    InfrastructureError::configuration(format!("provider '{name}': {error}"))
                })?,
            );
            let bytes = type_system::to_message_pack(&configuration, &value_type)?;
            // Low-level callers can supply provider credentials directly.
            // Register them before validation/configuration RPCs can echo them.
            register_typed_attributes(&configuration, &block.sensitive_attributes(), &value_type);
            self.ensure_not_stopped()?;
            let diagnostics = provider
                .client
                .validate_provider_configuration(bytes.clone())
                .await?;
            self.ensure_not_stopped()?;
            check_diagnostics(
                &format!("validate provider '{name}'"),
                &diagnostics,
                warnings,
            )?;
            let diagnostics = provider.client.configure(bytes).await?;
            self.ensure_not_stopped()?;
            check_diagnostics(
                &format!("configure provider '{name}'"),
                &diagnostics,
                warnings,
            )
        }
        .await;
        if let Err(error) = initialized {
            if let Some(provider) = self.providers.remove(name) {
                provider.client.shutdown().await;
            }
            return Err(error);
        }
        if let Some(provider) = self.providers.get_mut(name) {
            provider.configured = true;
        }
        Ok(())
    }

    async fn resolve_binary(
        &self,
        name: &str,
        declaration: &InfrastructureProvider,
        source: &ProviderSource,
    ) -> Result<PathBuf> {
        let declared_at = format!("{}.{name}", self.scope().providers());
        if let Some(path) = &declaration.path {
            if path.is_empty() {
                return Err(InfrastructureError::configuration(format!(
                    "{declared_at}.path: must not be empty"
                )));
            }
            if declaration.version.is_some() {
                return Err(InfrastructureError::configuration(format!(
                    "{declared_at}: provider '{name}' sets both `path` and `version`; set exactly one"
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
                "{declared_at}: provider '{name}' needs an exact `version` (or a local `path`); set exactly one"
            ))
        })?;
        let cache = self
            .options
            .plugin_cache_directory
            .clone()
            .unwrap_or_else(default_cache_directory);
        let installer = ProviderInstaller::try_new(cache)?;
        let Some(locked) = self.options.locked_providers.get(&source.to_string()) else {
            return installer
                .install(&Installation {
                    source,
                    version,
                    archive_sha256: None,
                    signing_keys: &declaration.signing_keys,
                })
                .await;
        };
        if locked.version != version {
            return Err(InfrastructureError::configuration(format!(
                "{declared_at}: provider '{name}' asks for {source} {version}, but cuenv.lock pins \
                 {}; run `cuenv infrastructure provider add {source}@{version}` to move the pin \
                 and regenerate its CUE types",
                locked.version
            )));
        }
        let platform = current_platform()?.to_string();
        let archive_sha256 = locked.archive_sha256(&platform).ok_or_else(|| {
            InfrastructureError::install(format!(
                "cuenv.lock pins {source} {version} but records no archive for {platform}; the \
                 release is not published for this platform, or the pin was made before it was"
            ))
        })?;
        installer
            .install(&Installation {
                source,
                version,
                archive_sha256: Some(archive_sha256),
                signing_keys: &declaration.signing_keys,
            })
            .await
    }

    /// Refuse a provider whose live schema is not the one the project's
    /// generated CUE types (`schemaDigest`) or `cuenv.lock` were made from.
    fn check_schema_digest(
        &self,
        provider: &DeclaredProvider<'_>,
        schema: &crate::schema::ProviderSchema,
    ) -> Result<()> {
        let DeclaredProvider {
            name,
            declaration,
            source,
        } = provider;
        let live = crate::cue_types::schema_digest(schema);
        let declared_at = format!("{}.{name}", self.scope().providers());
        let release = declaration.version.as_deref().map_or_else(
            || {
                format!(
                    "{source} at {}",
                    declaration.path.as_deref().unwrap_or_default()
                )
            },
            |version| format!("{source} {version}"),
        );
        if let Some(version) = &declaration.version
            && let Some(locked) = self.options.locked_providers.get(&source.to_string())
            && locked.schema_digest != live
        {
            return Err(InfrastructureError::configuration(format!(
                "{declared_at}: cuenv.lock pins {source} {version} with schema {}, but the \
                 installed provider reports {live}; the schema or cuenv generation contract has \
                 changed. Run `cuenv infrastructure provider add {source}@{version}` to pin it \
                 again",
                locked.schema_digest
            )));
        }
        if let Some(declared) = &declaration.schema_digest
            && *declared != live
        {
            return Err(InfrastructureError::configuration(format!(
                "{declared_at}: the CUE types for provider '{name}' were generated from schema \
                 {declared}, but {release} reports {live}. Run `cuenv sync infrastructure` to \
                 regenerate them from cuenv.lock, or `cuenv infrastructure provider add` for \
                 another version"
            )));
        }
        Ok(())
    }
}

/// A provider declaration with its local name and parsed source.
struct DeclaredProvider<'declaration> {
    name: &'declaration str,
    declaration: &'declaration InfrastructureProvider,
    source: &'declaration ProviderSource,
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

struct InterruptedOperation<'operation> {
    address: &'operation ResourceAddress,
    progress: Progress,
    on_event: &'operation mut (dyn FnMut(ApplyEvent) + Send),
}

impl Progress {
    const fn interrupted(self) -> InfrastructureError {
        InfrastructureError::Interrupted {
            completed: self.completed,
            total: self.total,
        }
    }
}

/// What [`InfrastructureEngine::converge`] runs.
struct Convergence<'apply> {
    plan: &'apply Plan,
    lock: &'apply StateLock,
    rows: BTreeMap<ResourceAddress, ManagedResource>,
    on_event: &'apply mut (dyn FnMut(ApplyEvent) + Send),
}

/// Runs the operations of a schedule. The engine is the only runner outside
/// tests.
#[async_trait]
trait OperationRunner {
    async fn run_operation(
        &self,
        operation: &ApplyOperation<'_>,
        context: OperationRunContext<'_>,
    ) -> std::result::Result<(), OperationFailure>;

    async fn run_data_source(
        &self,
        read: &DataSourcePlan,
        context: DataSourceReadContext<'_>,
    ) -> Result<(Value, Vec<String>)>;
}

#[async_trait]
impl OperationRunner for InfrastructureEngine {
    async fn run_operation(
        &self,
        operation: &ApplyOperation<'_>,
        context: OperationRunContext<'_>,
    ) -> std::result::Result<(), OperationFailure> {
        self.apply_operation(operation, context).await
    }

    async fn run_data_source(
        &self,
        read: &DataSourcePlan,
        context: DataSourceReadContext<'_>,
    ) -> Result<(Value, Vec<String>)> {
        self.resolve_and_read_data_source(read, context.rows, context.values)
            .await
    }
}

/// An apply in progress: where it stands in its schedule, how far it got,
/// and what it writes to.
struct Execution<'run, 'schedule, 'plan> {
    run: ScheduleRun<'schedule, 'plan>,
    progress: Progress,
    lock: &'run StateLock,
    rows: BTreeMap<ResourceAddress, ManagedResource>,
    data_sources: DataSourceExecution,
    data_source_plans: &'plan [DataSourcePlan],
    on_event: &'run mut (dyn FnMut(ApplyEvent) + Send),
}

fn pending_addresses(remaining: &BTreeMap<ResourceAddress, usize>) -> BTreeSet<ResourceAddress> {
    remaining
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(address, _)| address.clone())
        .collect()
}

fn settle(remaining: &mut BTreeMap<ResourceAddress, usize>, address: &ResourceAddress) {
    if let Some(count) = remaining.get_mut(address) {
        *count = count.saturating_sub(1);
    }
}

/// How one operation failed.
#[derive(Debug)]
enum OperationFailure {
    /// The provider reported the failure and the operation was recorded as
    /// far as it got: the rest of the run does not depend on it.
    Continue(InfrastructureError),
    /// The run cannot go on: the outcome is unknown, or recording failed.
    Abort(InfrastructureError),
    /// The run cannot go on, and the provider has deleted the object: only
    /// recording the deletion failed.
    AbortAfterDelete(InfrastructureError),
}

impl OperationFailure {
    /// A consumer replan can fail safely when validation or the provider
    /// rejects its final configuration. Storage, lock, interruption, and
    /// recovery failures stop the apply run so no other mutation proceeds
    /// without a trustworthy view of state.
    fn from_reference_replan(error: InfrastructureError) -> Self {
        match error {
            InfrastructureError::Configuration(_)
            | InfrastructureError::Codec(_)
            | InfrastructureError::Plugin(_)
            | InfrastructureError::RemoteProcedure { .. }
            | InfrastructureError::Diagnostics { .. } => Self::Continue(error),
            _ => Self::Abort(error),
        }
    }

    const fn continues(&self) -> bool {
        matches!(self, Self::Continue(_))
    }

    /// What the failure leaves behind for an operation of `kind`: a create
    /// whose record could not be written (and was saved locally instead)
    /// has made its object, and a delete that could not be recorded has
    /// removed it.
    fn aftermath(&self, kind: OperationKind) -> Aftermath {
        match self {
            Self::AbortAfterDelete(_) => Aftermath::Deleted,
            Self::Abort(
                InfrastructureError::UnrecordedChange { .. }
                | InfrastructureError::UnrecordedChangeLost { .. },
            ) if kind == OperationKind::ReplaceCreate => Aftermath::Created,
            Self::Continue(_) | Self::Abort(_) => Aftermath::Unknown,
        }
    }

    fn into_error(self) -> InfrastructureError {
        match self {
            Self::Continue(error) | Self::Abort(error) | Self::AbortAfterDelete(error) => error,
        }
    }
}

impl From<InfrastructureError> for OperationFailure {
    fn from(error: InfrastructureError) -> Self {
        Self::Abort(error)
    }
}

/// A create over a remotely missing object updates its existing record;
/// only an absent-record insertion allocates a fresh generation.
fn write_generation(expected: RecordVersion) -> uuid::Uuid {
    match expected {
        RecordVersion::Generation { generation, .. } => generation,
        RecordVersion::Absent => uuid::Uuid::new_v4(),
    }
}

/// Decode the state `ApplyResourceChange` returned. When it cannot be
/// decoded, the provider's error diagnostics (which usually explain why)
/// are reported rather than hidden behind the decoding failure.
fn applied_value(
    response: &protocol::ApplyResourceChangeResponse,
    value_type: &Type,
    context: &str,
) -> Result<Value> {
    match decode_provider_value(response.new_state.as_ref(), value_type) {
        Ok(returned) => Ok(returned),
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

/// A state store failure writing refreshed records, naming their addresses.
fn naming_addresses(
    error: InfrastructureError,
    records: &[ManagedResource],
) -> InfrastructureError {
    match error {
        InfrastructureError::State(message) => {
            let addresses: Vec<String> = records
                .iter()
                .map(|record| record.address.to_string())
                .collect();
            InfrastructureError::State(format!(
                "recording the refreshed state of {}: {message}",
                addresses.join(", ")
            ))
        }
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
    /// The object's identity, MessagePack against the identity schema.
    identity: Option<Vec<u8>>,
}

/// The identity to record after an apply, and a warning when the provider
/// returned identity that cannot be kept. The change already happened, so a
/// bad identity never fails it: the record keeps its earlier identity, as
/// a client without identity support would.
fn applied_identity(
    provider: &LoadedProvider,
    address: &ResourceAddress,
    response: &protocol::ApplyResourceChangeResponse,
    stored: Option<&ManagedResource>,
) -> (Option<serde_json::Value>, Option<String>) {
    let schema = provider.identity_schemas.get(&address.resource_type);
    let earlier = || stored.and_then(|stored| stored.identity.clone());
    match identity::received(
        schema,
        response.new_identity.as_ref(),
        &format!("apply {address}"),
    )
    .and_then(|bytes| identity::to_stored(schema, bytes.as_deref()))
    {
        Ok(Some(identity)) => (Some(identity), None),
        Ok(None) => (earlier(), None),
        Err(error) => (
            earlier(),
            Some(format!(
                "{error}; the earlier identity of {address} is kept"
            )),
        ),
    }
}

fn delete_step(prior: &Refreshed) -> ApplyStep {
    ApplyStep {
        kind: StepKind::Delete,
        prior: prior.bytes.clone(),
        planned: NULL_MESSAGE_PACK.to_vec(),
        configuration: NULL_MESSAGE_PACK.to_vec(),
        planned_private: prior.private.clone(),
        prior_private: prior.private.clone(),
        planned_value: Value::Null,
        // As in Terraform, a delete carries the identity of the object it
        // deletes.
        prior_identity: prior.identity.clone(),
        planned_identity: prior.identity.clone(),
    }
}

/// The two steps of a replacement. As in Terraform, both halves carry the
/// planned private data of the one change, which is the create plan's: the
/// delete is not given the private data refreshed from the old object.
fn replacement_steps(prior: &Refreshed, create: ApplyStep) -> Vec<ApplyStep> {
    let mut delete = delete_step(prior);
    delete.planned_private.clone_from(&create.planned_private);
    vec![delete, create]
}

/// Inputs for [`plan_destroy`].
struct DestroyPlanInput<'input> {
    provider: &'input LoadedProvider,
    address: &'input ResourceAddress,
    value_type: &'input Type,
    prior: &'input Refreshed,
    sensitive: &'input [String],
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
            prior_identity: input.prior.identity.clone(),
        })
        .await?;
    let (planned, _) = sensitive_provider_value(SensitiveProviderValue {
        value: response.planned_state.as_ref(),
        value_type: input.value_type,
        sensitive: input.sensitive,
    })?;
    let schema = resource_schema(
        input.provider,
        &input.provider.source,
        &address.resource_type,
    )?;
    crate::write_only::check_state(&schema.block, &planned, &format!("plan destroy {address}"))?;
    check_diagnostics(
        &format!("plan destroy of {address}"),
        &response.diagnostics,
        warnings,
    )?;
    reject_deferral(address, response.deferred.as_ref())?;
    if !planned.is_null() {
        return Err(InfrastructureError::plugin(format!(
            "provider planned a non-null destroy value for {address}, which is a provider bug"
        )));
    }
    Ok(response.planned_private)
}

struct RefreshInput<'input> {
    row: &'input ManagedResource,
    /// Includes newly inherited marks as well as stored and schema marks.
    sensitive: &'input [String],
}

async fn refresh(
    provider: &LoadedProvider,
    input: RefreshInput<'_>,
    warnings: &mut Vec<String>,
) -> Result<Option<Refreshed>> {
    let RefreshInput { row, sensitive } = input;
    let type_name = &row.address.resource_type;
    let schema = resource_schema(provider, &row.provider, type_name)?;
    let value_type = schema.block.implied_type();
    register_stored_values(row, sensitive);
    let (_, upgraded_bytes) = upgrade_state(
        provider,
        UpgradeStateInput {
            row,
            schema,
            sensitive,
        },
        warnings,
    )
    .await?;

    let identity_schema = provider.identity_schemas.get(type_name);
    let current_identity = identity::from_stored(identity::StoredIdentity {
        client: &provider.client,
        type_name,
        schema: identity_schema,
        stored: row.identity.as_ref(),
    })
    .await?;
    let response = provider
        .client
        .read_resource(ReadRequest {
            type_name,
            current_state: upgraded_bytes,
            private: row.private.clone(),
            current_identity: current_identity.clone(),
        })
        .await?;
    let (value, bytes) = sensitive_provider_value(SensitiveProviderValue {
        value: response.new_state.as_ref(),
        value_type: &value_type,
        sensitive,
    })?;
    crate::write_only::check_state(&schema.block, &value, &format!("refresh {}", row.address))?;
    let identity = identity::received(
        identity_schema,
        response.new_identity.as_ref(),
        &format!("refresh {}", row.address),
    )?
    .or(current_identity);
    check_diagnostics(
        &format!("refresh {}", row.address),
        &response.diagnostics,
        warnings,
    )?;
    reject_deferral(&row.address, response.deferred.as_ref())?;
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
        identity,
    }))
}

/// Ask the provider to upgrade a stored record to its current schema
/// version. Returns the upgraded state and its MessagePack encoding.
struct UpgradeStateInput<'input> {
    row: &'input ManagedResource,
    schema: &'input Schema,
    sensitive: &'input [String],
}

async fn upgrade_state(
    provider: &LoadedProvider,
    input: UpgradeStateInput<'_>,
    warnings: &mut Vec<String>,
) -> Result<(Value, Vec<u8>)> {
    let UpgradeStateInput {
        row,
        schema,
        sensitive,
    } = input;
    register_stored_values(row, sensitive);
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
    if row.schema_version == schema.version {
        // UpgradeResourceState accepts raw JSON and provider codecs can round
        // or underflow its numbers. Check with the known schema before sending.
        let stored = type_system::from_json_bytes(&state_json, &value_type)?;
        crate::write_only::check_state(
            &schema.block,
            &stored,
            &format!("stored state of {}", row.address),
        )?;
        let _ = type_system::to_message_pack(&stored, &value_type)?;
    } else {
        // The old schema is unavailable. A numeric-looking string may be our
        // lossless numeric state representation, so check it conservatively
        // instead of risking silent corruption during a schema upgrade.
        validate_historical_state_numbers(&row.state, 0)?;
    }
    let upgrade = provider
        .client
        .upgrade_resource_state(&row.address.resource_type, row.schema_version, state_json)
        .await?;
    let (upgraded, upgraded_bytes) = sensitive_provider_value(SensitiveProviderValue {
        value: upgrade.upgraded_state.as_ref(),
        value_type: &value_type,
        sensitive,
    })?;
    crate::write_only::check_state(
        &schema.block,
        &upgraded,
        &format!("upgrade {}", row.address),
    )?;
    check_diagnostics(
        &format!("upgrade state of {}", row.address),
        &upgrade.diagnostics,
        warnings,
    )?;
    check_upgraded(&row.address, &upgraded)?;
    Ok((upgraded, upgraded_bytes))
}

fn validate_historical_state_numbers(value: &serde_json::Value, depth: usize) -> Result<()> {
    if depth >= 128 {
        return Err(InfrastructureError::state(
            "stored state nesting exceeds the supported depth",
        ));
    }
    match value {
        serde_json::Value::Number(number) => {
            let value = Value::Number(number.clone().into());
            let _ = type_system::to_message_pack(&value, &Type::Number)?;
        }
        serde_json::Value::String(text) => {
            if let Ok(number) = text.parse::<type_system::Number>() {
                let _ = type_system::to_message_pack(&Value::Number(number), &Type::Number)?;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                validate_historical_state_numbers(item, depth + 1)?;
            }
        }
        serde_json::Value::Object(fields) => {
            for item in fields.values() {
                validate_historical_state_numbers(item, depth + 1)?;
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) => {}
    }
    Ok(())
}

/// A stored record's state in the provider's current schema: decoded
/// directly when it was written with that schema version, and otherwise
/// upgraded by the provider first.
async fn upgraded_state(
    provider: &LoadedProvider,
    row: &ManagedResource,
    schema: &Schema,
    warnings: &mut Vec<String>,
) -> Result<Value> {
    let sensitive = resource_sensitivity(row, schema);
    register_stored_values(row, &sensitive);
    if row.schema_version != schema.version {
        return Ok(upgrade_state(
            provider,
            UpgradeStateInput {
                row,
                schema,
                sensitive: &sensitive,
            },
            warnings,
        )
        .await?
        .0);
    }
    let state_json = serde_json::to_vec(&row.state)
        .map_err(|error| InfrastructureError::state(json_error_category(&error)))?;
    let value = type_system::from_json_bytes(&state_json, &schema.block.implied_type())?;
    register_attributes(&value, &sensitive);
    Ok(value)
}

/// Inputs for planning a create.
struct CreatePlanInput<'input> {
    provider: &'input LoadedProvider,
    address: &'input ResourceAddress,
    schema: &'input Schema,
    configuration: &'input Value,
    configuration_bytes: &'input [u8],
    sensitive: &'input [String],
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
    let prior_private_for_plan = prior_private.clone();
    let response = input
        .provider
        .client
        .plan_resource_change(PlanRequest {
            type_name: &address.resource_type,
            prior_state: NULL_MESSAGE_PACK.to_vec(),
            proposed_new_state: type_system::to_message_pack(&proposed, &value_type)?,
            configuration: input.configuration_bytes.to_vec(),
            prior_private,
            prior_identity: None,
        })
        .await?;
    let (planned, planned_bytes) = sensitive_provider_value(SensitiveProviderValue {
        value: response.planned_state.as_ref(),
        value_type: &value_type,
        sensitive: input.sensitive,
    })?;
    crate::write_only::check_state(&input.schema.block, &planned, "plan create")?;
    check_diagnostics(
        &format!("plan create of {address}"),
        &response.diagnostics,
        warnings,
    )?;
    reject_deferral(address, response.deferred.as_ref())?;
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
            prior_private: prior_private_for_plan,
            planned_value: planned,
            prior_identity: None,
            planned_identity: identity::planned(
                input.provider.identity_schemas.get(&address.resource_type),
                response.planned_identity.as_ref(),
            ),
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
        crate::write_only::check_state(
            self.block,
            self.planned,
            &format!("plan {}", self.address),
        )?;
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
    block: &'check Block,
}

#[derive(Debug, Default)]
struct ReplacementPaths {
    rendered: Vec<String>,
    identities: Vec<[u8; 32]>,
}

impl ReplacementPaths {
    fn is_empty(&self) -> bool {
        self.rendered.is_empty()
    }
}

/// The replacement paths whose value actually changes (or is not yet
/// known), as Terraform honours them; SDKv2 providers report spurious ones.
/// Values are compared with their schema type, so set order and number
/// representation do not count as changes.
fn replacement_paths(check: &ReplacementCheck<'_>) -> Result<ReplacementPaths> {
    let mut changed = ReplacementPaths::default();
    for path in check.paths {
        let steps = path_steps(path);
        let before = type_system::value_at_path(check.prior, &steps);
        let after = type_system::value_at_path(check.planned, &steps);
        if before.is_none() && after.is_none() {
            return Err(InfrastructureError::plugin(format!(
                "provider reported that changing `{}` of {} requires replacement, but that \
                 attribute path exists in neither the prior nor the planned state",
                render_schema_path(&steps, check.value_type),
                check.address
            )));
        }
        let value_type = check.value_type.at_path(&steps).unwrap_or(Type::Dynamic);
        let before = before.unwrap_or(&Value::Null);
        let after = after.unwrap_or(&Value::Null);
        let write_only = crate::write_only::paths(check.block, check.planned)
            .iter()
            .any(|path| path.starts_with(&steps))
            || crate::write_only::paths(check.block, check.prior)
                .iter()
                .any(|path| path.starts_with(&steps));
        if write_only || !type_system::semantically_equal(before, after, &value_type) {
            changed
                .rendered
                .push(render_schema_path(&steps, check.value_type));
            let mut digest = DigestWriter::default();
            digest.path(&steps);
            changed.identities.push(digest.hasher.finalize().into());
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

/// Decode a provider value without preparing another provider request.
/// Absent or empty values are null.
fn decode_provider_value(
    value: Option<&protocol::DynamicValue>,
    value_type: &Type,
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

/// Decode a value and prepare its bytes for a subsequent provider call. Apply
/// responses use decode_provider_value instead: a successful mutation must be
/// recorded even when its returned number cannot safely be sent back later.
fn provider_value(
    value: Option<&protocol::DynamicValue>,
    value_type: &Type,
) -> Result<(Value, Vec<u8>)> {
    let decoded = decode_provider_value(value, value_type)?;
    let bytes = match value {
        Some(dynamic_value) if !dynamic_value.message_pack.is_empty() => {
            // Keep provider bytes to preserve unknown refinements, but check
            // every decoded number before reusing them in another request.
            let _ = type_system::to_message_pack(&decoded, value_type)?;
            dynamic_value.message_pack.clone()
        }
        Some(dynamic_value) if !dynamic_value.json.is_empty() => {
            type_system::to_message_pack(&decoded, value_type)?
        }
        _ => NULL_MESSAGE_PACK.to_vec(),
    };
    Ok((decoded, bytes))
}

fn is_error(diagnostic: &Diagnostic) -> bool {
    Severity::try_from(diagnostic.severity).map_or(true, |severity| severity != Severity::Warning)
}

/// Render a diagnostic as a single human-readable string, without the
/// control characters a provider could use to drive the terminal.
///
/// Provider text is redacted before it is stripped: a secret that
/// contains a control character would no longer match once stripped.
fn render_diagnostic(diagnostic: &Diagnostic) -> String {
    // Engine-discovered sensitive outputs are registered with the event
    // registry even when a caller supplies its own provider-log redactor.
    render_diagnostic_with(diagnostic, |text| {
        cuenv_events::redact(&redact_provider_text(text))
    })
}

fn render_diagnostic_with(diagnostic: &Diagnostic, redact: fn(&str) -> String) -> String {
    let sanitize = |text: &str| strip_control_characters_except_newlines(&redact(text));
    let mut rendered = sanitize(&diagnostic.summary);
    if !diagnostic.detail.is_empty() {
        rendered.push_str(": ");
        rendered.push_str(&sanitize(&diagnostic.detail));
    }
    if let Some(path) = &diagnostic.attribute
        && !path.steps.is_empty()
    {
        let _ = write!(rendered, " (at {})", sanitize(&render_provider_path(path)));
    }
    rendered
}

/// Fail on error diagnostics; collect warnings.
pub(crate) fn check_diagnostics(
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

/// Provider diagnostics are untrusted and do not carry a schema. Attribute
/// names may themselves be dynamic keys, so do not print them as labels.
fn render_provider_path(path: &protocol::AttributePath) -> String {
    let steps: Vec<_> = path_steps(path)
        .into_iter()
        .map(|step| match step {
            PathStep::Attribute(_) => PathStep::Attribute("<hidden field>".into()),
            other => other,
        })
        .collect();
    crate::object_change::render_diagnostic_steps(&steps)
}

/// A replacement path can show schema-known object attributes. Map keys,
/// dynamic object labels and unknown provider-supplied selectors stay hidden.
fn render_schema_path(steps: &[PathStep], value_type: &Type) -> String {
    let mut current = Some(value_type.clone());
    let mut safe = Vec::with_capacity(steps.len());
    for step in steps {
        let shown = match (&current, step) {
            (Some(Type::Object(fields)), PathStep::Attribute(name))
                if fields.contains_key(name) =>
            {
                step.clone()
            }
            (_, PathStep::Attribute(name)) => PathStep::Key(name.clone()),
            _ => step.clone(),
        };
        current = current.and_then(|kind| kind.at_path(std::slice::from_ref(step)));
        safe.push(shown);
    }
    crate::object_change::render_diagnostic_steps(&safe)
}

/// One reference's source and destination schemas, checked together.
struct OutputReferenceCheck<'check> {
    destination: &'check ResourceAddress,
    reference: &'check ResourceOutputReference,
    destination_block: &'check Block,
    source_block: &'check Block,
    declared_type: Option<&'check InfrastructureOutputType>,
}

/// The source attribute a valid reference reads.
struct JoinedOutput {
    sensitive: bool,
}

impl OutputReferenceCheck<'_> {
    fn check(&self) -> Result<JoinedOutput> {
        let Self {
            destination,
            reference,
            destination_block,
            source_block,
            declared_type,
        } = *self;
        let destination_attribute = destination_block
            .reference_destination(&reference.destination)
            .map_err(|_| {
                reference_error(
                    destination,
                    reference,
                    "destination must be a settable scalar path",
                )
            })?;
        let source_attribute = source_block
            .attributes
            .get(&reference.attribute)
            .filter(|attribute| {
                attribute.nested.is_none()
                    && !attribute.write_only
                    && is_scalar(&attribute.value_type)
            })
            .ok_or_else(|| {
                reference_error(
                    destination,
                    reference,
                    "source must be a top-level scalar attribute",
                )
            })?;
        if let Some(declared) = declared_type
            && declared_value_type(*declared) != source_attribute.value_type
        {
            return Err(reference_error(
                destination,
                reference,
                "native CUE output declaration type does not match the provider schema",
            ));
        }
        if !reference.native.computed
            && &source_attribute.value_type != destination_attribute.value_type()
        {
            return Err(reference_error(
                destination,
                reference,
                "source and destination scalar types must match",
            ));
        }
        Ok(JoinedOutput {
            sensitive: source_attribute.sensitive,
        })
    }
}

fn declared_value_type(declared: InfrastructureOutputType) -> Type {
    match declared {
        InfrastructureOutputType::String => Type::String,
        InfrastructureOutputType::Number => Type::Number,
        InfrastructureOutputType::Boolean => Type::Boolean,
    }
}

/// Check every output a declaration names against its provider's schema, whether or not
/// anything reads it: its real value could not be checked until the object exists.
fn validate_output_declarations(
    source: &SourceAddress,
    evaluator: &dyn ConfigurationEvaluator,
    block: &Block,
) -> Result<()> {
    for identity in evaluator
        .declared_outputs()
        .iter()
        .filter(|identity| &identity.source == source)
    {
        let Some(declared) = evaluator.declared_output_type(identity) else {
            continue;
        };
        let attribute = &identity.attribute;
        let schema = block
            .attributes
            .get(attribute)
            .filter(|schema| {
                schema.nested.is_none()
                    && !schema.write_only
                    && (is_scalar(&schema.value_type) || schema.value_type == Type::Dynamic)
            })
            .ok_or_else(|| {
                InfrastructureError::configuration(format!(
                    "{source}: output {attribute} must be a top-level scalar attribute of the \
                     provider schema"
                ))
            })?;
        // A dynamic attribute can hold a scalar: only its real value can say.
        if schema.value_type != Type::Dynamic && declared_value_type(declared) != schema.value_type
        {
            return Err(InfrastructureError::configuration(format!(
                "{source}: output {attribute}: native CUE output declaration type does not \
                 match the provider schema"
            )));
        }
    }
    Ok(())
}

/// Attributes a plan masks: the block's own sensitive attributes and the
/// arguments set from a sensitive output of another resource.
fn masked_attributes(block: &Block, sensitive_destinations: Vec<String>) -> Vec<String> {
    let mut masked = block.sensitive_attributes();
    for name in sensitive_destinations {
        if !masked.contains(&name) {
            masked.push(name);
        }
    }
    masked
}

/// Removing a reference must not erase sensitivity from state that still
/// contains the copied value, including records retained after failed writes.
fn remembered_sensitive_attributes(
    current: &[String],
    prior: Option<&ManagedResource>,
) -> Vec<String> {
    current
        .iter()
        .chain(
            prior
                .into_iter()
                .flat_map(|record| &record.sensitive_attributes),
        )
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Register every known string inside a change's masked attributes, before
/// and after, so a copy of one printed anywhere else is redacted too: in a
/// consumer's provider diagnostics, in an error, or in the prior state of a
/// resource whose reference has since been removed.
fn register_sensitive_values(change: &ResourceChange) {
    for value in [&change.before, &change.after] {
        for name in &change.sensitive {
            if let Some(attribute) = value.attribute(name) {
                register_value_strings(attribute);
            }
        }
    }
}

fn register_typed_sensitive_values(change: &ResourceChange, value_type: &Type) {
    for value in [&change.before, &change.after] {
        register_typed_attributes(value, &change.sensitive, value_type);
    }
}

fn register_value_strings(value: &Value) {
    match value {
        Value::String(text) => cuenv_events::register_secret(text.clone()),
        Value::List(items) => items.iter().for_each(register_value_strings),
        Value::Object(attributes) => {
            // Value represents both maps and objects. Within an already
            // sensitive subtree, keys can be provider/user data too. The
            // enclosing schema attribute name is never passed here.
            for (name, value) in attributes {
                cuenv_events::register_secret(name.clone());
                register_value_strings(value);
            }
        }
        Value::Typed(typed) => register_value_strings(&typed.value),
        Value::Null | Value::Unknown | Value::Boolean(_) | Value::Number(_) => {}
    }
}

fn is_scalar(value_type: &Type) -> bool {
    matches!(value_type, Type::Boolean | Type::Number | Type::String)
}

fn reference_error(
    destination: &ResourceAddress,
    reference: &ResourceOutputReference,
    reason: &str,
) -> InfrastructureError {
    InfrastructureError::configuration(format!(
        "resource {destination} configuration.{}: output {}.{} {reason}",
        reference.destination, reference.source, reference.attribute
    ))
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
    for read in &plan.data_sources {
        let timing = if read.deferred {
            "read during apply"
        } else {
            "read during plan"
        };
        let _ = writeln!(rendered, "  <= data.{} ({timing})", read.address);
        for path in sensitive_reference_paths(&read.references, &read.sensitive) {
            let _ = writeln!(rendered, "      # {path} = (sensitive)");
        }
    }
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
        for argument in change
            .references
            .secrets()
            .iter()
            .filter(|argument| argument.write_only.is_some())
        {
            let _ = writeln!(rendered, "      {} = (write-only)", argument.destination);
        }
        for path in sensitive_reference_paths(&change.references, &change.sensitive) {
            let _ = writeln!(rendered, "      # {path} = (sensitive)");
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

/// Describe nested reference locations without exposing their values or map
/// keys. Ordinary event redaction still applies, including historical secrets
/// whose text happens to equal a fixed schema field label.
fn sensitive_reference_paths(
    references: &ConfigurationReferences,
    sensitive: &[String],
) -> BTreeSet<String> {
    references
        .references()
        .iter()
        .filter(|reference| {
            reference.destination.steps().len() > 1
                && sensitive.contains(&reference.destination_attribute)
        })
        .map(|reference| reference.destination.to_string())
        .collect()
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
