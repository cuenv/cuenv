//! The context a command runs in, and dispatching an action to its handler.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use cuenv_events::emit_stderr;
use cuenv_infrastructure::{
    D1Configuration, D1StateStore, EngineOptions, EngineSetup, InfrastructureError, PlanMode,
    ProjectInstance, StateStore, TenantKey, TursoConfiguration, TursoStateStore, UnrecordedStore,
    strip_control_characters,
};
use cuenv_manifest::lockfile::LockedInfrastructureProvider;
use cuenv_manifest::manifest::{Infrastructure, InfrastructureState};

use super::action::{InfrastructureAction, StateAction};
use super::apply::{Convergence, ImportRequest, converge, import, plan};
use super::confirm::Answers;
use super::failure::failure;
use super::guard::{Siblings, tenant_label};
use super::interrupts::Interrupts;
use super::invocation::Invocation;
use super::output::Output;
use super::resolve::StateCredential;
use super::state_actions::{
    adopt, recover, remove_resource, resources_in_other_environments, unlock,
};
use super::types;
use crate::cli::CliError;

/// What every stateful subcommand works with.
pub(super) struct CommandContext<'context> {
    pub(super) store: &'context Arc<dyn StateStore>,
    pub(super) tenant: &'context TenantKey,
    /// The CUE instance (directory and package) this run evaluated.
    pub(super) instance: &'context ProjectInstance,
    /// The project's other state identities.
    pub(super) siblings: &'context Siblings,
    /// How hints name this run's project and environment.
    pub(super) invocation: &'context Invocation,
    pub(super) output: &'context Output,
    pub(super) interrupts: &'context Interrupts,
    pub(super) answers: &'context dyn Answers,
}

/// What an engine is built from, besides the context.
pub(super) struct EngineInputs {
    pub(super) infrastructure: Infrastructure,
    /// Provider releases the CUE module's `cuenv.lock` pins.
    pub(super) locked_providers: BTreeMap<String, LockedInfrastructureProvider>,
    pub(super) project_directory: PathBuf,
    /// Resolved project variables overlaid on the provider's host environment.
    pub(super) provider_environment_variables: BTreeMap<String, String>,
    /// Project variables rejected by policy must not leak through inherited
    /// host variables with the same name.
    pub(super) withheld_environment_variables: Vec<String>,
    /// Where unrecorded changes are saved; the user state directory when
    /// `None`.
    pub(super) unrecorded_directory: Option<PathBuf>,
}

pub(super) async fn dispatch(
    action: &InfrastructureAction,
    context: &CommandContext<'_>,
    inputs: EngineInputs,
) -> Result<(), CliError> {
    match action {
        InfrastructureAction::State(StateAction::List) => {
            let resources = context
                .interrupts
                .until_interrupted(context.store.list(context.tenant))
                .await?
                .map_err(|error| failure(&error, context.invocation))?;
            context.output.state(context.tenant, &resources);
            context
                .output
                .other_environments(&resources_in_other_environments(context).await);
            Ok(())
        }
        InfrastructureAction::State(StateAction::Locks) => {
            let locks = context
                .interrupts
                .until_interrupted(context.store.locks())
                .await?
                .map_err(|error| failure(&error, context.invocation))?;
            context.output.locks(&locks, context.invocation);
            Ok(())
        }
        InfrastructureAction::State(StateAction::Remove { address }) => {
            remove_resource(context, address).await
        }
        InfrastructureAction::State(StateAction::Recover { overrides }) => {
            recover(context, &unrecorded_store(&inputs, context)?, *overrides).await
        }
        InfrastructureAction::State(StateAction::Adopt) => {
            adopt(context);
            Ok(())
        }
        InfrastructureAction::Unlock {
            lock_identifier, ..
        } => {
            context
                .interrupts
                .until_interrupted(unlock(context, lock_identifier.as_deref()))
                .await?
        }
        InfrastructureAction::Plan => plan(context, inputs).await,
        InfrastructureAction::Import {
            address,
            identifier,
        } => {
            import(ImportRequest {
                context,
                inputs,
                address,
                identifier,
            })
            .await
        }
        InfrastructureAction::Apply { confirmation, .. } => {
            converge(
                &Convergence {
                    mode: PlanMode::Apply,
                    confirmation: *confirmation,
                    context,
                },
                inputs,
            )
            .await
        }
        InfrastructureAction::Destroy { confirmation } => {
            converge(
                &Convergence {
                    mode: PlanMode::Destroy,
                    confirmation: *confirmation,
                    context,
                },
                inputs,
            )
            .await
        }
    }
}

