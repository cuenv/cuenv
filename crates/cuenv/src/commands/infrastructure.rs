//! Implementation of `cuenv infrastructure` (short form `cuenv i`).
//!
//! Evaluates the project's `infrastructure` block and hands it to
//! `cuenv-infrastructure`, which drives Terraform provider plugins over gRPC
//! and stores each managed resource in the configured Turso database. State
//! is keyed by the CUE module path and the project name.
//!
//! Converging runs plan without the lock, ask for confirmation, then take the
//! lock and plan again; they refuse to apply if the second plan differs from
//! the one the operator confirmed. While a run holds the lock it owns
//! interrupt handling: the first Ctrl-C or SIGTERM stops after the resource
//! in flight and releases the lock, a second exits immediately.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cuenv_core::InstanceKind;
use cuenv_core::manifest::Project;
use cuenv_events::{emit_stderr, emit_stdout};
use cuenv_infrastructure::{
    ApplyContext, ApplyEvent, Cancellation, EngineOptions, EngineSetup, InfrastructureEngine,
    InfrastructureError, Plan, PlanMode, StateLock, StateStore, TenantKey, TursoConfiguration,
    TursoStateStore,
};
use cuenv_manifest::manifest::Infrastructure;

use super::{CommandExecutor, relative_path_from_root};
use crate::cli::{CliError, InfrastructureLockState, OutputFormat};

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

/// Set while a converging run owns interrupt handling; `main` then leaves
/// Ctrl-C to this command instead of abandoning it mid-apply.
static INTERRUPTS_OWNED: AtomicBool = AtomicBool::new(false);

/// Whether the running infrastructure command handles interrupts itself.
#[must_use]
pub fn command_owns_interrupts() -> bool {
    INTERRUPTS_OWNED.load(Ordering::SeqCst)
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
    let locked = if matches!(error, InfrastructureError::Locked { .. }) {
        InfrastructureLockState::HeldElsewhere
    } else {
        InfrastructureLockState::NotLocked
    };
    CliError::infrastructure(error.to_string(), help, locked)
}

/// Evaluated inputs for one run.
struct Target {
    tenant: TenantKey,
    infrastructure: Infrastructure,
    project_directory: std::path::PathBuf,
}

/// Execute `cuenv infrastructure`.
///
/// # Errors
///
/// Returns an error if evaluation fails, the project has no `infrastructure`
/// block, its name is not unique in the module, the state store is
/// unreachable or locked, or a provider fails.
#[tracing::instrument(skip_all, fields(path = %options.path, action = ?options.action))]
pub async fn execute_infrastructure(
    options: &InfrastructureOptions,
    executor: &CommandExecutor,
) -> Result<(), CliError> {
    let target = evaluate(options, executor)?;
    let store = open_store(&target.infrastructure)
        .await
        .map_err(|error| failure(&error))?;
    let engine_options = EngineOptions {
        project_directory: target.project_directory.clone(),
        plugin_cache_directory: None,
        withheld_environment_variables: vec![
            target
                .infrastructure
                .state
                .turso
                .authentication_token_environment_variable
                .clone(),
        ],
        unrecorded_directory: None,
        cancellation: Cancellation::default(),
    };
    let setup = EngineSetup {
        tenant: target.tenant.clone(),
        store: Arc::clone(&store),
        infrastructure: target.infrastructure,
        options: engine_options,
    };

    match options.action.clone() {
        InfrastructureAction::State => show_state(store.as_ref(), &target.tenant, options.output)
            .await
            .map_err(|error| failure(&error)),
        InfrastructureAction::Unlock { lock_identifier } => {
            unlock(store.as_ref(), &target.tenant, lock_identifier.as_deref())
                .await
                .map_err(|error| failure(&error))
        }
        InfrastructureAction::Plan => {
            let mut engine = InfrastructureEngine::new(setup);
            let result = engine.plan(PlanMode::Apply).await;
            engine.shutdown().await;
            let plan = result.map_err(|error| failure(&error))?;
            print_plan(&plan, options.output);
            Ok(())
        }
        InfrastructureAction::Apply { confirmation } => {
            converge(setup, PlanMode::Apply, confirmation, options.output).await
        }
        InfrastructureAction::Destroy { confirmation } => {
            converge(setup, PlanMode::Destroy, confirmation, options.output).await
        }
    }
}

