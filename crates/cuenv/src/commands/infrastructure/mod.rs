//! Implementation of `cuenv infrastructure` (short form `cuenv i`).
//!
//! Evaluates the project's `infrastructure` block and hands it to
//! `cuenv-infrastructure`, which drives Terraform provider plugins over gRPC
//! and stores each managed resource in the configured Turso database. State
//! is keyed by the CUE module path and the project name.
//!
//! `apply` and `destroy` with `--yes` take the lock, plan once and apply that
//! plan. With a prompt they plan without the lock, ask for confirmation, then
//! take the lock, plan again and refuse to apply unless the new plan's digest
//! matches the one the operator confirmed. The command owns interrupt
//! handling for its whole run (see [`interrupts`]).

mod evaluation;
mod holder;
mod interrupts;
mod output;

use std::future::Future;
use std::io::IsTerminal;
use std::sync::Arc;
use std::time::Duration;

use cuenv_infrastructure::{
    ApplyContext, ApplyEvent, EngineOptions, EngineSetup, InfrastructureEngine,
    InfrastructureError, Plan, PlanMode, StateLock, StateStore, TenantKey, TursoConfiguration,
    TursoStateStore,
};
use cuenv_manifest::manifest::Infrastructure;

use self::evaluation::{NameCheck, Target, TargetRequest};
use self::interrupts::{HeldLock, Interrupts};
use self::output::{Converged, LockOutcome, LockReport, Output};
use crate::cli::{CliError, InfrastructureFailureKind, OutputFormat};

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
    Recover,
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
    /// Plans and changes check the project name across the whole module;
    /// inspecting and repairing state and unlocking must keep working when a
    /// sibling instance is broken.
    const fn name_check(&self) -> NameCheck {
        match self {
            Self::Plan | Self::Apply { .. } | Self::Destroy { .. } => NameCheck::WholeModule,
            Self::State(_) | Self::Unlock { .. } => NameCheck::TargetOnly,
        }
    }

    /// Whether the action writes state, and so may create or upgrade the
    /// state tables. Reads work against a database without them, so a
    /// read-only token is enough for them.
    const fn store_access(&self) -> StoreAccess {
        match self {
            Self::Apply { .. }
            | Self::Destroy { .. }
            | Self::State(StateAction::Remove { .. } | StateAction::Recover)
            | Self::Unlock {
                lock_identifier: Some(_),
            } => StoreAccess::ReadWrite,
            Self::Plan
            | Self::State(StateAction::List)
            | Self::Unlock {
                lock_identifier: None,
            } => StoreAccess::ReadOnly,
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

/// Whether opening the store may migrate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreAccess {
    /// Never run migrations.
    ReadOnly,
    /// Create or upgrade the tables before writing.
    ReadWrite,
}

fn failure(error: &InfrastructureError) -> CliError {
    let (kind, help) = match error {
        InfrastructureError::Configuration(_) => return CliError::config(error.to_string()),
        InfrastructureError::Locked { .. } => (
            InfrastructureFailureKind::Locked,
            Some("Wait for the other run to finish, then run the command again.".to_string()),
        ),
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
            Some(
                "cuenv cannot import resources yet: find the resource with the provider's own \
                 tools and delete it, then plan again (the next apply creates it anew)."
                    .to_string(),
            ),
        ),
        InfrastructureError::Interrupted { .. } | InfrastructureError::InterruptedWhilePlanning => {
            (InfrastructureFailureKind::Interrupted, None)
        }
        InfrastructureError::State(_) => (
            InfrastructureFailureKind::Failed,
            Some("Check the Turso URL, the authentication token and network access.".to_string()),
        ),
        // The messages of these name the command to run next.
        InfrastructureError::UnrecordedChange { .. }
        | InfrastructureError::UnrecordedChangesPending { .. }
        | InfrastructureError::Codec(_)
        | InfrastructureError::Plugin(_)
        | InfrastructureError::RemoteProcedure { .. }
        | InfrastructureError::Diagnostics { .. }
        | InfrastructureError::Install(_)
        | InfrastructureError::InputOutput { .. }
        | InfrastructureError::UnrecordedFile { .. }
        | InfrastructureError::StateChanged { .. }
        | InfrastructureError::PlanOutdated { .. }
        | InfrastructureError::OwnedByAnotherInstance { .. }
        | InfrastructureError::StateFromNewerProvider { .. } => {
            (InfrastructureFailureKind::Failed, None)
        }
    };
    CliError::infrastructure(error.to_string(), help, kind)
}

