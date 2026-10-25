//! Resolving the state store, its credential and the project variables.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use cuenv_infrastructure::StateStore;
use cuenv_manifest::environment::EnvValue;
use cuenv_manifest::manifest::{
    InfrastructurePolicyAction, InfrastructureState, ProviderEnvironment, StateToken,
};

use super::action::InfrastructureAction;
use super::dispatch::connect;
use super::evaluation::Target;
use super::failure::failure;
use super::invocation::Invocation;
use super::provider_environment;
use super::provider_environment::ProviderEnvironmentInputs;
use crate::cli::CliError;

/// What running the command needs from the environment and the state store.
pub(super) struct Resolved {
    pub(super) store: Arc<dyn StateStore>,
    /// Resolved project variables overlaid on the provider's host environment.
    pub(super) provider_environment_variables: BTreeMap<String, String>,
    /// Names removed from the provider's environment after the overlay.
    pub(super) withheld_environment_variables: Vec<String>,
}

/// Resolve the project's environment variables the action may use, connect
/// to the state store, and decide what providers inherit.
pub(super) async fn resolve(
    action: &InfrastructureAction,
    target: &Target,
    invocation: &Invocation,
) -> Result<Resolved, CliError> {
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
    let state = &target.infrastructure.state;
    let token_variable = state
        .token()
        .and_then(StateToken::passthrough_variable)
        .map(str::to_string);
    // Only the state backend's token is resolved before the state store is
    // asked who owns the project: a secret is a command or a call to a secret
    // provider, and nothing should run for a run the owner check refuses.
    let credential = resolve_state_token(policy_name, state).await?;
    let store = connect(&target.infrastructure, credential)
        .map_err(|error| failure(&error, &Invocation::default()))?;
    if action.uses_provider_environment() {
        refuse_another_owner(store.as_ref(), target, invocation).await?;
    }
    let (resolved_variables, secrets) = resolve_environment_variables(
        policy_name,
        &environment_variables_for_action(action, &project_environment_variables),
    )
    .await?;
    cuenv_events::register_secrets(secrets);
    // A project variable named like the token's passthrough variable never
    // reaches a provider either.
    let mut provider_environment_variables: BTreeMap<String, String> = resolved_variables
        .into_iter()
        .filter(|(name, _)| Some(name) != token_variable.as_ref())
        .collect();
    let provided: Vec<String> = provider_environment_variables.keys().cloned().collect();
    // The remote cache's credentials are named by the project's configuration.
    // Their values are secrets cuenv holds: redact them, and (below) keep
    // them from providers unless the project passes them.
    cuenv_events::register_secrets(
        target
            .remote_cache_credential_variables
            .iter()
            .filter_map(|name| std::env::var(name).ok())
            .filter(|value| !value.is_empty()),
    );
    let mut withheld_environment_variables = if action.uses_provider_environment() {
        if target.provider_environment == ProviderEnvironment::Isolated {
            // Proxy URLs carrying credentials reach isolated providers
            // without them (an overlay replaces the inherited value).
            provider_environment_variables.extend(provider_environment::isolated_proxy_overlay(
                std::env::vars_os(),
                &provided,
            ));
        }
        provider_environment::withheld_environment_variables(&ProviderEnvironmentInputs {
            mode: target.provider_environment,
            ambient: std::env::vars_os().map(|(name, _)| name).collect(),
            provided: &provided,
            policy_withheld: &policy_withheld,
            configured_credentials: &target.remote_cache_credential_variables,
            token_variable: token_variable.as_deref(),
        })
    } else {
        Vec::new()
    };
    // Withheld whatever the action, so an engine never hands it on.
    withheld_environment_variables.extend(token_variable);
    withheld_environment_variables.sort();
    withheld_environment_variables.dedup();
    Ok(Resolved {
        store,
        provider_environment_variables,
        withheld_environment_variables,
    })
}

