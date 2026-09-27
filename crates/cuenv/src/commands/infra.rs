//! Implementation of `cuenv infra`.
//!
//! Evaluates the project's `infra` block and hands it to `cuenv-infra`,
//! which drives Terraform provider plugins over gRPC and stores each managed
//! resource in the configured Turso database. State is keyed by the CUE
//! module path and the project name.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;

use cuenv_core::manifest::Project;
use cuenv_events::{emit_stderr, emit_stdout};
use cuenv_infra::{
    ApplyEvent, EngineOptions, InfraEngine, InfraError, PlanMode, StateStore, TenantKey,
    TursoConfig, TursoStateStore,
};
use cuenv_manifest::manifest::Infra;

use super::{CommandExecutor, relative_path_from_root};

/// What `cuenv infra` should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfraAction {
    /// Show the changes an apply would make.
    Plan,
    /// Converge infrastructure on the configuration.
    Apply {
        /// Skip the interactive confirmation.
        auto_approve: bool,
    },
    /// Delete every managed resource of the project.
    Destroy {
        /// Skip the interactive confirmation.
        auto_approve: bool,
    },
    /// List managed resources recorded in state.
    State,
    /// Force-release the project's state lock.
    Unlock,
}

/// Options for `cuenv infra`.
#[derive(Debug, Clone)]
pub struct InfraOptions {
    /// Path to the project directory.
    pub path: String,
    /// CUE package name to evaluate.
    pub package: String,
    /// Action to perform.
    pub action: InfraAction,
}

fn infra_error(err: InfraError) -> cuenv_core::Error {
    match err {
        InfraError::Config(msg) => cuenv_core::Error::configuration(msg),
        other => cuenv_core::Error::execution(other.to_string()),
    }
}

/// Execute `cuenv infra`.
///
/// # Errors
///
/// Returns an error if evaluation fails, the project has no `infra` block,
/// the state store is unreachable or locked, or a provider fails.
pub async fn execute_infra(
    options: &InfraOptions,
    executor: &CommandExecutor,
) -> cuenv_core::Result<()> {
    let target_path = Path::new(&options.path).canonicalize().map_err(|e| {
        cuenv_core::Error::io_with_path(
            "canonicalize path",
            Path::new(&options.path).to_path_buf(),
            e,
        )
    })?;

    // The module guard is not Send; extract everything before awaiting.
    let (project_name, infra, module_root) = {
        let module = executor.get_module(&target_path)?;
        let relative_path = relative_path_from_root(&module.root, &target_path);
        let instance = module.get(&relative_path).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "No CUE instance found at path: {}",
                target_path.display()
            ))
        })?;
        let project: Project = instance.deserialize()?;
        let infra = project.infra.ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "project '{}' has no `infra` block",
                project.name
            ))
        })?;
        (project.name, infra, module.root.clone())
    };

    let module_path = cuenv_infra::read_module_path(&module_root).map_err(infra_error)?;
    let tenant = TenantKey::new(module_path, project_name).map_err(infra_error)?;
    let store = open_store(&infra).await.map_err(infra_error)?;

    match options.action {
        InfraAction::State => show_state(store.as_ref(), &tenant).await,
        InfraAction::Unlock => {
            store.force_unlock(&tenant).await.map_err(infra_error)?;
            emit_stdout!(format!("Released state lock for {tenant}"));
            Ok(())
        }
        InfraAction::Plan => {
            let mut engine = InfraEngine::new(tenant, store, infra, engine_options(&target_path));
            let result = engine.plan(PlanMode::Apply).await;
            engine.shutdown().await;
            let plan = result.map_err(infra_error)?;
            print_plan(&plan);
            Ok(())
        }
        InfraAction::Apply { auto_approve } => {
            converge(ConvergeRequest {
                tenant,
                store,
                infra,
                project_dir: &target_path,
                mode: PlanMode::Apply,
                auto_approve,
            })
            .await
        }
        InfraAction::Destroy { auto_approve } => {
            converge(ConvergeRequest {
                tenant,
                store,
                infra,
                project_dir: &target_path,
                mode: PlanMode::Destroy,
                auto_approve,
            })
            .await
        }
    }
}