/// Execute `cuenv infrastructure`.
///
/// The command claims SIGINT and SIGTERM before doing anything else and
/// keeps them until it returns.
///
/// # Errors
///
/// Returns an error if evaluation fails, the project has no `infrastructure`
/// block, its name is not unique in the module, the state store is
/// unreachable or locked, a provider fails, or the run is interrupted.
#[tracing::instrument(skip_all, fields(path = %options.path, action = ?options.action))]
pub async fn execute_infrastructure(options: &InfrastructureOptions) -> Result<(), CliError> {
    let interrupts = Interrupts::claim()?;
    let result = run(options, &interrupts).await;
    // Dropping the claim aborts the signal watcher.
    drop(interrupts);
    result
}

async fn run(options: &InfrastructureOptions, interrupts: &Interrupts) -> Result<(), CliError> {
    let output = Output::new(options.output);
    if output.is_json()
        && let InfrastructureAction::Apply {
            confirmation: ConfirmationPolicy::Prompt,
        }
        | InfrastructureAction::Destroy {
            confirmation: ConfirmationPolicy::Prompt,
        } = options.action
    {
        return Err(CliError::config_with_help(
            "--json needs --yes for apply and destroy: the confirmation prompt cannot share \
             standard output with the JSON result",
            "Review `cuenv infrastructure plan --json` first, then run with --yes.",
        ));
    }

    // CUE evaluation cannot be cancelled; run it off the runtime's worker
    // threads so the signal watcher keeps running, and honour an interrupt
    // received meanwhile as soon as it returns.
    let (path, package, name_check) = (
        options.path.clone(),
        options.package.clone(),
        options.action.name_check(),
    );
    let target = tokio::task::spawn_blocking(move || {
        evaluation::evaluate(TargetRequest {
            path: &path,
            package: &package,
            name_check,
        })
    })
    .await
    .map_err(|error| CliError::other(format!("evaluation stopped unexpectedly: {error}")))??;
    interrupts.check()?;
    let store = interrupts
        .until_interrupted(open_store(
            &target.infrastructure,
            options.action.store_access(),
        ))
        .await?
        .map_err(|error| failure(&error))?;
    let tenant = target.tenant.clone();
    let context = CommandContext {
        store: &store,
        tenant: &tenant,
        output,
        interrupts,
    };

    match &options.action {
        InfrastructureAction::State(StateAction::List) => {
            let resources = interrupts
                .until_interrupted(store.list(&tenant))
                .await?
                .map_err(|error| failure(&error))?;
            output.state(&tenant, &resources);
            Ok(())
        }
        InfrastructureAction::State(StateAction::Remove { address }) => {
            remove_resource(&context, address).await
        }
        InfrastructureAction::State(StateAction::Recover) => {
            let engine = InfrastructureEngine::new(engine_setup(target, store.clone(), interrupts));
            let result = recover(&context, &engine).await;
            engine.shutdown().await;
            result
        }
        InfrastructureAction::Unlock { lock_identifier } => {
            interrupts
                .until_interrupted(unlock(&UnlockRequest {
                    store: store.as_ref(),
                    tenant: &tenant,
                    lock_identifier: lock_identifier.as_deref(),
                    output,
                }))
                .await?
        }
        InfrastructureAction::Plan => {
            let mut engine =
                InfrastructureEngine::new(engine_setup(target, store.clone(), interrupts));
            // Planning honours the interrupt itself: providers are asked to
            // stop and the next resource is not planned.
            let result = engine.plan(PlanMode::Apply).await;
            engine.shutdown().await;
            let plan = result.map_err(|error| failure(&error))?;
            output.plan(&plan);
            Ok(())
        }
        InfrastructureAction::Apply { confirmation } => {
            converge(
                engine_setup(target, store.clone(), interrupts),
                &Convergence {
                    mode: PlanMode::Apply,
                    confirmation: *confirmation,
                    context: &context,
                },
            )
            .await
        }
        InfrastructureAction::Destroy { confirmation } => {
            converge(
                engine_setup(target, store.clone(), interrupts),
                &Convergence {
                    mode: PlanMode::Destroy,
                    confirmation: *confirmation,
                    context: &context,
                },
            )
            .await
        }
    }
}

