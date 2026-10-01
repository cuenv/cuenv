//! Implementation of `cuenv infrastructure` (short form `cuenv i`).
//!
//! Evaluates the project's `infrastructure` block and hands it to
//! `cuenv-infrastructure`, which drives Terraform provider plugins over gRPC
//! and stores each managed resource in the configured Turso database. State
//! is keyed by the CUE module path and the project name, and owned by one
//! CUE instance (directory and package).
//!
//! `apply` and `destroy` follow Terraform: take the lock, plan, show the
//! plan and (unless `--yes`) ask for confirmation while holding the lock,
//! then apply exactly that plan. Every write happens under [`under_lock`],
//! which also creates or upgrades the state tables first, so commands that
//! only read never do. The command owns interrupt handling for its whole
//! run (see [`interrupts`]).

mod evaluation;
mod holder;
mod interrupts;
mod invocation;
mod output;
mod provider_environment;

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use cuenv_events::emit_stderr;
use cuenv_infrastructure::{
    ApplyContext, ApplyEvent, EngineOptions, EngineSetup, InfrastructureEngine,
    InfrastructureError, LockRequest, OwnerClaim, OwnerClaimMode, Plan, PlanMode, ProjectInstance,
    RecoverOptions, RecoverOverwrite, StateLock, StateStore, TenantKey, TursoConfiguration,
    TursoStateStore, UnrecordedStore, strip_control_characters,
    strip_control_characters_except_newlines, validate_configuration,
};
use cuenv_manifest::environment::EnvValue;
use cuenv_manifest::manifest::Infrastructure;

use self::evaluation::{NameCheck, Needs, Target, TargetRequest};
use self::interrupts::{HeldLock, Interrupts};
use self::invocation::Invocation;
use self::output::{Adoption, Converged, Finish, LockOutcome, LockReport, Output};
use self::provider_environment::{ProviderEnvironment, ProviderEnvironmentInputs};
use crate::cli::{CliError, InfrastructureFailureKind, LockStatus, OutputFormat};

/// Whether an operator must confirm before changes are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationPolicy {
    /// Show the plan and ask on the terminal.
    Prompt,
    /// Apply without asking (`--yes`).
    AssumeYes,
}

/// What `cuenv infrastructure state` should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateAction {
    /// List managed resources recorded in state.
    List,
    /// Forget one managed resource without touching the real object.
    Remove {
        /// Address of the resource (`type.name`).
        address: String,
    },
    /// Record changes an earlier run could not record and saved locally.
    Recover {
        /// Whether a stored record that changed since the change was saved
        /// may be overwritten (`--force`).
        overwrite: RecoverOverwrite,
    },
    /// Make this project's CUE instance the owner of its state.
    Adopt,
}

/// What `cuenv infrastructure` should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InfrastructureAction {
    /// Show the changes an apply would make.
    Plan,
    /// Converge infrastructure on the configuration.
    Apply {
        /// Whether to ask before applying.
        confirmation: ConfirmationPolicy,
    },
    /// Delete every managed resource of the project.
    Destroy {
        /// Whether to ask before destroying.
        confirmation: ConfirmationPolicy,
    },
    /// Inspect or repair recorded state.
    State(StateAction),
    /// Show the project's state lock, or release it when its identifier is given.
    Unlock {
        /// Identifier of the lock to release.
        lock_identifier: Option<String>,
    },
}

impl InfrastructureAction {
    /// Stable policy name used to decide which project variables this action
    /// may resolve and pass to providers.
    const fn policy_name(&self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Apply { .. } => "apply",
            Self::Destroy { .. } => "destroy",
            Self::State(StateAction::List) => "state-list",
            Self::State(StateAction::Remove { .. }) => "state-remove",
            Self::State(StateAction::Recover { .. }) => "state-recover",
            Self::State(StateAction::Adopt) => "state-adopt",
            Self::Unlock { .. } => "unlock",
        }
    }

    /// Whether this action starts providers and needs the full project
    /// environment. State-only commands resolve just the backend token.
    const fn uses_provider_environment(&self) -> bool {
        match self {
            Self::Plan | Self::Apply { .. } | Self::Destroy { .. } => true,
            Self::State(_) | Self::Unlock { .. } => false,
        }
    }

    /// Plans, changes and ownership transfers check the project name
    /// across the whole module; inspecting and repairing state and unlocking
    /// must keep working when a sibling instance is broken.
    const fn name_check(&self) -> NameCheck {
        match self {
            Self::Plan
            | Self::Apply { .. }
            | Self::Destroy { .. }
            | Self::State(StateAction::Adopt) => NameCheck::WholeModule,
            Self::State(
                StateAction::List | StateAction::Remove { .. } | StateAction::Recover { .. },
            )
            | Self::Unlock { .. } => NameCheck::TargetOnly,
        }
    }

    /// How much of the infrastructure configuration the action needs:
    /// state-only commands need only the state backend, so state can always
    /// be listed, unlocked and removed.
    const fn needs(&self) -> Needs {
        if self.uses_provider_environment() {
            Needs::Configuration
        } else {
            Needs::StateOnly
        }
    }

    /// Whether the action asks for confirmation.
    const fn confirmation(&self) -> ConfirmationPolicy {
        match self {
            Self::Apply { confirmation } | Self::Destroy { confirmation } => *confirmation,
            Self::Plan | Self::State(_) | Self::Unlock { .. } => ConfirmationPolicy::AssumeYes,
        }
    }
}

/// Options for `cuenv infrastructure`.
#[derive(Debug, Clone)]
pub struct InfrastructureOptions {
    /// Path to the project directory.
    pub path: String,
    /// CUE package name to evaluate.
    pub package: String,
    /// Named infrastructure and project environment selected with `--env`.
    pub environment: Option<String>,
    /// Action to perform.
    pub action: InfrastructureAction,
    /// Text or JSON output.
    pub output: OutputFormat,
}

const IMPORT_HELP: &str = "cuenv cannot import resources yet: find the resource with the \
                           provider's own tools and delete it, then plan again (the next apply \
                           creates it anew).";

/// Text from a provider or the store, for display: redacted from the raw
/// text first and then stripped of control characters except newlines
/// (stripping first would change a secret that contains one).
fn printable_text(text: &str) -> String {
    strip_control_characters_except_newlines(&cuenv_events::redact(text))
}

/// Some engine and store messages name a repair command themselves (as
/// "`cuenv infrastructure state recover`"), without knowing which
/// environment or project the run selected. Name it with the run's flags.
fn with_selected_commands(message: String, invocation: &Invocation) -> String {
    ["state recover", "state adopt"]
        .into_iter()
        .fold(message, |message, subcommand| {
            message.replace(
                &format!("`cuenv infrastructure {subcommand}`"),
                &format!("`{}`", invocation.command(subcommand)),
            )
        })
}

