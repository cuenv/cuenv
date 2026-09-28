//! Implementation of `cuenv infrastructure` (short form `cuenv i`).
//!
//! Evaluates the project's `infrastructure` block and hands it to
//! `cuenv-infrastructure`, which drives Terraform provider plugins over gRPC
//! and stores each managed resource in the configured Turso database. State
//! is keyed by the CUE module path and the project name.
//!
//! Converging runs plan without the lock, ask for confirmation, then take the
//! lock and plan again; they refuse to apply if the second plan differs from
//! the one the operator confirmed. The command owns interrupt handling for
//! its whole run (see [`interrupts`]).

mod evaluation;
mod holder;
mod interrupts;
mod output;

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
    /// List managed resources recorded in state.
    State,
    /// Show the project's state lock, or release it when its identifier is given.
    Unlock {
        /// Identifier of the lock to release.
        lock_identifier: Option<String>,
    },
}

impl InfrastructureAction {
    /// Plans and changes check the project name across the whole module;
    /// inspecting and unlocking state must keep working when a sibling
    /// instance is broken.
    const fn name_check(&self) -> NameCheck {
        match self {
            Self::Plan | Self::Apply { .. } | Self::Destroy { .. } => NameCheck::WholeModule,
            Self::State | Self::Unlock { .. } => NameCheck::TargetOnly,
        }
    }

    /// Whether the action writes state, and so may create or upgrade the
    /// state tables. Reads work against a database without them, so a
    /// read-only token is enough for them.
    const fn store_access(&self) -> StoreAccess {
        match self {
            Self::Apply { .. }
            | Self::Destroy { .. }
            | Self::Unlock {
                lock_identifier: Some(_),
            } => StoreAccess::ReadWrite,
            Self::Plan
            | Self::State
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
    let help = match error {
        InfrastructureError::Locked {
            lock_identifier, ..
        } => Some(format!(
            "Wait for the other run to finish. If it is gone, release it with \
             `cuenv infrastructure unlock {lock_identifier}`."
        )),
        InfrastructureError::LockLost { .. } => Some(
            "Another run released or took this project's lock. Review `cuenv infrastructure plan` \
             before applying again."
                .to_string(),
        ),
        InfrastructureError::UnrecordedChange { .. } => Some(
            "The resource exists but is not in state. Restore connectivity to the state store and \
             re-record it before applying again."
                .to_string(),
        ),
        InfrastructureError::State(_) => {
            Some("Check the Turso URL, the authentication token and network access.".to_string())
        }
        InfrastructureError::Configuration(_) => {
            return CliError::config(error.to_string());
        }
        _ => None,
    };
    let kind = if matches!(error, InfrastructureError::Locked { .. }) {
        InfrastructureFailureKind::Locked
    } else {
        InfrastructureFailureKind::Failed
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

    match &options.action {
        InfrastructureAction::State => {
            let resources = interrupts
                .until_interrupted(store.list(&target.tenant))
                .await?
                .map_err(|error| failure(&error))?;
            output.state(&target.tenant, &resources);
            Ok(())
        }
        InfrastructureAction::Unlock { lock_identifier } => {
            interrupts
                .until_interrupted(unlock(&UnlockRequest {
                    store: store.as_ref(),
                    tenant: &target.tenant,
                    lock_identifier: lock_identifier.as_deref(),
                    output,
                }))
                .await?
        }
        InfrastructureAction::Plan => {
            let mut engine = InfrastructureEngine::new(engine_setup(target, store));
            let result = interrupts
                .until_interrupted(engine.plan(PlanMode::Apply))
                .await;
            engine.shutdown().await;
            let plan = result?.map_err(|error| failure(&error))?;
            output.plan(&plan);
            Ok(())
        }
        InfrastructureAction::Apply { confirmation } => {
            converge(
                engine_setup(target, store),
                &Convergence {
                    mode: PlanMode::Apply,
                    confirmation: *confirmation,
                    output,
                    interrupts,
                },
            )
            .await
        }
        InfrastructureAction::Destroy { confirmation } => {
            converge(
                engine_setup(target, store),
                &Convergence {
                    mode: PlanMode::Destroy,
                    confirmation: *confirmation,
                    output,
                    interrupts,
                },
            )
            .await
        }
    }
}

fn engine_setup(target: Target, store: Arc<dyn StateStore>) -> EngineSetup {
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
    output: Output,
    interrupts: &'run Interrupts,
}

async fn converge(setup: EngineSetup, convergence: &Convergence<'_>) -> Result<(), CliError> {
    let tenant = setup.tenant.clone();
    let store = Arc::clone(&setup.store);
    let mut engine = InfrastructureEngine::new(setup);
    let result = converge_with(
        &mut engine,
        &ConvergeTarget {
            tenant: &tenant,
            store: &store,
        },
        convergence,
    )
    .await;
    engine.shutdown().await;
    result
}

/// Where a converging command writes.
struct ConvergeTarget<'target> {
    tenant: &'target TenantKey,
    store: &'target Arc<dyn StateStore>,
}

async fn converge_with(
    engine: &mut InfrastructureEngine,
    target: &ConvergeTarget<'_>,
    convergence: &Convergence<'_>,
) -> Result<(), CliError> {
    let Convergence {
        mode,
        confirmation,
        output,
        interrupts,
    } = *convergence;
    // Plan and confirm without holding the lock, so an unanswered prompt
    // never blocks other runs.
    let preview = interrupts
        .until_interrupted(engine.plan(mode))
        .await?
        .map_err(|error| failure(&error))?;
    output.preview(&preview);
    if !preview.has_changes() {
        output.converged(&Converged {
            mode,
            plan: &preview,
            applied: None,
        });
        return Ok(());
    }
    if confirmation == ConfirmationPolicy::Prompt
        && confirm(mode, interrupts).await? == Answer::Declined
    {
        output.text(match mode {
            PlanMode::Apply => "Apply cancelled.",
            PlanMode::Destroy => "Destroy cancelled.",
        });
        return Ok(());
    }

    // Lock acquisition is never abandoned half way: an interrupt while it
    // runs is honoured once it returns, by releasing the lock again.
    let lock = target
        .store
        .lock(target.tenant, &holder::describe(mode))
        .await
        .map_err(|error| failure(&error))?;
    interrupts.hold(HeldLock {
        store: Arc::clone(target.store),
        tenant: target.tenant.clone(),
        lock: lock.clone(),
    });
    let applied = match interrupts.check() {
        Ok(()) => {
            apply_locked(
                engine,
                &LockedRun {
                    preview: &preview,
                    lock: &lock,
                    convergence,
                },
            )
            .await
        }
        Err(interrupted) => Err(interrupted),
    };
    let released = release(target.store.as_ref(), target.tenant, &lock).await;
    interrupts.released();
    match (applied, released) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(release_error)) => Err(CliError::infrastructure(
            format!(
                "{} succeeded but the lock was NOT released: {release_error}",
                output::operation_name(mode)
            ),
            Some(format!(
                "Release it with `cuenv infrastructure unlock {}`.",
                lock.lock_identifier
            )),
            InfrastructureFailureKind::Failed,
        )),
        (Err(apply_error), Ok(())) => Err(apply_error),
        (Err(apply_error), Err(release_error)) => {
            cuenv_events::emit_stderr!(format!(
                "error: the lock was NOT released ({release_error}); release it with \
                 `cuenv infrastructure unlock {}`",
                lock.lock_identifier
            ));
            Err(apply_error)
        }
    }
}