fn evaluate(
    options: &InfrastructureOptions,
    executor: &CommandExecutor,
) -> Result<Target, CliError> {
    let target_path = Path::new(&options.path).canonicalize().map_err(|error| {
        CliError::config(format!("cannot resolve path {}: {error}", options.path))
    })?;
    // Evaluate the target on its own first so its errors are reported
    // exactly. The module guard is not Send; extract everything before awaiting.
    let (project, module_root, relative_path) = {
        let module = executor.get_module(&target_path).map_err(CliError::from)?;
        let relative_path = relative_path_from_root(&module.root, &target_path);
        let instance = module.get(&relative_path).ok_or_else(|| {
            CliError::config(format!(
                "No CUE instance found at path: {}",
                target_path.display()
            ))
        })?;
        let project: Project = instance.deserialize().map_err(CliError::from)?;
        (project, module.root.clone(), relative_path)
    };
    let infrastructure = project.infrastructure.clone().ok_or_else(|| {
        CliError::config(format!(
            "project '{}' has no `infrastructure` block",
            project.name
        ))
    })?;

    // State is keyed by module path and project name, so two projects with
    // the same name in one module would share (and delete) each other's
    // resources. Refuse rather than guess; this needs every instance in the
    // module, not just the target.
    let duplicates: Vec<String> = {
        let workspace = executor
            .discover_all_modules(&target_path)
            .map_err(CliError::from)?;
        workspace
            .instances
            .iter()
            .filter(|(path, other)| {
                **path != relative_path
                    && other.kind == InstanceKind::Project
                    && other.value.get("name").and_then(serde_json::Value::as_str)
                        == Some(project.name.as_str())
            })
            .map(|(path, _)| path.display().to_string())
            .collect()
    };
    if !duplicates.is_empty() {
        return Err(CliError::config_with_help(
            format!(
                "project name '{}' is also used by {} in this CUE module; infrastructure state is \
                 keyed by module path and project name, so they would share state",
                project.name,
                duplicates.join(", ")
            ),
            "Give each project in the module a unique `name`.",
        ));
    }

    let module_path =
        cuenv_infrastructure::read_module_path(&module_root).map_err(|error| failure(&error))?;
    let tenant = TenantKey::new(module_path, project.name).map_err(|error| failure(&error))?;
    Ok(Target {
        tenant,
        infrastructure,
        project_directory: target_path,
    })
}

async fn open_store(
    infrastructure: &Infrastructure,
) -> cuenv_infrastructure::Result<Arc<dyn StateStore>> {
    let turso = &infrastructure.state.turso;
    let variable = &turso.authentication_token_environment_variable;
    let authentication_token = std::env::var(variable)
        .ok()
        .filter(|token| !token.is_empty());
    if authentication_token.is_none() {
        emit_stderr!(format!(
            "warning: {variable} is not set; connecting to Turso without an authentication token"
        ));
    }
    let store: Arc<dyn StateStore> = Arc::new(TursoStateStore::new(TursoConfiguration {
        url: turso.url.clone(),
        authentication_token,
    })?);
    store.migrate().await?;
    Ok(store)
}

async fn show_state(
    store: &dyn StateStore,
    tenant: &TenantKey,
    output: OutputFormat,
) -> cuenv_infrastructure::Result<()> {
    let resources = store.list(tenant).await?;
    if output.is_json() {
        let rows: Vec<serde_json::Value> = resources
            .iter()
            .map(|resource| {
                serde_json::json!({
                    "address": resource.address.to_string(),
                    "provider": resource.provider,
                    "providerSource": resource.provider_source,
                    "schemaVersion": resource.schema_version,
                    "tainted": resource.tainted,
                })
            })
            .collect();
        print_json(&serde_json::json!({"tenant": tenant.to_string(), "resources": rows}));
        return Ok(());
    }
    if resources.is_empty() {
        emit_stdout!(format!("No managed resources for {tenant}"));
        return Ok(());
    }
    emit_stdout!(format!("{:<40} {:<12} {}", "ADDRESS", "PROVIDER", "SOURCE"));
    for resource in resources {
        let marker = if resource.tainted { " (tainted)" } else { "" };
        emit_stdout!(format!(
            "{:<40} {:<12} {}{marker}",
            resource.address.to_string(),
            resource.provider,
            resource.provider_source
        ));
    }
    Ok(())
}

async fn unlock(
    store: &dyn StateStore,
    tenant: &TenantKey,
    lock_identifier: Option<&str>,
) -> cuenv_infrastructure::Result<()> {
    let Some(current) = store.current_lock(tenant).await? else {
        emit_stdout!(format!("{tenant} is not locked"));
        return Ok(());
    };
    let Some(lock_identifier) = lock_identifier else {
        emit_stdout!(format!(
            "{tenant} is locked by '{}' since {} (lock {}).\n\
             Confirm that run is gone, then release it with \
             `cuenv infrastructure unlock {}`.",
            current.holder, current.acquired_at, current.lock_identifier, current.lock_identifier
        ));
        return Ok(());
    };
    if store.force_unlock(tenant, lock_identifier).await? {
        emit_stdout!(format!(
            "Released lock {lock_identifier} held by '{}' on {tenant}",
            current.holder
        ));
        Ok(())
    } else {
        Err(InfrastructureError::configuration(format!(
            "{tenant} is locked by {}, not {lock_identifier}; nothing was released",
            current.lock_identifier
        )))
    }
}