/// Map an engine or store error to the command's error, with the help an
/// operator needs. Every command the help names carries the run's `--env`,
/// `-p` and `--package`.
fn failure(error: &InfrastructureError, invocation: &Invocation) -> CliError {
    let message = with_selected_commands(printable_text(&error.to_string()), invocation);
    let (kind, help) = match error {
        InfrastructureError::Configuration(_) => return CliError::config(message),
        InfrastructureError::Locked {
            lock_identifier, ..
        } => {
            return CliError::infrastructure(
                message,
                Some("Wait for the other run to finish, then run the command again.".to_string()),
                InfrastructureFailureKind::Locked,
            )
            .with_lock(LockStatus {
                identifier: strip_control_characters(lock_identifier),
                released: false,
            });
        }
        InfrastructureError::LockLost { .. } => (
            InfrastructureFailureKind::Failed,
            Some(format!(
                "Another run released or took this project's lock. Review `{}` before applying \
                 again.",
                invocation.command("plan")
            )),
        ),
        InfrastructureError::UnrecordedChangeLost { .. } => (
            InfrastructureFailureKind::Failed,
            Some(IMPORT_HELP.to_string()),
        ),
        InfrastructureError::Interrupted { .. }
        | InfrastructureError::InterruptedWhilePlanning
        | InfrastructureError::InterruptedUnknownOutcome { .. } => {
            (InfrastructureFailureKind::Interrupted, None)
        }
        InfrastructureError::State(_) => (
            InfrastructureFailureKind::Failed,
            Some("Check the Turso URL, the authentication token and network access.".to_string()),
        ),
        // A local file problem, never a state store one.
        InfrastructureError::UnrecordedFile { path, .. } => (
            InfrastructureFailureKind::Failed,
            Some(format!(
                "{} is in cuenv's unrecorded change directory but cannot be used. Check it, then \
                 move it out of that directory and run the command again; moving it aside \
                 discards the change it records from cuenv's view (the resource itself is not \
                 touched).",
                strip_control_characters(path)
            )),
        ),
        InfrastructureError::StateChanged { address, .. } => (
            InfrastructureFailureKind::Failed,
            Some(format!(
                "The state store's record of {} changed after the unrecorded change was saved, \
                 so recording it would overwrite the newer record. Review the resource, then \
                 either run `{}` to write the saved record anyway, or move its file out of the \
                 unrecorded change directory to keep the stored record.",
                strip_control_characters(address),
                invocation.command("state recover --force")
            )),
        ),
        InfrastructureError::PlanEnvironmentChanged => (
            InfrastructureFailureKind::Failed,
            Some("Run the command again; it plans again under the lock.".to_string()),
        ),
        InfrastructureError::PlanOutdated { .. } => (
            InfrastructureFailureKind::Failed,
            Some(
                "The stored state changed after the plan was made. Run the command again; it \
                 plans again under the lock."
                    .to_string(),
            ),
        ),
        InfrastructureError::OwnedByAnotherInstance { .. } => (
            InfrastructureFailureKind::Failed,
            Some(format!(
                "Each project's state belongs to one CUE instance (directory and package). If \
                 this instance should manage it now (for example, the project moved), run \
                 `{}` here; otherwise give this project a different `name`.",
                invocation.command("state adopt")
            )),
        ),
        InfrastructureError::StateFromNewerProvider { .. } => (
            InfrastructureFailureKind::Failed,
            Some(
                "Upgrade the provider in `infrastructure.providers` to the version that wrote \
                 this record, or a newer one; cuenv never downgrades stored state."
                    .to_string(),
            ),
        ),
        InfrastructureError::UnrecordedChangesPending { .. } => (
            InfrastructureFailureKind::Failed,
            Some(format!(
                "Run `{}`, then run this command again.",
                invocation.command("state recover")
            )),
        ),
        // The messages of these say what to do next, or carry the
        // provider's own diagnostics.
        InfrastructureError::UnrecordedChange { .. }
        | InfrastructureError::Codec(_)
        | InfrastructureError::Plugin(_)
        | InfrastructureError::RemoteProcedure { .. }
        | InfrastructureError::Diagnostics { .. }
        | InfrastructureError::ApplyIncomplete(_)
        | InfrastructureError::Install(_)
        | InfrastructureError::InputOutput { .. } => (InfrastructureFailureKind::Failed, None),
    };
    CliError::infrastructure(message, help, kind)
}

/// Execute `cuenv infrastructure`.
///
/// The command claims interrupt signals before doing anything else and
/// keeps them until it returns. In JSON mode its result envelope is printed
/// last, after the lock is released.
///
/// # Errors
///
/// Returns an error if evaluation fails, the project has no `infrastructure`
/// block, its name is not unique in the module, another instance owns its
/// state, the state store is unreachable or locked, the plan is not
/// confirmed, a provider fails, or the run is interrupted.
#[tracing::instrument(skip_all, fields(path = %options.path, action = ?options.action))]
pub async fn execute_infrastructure(options: &InfrastructureOptions) -> Result<(), CliError> {
    // Text read from providers (their log lines and error messages) is
    // redacted as it is read, before control characters are stripped.
    cuenv_infrastructure::plugin::install_log_redactor(cuenv_events::redact);
    let output = Output::new(options.output);
    let interrupts = Interrupts::claim(&output, Invocation::of(options))?;
    let result = run(options, &output, &interrupts).await;
    match output.finish() {
        // A forced exit is writing the only report and ending the process;
        // returning would let `main` print a second one.
        Finish::Exiting => return std::future::pending().await,
        Finish::Report(Some(payload)) if result.is_ok() => output::print_envelope(&payload),
        Finish::Report(_) => {}
    }
    // Dropping the claim aborts the signal watcher.
    drop(interrupts);
    result
}

async fn run(
    options: &InfrastructureOptions,
    output: &Output,
    interrupts: &Interrupts,
) -> Result<(), CliError> {
    let answers = TerminalAnswers;
    let invocation = Invocation::of(options);
    refuse_unconfirmable(&options.action, output, &answers, &invocation)?;
    let target = evaluate_target(options, interrupts).await?;
    preflight(&options.action, &target, &invocation)?;
    let resolved = resolve(&options.action, &target).await?;
    let Target {
        tenant,
        unselected_tenant,
        instance,
        infrastructure,
        environment,
        declared_environments,
        project_directory,
        ..
    } = target;
    let siblings = Siblings {
        unselected_tenant,
        declared_environments,
        selected: environment,
    };
    let context = CommandContext {
        store: &resolved.store,
        tenant: &tenant,
        instance: &instance,
        siblings: &siblings,
        invocation: &invocation,
        output,
        interrupts,
        answers: &answers,
    };
    if options.action.uses_provider_environment() {
        refuse_unmoved_state(&context).await?;
    }
    dispatch(
        &options.action,
        &context,
        EngineInputs {
            infrastructure,
            project_directory,
            provider_environment_variables: resolved.provider_environment_variables,
            withheld_environment_variables: resolved.withheld_environment_variables,
            // The user state directory (see `UnrecordedStore`).
            unrecorded_directory: None,
        },
    )
    .await
}