/// A confirmed plan about to be applied under the lock.
struct LockedRun<'run> {
    preview: &'run Plan,
    lock: &'run StateLock,
    convergence: &'run Convergence<'run>,
}

async fn apply_locked(
    engine: &mut InfrastructureEngine,
    run: &LockedRun<'_>,
) -> Result<(), CliError> {
    let Convergence {
        mode,
        output,
        interrupts,
        ..
    } = *run.convergence;
    // State may have moved between the preview and taking the lock.
    let plan = interrupts
        .until_interrupted(engine.plan(mode))
        .await?
        .map_err(|error| failure(&error))?;
    if plan.intent() != run.preview.intent() {
        output.preview(&plan);
        return Err(CliError::infrastructure(
            "the plan changed after it was confirmed; nothing was applied",
            Some(if output.is_json() {
                "Review `cuenv infrastructure plan` and run the command again.".to_string()
            } else {
                "Review the new plan above and run the command again.".to_string()
            }),
            InfrastructureFailureKind::PlanChanged,
        ));
    }
    let mut on_event = |event: ApplyEvent| match event {
        ApplyEvent::Started { address, action } => {
            output.progress(format!("{} {address}: applying...", action.symbol()));
        }
        ApplyEvent::Finished { address, .. } => output.progress(format!("  {address}: done")),
        ApplyEvent::Warning(warning) => {
            cuenv_events::emit_stderr!(format!("warning: {warning}"));
        }
    };
    let applied = engine
        .apply(
            &plan,
            ApplyContext {
                lock: run.lock,
                cancellation: interrupts.cancellation(),
            },
            &mut on_event,
        )
        .await
        .map_err(|error| failure(&error))?;
    output.converged(&Converged {
        mode,
        plan: &plan,
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
