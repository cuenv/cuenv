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
mod output;

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
    strip_control_characters_except_newlines,
};
use cuenv_manifest::manifest::Infrastructure;

use self::evaluation::{NameCheck, Target, TargetRequest};
use self::interrupts::{HeldLock, Interrupts};
use self::output::{Adoption, Converged, Finish, LockOutcome, LockReport, Output};
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
    /// Action to perform.
    pub action: InfrastructureAction,
    /// Text or JSON output.
    pub output: OutputFormat,
}

const IMPORT_HELP: &str = "cuenv cannot import resources yet: find the resource with the \
                           provider's own tools and delete it, then plan again (the next apply \
                           creates it anew).";

/// Map an engine or store error to the command's error, with the help an
/// operator needs. Text from providers and the store is stripped of control
/// characters.
fn failure(error: &InfrastructureError) -> CliError {
    let message = strip_control_characters_except_newlines(&error.to_string());
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
            Some(
                "Another run released or took this project's lock. Review `cuenv infrastructure \
                 plan` before applying again."
                    .to_string(),
            ),
        ),
        InfrastructureError::UnrecordedChangeLost { .. } => (
            InfrastructureFailureKind::Failed,
            Some(IMPORT_HELP.to_string()),
        ),
        InfrastructureError::Interrupted { .. } | InfrastructureError::InterruptedWhilePlanning => {
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
                 either run `cuenv infrastructure state recover --force` to write the saved \
                 record anyway, or move its file out of the unrecorded change directory to keep \
                 the stored record.",
                strip_control_characters(address)
            )),
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
            Some(
                "Each project's state belongs to one CUE instance (directory and package). If \
                 this instance should manage it now (for example, the project moved), run \
                 `cuenv infrastructure state adopt` here; otherwise give this project a \
                 different `name`."
                    .to_string(),
            ),
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
            Some(
                "Run `cuenv infrastructure state recover`, then run this command again."
                    .to_string(),
            ),
        ),
        // The messages of these say what to do next, or carry the
        // provider's own diagnostics.
        InfrastructureError::UnrecordedChange { .. }
        | InfrastructureError::Codec(_)
        | InfrastructureError::Plugin(_)
        | InfrastructureError::RemoteProcedure { .. }
        | InfrastructureError::Diagnostics { .. }
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
    let output = Output::new(options.output);
    let interrupts = Interrupts::claim(&output)?;
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
    refuse_unconfirmable(&options.action, output, &answers)?;
    let Target {
        tenant,
        instance,
        infrastructure,
        project_directory,
    } = evaluate_target(options, interrupts).await?;
    let store = connect(&infrastructure).map_err(|error| failure(&error))?;
    let context = CommandContext {
        store: &store,
        tenant: &tenant,
        instance: &instance,
        output,
        interrupts,
        answers: &answers,
    };
    dispatch(
        &options.action,
        &context,
        EngineInputs {
            infrastructure,
            project_directory,
            // The user state directory (see `UnrecordedStore`).
            unrecorded_directory: None,
        },
    )
    .await
}