/// The checks that need neither a secret nor the state store, run before
/// either is touched: the operator's warnings, the state URL, the choice of
/// identity, and (for commands that start providers) the providers and
/// resources. The engine repeats the last one before planning as defense in
/// depth.
fn preflight(
    action: &InfrastructureAction,
    target: &Target,
    invocation: &Invocation,
) -> Result<(), CliError> {
    for warning in &target.warnings {
        emit_stderr!(format!("warning: {warning}"));
    }
    validate_state_url(&target.infrastructure)?;
    if action.uses_provider_environment() {
        refuse_unselected_environment(target, invocation)?;
        validate_configuration(&target.infrastructure, target.environment.as_deref())
            .map_err(|error| failure(&error, invocation))?;
    }
    Ok(())
}

/// Parse and validate the Turso URL before any secret is resolved: nothing
/// is sent, so a mistake in the URL never costs a call to a secret provider.
/// (A configured URL is validated again when the store is created.)
fn validate_state_url(infrastructure: &Infrastructure) -> Result<(), CliError> {
    TursoStateStore::new(TursoConfiguration {
        url: infrastructure.state.turso.url.clone(),
        authentication_token: None,
    })
    .map(drop)
    .map_err(|error| failure(&error, &Invocation::default()))
}

/// Refuse a run without `--env` when the project declares environments but
/// no top-level resources. State is recorded separately with and without
/// `--env`, so such a run would act on the no-flag identity: it would plan
/// to delete whatever is recorded there, or report "no changes" and leave
/// the operator thinking an environment was converged.
fn refuse_unselected_environment(
    target: &Target,
    invocation: &Invocation,
) -> Result<(), CliError> {
    if target.environment.is_some()
        || target.declared_environments.is_empty()
        || !target.infrastructure.resources.is_empty()
    {
        return Ok(());
    }
    let declared = evaluation::declared_environments_phrase(&target.declared_environments);
    let example = invocation
        .with_environment(target.declared_environments.first().map(String::as_str))
        .command("plan");
    Err(CliError::config_with_help(
        format!(
            "this project declares infrastructure environments but no top-level `resources`, \
             so a run without --env has nothing to manage ({declared})"
        ),
        format!(
            "Select an environment with --env, for example `{example}`. Without --env the run \
             acts on the state recorded without an environment, and would plan to delete \
             everything recorded there; declare top-level `resources` if that configuration \
             should still be managed. `state list`, `state remove` and `unlock` work without \
             --env."
        ),
    ))
}

/// What running the command needs from the environment and the state store.
struct Resolved {
    store: Arc<dyn StateStore>,
    /// Resolved project variables overlaid on the provider's host environment.
    provider_environment_variables: BTreeMap<String, String>,
    /// Names removed from the provider's environment after the overlay.
    withheld_environment_variables: Vec<String>,
}

/// Resolve the project's environment variables the action may use, connect
/// to the state store, and decide what providers inherit.
async fn resolve(action: &InfrastructureAction, target: &Target) -> Result<Resolved, CliError> {
    let project_environment_variables =
        target
            .project_environment
            .as_ref()
            .map_or_else(HashMap::new, |project_environment| {
                target.environment.as_deref().map_or_else(
                    || project_environment.base.clone(),
                    |name| project_environment.for_environment(name),
                )
            });
    let policy_name = action.policy_name();
    let policy_withheld: Vec<String> = if action.uses_provider_environment() {
        project_environment_variables
            .iter()
            .filter(|(_, value)| !value.is_accessible_by_infrastructure(policy_name))
            .map(|(name, _)| name.clone())
            .collect()
    } else {
        Vec::new()
    };
    let token_variable = target
        .infrastructure
        .state
        .turso
        .authentication_token_environment_variable
        .clone();
    if project_environment_variables
        .get(&token_variable)
        .is_some_and(|value| !value.is_accessible_by_infrastructure(policy_name))
    {
        return Err(CliError::config(format!(
            "environment variable {token_variable} is restricted from infrastructure action {policy_name}"
        )));
    }
    let environment_to_resolve = environment_variables_for_action(
        action,
        &project_environment_variables,
        &token_variable,
    );
    let (resolved_variables, secret_values) =
        resolve_environment_variables(policy_name, &environment_to_resolve).await?;
    // Register before connecting to state or launching providers so their
    // diagnostics and output redact every resolved secret part.
    cuenv_events::register_secrets(secret_values);
    let store = connect(
        &target.infrastructure,
        &project_environment_variables,
        &resolved_variables,
    )
    .map_err(|error| failure(&error, &Invocation::default()))?;
    let provider_environment_variables: BTreeMap<String, String> = resolved_variables
        .into_iter()
        .filter(|(name, _)| name != &token_variable)
        .collect();
    let provided: Vec<String> = provider_environment_variables.keys().cloned().collect();
    let withheld_environment_variables = if action.uses_provider_environment() {
        provider_environment::withheld_environment_variables(&ProviderEnvironmentInputs {
            // TODO(m5-integration): read the selected configuration's
            // `provider_environment` here once the manifest has the field.
            mode: ProviderEnvironment::default(),
            ambient: std::env::vars_os().map(|(name, _)| name).collect(),
            provided: &provided,
            policy_withheld: &policy_withheld,
            token_variable: &token_variable,
        })
    } else {
        Vec::new()
    };
    Ok(Resolved {
        store,
        provider_environment_variables,
        withheld_environment_variables,
    })
}