/// Refuse a project whose state another CUE instance owns, before any of the
/// project's secrets is resolved. Reads only; the owner is checked again under
/// the lock where the run writes.
pub(super) async fn refuse_another_owner(
    store: &dyn StateStore,
    target: &Target,
    invocation: &Invocation,
) -> Result<(), CliError> {
    let owner = store
        .owner(&target.tenant)
        .await
        .map_err(|error| failure(&error, invocation))?;
    owner.map_or(Ok(()), |owner| {
        owner
            .require(&target.tenant, &target.instance)
            .map_err(|error| failure(&error, invocation))
    })
}

/// Resolve the variables the policy of `action` allows, one resolution per
/// variable and all at once, so a failure names the variable it concerns.
/// Returns the values and every secret part, for redaction.
pub(super) async fn resolve_environment_variables(
    action: InfrastructurePolicyAction,
    variables: &HashMap<String, EnvValue>,
) -> Result<(HashMap<String, String>, Vec<String>), CliError> {
    let mut resolutions = tokio::task::JoinSet::new();
    for (name, value) in variables
        .iter()
        .filter(|(_, value)| value.is_accessible_by_infrastructure(action))
    {
        let (name, value) = (name.clone(), value.clone());
        resolutions.spawn(async move {
            let single = HashMap::from([(name.clone(), value)]);
            let resolved =
                cuenv_core::environment::Environment::resolve_for_infrastructure_with_secrets(
                    action, &single,
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
pub(super) fn secret_failure(variable: &str, error: cuenv_core::Error) -> CliError {
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

/// The state backend's token, resolved, and what to say when there is none.
#[derive(Debug)]
pub(super) struct StateCredential {
    pub(super) token: Option<String>,
    /// Why `token` is `None`, naming the configuration field.
    pub(super) absence: String,
}

/// Resolve the state token: read a passthrough variable from the cuenv
/// process environment, or resolve a literal, secret or interpolation like a
/// project environment variable. Every resolved secret part and the token
/// itself are registered for redaction before anything is connected.
pub(super) async fn resolve_state_token(
    action: InfrastructurePolicyAction,
    state: &InfrastructureState,
) -> Result<StateCredential, CliError> {
    let field = state.token_field().to_string();
    let (token, absence) = match state.token() {
        None => (None, format!("no state token is configured (`{field}`)")),
        Some(StateToken::Passthrough { name }) => (
            std::env::var(name).ok(),
            format!("`{field}` reads {name}, which is not set or is empty"),
        ),
        Some(StateToken::Value(value)) => {
            let single = HashMap::from([(field.clone(), value.clone())]);
            let (mut values, secrets) =
                cuenv_core::environment::Environment::resolve_for_infrastructure_with_secrets(
                    action, &single,
                )
                .await
                .map_err(|error| state_token_failure(&field, error))?;
            cuenv_events::register_secrets(secrets);
            (
                values.remove(&field),
                format!("`{field}` resolved to an empty value"),
            )
        }
    };
    let token = token.filter(|token| !token.is_empty());
    // Redact the token from every output from here on, including provider
    // and store errors that might echo it.
    if let Some(token) = &token {
        cuenv_events::register_secret(token.clone());
    }
    Ok(StateCredential { token, absence })
}

/// The error for a state token whose secret could not be resolved, a failure
/// of the secret provider like [`secret_failure`].
pub(super) fn state_token_failure(field: &str, error: cuenv_core::Error) -> CliError {
    let message = CliError::from(error).message().to_string();
    let reason = message
        .strip_prefix("Failed to resolve secret 'secret': ")
        .unwrap_or(&message);
    CliError::eval_with_help(
        format!("cannot resolve the state token `{field}`: {reason}"),
        format!(
            "Check the configuration and credentials of the secret provider behind `{field}` \
             (1Password, AWS, Vault, a command...)."
        ),
    )
}

/// The project variables an action resolves: all of them for an action that
/// starts providers, none for a state-only action.
pub(super) fn environment_variables_for_action(
    action: &InfrastructureAction,
    project_environment: &HashMap<String, EnvValue>,
) -> HashMap<String, EnvValue> {
    if action.uses_provider_environment() {
        project_environment.clone()
    } else {
        HashMap::new()
    }
}