/// The provider releases the `cuenv.lock` of the CUE module holding
/// `project_directory` pins; none outside a CUE module or without a lockfile.
pub(super) fn locked_providers(
    project_directory: &std::path::Path,
) -> Result<BTreeMap<String, LockedInfrastructureProvider>, CliError> {
    match types::Module::containing(project_directory) {
        Ok(module) => Ok(module.lockfile()?.infrastructure_providers),
        Err(_) => Ok(BTreeMap::new()),
    }
}

pub(super) fn engine_setup(context: &CommandContext<'_>, inputs: EngineInputs) -> EngineSetup {
    let withheld_environment_variables = inputs.withheld_environment_variables;
    EngineSetup {
        tenant: context.tenant.clone(),
        store: Arc::clone(context.store),
        infrastructure: inputs.infrastructure,
        options: EngineOptions {
            project_directory: inputs.project_directory,
            plugin_cache_directory: None,
            withheld_environment_variables,
            provider_environment_variables: inputs.provider_environment_variables,
            unrecorded_directory: inputs.unrecorded_directory,
            // Every provider the engine launches is registered with the
            // command's interrupt handling.
            cancellation: context.interrupts.cancellation().clone(),
            locked_providers: inputs.locked_providers,
        },
    }
}

pub(super) fn unrecorded_store(
    inputs: &EngineInputs,
    context: &CommandContext<'_>,
) -> Result<UnrecordedStore, CliError> {
    let unrecorded = inputs
        .unrecorded_directory
        .as_ref()
        .map_or_else(UnrecordedStore::default_location, |directory| {
            Ok(UnrecordedStore::at(directory))
        })
        .map_err(|error| failure(&error, context.invocation))?;
    match context.store.recovery_identity() {
        Some(identity) => unrecorded
            .with_backend_identity(&identity)
            .map_err(|error| failure(&error, context.invocation)),
        None => Ok(unrecorded),
    }
}

/// Connect to the state store. Nothing is sent yet: reads never create
/// tables, and [`under_lock`] creates or upgrades them before any write.
pub(super) fn connect(
    infrastructure: &Infrastructure,
    credential: StateCredential,
) -> cuenv_infrastructure::Result<Arc<dyn StateStore>> {
    let StateCredential { token, absence } = credential;
    match &infrastructure.state {
        InfrastructureState::Turso(turso) => {
            if token.is_none() {
                emit_stderr!(format!(
                    "warning: {absence}; connecting to Turso without an authentication token"
                ));
            }
            Ok(Arc::new(TursoStateStore::try_new(TursoConfiguration {
                url: turso.url.clone(),
                authentication_token: token,
            })?))
        }
        // The Cloudflare API refuses every request without a token.
        InfrastructureState::D1(d1) => {
            let api_token = token.ok_or_else(|| {
                InfrastructureError::configuration(format!(
                    "{absence}; D1 state needs a Cloudflare API token with D1 Edit access"
                ))
            })?;
            Ok(Arc::new(D1StateStore::try_new(D1Configuration {
                account_id: d1.account_id.clone(),
                database_id: d1.database_id.clone(),
                api_token,
            })?))
        }
    }
}

/// Say where the project's state was last written from, when that was
/// another directory: the project moved, or another checkout with the same
/// module path wrote it. Reads only; nothing is refused, since project names
/// are unique and the state belongs to the project, not to a directory.
pub(super) async fn note_last_writer(context: &CommandContext<'_>) -> Result<(), CliError> {
    let writer = context
        .interrupts
        .until_interrupted(context.store.last_writer(context.tenant))
        .await?
        .map_err(|error| failure(&error, context.invocation))?;
    if let Some(writer) = writer
        && writer.instance != *context.instance
    {
        emit_stderr!(format!(
            "note: the state of {} was last written from {} ({}); this run is {}",
            tenant_label(context.tenant),
            strip_control_characters(writer.instance.as_str()),
            strip_control_characters(&writer.recorded_at),
            strip_control_characters(context.instance.as_str())
        ));
    }
    Ok(())
}
