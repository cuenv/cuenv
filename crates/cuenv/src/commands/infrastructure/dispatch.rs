//! The context a command runs in, and dispatching an action to its handler.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cuenv_events::emit_stderr;
use cuenv_infrastructure::{
    AddressingChoice, Credentials, D1Configuration, D1StateStore, EngineOptions, EngineSetup,
    InfrastructureError, ObjectLocation, ObjectStateStore, ObjectStoreConfiguration, PlanMode,
    ProjectInstance, StateStore, TenantKey, TursoConfiguration, TursoStateStore, UnrecordedStore,
    strip_control_characters,
};
use cuenv_manifest::lockfile::LockedInfrastructureProvider;
use cuenv_manifest::manifest::{Infrastructure, InfrastructureState, S3Addressing, S3State};

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

/// What a connection is for: the CUE instance whose package an object store
/// keys state by, and the tenant whose prefix its checks use.
pub(super) struct StoreScope<'scope> {
    pub(super) instance: &'scope ProjectInstance,
    pub(super) tenant: &'scope TenantKey,
}

/// Connect to the state store. Nothing is sent yet: reads never create
/// tables, and [`under_lock`] creates or upgrades them (or, for an object
/// store, checks the bucket's conditional requests) before any write.
pub(super) fn connect(
    state: &InfrastructureState,
    credential: StateCredential,
    scope: &StoreScope<'_>,
) -> cuenv_infrastructure::Result<Arc<dyn StateStore>> {
    match state {
        InfrastructureState::Turso(turso) => {
            let (token, absence) = single_token(credential);
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
            let (token, absence) = single_token(credential);
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
        InfrastructureState::S3(s3) => {
            let required = |field: &str| -> cuenv_infrastructure::Result<String> {
                let resolved = credential.get(field);
                resolved
                    .and_then(|resolved| resolved.value.clone())
                    .ok_or_else(|| {
                        let absence = resolved.map_or_else(
                            || format!("`{field}` is not configured"),
                            |resolved| resolved.absence.clone(),
                        );
                        InfrastructureError::configuration(format!(
                            "{absence}; S3 state needs an access key ID and a secret access key"
                        ))
                    })
            };
            let credentials = Credentials {
                access_key_id: required(S3State::ACCESS_KEY_ID_FIELD)?,
                secret_access_key: required(S3State::SECRET_ACCESS_KEY_FIELD)?,
                session_token: credential
                    .get(S3State::SESSION_TOKEN_FIELD)
                    .and_then(|resolved| resolved.value.clone()),
            };
            Ok(Arc::new(ObjectStateStore::try_new(
                ObjectStoreConfiguration {
                    endpoint: s3.endpoint.clone(),
                    bucket: s3.bucket.clone(),
                    region: s3.region.clone(),
                    root: s3.root.clone(),
                    addressing: object_location(s3).addressing,
                    credentials,
                    package: scope.instance.package().to_string(),
                    history: s3.history,
                    minimum_write_interval: s3
                        .min_write_interval
                        .as_deref()
                        .map(parse_write_interval)
                        .transpose()?,
                    probe_tenant: Some(scope.tenant.clone()),
                },
            )?))
        }
    }
}

/// Where an `s3` state block keeps state.
pub(super) fn object_location(s3: &S3State) -> ObjectLocation<'_> {
    ObjectLocation {
        endpoint: s3.endpoint.as_deref(),
        bucket: &s3.bucket,
        region: &s3.region,
        root: s3.root.as_deref(),
        addressing: match s3.addressing {
            S3Addressing::Auto => AddressingChoice::Auto,
            S3Addressing::VirtualHosted => AddressingChoice::VirtualHosted,
            S3Addressing::Path => AddressingChoice::Path,
        },
    }
}

/// The token of a database backend, and why it is missing.
fn single_token(credential: StateCredential) -> (Option<String>, String) {
    credential.values.into_iter().next().map_or_else(
        || (None, "no state token is configured".to_string()),
        |resolved| (resolved.value, resolved.absence),
    )
}

/// Parse `minWriteInterval`: a whole number of milliseconds (`250ms`) or
/// seconds (`1s`).
pub(super) fn parse_write_interval(text: &str) -> cuenv_infrastructure::Result<Duration> {
    let invalid = || {
        InfrastructureError::configuration(format!(
            "`infrastructure.state.s3.minWriteInterval` must be a whole number of `ms` or `s`, \
             such as \"250ms\" or \"1s\", not \"{}\"",
            strip_control_characters(text)
        ))
    };
    let (digits, unit) = text
        .strip_suffix("ms")
        .map(|digits| (digits, Duration::from_millis as fn(u64) -> Duration))
        .or_else(|| {
            text.strip_suffix('s')
                .map(|digits| (digits, Duration::from_secs as fn(u64) -> Duration))
        })
        .ok_or_else(invalid)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    digits.parse::<u64>().map(unit).map_err(|_| invalid())
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