/// What every stateful subcommand works with.
struct CommandContext<'context> {
    store: &'context Arc<dyn StateStore>,
    tenant: &'context TenantKey,
    output: Output,
    interrupts: &'context Interrupts,
}

fn engine_setup(
    target: Target,
    store: Arc<dyn StateStore>,
    interrupts: &Interrupts,
) -> EngineSetup {
    let withheld_environment_variables = vec![
        target
            .infrastructure
            .state
            .turso
            .authentication_token_environment_variable
            .clone(),
    ];
    EngineSetup {
        tenant: target.tenant,
        store,
        infrastructure: target.infrastructure,
        options: EngineOptions {
            project_directory: target.project_directory,
            plugin_cache_directory: None,
            withheld_environment_variables,
            // The user state directory (see `UnrecordedStore`).
            unrecorded_directory: None,
            // Every provider the engine launches is registered with the
            // command's interrupt handling.
            cancellation: interrupts.cancellation().clone(),
        },
    }
}

async fn open_store(
    infrastructure: &Infrastructure,
    access: StoreAccess,
) -> cuenv_infrastructure::Result<Arc<dyn StateStore>> {
    let turso = &infrastructure.state.turso;
    let variable = &turso.authentication_token_environment_variable;
    let authentication_token = std::env::var(variable)
        .ok()
        .filter(|token| !token.is_empty());
    match &authentication_token {
        // Redact the token from every output from here on, including
        // provider and store errors that might echo it.
        Some(token) => cuenv_events::register_secret(token.clone()),
        None => cuenv_events::emit_stderr!(format!(
            "warning: {variable} is not set; connecting to Turso without an authentication token"
        )),
    }
    let store: Arc<dyn StateStore> = Arc::new(TursoStateStore::new(TursoConfiguration {
        url: turso.url.clone(),
        authentication_token,
    })?);
    if access == StoreAccess::ReadWrite {
        store.migrate().await?;
    }
    Ok(store)
}