/// Resolve the variables the policy of `action` allows, one resolution per
/// variable and all at once, so a failure names the variable it concerns.
/// Returns the values and every secret part, for redaction.
async fn resolve_environment_variables(
    action: &str,
    variables: &HashMap<String, EnvValue>,
) -> Result<(HashMap<String, String>, Vec<String>), CliError> {
    let mut resolutions = tokio::task::JoinSet::new();
    for (name, value) in variables
        .iter()
        .filter(|(_, value)| value.is_accessible_by_infrastructure(action))
    {
        let (name, value, action) = (name.clone(), value.clone(), action.to_string());
        resolutions.spawn(async move {
            let single = HashMap::from([(name.clone(), value)]);
            let resolved =
                cuenv_core::environment::Environment::resolve_for_infrastructure_with_secrets(
                    &action, &single,
                )
                .await;
            (name, resolved)
        });
    }
    let mut resolved_variables = HashMap::new();
    let mut secret_values = Vec::new();
    let mut failures: BTreeMap<String, CliError> = BTreeMap::new();
    while let Some(joined) = resolutions.join_next().await {
        let (name, resolved) = joined.map_err(|error| {
            CliError::eval(format!("a secret resolution stopped unexpectedly: {error}"))
        })?;
        match resolved {
            Ok((values, secrets)) => {
                resolved_variables.extend(values);
                secret_values.extend(secrets);
            }
            Err(error) => {
                failures.insert(name.clone(), secret_failure(&name, error));
            }
        }
    }
    // Several can fail at once; report the first by name, deterministically.
    match failures.into_values().next() {
        Some(failure) => Err(failure),
        None => Ok((resolved_variables, secret_values)),
    }
}

/// The error for an environment variable whose secret could not be resolved:
/// it names the variable, and it is a failure of the secret provider (exit
/// code 3, like every other secret resolution error), not of the command line.
fn secret_failure(variable: &str, error: cuenv_core::Error) -> CliError {
    let message = CliError::from(error).message().to_string();
    let reason = message
        .strip_prefix("Failed to resolve secret 'secret': ")
        .unwrap_or(&message);
    CliError::eval_with_help(
        format!("cannot resolve the secret for environment variable {variable}: {reason}"),
        format!(
            "Check the configuration and credentials of the secret provider behind {variable} \
             (1Password, AWS, Vault, a command...), and that the action's policy \
             (`allowInfrastructure`) lets it be used."
        ),
    )
}

/// The other state identities of the project, besides the one this run acts
/// on: what hints and refusals look at when state exists under a different
/// `--env` than the one given (or the one forgotten).
#[derive(Debug)]
struct Siblings {
    /// The project's identity without `--env`.
    unselected_tenant: TenantKey,
    /// Every environment the project declares.
    declared_environments: Vec<String>,
    /// The environment this run selected.
    selected: Option<String>,
}

impl Siblings {
    /// The identities a run without `--env` should mention besides its own:
    /// one per declared environment. A run with `--env` mentions none.
    fn declared(&self) -> Vec<(String, TenantKey)> {
        if self.selected.is_some() {
            return Vec::new();
        }
        self.declared_environments
            .iter()
            .filter_map(|name| {
                TenantKey::with_environment(
                    self.unselected_tenant.module_path(),
                    self.unselected_tenant.project(),
                    name,
                )
                .ok()
                .map(|tenant| (name.clone(), tenant))
            })
            .collect()
    }
}

/// Refuse to plan, apply or destroy a named environment that has no state
/// while the same project has state recorded without `--env`.
///
/// The two identities are separate: a run with `--env` would plan to create
/// every resource again beside the objects the no-flag state still manages,
/// and both identities would claim the same real objects. Moving records
/// from one identity to the other (`state move`) is not available yet, so
/// the operator is told what to do instead.
async fn refuse_unmoved_state(context: &CommandContext<'_>) -> Result<(), CliError> {
    let Some(environment) = context.tenant.environment() else {
        return Ok(());
    };
    let environment_records = context
        .interrupts
        .until_interrupted(context.store.list(context.tenant))
        .await?
        .map_err(|error| failure(&error, context.invocation))?;
    if !environment_records.is_empty() {
        return Ok(());
    }
    let unselected_records = context
        .interrupts
        .until_interrupted(context.store.list(&context.siblings.unselected_tenant))
        .await?
        .map_err(|error| failure(&error, context.invocation))?;
    if unselected_records.is_empty() {
        return Ok(());
    }
    let unselected = context.invocation.with_environment(None);
    let environment = evaluation::escape_control_characters(environment);
    Err(CliError::config_with_help(
        format!(
            "environment '{environment}' of {} has no recorded state, but {} resource(s) are \
             recorded for the same project without --env",
            tenant_label(&context.siblings.unselected_tenant),
            unselected_records.len()
        ),
        format!(
            "State is recorded separately for runs with and without --env, so this run would \
             plan to create every resource again next to the objects the state without --env \
             still manages (both would claim the same real objects). Moving the records into \
             '{environment}' (`state move`) is not available yet. Keep running without --env, or \
             first remove the existing resources with `{}` (they are deleted), or forget them \
             without touching the real objects with `{}` for each address, and then use \
             --env {environment}.",
            unselected.command("destroy"),
            unselected.command("state remove <address>")
        ),
    ))
}

/// A tenant as text for messages: its module path, project and environment,
/// with control characters escaped.
fn tenant_label(tenant: &TenantKey) -> String {
    evaluation::escape_control_characters(&tenant.to_string())
}


/// Refuse a prompt that cannot be answered before evaluating anything.
fn refuse_unconfirmable(
    action: &InfrastructureAction,
    output: &Output,
    answers: &dyn Answers,
    invocation: &Invocation,
) -> Result<(), CliError> {
    if action.confirmation() == ConfirmationPolicy::AssumeYes {
        return Ok(());
    }
    if output.is_json() {
        return Err(CliError::config_with_help(
            "--json needs --yes for apply and destroy: the confirmation prompt cannot share \
             standard output with the JSON result",
            format!(
                "Review `{} --json` first, then run with --yes.",
                invocation.command("plan")
            ),
        ));
    }
    if !answers.is_interactive() {
        return Err(CliError::config_with_help(
            "refusing to change infrastructure without confirmation: standard input is not a \
             terminal",
            "Pass --yes to apply without the prompt.",
        ));
    }
    Ok(())
}

fn environment_variables_for_action(
    action: &InfrastructureAction,
    project_environment: &HashMap<String, EnvValue>,
    token_variable: &str,
) -> HashMap<String, EnvValue> {
    if action.uses_provider_environment() {
        return project_environment.clone();
    }
    project_environment
        .get(token_variable)
        .map(|value| HashMap::from([(token_variable.to_string(), value.clone())]))
        .unwrap_or_default()
}

/// Evaluate the target on a thread of its own. CUE evaluation cannot be
/// cancelled, and nothing is held yet, so an interrupt abandons it at once:
/// the thread is not waited for, and the process exits without it.
async fn evaluate_target(
    options: &InfrastructureOptions,
    interrupts: &Interrupts,
) -> Result<Target, CliError> {
    let (path, package, name_check, needs, environment) = (
        options.path.clone(),
        options.package.clone(),
        options.action.name_check(),
        options.action.needs(),
        options.environment.clone(),
    );
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("cuenv-infrastructure-evaluation".to_string())
        .spawn(move || {
            let target = evaluation::evaluate(TargetRequest {
                path: &path,
                package: &package,
                name_check,
                needs,
                environment: environment.as_deref(),
            });
            // Nobody waits any more after an interrupt; the result is
            // meant to be dropped then.
            let _ = sender.send(target);
        })
        .map_err(|error| CliError::eval(format!("cannot start CUE evaluation: {error}")))?;
    interrupts
        .until_interrupted(receiver)
        .await?
        .map_err(|_| CliError::eval("CUE evaluation stopped unexpectedly"))?
}

