//! Planning, importing, converging and applying changes.

use cuenv_events::emit_stderr;
use cuenv_infrastructure::{
    ApplyContext, ApplyEvent, InfrastructureEngine, InfrastructureError, Plan, PlanMode, StateLock,
};

use super::action::ConfirmationPolicy;
use super::confirm::confirm;
use super::dispatch::{CommandContext, EngineInputs, claim_ownership, engine_setup, require_owner};
use super::failure::{failure, printable_text};
use super::interrupts::{Interrupts, interrupted_after_completion};
use super::invocation::Invocation;
use super::lock::under_lock;
use super::output;
use super::output::{Converged, Output};
use super::run::parse_resource_address;
use crate::cli::CliError;

/// `plan`: refresh and plan without the lock.
pub(super) async fn plan(
    context: &CommandContext<'_>,
    inputs: EngineInputs,
) -> Result<(), CliError> {
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

/// The inputs needed to import a provider resource.
pub(super) struct ImportRequest<'command, 'context> {
    pub(super) context: &'command CommandContext<'context>,
    pub(super) inputs: EngineInputs,
    pub(super) address: &'command str,
    pub(super) identifier: &'command str,
}

/// Import a provider resource while holding the tenant lock and after
/// confirming this CUE instance owns the state.
pub(super) async fn import(request: ImportRequest<'_, '_>) -> Result<(), CliError> {
    let ImportRequest {
        context,
        inputs,
        address,
        identifier,
    } = request;
    let target = parse_resource_address(address)?;
    let mut engine = InfrastructureEngine::new(engine_setup(context, inputs));
    let importing = &mut engine;
    let result = under_lock(context, "import", |lock| async move {
        claim_ownership(context, &lock).await?;
        let warnings = importing
            .import(&target, identifier, &lock)
            .await
            .map_err(|error| failure(&error, context.invocation))?;
        context.interrupts.check()?;
        for warning in warnings {
            emit_stderr!(format!("warning: {warning}"));
        }
        context
            .output
            .imported(context.tenant, &target, &context.invocation.command("plan"));
        Ok(())
    })
    .await;
    engine.shutdown().await;
    result
}

/// How a converging command runs.
pub(super) struct Convergence<'run> {
    pub(super) mode: PlanMode,
    pub(super) confirmation: ConfirmationPolicy,
    pub(super) context: &'run CommandContext<'run>,
}

/// `apply` and `destroy`: take the lock, claim ownership, plan, confirm
/// while holding the lock (unless `--yes`), apply exactly that plan.
pub(super) async fn converge(
    convergence: &Convergence<'_>,
    inputs: EngineInputs,
) -> Result<(), CliError> {
    let Convergence { mode, context, .. } = *convergence;
    let mut engine = InfrastructureEngine::new(engine_setup(context, inputs));
    let planning = &mut engine;
    let result = under_lock(context, output::operation_name(mode), |lock| async move {
        claim_ownership(context, &lock).await?;
        let plan = planning
            .plan(mode)
            .await
            .map_err(|error| failure(&error, context.invocation))?;
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
            interrupts: context.interrupts,
        })
        .await
    })
    .await;
    engine.shutdown().await;
    result
}

/// A plan to apply under a held lock.
pub(super) struct Application<'application> {
    pub(super) engine: &'application mut InfrastructureEngine,
    pub(super) plan: &'application Plan,
    pub(super) lock: &'application StateLock,
    pub(super) mode: PlanMode,
    pub(super) output: &'application Output,
    pub(super) invocation: &'application Invocation,
    pub(super) interrupts: &'application Interrupts,
}

/// Apply a plan made under the lock, when it has any work.
pub(super) async fn apply_plan(application: Application<'_>) -> Result<(), CliError> {
    let Application {
        engine,
        plan,
        lock,
        mode,
        output,
        invocation,
        interrupts,
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
            emit_stderr!(format!("warning: {}", printable_text(&warning)));
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
    let converged = Converged {
        mode,
        plan,
        applied: Some(applied),
    };
    // The last operation may have been in flight when the signal arrived: it
    // finished and was recorded, but the run was interrupted and says so (the
    // exit code is 130, like any interrupted run), still reporting what was
    // applied.
    if interrupts.is_requested() {
        return Err(
            interrupted_after_completion(&output::applied_phrase(&converged), invocation)
                .with_detail("result", output::converged_json(&converged)),
        );
    }
    output.converged(&converged);
    Ok(())
}

/// Addresses of the replacements a failed apply deleted (the first half of a
/// replacement) and did not recreate: the engine's own list when the apply
/// ended incomplete, plus every one it reported while running (the only
/// source when the run was interrupted or failed fatally). Each address once.
pub(super) fn deleted_not_recreated(
    error: &InfrastructureError,
    reported: &[String],
) -> Vec<String> {
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