/// Take the project's lock, run `work` with it, and release it.
///
/// Acquisition is never abandoned half way: an interrupt while it runs is
/// honoured once it returns, by releasing the lock again. While the lock is
/// held, a second interrupt can release it before the process exits.
async fn under_lock<Outcome, Work>(
    context: &CommandContext<'_>,
    operation: &str,
    work: impl FnOnce(StateLock) -> Work,
) -> Result<Outcome, CliError>
where
    Work: Future<Output = Result<Outcome, CliError>>,
{
    let lock = context
        .store
        .lock(context.tenant, &holder::describe(operation))
        .await
        .map_err(|error| failure(&error))?;
    context.interrupts.hold(HeldLock {
        store: Arc::clone(context.store),
        tenant: context.tenant.clone(),
        lock: lock.clone(),
    });
    let result = match context.interrupts.check() {
        Ok(()) => work(lock.clone()).await,
        Err(interrupted) => Err(interrupted),
    };
    let released = release(context.store.as_ref(), context.tenant, &lock).await;
    context.interrupts.released();
    match (result, released) {
        (Ok(output), Ok(())) => Ok(output),
        (Ok(_), Err(release_error)) => Err(CliError::infrastructure(
            format!("{operation} succeeded but the lock was NOT released: {release_error}"),
            Some(format!(
                "Release it with `cuenv infrastructure unlock {}`.",
                lock.lock_identifier
            )),
            InfrastructureFailureKind::Failed,
        )),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(release_error)) => {
            cuenv_events::emit_stderr!(format!(
                "error: the lock was NOT released ({release_error}); release it with \
                 `cuenv infrastructure unlock {}`",
                lock.lock_identifier
            ));
            Err(error)
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
                .map(|resource| resource.address.to_string())
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
/// lock, deleting each saved file once it is recorded.
async fn recover(
    context: &CommandContext<'_>,
    engine: &InfrastructureEngine,
) -> Result<(), CliError> {
    let unrecorded = engine.unrecorded_store().map_err(|error| failure(&error))?;
    under_lock(context, "state recover", |lock| async move {
        let recovered = unrecorded
            .recover(
                context.store.as_ref(),
                context.tenant,
                &cuenv_infrastructure::RecoverOptions {
                    lock: &lock,
                    overwrite: cuenv_infrastructure::RecoverOverwrite::IfUnchanged,
                },
            )
            .await
            .map_err(|error| failure(&error))?;
        context.output.recovered(context.tenant, &recovered);
        Ok(())
    })
    .await
}

/// Inputs for [`unlock`].
struct UnlockRequest<'request> {
    store: &'request dyn StateStore,
    tenant: &'request TenantKey,
    lock_identifier: Option<&'request str>,
    output: Output,
}

async fn unlock(request: &UnlockRequest<'_>) -> Result<(), CliError> {
    let tenant = request.tenant;
    let current = request
        .store
        .current_lock(tenant)
        .await
        .map_err(|error| failure(&error))?;
    let (Some(current), Some(lock_identifier)) = (current.as_ref(), request.lock_identifier) else {
        request.output.lock(&LockReport {
            tenant,
            lock: current.as_ref(),
            outcome: LockOutcome::Shown,
        });
        return Ok(());
    };
    let released = request
        .store
        .force_unlock(tenant, lock_identifier)
        .await
        .map_err(|error| failure(&error))?;
    if !released {
        // The lock is held, just not by the run the operator named: that
        // is concurrent activity, not a configuration mistake.
        return Err(CliError::infrastructure(
            format!(
                "{tenant} is locked by {}, not {lock_identifier}; nothing was released",
                current.lock_identifier
            ),
            Some(
                "Run `cuenv infrastructure unlock` without an identifier to see who holds the \
                 lock now."
                    .to_string(),
            ),
            InfrastructureFailureKind::Locked,
        ));
    }
    request.output.lock(&LockReport {
        tenant,
        lock: Some(current),
        outcome: LockOutcome::Released,
    });
    Ok(())
}

/// How a converging command runs.
struct Convergence<'run> {
    mode: PlanMode,
    confirmation: ConfirmationPolicy,
    context: &'run CommandContext<'run>,
}

async fn converge(setup: EngineSetup, convergence: &Convergence<'_>) -> Result<(), CliError> {
    let mut engine = InfrastructureEngine::new(setup);
    let result = match convergence.confirmation {
        ConfirmationPolicy::AssumeYes => converge_unattended(&mut engine, convergence).await,
        ConfirmationPolicy::Prompt => converge_confirmed(&mut engine, convergence).await,
    };
    engine.shutdown().await;
    result
}