/// What every stateful subcommand works with.
struct CommandContext<'context> {
    store: &'context Arc<dyn StateStore>,
    tenant: &'context TenantKey,
    /// The CUE instance (directory and package) this run evaluated.
    instance: &'context ProjectInstance,
    /// The project's other state identities.
    siblings: &'context Siblings,
    /// How hints name this run's project and environment.
    invocation: &'context Invocation,
    output: &'context Output,
    interrupts: &'context Interrupts,
    answers: &'context dyn Answers,
}

/// What an engine is built from, besides the context.
struct EngineInputs {
    infrastructure: Infrastructure,
    project_directory: PathBuf,
    /// Resolved project variables overlaid on the provider's host environment.
    provider_environment_variables: BTreeMap<String, String>,
    /// Project variables rejected by policy must not leak through inherited
    /// host variables with the same name.
    withheld_environment_variables: Vec<String>,
    /// Where unrecorded changes are saved; the user state directory when
    /// `None`.
    unrecorded_directory: Option<PathBuf>,
}

async fn dispatch(
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
            Ok(())
        }
        InfrastructureAction::State(StateAction::Remove { address }) => {
            remove_resource(context, address).await
        }
        InfrastructureAction::State(StateAction::Recover { overwrite }) => {
            recover(context, &unrecorded_store(&inputs, context)?, *overwrite).await
        }
        InfrastructureAction::State(StateAction::Adopt) => adopt(context).await,
        InfrastructureAction::Unlock { lock_identifier } => {
            context
                .interrupts
                .until_interrupted(unlock(context, lock_identifier.as_deref()))
                .await?
        }
        InfrastructureAction::Plan => plan(context, inputs).await,
        InfrastructureAction::Apply { confirmation } => {
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

fn engine_setup(context: &CommandContext<'_>, inputs: EngineInputs) -> EngineSetup {
    let mut withheld_environment_variables = vec![
        inputs
            .infrastructure
            .state
            .turso
            .authentication_token_environment_variable
            .clone(),
    ];
    withheld_environment_variables.extend(inputs.withheld_environment_variables);
    withheld_environment_variables.sort();
    withheld_environment_variables.dedup();
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
        },
    }
}

fn unrecorded_store(
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
fn connect(
    infrastructure: &Infrastructure,
    project_environment: &HashMap<String, EnvValue>,
    resolved_environment: &HashMap<String, String>,
) -> cuenv_infrastructure::Result<Arc<dyn StateStore>> {
    let turso = &infrastructure.state.turso;
    let variable = &turso.authentication_token_environment_variable;
    let authentication_token = if project_environment.contains_key(variable) {
        resolved_environment.get(variable).cloned()
    } else {
        std::env::var(variable).ok()
    }
    .filter(|token| !token.is_empty());
    match &authentication_token {
        // Redact the token from every output from here on, including
        // provider and store errors that might echo it.
        Some(token) => cuenv_events::register_secret(token.clone()),
        None => emit_stderr!(format!(
            "warning: {variable} is not set; connecting to Turso without an authentication token"
        )),
    }
    Ok(Arc::new(TursoStateStore::new(TursoConfiguration {
        url: turso.url.clone(),
        authentication_token,
    })?))
}

/// Refuse to act on state another CUE instance owns. Reads only.
async fn require_owner(context: &CommandContext<'_>) -> Result<(), CliError> {
    let owner = context
        .interrupts
        .until_interrupted(context.store.owner(context.tenant))
        .await?
        .map_err(|error| failure(&error, context.invocation))?;
    owner.map_or(Ok(()), |owner| {
        owner
            .require(context.tenant, context.instance)
            .map_err(|error| failure(&error, context.invocation))
    })
}

/// Under the lock: record this instance as the owner when the state has
/// none, and refuse when another instance owns it.
async fn claim_ownership(context: &CommandContext<'_>, lock: &StateLock) -> Result<(), CliError> {
    let owner = context
        .store
        .claim_owner(
            context.tenant,
            lock,
            &OwnerClaim {
                instance: context.instance,
                mode: OwnerClaimMode::IfUnowned,
            },
        )
        .await
        .map_err(|error| failure(&error, context.invocation))?;
    owner
        .require(context.tenant, context.instance)
        .map_err(|error| failure(&error, context.invocation))
}

/// Take the project's lock, run `work` with it, and release it.
///
/// Creates or upgrades the state tables first: every write happens here,
/// and only here. The lock identifier is chosen and registered with the
/// interrupt handling before it is requested, so a forced exit during
/// acquisition can name it. Acquisition is never abandoned half way: an
/// interrupt while it runs is honoured once it returns, by releasing the
/// lock again. The outcome says whether the lock was released.
async fn under_lock<Outcome, Work>(
    context: &CommandContext<'_>,
    operation: &str,
    work: impl FnOnce(StateLock) -> Work,
) -> Result<Outcome, CliError>
where
    Work: Future<Output = Result<Outcome, CliError>>,
{
    context
        .store
        .migrate()
        .await
        .map_err(|error| failure(&error, context.invocation))?;
    context.interrupts.check()?;
    let lock = StateLock::generate();
    let held = HeldLock {
        store: Arc::clone(context.store),
        tenant: context.tenant.clone(),
        lock: lock.clone(),
    };
    context.interrupts.acquiring(held.clone());
    let holder = holder::describe(operation);
    let acquired = context
        .store
        .acquire_lock(
            context.tenant,
            &LockRequest {
                lock: &lock,
                holder: &holder,
            },
        )
        .await;
    if let Err(error) = acquired {
        context.interrupts.released();
        return Err(acquisition_failure(&error, &lock, context.invocation));
    }
    context.interrupts.hold(held);
    emit_stderr!(format!(
        "Acquired lock {} for {}",
        lock.lock_identifier,
        tenant_label(context.tenant)
    ));

    let result = match context.interrupts.check() {
        Ok(()) => work(lock.clone()).await,
        Err(interrupted) => Err(interrupted),
    };
    let released = release(context.store.as_ref(), context.tenant, &lock).await;
    context.interrupts.released();
    if released.is_ok() {
        emit_stderr!(format!(
            "Released lock {} for {}",
            lock.lock_identifier,
            tenant_label(context.tenant)
        ));
    }
    settle(Settlement {
        operation,
        lock: &lock,
        invocation: context.invocation,
        result,
        released,
    })
}

/// A failed acquisition. When its outcome is uncertain (the response was
/// lost), the error names this run's lock so it can be released.
fn acquisition_failure(
    error: &InfrastructureError,
    lock: &StateLock,
    invocation: &Invocation,
) -> CliError {
    let failure = failure(error, invocation);
    match error {
        InfrastructureError::Locked { .. } => failure,
        _ if error.to_string().contains(&lock.lock_identifier) => failure.with_lock(LockStatus {
            identifier: lock.lock_identifier.clone(),
            released: false,
        }),
        _ => failure,
    }
}

/// The work's outcome and the release's, for [`settle`].
struct Settlement<'settlement, Outcome> {
    operation: &'settlement str,
    lock: &'settlement StateLock,
    invocation: &'settlement Invocation,
    result: Result<Outcome, CliError>,
    released: cuenv_infrastructure::Result<()>,
}