fn print_plan(plan: &Plan, output: OutputFormat) {
    for warning in &plan.warnings {
        emit_stderr!(format!("warning: {warning}"));
    }
    if output.is_json() {
        let summary = plan.summary();
        let changes: Vec<serde_json::Value> = plan
            .changes
            .iter()
            .map(|change| {
                serde_json::json!({
                    "address": change.address.to_string(),
                    "action": format!("{:?}", change.action).to_lowercase(),
                    "requiresReplace": change.requires_replace,
                })
            })
            .collect();
        print_json(&serde_json::json!({
            "tenant": plan.tenant.to_string(),
            "changes": changes,
            "summary": {
                "create": summary.create,
                "update": summary.update,
                "replace": summary.replace,
                "delete": summary.delete,
                "unchanged": summary.unchanged,
            },
        }));
        return;
    }
    emit_stdout!(format!("cuenv infrastructure: {}", plan.tenant));
    if plan.has_changes() {
        emit_stdout!(cuenv_infrastructure::render_plan(plan));
    } else {
        emit_stdout!("No changes. Infrastructure matches the configuration.");
    }
}

/// Print a JSON result in cuenv's standard success envelope.
fn print_json(payload: &serde_json::Value) {
    let envelope = crate::cli::OkEnvelope::new(payload);
    match serde_json::to_string(&envelope) {
        Ok(json) => cuenv_events::println_redacted(&json),
        Err(error) => emit_stderr!(format!("error: could not serialize JSON output: {error}")),
    }
}

fn lock_holder(mode: PlanMode) -> String {
    let command = match mode {
        PlanMode::Apply => "apply",
        PlanMode::Destroy => "destroy",
    };
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown user".to_string());
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown host".to_string());
    let continuous_integration = match (
        std::env::var("GITHUB_SERVER_URL"),
        std::env::var("GITHUB_REPOSITORY"),
        std::env::var("GITHUB_RUN_ID"),
    ) {
        (Ok(server), Ok(repository), Ok(run)) => {
            format!(", {server}/{repository}/actions/runs/{run}")
        }
        _ => String::new(),
    };
    format!(
        "cuenv infrastructure {command} by {user} on {host}, process {}{continuous_integration}",
        std::process::id()
    )
}

/// Marks this command as owning interrupts for as long as it lives.
struct InterruptOwnership;

impl InterruptOwnership {
    fn claim() -> Self {
        INTERRUPTS_OWNED.store(true, Ordering::SeqCst);
        Self
    }
}

impl Drop for InterruptOwnership {
    fn drop(&mut self) {
        INTERRUPTS_OWNED.store(false, Ordering::SeqCst);
    }
}

/// First interrupt: stop after the resource in flight. Second: exit now.
fn watch_interrupts(cancellation: Cancellation, lock_identifier: String) {
    tokio::spawn(async move {
        let mut received = 0_u8;
        loop {
            if !next_interrupt().await {
                return;
            }
            received += 1;
            if received == 1 {
                cancellation.stop();
                emit_stderr!(
                    "Interrupted: finishing the resource in flight, recording it and releasing \
                     the lock. Interrupt again to exit immediately."
                );
            } else {
                emit_stderr!(format!(
                    "Exiting immediately. The lock may remain; after checking no run is active, \
                     release it with `cuenv infrastructure unlock {lock_identifier}`."
                ));
                std::process::exit(130);
            }
        }
    });
}

/// Wait for Ctrl-C or, on unix, SIGTERM. Returns `false` if signals cannot
/// be watched.
async fn next_interrupt() -> bool {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut terminate) = signal(SignalKind::terminate()) else {
            return tokio::signal::ctrl_c().await.is_ok();
        };
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.is_ok(),
            received = terminate.recv() => received.is_some(),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.is_ok()
    }
}

async fn converge(
    setup: EngineSetup,
    mode: PlanMode,
    confirmation: ConfirmationPolicy,
    output: OutputFormat,
) -> Result<(), CliError> {
    let tenant = setup.tenant.clone();
    let store = Arc::clone(&setup.store);
    let mut engine = InfrastructureEngine::new(setup);
    let result = converge_with(
        &mut engine,
        &store,
        ConvergeRequest {
            tenant: &tenant,
            mode,
            confirmation,
            output,
        },
    )
    .await;
    engine.shutdown().await;
    result
}

struct ConvergeRequest<'request> {
    tenant: &'request TenantKey,
    mode: PlanMode,
    confirmation: ConfirmationPolicy,
    output: OutputFormat,
}

