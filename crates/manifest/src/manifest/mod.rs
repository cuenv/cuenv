//! Manifest configuration DTOs shared across directories.
//!
//! Based on schema/core.cue.

mod base;
mod cache;
mod codegen;
mod diagnostics;
mod formatters;
mod hooks;
mod infrastructure;
mod project;
mod rules;
mod runtime;
mod services;
mod vcs;

pub use base::Base;
pub use cache::{Cache, CacheAuth, CacheAuthHeader, RemoteCache};
pub use codegen::*;
pub use diagnostics::NodeDiagnostics;
pub use formatters::*;
pub use hooks::*;
pub use infrastructure::{
    D1State, DataSourceDeclaration, Infrastructure, InfrastructureApplyTrigger,
    InfrastructureApplyWorkflow, InfrastructureConfiguration, InfrastructureOutputType,
    InfrastructurePlanWorkflow, InfrastructurePolicyAction, InfrastructureProvider,
    InfrastructureSelectionError, InfrastructureState, InfrastructureWorkflows,
    ManagedResourceDeclaration, ProviderEnvironment, S3Addressing, S3State, StateCredentialField,
    StateToken, TursoState,
};
pub use project::Project;
pub use rules::*;
pub use runtime::*;
pub use services::*;
pub use vcs::VcsDependency;
