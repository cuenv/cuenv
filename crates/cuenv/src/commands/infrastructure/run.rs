//! One run of the command: evaluate the target, check it and dispatch it.

use std::collections::BTreeMap;

use cuenv_events::emit_stderr;
use cuenv_infrastructure::{
    D1StateStore, ObjectStateStore, ResourceAddress, TenantKey, TursoStateStore, declared_resource,
    strip_control_characters, validate_configuration,
};
use cuenv_manifest::manifest::{Infrastructure, InfrastructureState, S3State};

use super::action::{InfrastructureAction, InfrastructureOptions, UnlockScope};
use super::confirm::{PromptSetting, TerminalAnswers, refuse_unconfirmable};
use super::dispatch::{CommandContext, EngineInputs, dispatch, locked_providers};
use super::evaluation;
use super::evaluation::{Target, TargetRequest};
use super::failure::failure;
use super::guard::{GuardRequest, SelectionFacts, Siblings, guard_selection};
use super::interrupts::Interrupts;
use super::invocation::Invocation;
use super::output::Output;
use super::resolve::resolve;
use crate::cli::CliError;

pub(super) async fn run(
    options: &InfrastructureOptions,
    output: &Output,
    interrupts: &Interrupts,
) -> Result<(), CliError> {
    let answers = TerminalAnswers;
    let invocation = Invocation::of(options);
    refuse_unconfirmable(
        &options.action,
        &PromptSetting {
            output,
            answers: &answers,
            invocation: &invocation,
        },
    )?;
    let target = evaluate_target(options, interrupts).await?;
    preflight(&options.action, &target, &invocation)?;
    let resolved = resolve(&options.action, &target).await?;
    let facts = SelectionFacts::of(&target, &options.action);
    let Target {
        tenant,
        unselected_tenant,
        instance,
        infrastructure,
        environment,
        declared_environments,
        top_level,
        project_directory,
        ..
    } = target;
    let mut siblings = Siblings {
        unselected_tenant,
        declared_environments,
        selected: environment,
        top_level,
    };
    let tenant = match &options.action {
        InfrastructureAction::Unlock { scope, .. } if scope.is_named() => {
            scoped_tenant(&tenant, scope, &invocation)?
        }
        _ => tenant,
    };
    if let InfrastructureAction::Unlock { scope, .. } = &options.action
        && scope.is_named()
    {
        // The evaluated project's other environments are not this lock's.
        siblings.declared_environments.clear();
        siblings.unselected_tenant = tenant.clone();
    }
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
    let locked_providers = if options.action.uses_provider_environment() {
        locked_providers(&project_directory)?
    } else {
        BTreeMap::new()
    };
    let inputs = EngineInputs {
        infrastructure,
        locked_providers,
        project_directory,
        provider_environment_variables: resolved.provider_environment_variables,
        withheld_environment_variables: resolved.withheld_environment_variables,
        // The user state directory (see `UnrecordedStore`).
        unrecorded_directory: None,
    };
    let request = GuardRequest {
        action: &options.action,
        facts: &facts,
        inputs: &inputs,
    };
    guard_selection(&context, &request).await?;
    dispatch(&options.action, &context, inputs).await
}

/// The tenant `unlock` acts on when it names a project: the evaluated
/// project's module and name, each replaced by what was given, in the
/// selected environment.
pub(super) fn scoped_tenant(
    evaluated: &TenantKey,
    scope: &UnlockScope,
    invocation: &Invocation,
) -> Result<TenantKey, CliError> {
    let module_path = scope
        .module_path
        .as_deref()
        .unwrap_or_else(|| evaluated.module_path());
    let project = scope
        .project
        .clone()
        .unwrap_or_else(|| evaluated.project().to_string());
    match evaluated.environment() {
        Some(environment) => TenantKey::with_environment(module_path, project, environment),
        None => TenantKey::try_new(module_path, project),
    }
    .map_err(|error| failure(&error, invocation))
}

/// The checks that need neither a secret nor the state store, run before
/// either is touched: the operator's warnings, the state URL, and (for
/// commands that start providers) the providers and resources. The engine
/// repeats the last one before planning as defense in depth. The checks of
/// the state identity a run acts on need the state store and run after it is
/// connected ([`guard_selection`]).
pub(super) fn preflight(
    action: &InfrastructureAction,
    target: &Target,
    invocation: &Invocation,
) -> Result<(), CliError> {
    for warning in &target.warnings {
        emit_stderr!(format!("warning: {warning}"));
    }
    validate_state_url(&target.infrastructure)?;
    if action.uses_provider_environment() {
        validate_configuration(&target.infrastructure, target.environment.as_deref())
            .map_err(|error| failure(&error, invocation))?;
    }
    if let InfrastructureAction::Import { address, .. } = action
        && declared_resource(
            &target.infrastructure.resources,
            &parse_resource_address(address)?,
        )
        .is_none()
    {
        return Err(CliError::config(format!(
            "cannot import {}: declare that resource in the selected infrastructure configuration first",
            strip_control_characters(address)
        )));
    }
    Ok(())
}

/// Parse a `type.name` address with the same rules as output references.
pub(super) fn parse_resource_address(address: &str) -> Result<ResourceAddress, CliError> {
    ResourceAddress::parse(address).ok_or_else(|| {
        CliError::config(format!(
            "invalid resource address {}; expected type.name",
            strip_control_characters(address)
        ))
    })
}

/// Validate the state backend's address (the Turso URL, the D1 account and
/// database identifiers, or the S3 bucket, region, endpoint and root) before any secret is resolved: nothing is sent and no
/// client is built, so a mistake never costs a call to a secret provider. (The
/// address is validated again when the store is created.)
pub(super) fn validate_state_url(infrastructure: &Infrastructure) -> Result<(), CliError> {
    match &infrastructure.state {
        InfrastructureState::Turso(turso) => TursoStateStore::validate_url(&turso.url),
        InfrastructureState::D1(d1) => {
            D1StateStore::validate_identifiers(&d1.account_id, &d1.database_id)
        }
        InfrastructureState::S3(s3) => validate_object_location(s3),
    }
    .map_err(|error| failure(&error, &Invocation::default()))?;
    if let Some(target) = &infrastructure.migrate_state_to {
        validate_state_backend(target).map_err(|error| failure(&error, &Invocation::default()))?;
    }
    Ok(())
}

/// Validate a migration target the way [`validate_state_url`] validates the
/// state backend.
fn validate_state_backend(state: &InfrastructureState) -> cuenv_infrastructure::Result<()> {
    match state {
        InfrastructureState::Turso(turso) => TursoStateStore::validate_url(&turso.url),
        InfrastructureState::D1(d1) => {
            D1StateStore::validate_identifiers(&d1.account_id, &d1.database_id)
        }
        InfrastructureState::S3(s3) => validate_object_location(s3),
    }
}

fn validate_object_location(s3: &S3State) -> cuenv_infrastructure::Result<()> {
    ObjectStateStore::validate_location(&super::dispatch::object_location(s3))?;
    if let Some(interval) = &s3.min_write_interval {
        super::dispatch::parse_write_interval(interval)?;
    }
    Ok(())
}

/// Evaluate the target on a thread of its own. CUE evaluation cannot be
/// cancelled, and nothing is held yet, so an interrupt abandons it at once:
/// the thread is not waited for, and the process exits without it.
pub(super) async fn evaluate_target(
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