async fn converge_with(
    engine: &mut InfrastructureEngine,
    store: &Arc<dyn StateStore>,
    request: ConvergeRequest<'_>,
) -> Result<(), CliError> {
    // Plan and confirm without holding the lock, so an unanswered prompt
    // never blocks other runs.
    let preview = engine
        .plan(request.mode)
        .await
        .map_err(|error| failure(&error))?;
    print_plan(&preview, request.output);
    if !preview.has_changes() {
        return Ok(());
    }
    if request.confirmation == ConfirmationPolicy::Prompt && !confirm(request.mode).await? {
        emit_stdout!(match request.mode {
            PlanMode::Apply => "Apply cancelled.",
            PlanMode::Destroy => "Destroy cancelled.",
        });
        return Ok(());
    }

    let lock = store
        .lock(request.tenant, &lock_holder(request.mode))
        .await
        .map_err(|error| failure(&error))?;
    let _ownership = InterruptOwnership::claim();
    let cancellation = engine.cancellation().clone();
    watch_interrupts(cancellation.clone(), lock.lock_identifier.clone());

    let applied = apply_locked(
        engine,
        LockedRun {
            preview: &preview,
            mode: request.mode,
            lock: &lock,
        },
    )
    .await;
    let released = release(store.as_ref(), request.tenant, &lock).await;
    match (applied, released) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(release_error)) => Err(CliError::infrastructure(
            format!("apply succeeded but the lock was NOT released: {release_error}"),
            Some(format!(
                "Release it with `cuenv infrastructure unlock {}`.",
                lock.lock_identifier
            )),
            InfrastructureLockState::NotLocked,
        )),
        (Err(apply_error), Ok(())) => Err(apply_error),
        (Err(apply_error), Err(release_error)) => {
            emit_stderr!(format!(
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
    mode: PlanMode,
    lock: &'run StateLock,
}

async fn apply_locked(
    engine: &mut InfrastructureEngine,
    run: LockedRun<'_>,
) -> Result<(), CliError> {
    let LockedRun {
        preview,
        mode,
        lock,
    } = run;
    // State may have moved between the preview and taking the lock.
    let plan = engine.plan(mode).await.map_err(|error| failure(&error))?;
    if plan.digest() != preview.digest() {
        print_plan(&plan, OutputFormat::from_json_flag(false));
        return Err(CliError::infrastructure(
            "the plan changed after it was confirmed; nothing was applied",
            Some("Review the new plan above and run the command again.".to_string()),
            InfrastructureLockState::NotLocked,
        ));
    }
    let mut on_event = |event: ApplyEvent| match event {
        ApplyEvent::Started { address, action } => {
            emit_stdout!(format!("{} {address}: applying...", action.symbol()));
        }
        ApplyEvent::Finished { address, .. } => emit_stdout!(format!("  {address}: done")),
        ApplyEvent::Refreshed { address } => {
            emit_stdout!(format!("  {address}: stored state refreshed"));
        }
        ApplyEvent::Warning(warning) => emit_stderr!(format!("warning: {warning}")),
    };
    let summary = engine
        .apply(&plan, ApplyContext { lock }, &mut on_event)
        .await
        .map_err(|error| failure(&error))?;
    emit_stdout!(format!(
        "Apply complete: {} created, {} updated, {} replaced, {} deleted.",
        summary.create, summary.update, summary.replace, summary.delete
    ));
    Ok(())
}

async fn release(
    store: &dyn StateStore,
    tenant: &TenantKey,
    lock: &StateLock,
) -> cuenv_infrastructure::Result<()> {
    let mut last_error = None;
    for attempt in 0..3_u32 {
        match store.unlock(tenant, lock).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                tokio::time::sleep(std::time::Duration::from_millis(
                    250 * u64::from(attempt + 1),
                ))
                .await;
            }
        }
    }
    Err(last_error.unwrap_or_else(|| InfrastructureError::state("unlock failed")))
}

async fn confirm(mode: PlanMode) -> Result<bool, CliError> {
    if !std::io::stdin().is_terminal() {
        return Err(CliError::config(
            "refusing to change infrastructure without confirmation: standard input is not a \
             terminal; pass --yes",
        ));
    }
    emit_stdout!(match mode {
        PlanMode::Apply => "Type 'yes' to apply these changes:",
        PlanMode::Destroy => "Type 'yes' to destroy every resource listed above:",
    });
    // A detached thread, so an interrupt at the prompt never waits on a
    // blocking read during shutdown.
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = sender.send(std::io::stdin().read_line(&mut line).map(|_| line));
    });
    tokio::select! {
        answer = receiver => {
            let answer = answer
                .map_err(|_| CliError::other("confirmation prompt closed"))?
                .map_err(|error| CliError::other(format!("failed to read confirmation: {error}")))?;
            Ok(answer.trim() == "yes")
        }
        _ = tokio::signal::ctrl_c() => Ok(false),
    }
}