/// Combine the work's outcome with the release's. Infrastructure errors
/// carry the lock and whether it was released; a lock that was not
/// released is never reported as released.
fn settle<Outcome>(settlement: Settlement<'_, Outcome>) -> Result<Outcome, CliError> {
    let Settlement {
        operation,
        lock,
        invocation,
        result,
        released,
    } = settlement;
    let identifier = &lock.lock_identifier;
    let status = |released| LockStatus {
        identifier: identifier.clone(),
        released,
    };
    let unlock_help = format!(
        "After checking no run is active, release it with `{}`.",
        invocation.command(&format!("unlock {identifier}"))
    );
    match (result, released) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Err(error), Ok(())) => Err(error.with_lock(status(true))),
        (Ok(_), Err(release_error)) => Err(CliError::infrastructure(
            format!(
                "{operation} succeeded but lock {identifier} was NOT released: {}",
                printable_text(&release_error.to_string())
            ),
            Some(unlock_help),
            InfrastructureFailureKind::Failed,
        )
        .with_lock(status(false))),
        (Err(error), Err(release_error)) => {
            let not_released = format!(
                "Lock {identifier} was NOT released ({}). {unlock_help}",
                printable_text(&release_error.to_string())
            );
            let help = error.help().map_or_else(
                || not_released.clone(),
                |help| format!("{help} {not_released}"),
            );
            Err(match error {
                CliError::Infrastructure { .. } => error.with_help(help).with_lock(status(false)),
                // Keep the lock in the report: an unreleased lock is an
                // infrastructure failure whatever stopped the work.
                other => CliError::infrastructure(
                    format!(
                        "{operation} failed and its lock was NOT released: {}",
                        other.message()
                    ),
                    Some(help),
                    InfrastructureFailureKind::Failed,
                )
                .with_lock(status(false)),
            })
        }
    }
}

/// `state remove`: forget one resource under the lock (a fenced delete).
async fn remove_resource(context: &CommandContext<'_>, address: &str) -> Result<(), CliError> {
    under_lock(context, "state remove", |lock| async move {
        require_owner(context).await?;
        let resources = context
            .store
            .list(context.tenant)
            .await
            .map_err(|error| failure(&error, context.invocation))?;
        let Some(resource) = resources
            .iter()
            .find(|resource| resource.address.to_string() == address)
        else {
            let known: Vec<String> = resources
                .iter()
                .map(|resource| strip_control_characters(&resource.address.to_string()))
                .collect();
            return Err(CliError::config_with_help(
                format!(
                    "{} has no managed resource {} in state",
                    tenant_label(context.tenant),
                    strip_control_characters(address)
                ),
                if known.is_empty() {
                    "No resources are recorded for this project.".to_string()
                } else {
                    format!("Recorded resources: {}.", known.join(", "))
                },
            ));
        };
        context
            .store
            .delete(context.tenant, &lock, &resource.address)
            .await
            .map_err(|error| failure(&error, context.invocation))?;
        context.output.removed(context.tenant, &resource.address);
        Ok(())
    })
    .await
}

/// `state recover`: record every unrecorded change of the tenant under the
/// lock, deleting each saved file once it is recorded. With nothing to
/// recover, the lock is not taken.
async fn recover(
    context: &CommandContext<'_>,
    unrecorded: &UnrecordedStore,
    overwrite: RecoverOverwrite,
) -> Result<(), CliError> {
    if !unrecorded
        .has_pending(context.tenant)
        .map_err(|error| failure(&error, context.invocation))?
    {
        context.output.recovered(context.tenant, &[]);
        emit_notes(&pending_in_other_environments(context, unrecorded));
        return Ok(());
    }
    let result = under_lock(context, "state recover", |lock| async move {
        require_owner(context).await?;
        if overwrite == RecoverOverwrite::Always
            && unrecorded.list(context.tenant).map_err(|error| failure(&error, context.invocation))?
                .iter().any(|record| record.requires_force() || unrecorded.requires_backend_force(record))
        {
            emit_stderr!("warning: forcing recovery of saved state whose generation or backend binding cannot be verified; inspect the saved object, stored object and configured backend before overwriting current state");
        }
        let recovered = unrecorded
            .recover(
                context.store.as_ref(),
                context.tenant,
                &RecoverOptions {
                    lock: &lock,
                    overwrite,
                },
            )
            .await
            .map_err(|error| failure(&error, context.invocation))?;
        context.output.recovered(context.tenant, &recovered);
        Ok(())
    })
    .await;
    emit_notes(&pending_in_other_environments(context, unrecorded));
    result
}

/// Print notes on standard error, where diagnostics go in both text and
/// JSON mode.
fn emit_notes(notes: &[String]) {
    for note in notes {
        emit_stderr!(format!("note: {note}"));
    }
}

/// What a run without `--env` says about other environments of the project
/// that have unrecorded changes saved: `state recover` only looks at the
/// identity it was given, and exiting quietly would leave them unnoticed.
fn pending_in_other_environments(
    context: &CommandContext<'_>,
    unrecorded: &UnrecordedStore,
) -> Vec<String> {
    context
        .siblings
        .declared()
        .into_iter()
        .filter(|(_, tenant)| unrecorded.has_pending(tenant).unwrap_or(false))
        .map(|(name, _)| {
            format!(
                "unrecorded changes are saved for environment '{}'; record them with `{}`",
                evaluation::escape_control_characters(&name),
                context
                    .invocation
                    .with_environment(Some(&name))
                    .command("state recover")
            )
        })
        .collect()
}