async fn open_store(infra: &Infra) -> cuenv_infra::Result<Arc<dyn StateStore>> {
    let turso = &infra.state.turso;
    let auth_token = std::env::var(&turso.auth_token_env).ok();
    if auth_token.is_none() && !turso.url.starts_with("http://") {
        emit_stderr!(format!(
            "warning: {} is not set; connecting to Turso without an auth token",
            turso.auth_token_env
        ));
    }
    let store: Arc<dyn StateStore> = Arc::new(TursoStateStore::new(TursoConfig {
        url: turso.url.clone(),
        auth_token,
    })?);
    store.migrate().await?;
    Ok(store)
}

fn engine_options(project_dir: &Path) -> EngineOptions {
    EngineOptions {
        project_dir: project_dir.to_path_buf(),
        plugin_cache_dir: None,
    }
}

async fn show_state(store: &dyn StateStore, tenant: &TenantKey) -> cuenv_core::Result<()> {
    let resources = store.list(tenant).await.map_err(infra_error)?;
    if resources.is_empty() {
        emit_stdout!(format!("No managed resources for {tenant}"));
        return Ok(());
    }
    emit_stdout!(format!("{:<40} {:<12} {}", "ADDRESS", "PROVIDER", "SOURCE"));
    for resource in resources {
        emit_stdout!(format!(
            "{:<40} {:<12} {}",
            resource.address.to_string(),
            resource.provider,
            resource.provider_source
        ));
    }
    Ok(())
}

fn print_plan(plan: &cuenv_infra::Plan) {
    for warning in &plan.warnings {
        emit_stderr!(format!("warning: {warning}"));
    }
    emit_stdout!(format!("cuenv infra: {}", plan.tenant));
    if plan.has_changes() {
        emit_stdout!(cuenv_infra::render_plan(plan));
    } else {
        emit_stdout!("No changes. Infrastructure matches the configuration.");
    }
}

struct ConvergeRequest<'a> {
    tenant: TenantKey,
    store: Arc<dyn StateStore>,
    infra: Infra,
    project_dir: &'a Path,
    mode: PlanMode,
    auto_approve: bool,
}

async fn converge(request: ConvergeRequest<'_>) -> cuenv_core::Result<()> {
    let holder = format!(
        "cuenv infra {} ({}, pid {})",
        match request.mode {
            PlanMode::Apply => "apply",
            PlanMode::Destroy => "destroy",
        },
        std::env::var("USER").unwrap_or_else(|_| "unknown".to_string()),
        std::process::id()
    );
    let lock = request
        .store
        .lock(&request.tenant, &holder)
        .await
        .map_err(infra_error)?;

    let mut engine = InfraEngine::new(
        request.tenant.clone(),
        Arc::clone(&request.store),
        request.infra,
        engine_options(request.project_dir),
    );
    let result = plan_and_apply(&mut engine, request.mode, request.auto_approve).await;
    engine.shutdown().await;

    let unlocked = request.store.unlock(&request.tenant, &lock).await;
    result?;
    unlocked.map_err(infra_error)
}

async fn plan_and_apply(
    engine: &mut InfraEngine,
    mode: PlanMode,
    auto_approve: bool,
) -> cuenv_core::Result<()> {
    let plan = engine.plan(mode).await.map_err(infra_error)?;
    print_plan(&plan);
    if !plan.has_changes() {
        return Ok(());
    }
    if !auto_approve && !confirm().await? {
        emit_stdout!("Apply cancelled.");
        return Ok(());
    }

    let mut on_event = |event: ApplyEvent| match event {
        ApplyEvent::Started { address, action } => {
            emit_stdout!(format!("{} {address}: applying...", action.symbol()));
        }
        ApplyEvent::Finished { address, .. } => {
            emit_stdout!(format!("  {address}: done"));
        }
        ApplyEvent::Warning(warning) => emit_stderr!(format!("warning: {warning}")),
    };
    let summary = engine
        .apply(&plan, &mut on_event)
        .await
        .map_err(infra_error)?;
    emit_stdout!(format!(
        "Apply complete: {} created, {} updated, {} replaced, {} deleted.",
        summary.create, summary.update, summary.replace, summary.delete
    ));
    Ok(())
}

async fn confirm() -> cuenv_core::Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Err(cuenv_core::Error::configuration(
            "refusing to apply without confirmation: stdin is not a terminal; pass --auto-approve",
        ));
    }
    emit_stdout!("Type 'yes' to apply these changes:");
    let answer = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).map(|_| line)
    })
    .await
    .map_err(|e| cuenv_core::Error::execution(format!("confirmation prompt failed: {e}")))?
    .map_err(|e| cuenv_core::Error::execution(format!("failed to read confirmation: {e}")))?;
    Ok(answer.trim() == "yes")
}