/// `--yes`: take the lock, plan once, apply that plan. There is no preview
/// without the lock, so nothing can change between planning and applying.
async fn converge_unattended(
    engine: &mut InfrastructureEngine,
    convergence: &Convergence<'_>,
) -> Result<(), CliError> {
    let operation = output::operation_name(convergence.mode);
    under_lock(convergence.context, operation, |lock| async move {
        let plan = engine
            .plan(convergence.mode)
            .await
            .map_err(|error| failure(&error))?;
        convergence.context.output.preview(&plan);
        apply_plan(engine, &plan, &lock, convergence).await
    })
    .await
}

/// With a prompt: plan and confirm without holding the lock, so an
/// unanswered prompt never blocks other runs; then lock, plan again and
/// apply only if the new plan is exactly the confirmed one.
async fn converge_confirmed(
    engine: &mut InfrastructureEngine,
    convergence: &Convergence<'_>,
) -> Result<(), CliError> {
    let Convergence { mode, context, .. } = *convergence;
    let preview = engine.plan(mode).await.map_err(|error| failure(&error))?;
    context.output.preview(&preview);
    if !preview.has_work() {
        context.output.converged(&Converged {
            mode,
            plan: &preview,
            applied: None,
        });
        return Ok(());
    }
    if confirm(mode, context.interrupts).await? == Answer::Declined {
        context.output.text(match mode {
            PlanMode::Apply => "Apply cancelled.",
            PlanMode::Destroy => "Destroy cancelled.",
        });
        return Ok(());
    }

    let confirmed = preview.digest();
    under_lock(context, output::operation_name(mode), |lock| async move {
        // State or infrastructure may have moved since the preview.
        let plan = engine.plan(mode).await.map_err(|error| failure(&error))?;
        if plan.digest() != confirmed {
            context.output.preview(&plan);
            return Err(CliError::infrastructure(
                "the plan changed after it was confirmed; nothing was applied",
                Some(if context.output.is_json() {
                    "Review `cuenv infrastructure plan` and run the command again.".to_string()
                } else {
                    "Review the new plan above and run the command again.".to_string()
                }),
                InfrastructureFailureKind::PlanChanged,
            ));
        }
        apply_plan(engine, &plan, &lock, convergence).await
    })
    .await
}

/// Apply a plan made under `lock`, when it has any work.
async fn apply_plan(
    engine: &mut InfrastructureEngine,
    plan: &Plan,
    lock: &StateLock,
    convergence: &Convergence<'_>,
) -> Result<(), CliError> {
    let Convergence { mode, context, .. } = *convergence;
    let output = context.output;
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
            cuenv_events::emit_stderr!(format!("warning: {warning}"));
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

/// The operator's answer at the confirmation prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Confirmed,
    Declined,
}

/// Identifier of the confirmation prompt in prompt events.
const PROMPT_IDENTIFIER: &str = "infrastructure-confirmation";

async fn confirm(mode: PlanMode, interrupts: &Interrupts) -> Result<Answer, CliError> {
    if !std::io::stdin().is_terminal() {
        return Err(CliError::config(
            "refusing to change infrastructure without confirmation: standard input is not a \
             terminal; pass --yes",
        ));
    }
    cuenv_events::emit_prompt_requested!(
        PROMPT_IDENTIFIER,
        match mode {
            PlanMode::Apply => "Type 'yes' to apply these changes:",
            PlanMode::Destroy => "Type 'yes' to destroy every resource listed above:",
        },
        Vec::<String>::new()
    );
    // A detached thread, so an interrupt at the prompt never waits on a
    // blocking read during shutdown.
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = sender.send(std::io::stdin().read_line(&mut line).map(|_| line));
    });
    let answer = interrupts
        .until_interrupted(receiver)
        .await?
        .map_err(|_| CliError::other("confirmation prompt closed"))?
        .map_err(|error| CliError::other(format!("failed to read confirmation: {error}")))?;
    let answer = answer.trim();
    cuenv_events::emit_prompt_resolved!(PROMPT_IDENTIFIER, answer);
    Ok(if answer == "yes" {
        Answer::Confirmed
    } else {
        Answer::Declined
    })
}

#[cfg(test)]
mod tests;