/// Refuse a prompt that cannot be answered before evaluating anything.
fn refuse_unconfirmable(
    action: &InfrastructureAction,
    output: &Output,
    answers: &dyn Answers,
) -> Result<(), CliError> {
    if action.confirmation() == ConfirmationPolicy::AssumeYes {
        return Ok(());
    }
    if output.is_json() {
        return Err(CliError::config_with_help(
            "--json needs --yes for apply and destroy: the confirmation prompt cannot share \
             standard output with the JSON result",
            "Review `cuenv infrastructure plan --json` first, then run with --yes.",
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

/// Evaluate the target on a thread of its own. CUE evaluation cannot be
/// cancelled, and nothing is held yet, so an interrupt abandons it at once:
/// the thread is not waited for, and the process exits without it.
async fn evaluate_target(
    options: &InfrastructureOptions,
    interrupts: &Interrupts,
) -> Result<Target, CliError> {
    let (path, package, name_check) = (
        options.path.clone(),
        options.package.clone(),
        options.action.name_check(),
    );
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("cuenv-infrastructure-evaluation".to_string())
        .spawn(move || {
            let target = evaluation::evaluate(TargetRequest {
                path: &path,
                package: &package,
                name_check,
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
    output: &'context Output,
    interrupts: &'context Interrupts,
    answers: &'context dyn Answers,
}

/// What an engine is built from, besides the context.
struct EngineInputs {
    infrastructure: Infrastructure,
    project_directory: PathBuf,
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
                .map_err(|error| failure(&error))?;
            context.output.state(context.tenant, &resources);
            Ok(())
        }
        InfrastructureAction::State(StateAction::Remove { address }) => {
            remove_resource(context, address).await
        }
        InfrastructureAction::State(StateAction::Recover { overwrite }) => {
            recover(context, &unrecorded_store(&inputs)?, *overwrite).await
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
    let withheld_environment_variables = vec![
        inputs
            .infrastructure
            .state
            .turso
            .authentication_token_environment_variable
            .clone(),
    ];
    EngineSetup {
        tenant: context.tenant.clone(),
        store: Arc::clone(context.store),
        infrastructure: inputs.infrastructure,
        options: EngineOptions {
            project_directory: inputs.project_directory,
            plugin_cache_directory: None,
            withheld_environment_variables,
            unrecorded_directory: inputs.unrecorded_directory,
            // Every provider the engine launches is registered with the
            // command's interrupt handling.
            cancellation: context.interrupts.cancellation().clone(),
        },
    }
}

fn unrecorded_store(inputs: &EngineInputs) -> Result<UnrecordedStore, CliError> {
    inputs
        .unrecorded_directory
        .as_ref()
        .map_or_else(UnrecordedStore::default_location, |directory| {
            Ok(UnrecordedStore::at(directory))
        })
        .map_err(|error| failure(&error))
}

/// Connect to the state store. Nothing is sent yet: reads never create
/// tables, and [`under_lock`] creates or upgrades them before any write.
fn connect(infrastructure: &Infrastructure) -> cuenv_infrastructure::Result<Arc<dyn StateStore>> {
    let turso = &infrastructure.state.turso;
    let variable = &turso.authentication_token_environment_variable;
    let authentication_token = std::env::var(variable)
        .ok()
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
        .map_err(|error| failure(&error))?;
    owner.map_or(Ok(()), |owner| {
        owner
            .require(context.tenant, context.instance)
            .map_err(|error| failure(&error))
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
        .map_err(|error| failure(&error))?;
    owner
        .require(context.tenant, context.instance)
        .map_err(|error| failure(&error))
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
        .map_err(|error| failure(&error))?;
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
        return Err(acquisition_failure(&error, &lock));
    }
    context.interrupts.hold(held);
    emit_stderr!(format!(
        "Acquired lock {} for {}",
        lock.lock_identifier, context.tenant
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
            lock.lock_identifier, context.tenant
        ));
    }
    settle(Settlement {
        operation,
        lock: &lock,
        result,
        released,
    })
}

/// A failed acquisition. When its outcome is uncertain (the response was
/// lost), the error names this run's lock so it can be released.
fn acquisition_failure(error: &InfrastructureError, lock: &StateLock) -> CliError {
    let failure = failure(error);
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
        result,
        released,
    } = settlement;
    let identifier = &lock.lock_identifier;
    let status = |released| LockStatus {
        identifier: identifier.clone(),
        released,
    };
    let unlock_help = format!(
        "After checking no run is active, release it with `cuenv infrastructure unlock \
         {identifier}`."
    );
    match (result, released) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Err(error), Ok(())) => Err(error.with_lock(status(true))),
        (Ok(_), Err(release_error)) => Err(CliError::infrastructure(
            format!(
                "{operation} succeeded but lock {identifier} was NOT released: {}",
                strip_control_characters_except_newlines(&release_error.to_string())
            ),
            Some(unlock_help),
            InfrastructureFailureKind::Failed,
        )
        .with_lock(status(false))),
        (Err(error), Err(release_error)) => {
            let not_released = format!(
                "Lock {identifier} was NOT released ({}). {unlock_help}",
                strip_control_characters_except_newlines(&release_error.to_string())
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
        let resources = context
            .store
            .list(context.tenant)
            .await
            .map_err(|error| failure(&error))?;
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
                    "{} has no managed resource {address} in state",
                    context.tenant
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
            .map_err(|error| failure(&error))?;
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
        .map_err(|error| failure(&error))?
    {
        context.output.recovered(context.tenant, &[]);
        return Ok(());
    }
    under_lock(context, "state recover", |lock| async move {
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
            .map_err(|error| failure(&error))?;
        context.output.recovered(context.tenant, &recovered);
        Ok(())
    })
    .await
}

/// `state adopt`: make this instance the owner of the tenant's state.
async fn adopt(context: &CommandContext<'_>) -> Result<(), CliError> {
    under_lock(context, "state adopt", |lock| async move {
        let previous = context
            .store
            .owner(context.tenant)
            .await
            .map_err(|error| failure(&error))?;
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
            .map_err(|error| failure(&error))?;
        context.output.adopted(&Adoption {
            tenant: context.tenant,
            previous: previous.as_ref(),
            owner: &owner,
        });
        Ok(())
    })
    .await
}

/// `unlock`: show the lock, or release the one named. Naming a lock when
/// none is held shows that and succeeds; naming another lock is refused.
async fn unlock(
    context: &CommandContext<'_>,
    lock_identifier: Option<&str>,
) -> Result<(), CliError> {
    let tenant = context.tenant;
    let current = context
        .store
        .current_lock(tenant)
        .await
        .map_err(|error| failure(&error))?;
    let (Some(current), Some(lock_identifier)) = (current.as_ref(), lock_identifier) else {
        context.output.lock(&LockReport {
            tenant,
            lock: current.as_ref(),
            outcome: LockOutcome::Shown,
        });
        return Ok(());
    };
    let released = context
        .store
        .force_unlock(tenant, lock_identifier)
        .await
        .map_err(|error| failure(&error))?;
    if !released {
        // The lock is held, just not by the run the operator named: that
        // is concurrent activity, not a configuration mistake.
        let holder = strip_control_characters(&current.lock_identifier);
        return Err(CliError::infrastructure(
            format!(
                "{tenant} is locked by {holder}, not {}; nothing was released",
                strip_control_characters(lock_identifier)
            ),
            Some(
                "Run `cuenv infrastructure unlock` without an identifier to see who holds the \
                 lock now."
                    .to_string(),
            ),
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
    });
    Ok(())
}

/// `plan`: refresh and plan without the lock.
async fn plan(context: &CommandContext<'_>, inputs: EngineInputs) -> Result<(), CliError> {
    require_owner(context).await?;
    let mut engine = InfrastructureEngine::new(engine_setup(context, inputs));
    // Planning honours the interrupt itself: providers are asked to stop
    // and the next resource is not planned.
    let result = engine.plan(PlanMode::Apply).await;
    engine.shutdown().await;
    let plan = result.map_err(|error| failure(&error))?;
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
        let plan = planning.plan(mode).await.map_err(|error| failure(&error))?;
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
}

/// Apply a plan made under the lock, when it has any work.
async fn apply_plan(application: Application<'_>) -> Result<(), CliError> {
    let Application {
        engine,
        plan,
        lock,
        mode,
        output,
    } = application;
    if !plan.has_work() {
        output.converged(&Converged {
            mode,
            plan,
            applied: None,
        });
        return Ok(());
    }
    let mut on_event = |event: ApplyEvent| match event {
        ApplyEvent::Started { address, action } => {
            output.progress(format!("{} {address}: applying...", action.symbol()));
        }
        ApplyEvent::Finished { address, .. } => output.progress(format!("  {address}: done")),
        ApplyEvent::Refreshed { address } => {
            output.progress(format!("  {address}: stored state refreshed"));
        }
        ApplyEvent::Warning(warning) => {
            emit_stderr!(format!(
                "warning: {}",
                strip_control_characters_except_newlines(&warning)
            ));
        }
    };
    let applied = engine
        .apply(plan, ApplyContext { lock }, &mut on_event)
        .await
        .map_err(|error| failure(&error))?;
    output.converged(&Converged {
        mode,
        plan,
        applied: Some(applied),
    });
    Ok(())
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
