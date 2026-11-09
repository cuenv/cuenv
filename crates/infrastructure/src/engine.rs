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

mod data_sources;
mod evaluation;
mod identity;
mod infrastructure_engine;
mod schedule;
mod sensitivity;
mod validation;

use crate::references::SourceAddress;
use crate::state::ResourceAddress;
pub use data_sources::DataSourcePlan;
use data_sources::{data_source_schema, register_attributes};
pub(crate) use infrastructure_engine::check_diagnostics;
pub use infrastructure_engine::{
    Action, ApplyContext, ApplyEvent, EngineOptions, EngineSetup, InfrastructureEngine, Plan,
    PlanDigest, PlanMode, PlanSummary, ResourceChange, ReviewedPlanDigest, declared_resource,
    render_plan,
};
use infrastructure_engine::{
    ApplyStep, ConfigurationInput, ConfigurationPhase, DataSourceReadContext, DigestWriter,
    LoadedProvider, OperationFailure, OperationRunner, ReferenceValueInput, SecretUse, StepKind,
    applied_value, decode_provider_value, masked_attributes, provider_value, reference_error,
    register_value_strings, remembered_sensitive_attributes, resource_schema, topological_order,
};
pub use validation::validate_configuration;