/// `state adopt`: make this instance the owner of the tenant's state.
async fn adopt(context: &CommandContext<'_>) -> Result<(), CliError> {
    under_lock(context, "state adopt", |lock| async move {
        let previous = context
            .store
            .owner(context.tenant)
            .await
            .map_err(|error| failure(&error, context.invocation))?;
        let owner = context
            .store
            .claim_owner(
                context.tenant,
                &lock,
                &OwnerClaim {
                    instance: context.instance,
                    mode: OwnerClaimMode::Transfer,
                },
            )
            .await
            .map_err(|error| failure(&error, context.invocation))?;
        context.output.adopted(&Adoption {
            tenant: context.tenant,
            previous: previous.as_ref(),
            owner: &owner,
        });
        Ok(())
    })
    .await
}

/// `unlock`: show the lock, or release the one named. Naming another lock
/// than the one held is refused, and so is naming a lock when none is held
/// (the identifier belongs to another environment, or to a lock that is
/// already gone): `unlock <id>` that matches no lock must not exit
/// successfully, or a script cannot tell it released nothing.
async fn unlock(
    context: &CommandContext<'_>,
    lock_identifier: Option<&str>,
) -> Result<(), CliError> {
    let tenant = context.tenant;
    let current = context
        .store
        .current_lock(tenant)
        .await
        .map_err(|error| failure(&error, context.invocation))?;
    let (Some(current), Some(lock_identifier)) = (current.as_ref(), lock_identifier) else {
        if let Some(lock_identifier) = lock_identifier {
            return Err(unmatched_lock(context, lock_identifier).await);
        }
        context.output.lock(&LockReport {
            tenant,
            lock: current.as_ref(),
            outcome: LockOutcome::Shown,
            unlock_command: current.as_ref().map(|lock| {
                context.invocation.command(&format!(
                    "unlock {}",
                    strip_control_characters(&lock.lock_identifier)
                ))
            }),
        });
        emit_notes(&locks_in_other_environments(context).await);
        return Ok(());
    };
    let released = context
        .store
        .force_unlock(tenant, lock_identifier)
        .await
        .map_err(|error| failure(&error, context.invocation))?;
    if !released {
        // The lock is held, just not by the run the operator named: that
        // is concurrent activity, not a configuration mistake.
        let holder = strip_control_characters(&current.lock_identifier);
        return Err(CliError::infrastructure(
            format!(
                "{} is locked by {holder}, not {}; nothing was released",
                tenant_label(tenant),
                strip_control_characters(lock_identifier)
            ),
            Some(format!(
                "Run `{}` without an identifier to see who holds the lock now.",
                context.invocation.command("unlock")
            )),
            InfrastructureFailureKind::Locked,
        )
        .with_lock(LockStatus {
            identifier: holder,
            released: false,
        }));
    }
    context.output.lock(&LockReport {
        tenant,
        lock: Some(current),
        outcome: LockOutcome::Released,
        unlock_command: None,
    });
    Ok(())
}

/// The error for `unlock <identifier>` when no lock is held: it says where
/// the identifier may belong instead.
async fn unmatched_lock(context: &CommandContext<'_>, lock_identifier: &str) -> CliError {
    let identifier = strip_control_characters(lock_identifier);
    let mut elsewhere = Vec::new();
    for (name, tenant) in context.siblings.declared() {
        let held = context.store.current_lock(&tenant).await.ok().flatten();
        if held.is_some_and(|lock| lock.lock_identifier == lock_identifier) {
            elsewhere.push(format!(
                "`{}`",
                context
                    .invocation
                    .with_environment(Some(&name))
                    .command(&format!("unlock {identifier}"))
            ));
        }
    }
    let help = if elsewhere.is_empty() {
        format!(
            "The lock may have been released already, or belong to another environment of the              project (locks are per environment); run `{}` to see the lock this selection              holds.",
            context.invocation.command("unlock")
        )
    } else {
        format!(
            "That lock is held by another environment of the project: run {}.",
            elsewhere.join(" or ")
        )
    };
    CliError::config_with_help(
        format!(
            "{} is not locked by {identifier}; nothing was released",
            tenant_label(context.tenant)
        ),
        help,
    )
}

/// What a run without `--env` says about other environments of the project
/// that hold a lock: `unlock` only looks at the identity it was given.
async fn locks_in_other_environments(context: &CommandContext<'_>) -> Vec<String> {
    let mut notes = Vec::new();
    for (name, tenant) in context.siblings.declared() {
        if let Ok(Some(lock)) = context.store.current_lock(&tenant).await {
            let identifier = strip_control_characters(&lock.lock_identifier);
            notes.push(format!(
                "environment '{}' is locked by '{}' (lock {identifier}); release it with `{}`",
                evaluation::escape_control_characters(&name),
                strip_control_characters(&lock.holder),
                context
                    .invocation
                    .with_environment(Some(&name))
                    .command(&format!("unlock {identifier}"))
            ));
        }
    }
    notes
}

/// `plan`: refresh and plan without the lock.
async fn plan(context: &CommandContext<'_>, inputs: EngineInputs) -> Result<(), CliError> {
    require_owner(context).await?;
    let mut engine = InfrastructureEngine::new(engine_setup(context, inputs));
    // Planning honours the interrupt itself: providers are asked to stop
    // and the next resource is not planned.
    let result = engine.plan(PlanMode::Apply).await;
    engine.shutdown().await;
    let plan = result.map_err(|error| failure(&error, context.invocation))?;
    context.output.plan(&plan);
    Ok(())
}

/// How a converging command runs.
struct Convergence<'run> {
    mode: PlanMode,
    confirmation: ConfirmationPolicy,
    context: &'run CommandContext<'run>,
}

/// `apply` and `destroy`: take the lock, claim ownership, plan, confirm
/// while holding the lock (unless `--yes`), apply exactly that plan.
async fn converge(convergence: &Convergence<'_>, inputs: EngineInputs) -> Result<(), CliError> {
    let Convergence { mode, context, .. } = *convergence;
    let mut engine = InfrastructureEngine::new(engine_setup(context, inputs));
    let planning = &mut engine;
    let result = under_lock(context, output::operation_name(mode), |lock| async move {
        claim_ownership(context, &lock).await?;
        let plan = planning.plan(mode).await.map_err(|error| failure(&error, context.invocation))?;
        context.output.preview(&plan);
        if plan.has_work() && convergence.confirmation == ConfirmationPolicy::Prompt {
            confirm(mode, context).await?;
        }
        apply_plan(Application {
            engine: planning,
            plan: &plan,
            lock: &lock,
            mode,
            output: context.output,
            invocation: context.invocation,
        })
        .await
    })
    .await;
    engine.shutdown().await;
    result
}

/// A plan to apply under a held lock.
struct Application<'application> {
    engine: &'application mut InfrastructureEngine,
    plan: &'application Plan,
    lock: &'application StateLock,
    mode: PlanMode,
    output: &'application Output,
    invocation: &'application Invocation,
}

/// Apply a plan made under the lock, when it has any work.
async fn apply_plan(application: Application<'_>) -> Result<(), CliError> {
    let Application {
        engine,
        plan,
        lock,
        mode,
        output,
        invocation,
    } = application;
    if !plan.has_work() {
        output.converged(&Converged {
            mode,
            plan,
            applied: None,
        });
        return Ok(());
    }
    // Replacements whose old object is gone and whose new one was not
    // created, as the engine reports them while it runs: the only source on
    // the paths that end without an `ApplyIncomplete` (an interrupt, a fatal
    // error).
    let mut reported_deleted_not_recreated: Vec<String> = Vec::new();
    let mut on_event = |event: ApplyEvent| match event {
        ApplyEvent::Started { address, action } => {
            output.progress(format!("{} {address}: applying...", action.symbol()));
        }
        ApplyEvent::Finished { address, .. } => output.progress(format!("  {address}: done")),
        ApplyEvent::Refreshed { address } => {
            output.progress(format!("  {address}: stored state refreshed"));
        }
        ApplyEvent::Failed { address, action } => {
            output.progress(format!("  {address}: {} failed", action.symbol()));
        }
        ApplyEvent::Skipped { address, action } => {
            output.progress(format!(
                "  {address}: {} skipped because a change it depends on failed",
                action.symbol()
            ));
        }
        ApplyEvent::DeletedNotRecreated { address } => {
            emit_stderr!(format!(
                "warning: {address} was deleted and NOT recreated; the next apply creates it"
            ));
            reported_deleted_not_recreated.push(address.to_string());
        }
        ApplyEvent::Warning(warning) => {
            emit_stderr!(format!(
                "warning: {}",
                printable_text(&warning)
            ));
        }
    };
    let applied = engine
        .apply(plan, ApplyContext { lock }, &mut on_event)
        .await
        .map_err(|error| {
            failure(&error, invocation).with_deleted_not_recreated(deleted_not_recreated(
                &error,
                &reported_deleted_not_recreated,
            ))
        })?;
    output.converged(&Converged {
        mode,
        plan,
        applied: Some(applied),
    });
    Ok(())
}

/// Addresses of the replacements a failed apply deleted (the first half of a
/// replacement) and did not recreate: the engine's own list when the apply
/// ended incomplete, plus every one it reported while running (the only
/// source when the run was interrupted or failed fatally). Each address once.
fn deleted_not_recreated(error: &InfrastructureError, reported: &[String]) -> Vec<String> {
    let mut addresses: Vec<String> = match error {
        InfrastructureError::ApplyIncomplete(incomplete) => incomplete
            .deleted_not_recreated
            .iter()
            .map(ToString::to_string)
            .collect(),
        _ => Vec::new(),
    };
    for address in reported {
        if !addresses.contains(address) {
            addresses.push(address.clone());
        }
    }
    addresses
}

/// Attempts to release the lock after a run.
const RELEASE_ATTEMPTS: u32 = 3;

/// Release the lock, retrying a failure twice (after 250 ms, then 500 ms)
/// and never waiting after the last attempt.
async fn release(
    store: &dyn StateStore,
    tenant: &TenantKey,
    lock: &StateLock,
) -> cuenv_infrastructure::Result<()> {
    let mut attempt = 1;
    loop {
        match store.unlock(tenant, lock).await {
            Ok(()) => return Ok(()),
            Err(error) if attempt >= RELEASE_ATTEMPTS => return Err(error),
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(250 * u64::from(attempt))).await;
                attempt += 1;
            }
        }
    }
}

/// A line read at the confirmation prompt: `None` at the end of input.
type AnswerFuture<'answers> =
    Pin<Box<dyn Future<Output = std::io::Result<Option<String>>> + Send + 'answers>>;

/// Where the answer to the confirmation prompt comes from.
trait Answers: Send + Sync {
    /// Whether anyone can answer (standard input is a terminal).
    fn is_interactive(&self) -> bool;

    /// Read one answer without blocking the runtime.
    fn read(&self) -> AnswerFuture<'_>;
}

/// Answers typed on the terminal.
#[derive(Debug)]
struct TerminalAnswers;

impl Answers for TerminalAnswers {
    fn is_interactive(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    fn read(&self) -> AnswerFuture<'_> {
        Box::pin(async {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            // A detached thread, so an interrupt at the prompt never waits
            // on a blocking read; nothing joins it.
            std::thread::Builder::new()
                .name("cuenv-infrastructure-prompt".to_string())
                .spawn(move || {
                    let mut line = String::new();
                    let read = std::io::stdin()
                        .read_line(&mut line)
                        .map(|bytes| (bytes > 0).then_some(line));
                    // The prompt is abandoned after an interrupt; the
                    // answer is meant to be dropped then.
                    let _ = sender.send(read);
                })?;
            receiver
                .await
                .map_err(|_| std::io::Error::other("the prompt reader stopped"))?
        })
    }
}

/// Identifier of the confirmation prompt in prompt events.
const PROMPT_IDENTIFIER: &str = "infrastructure-confirmation";

/// The error for a plan that was not confirmed.
fn cancelled(mode: PlanMode, reason: &str) -> CliError {
    CliError::infrastructure(
        format!(
            "{} cancelled: {reason}; nothing was applied",
            output::operation_name(mode)
        ),
        Some(
            "Answer 'yes' at the prompt to apply the plan shown, or pass --yes to apply without \
             the prompt."
                .to_string(),
        ),
        InfrastructureFailureKind::Cancelled,
    )
}

/// Ask for confirmation of the plan shown, while the lock is held. Only
/// `yes` confirms; any other answer, the end of input, or an unreadable
/// answer cancels, and an interrupt stops the run.
async fn confirm(mode: PlanMode, context: &CommandContext<'_>) -> Result<(), CliError> {
    cuenv_events::emit_prompt_requested!(
        PROMPT_IDENTIFIER,
        match mode {
            PlanMode::Apply => "Type 'yes' to apply these changes:",
            PlanMode::Destroy => "Type 'yes' to destroy every resource listed above:",
        },
        Vec::<String>::new()
    );
    let answer = context
        .interrupts
        .until_interrupted(context.answers.read())
        .await?;
    match answer {
        Ok(Some(line)) => {
            let answer = strip_control_characters(line.trim());
            cuenv_events::emit_prompt_resolved!(PROMPT_IDENTIFIER, answer.as_str());
            if answer == "yes" {
                Ok(())
            } else {
                Err(cancelled(mode, "the answer was not 'yes'"))
            }
        }
        Ok(None) => Err(cancelled(mode, "standard input ended without an answer")),
        Err(error) => Err(cancelled(
            mode,
            &format!("the answer could not be read ({error})"),
        )),
    }
}

#[cfg(test)]
mod tests;
