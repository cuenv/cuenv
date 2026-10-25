//! Guards that refuse, or warn about, a selection that would touch state
//! recorded for another instance or environment.

use cuenv_events::emit_stderr;
use cuenv_infrastructure::{ResourceAddress, TenantKey, strip_control_characters};

use super::action::{InfrastructureAction, SeparateState, StateAction};
use super::dispatch::{CommandContext, EngineInputs, unrecorded_store};
use super::evaluation::{EnvironmentSelection, Target, TopLevel};
use super::failure::failure;
use super::{evaluation, invocation};
use crate::cli::CliError;

/// The other state identities of the project, besides the one this run acts
/// on: what hints and refusals look at when state exists under a different
/// `--env` than the one given (or the one forgotten).
#[derive(Debug)]
pub(super) struct Siblings {
    /// The project's identity without `--env`.
    pub(super) unselected_tenant: TenantKey,
    /// Every environment the project declares.
    pub(super) declared_environments: Vec<String>,
    /// The environment this run selected.
    pub(super) selected: Option<String>,
    /// What the project declares at the top level, which an environment
    /// replaces entirely.
    pub(super) top_level: TopLevel,
}

impl Siblings {
    /// The identities a run without `--env` should mention besides its own:
    /// one per declared environment. A run with `--env` mentions none.
    pub(super) fn declared(&self) -> Vec<(String, TenantKey)> {
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

/// What the evaluated configuration says that the guards on the state
/// identity of a run need, besides the [`Siblings`].
#[derive(Debug)]
pub(super) struct SelectionFacts {
    /// Whether the selected environment is declared.
    pub(super) selection: EnvironmentSelection,
    /// The addresses the selected configuration manages.
    pub(super) configured: Vec<ResourceAddress>,
}

impl SelectionFacts {
    pub(super) fn of(target: &Target, action: &InfrastructureAction) -> Self {
        let mut configured: Vec<ResourceAddress> = target
            .infrastructure
            .resources
            .iter()
            .map(|(name, declaration)| {
                ResourceAddress::new(declaration.resource_type.clone(), name.clone())
            })
            .collect();
        if let InfrastructureAction::Import { address, .. } = action {
            configured.retain(|resource| resource.to_string() == *address);
        }
        Self {
            selection: target.selection,
            configured,
        }
    }
}

/// What the guards on the state identity of a run look at besides the
/// [`CommandContext`]: what the run does, what the evaluated configuration
/// says, and the engine inputs. (No `Debug`: the engine inputs hold resolved
/// secret values.)
pub(super) struct GuardRequest<'request> {
    pub(super) action: &'request InfrastructureAction,
    pub(super) facts: &'request SelectionFacts,
    pub(super) inputs: &'request EngineInputs,
}

/// The checks on the state identity a run acts on, which need the state
/// store. They run once it is connected and before the command does
/// anything.
///
/// `plan` only reads, so it warns where `apply` and `destroy` refuse.
pub(super) async fn guard_selection(
    context: &CommandContext<'_>,
    request: &GuardRequest<'_>,
) -> Result<(), CliError> {
    match request.action {
        InfrastructureAction::Plan
        | InfrastructureAction::Apply { .. }
        | InfrastructureAction::Import { .. }
        | InfrastructureAction::Destroy { .. } => guard_identity(context, request).await,
        // Whole database, or another project's tenant: nothing of the
        // evaluated project's environments applies.
        InfrastructureAction::State(StateAction::Locks) => Ok(()),
        InfrastructureAction::Unlock { scope, .. } if scope.is_named() => Ok(()),
        InfrastructureAction::State(_) | InfrastructureAction::Unlock { .. } => {
            guard_environment_known(context, request).await
        }
    }
}

/// The addresses recorded for a tenant, read without decoding the records, so
/// a damaged record never hides the others (or the guard itself).
pub(super) async fn recorded_addresses(
    context: &CommandContext<'_>,
    tenant: &TenantKey,
) -> Result<Vec<ResourceAddress>, CliError> {
    context
        .interrupts
        .until_interrupted(context.store.addresses(tenant))
        .await?
        .map_err(|error| failure(&error, context.invocation))
}

/// State is recorded separately for runs with and without `--env`; this
/// keeps a run from acting on the wrong identity.
///
/// - Without `--env`, when the project declares environments but no
///   top-level `resources`, there is nothing to manage: `plan` warns, `apply`
///   refuses, and `destroy` refuses unless state is recorded without `--env`
///   and the top-level `providers` that delete it are still declared (the
///   way out of an old layout).
/// - With `--env`, `apply` would create resources whose addresses the
///   no-flag identity already records: both identities would claim the same
///   real objects, so `apply` refuses (unless `--allow-separate-state`) and
///   `plan` warns. Resources at other addresses are not a conflict, which is
///   what lets top-level resources and environments coexist.
/// - Without `--env`, the same conflict from the other side: `apply` would
///   create resources whose addresses a declared environment already
///   records. Same outcomes.
pub(super) async fn guard_identity(
    context: &CommandContext<'_>,
    request: &GuardRequest<'_>,
) -> Result<(), CliError> {
    match context.tenant.environment() {
        None => guard_unselected_identity(context, request).await,
        Some(environment) => guard_named_identity(context, request, environment).await,
    }
}

/// A finding that stops `apply` and `destroy` and only warns for `plan`.
pub(super) fn refuse_or_warn(
    action: &InfrastructureAction,
    refusal: CliError,
) -> Result<(), CliError> {
    if matches!(action, InfrastructureAction::Plan) {
        emit_stderr!(format!(
            "warning: {}. {}",
            refusal.message(),
            refusal.help().unwrap_or_default()
        ));
        Ok(())
    } else {
        Err(refusal)
    }
}

pub(super) async fn guard_unselected_identity(
    context: &CommandContext<'_>,
    request: &GuardRequest<'_>,
) -> Result<(), CliError> {
    let siblings = context.siblings;
    let action = request.action;
    if siblings.declared_environments.is_empty() {
        return Ok(());
    }
    if siblings.top_level.resource_count > 0 {
        return guard_environment_claims(context, request).await;
    }
    let recorded = recorded_addresses(context, context.tenant).await?;
    let deletable = !recorded.is_empty() && siblings.top_level.provider_count > 0;
    if matches!(action, InfrastructureAction::Destroy { .. }) && deletable {
        return Ok(());
    }
    refuse_or_warn(action, unselected_refusal(context, &recorded))
}

/// What one declared environment already records of what a run without
/// `--env` would create.
#[derive(Debug)]
pub(super) struct EnvironmentClaim {
    pub(super) environment: String,
    /// The configured addresses that environment records, in configuration
    /// order.
    pub(super) addresses: Vec<ResourceAddress>,
}

/// The mirror of [`guard_named_identity`] for a run without `--env` in a
/// project that declares top-level `resources` beside its environments:
/// creating an address a declared environment records would manage one real
/// object under two identities.
pub(super) async fn guard_environment_claims(
    context: &CommandContext<'_>,
    request: &GuardRequest<'_>,
) -> Result<(), CliError> {
    let GuardRequest { action, facts, .. } = request;
    // Only creating can claim an object twice.
    if matches!(action, InfrastructureAction::Destroy { .. }) || facts.configured.is_empty() {
        return Ok(());
    }
    let own = recorded_addresses(context, context.tenant).await?;
    let mut claims = Vec::new();
    for (environment, tenant) in context.siblings.declared() {
        let recorded = recorded_addresses(context, &tenant).await?;
        let addresses: Vec<ResourceAddress> = facts
            .configured
            .iter()
            .filter(|address| !own.contains(address) && recorded.contains(address))
            .cloned()
            .collect();
        if !addresses.is_empty() {
            claims.push(EnvironmentClaim {
                environment,
                addresses,
            });
        }
    }
    if claims.is_empty() {
        return Ok(());
    }
    refuse_warn_or_allow(action, environment_claims_refusal(context, &claims))
}

/// The refusal of a run without `--env` in a project that declares
/// environments and no top-level `resources`. The help names only commands
/// that work from the state recorded now.
pub(super) fn unselected_refusal(
    context: &CommandContext<'_>,
    recorded: &[ResourceAddress],
) -> CliError {
    let siblings = context.siblings;
    let invocation = context.invocation;
    let declared = evaluation::declared_environments_phrase(&siblings.declared_environments);
    let example = invocation
        .with_environment(siblings.declared_environments.first().map(String::as_str))
        .command("plan");
    let select = format!("Select an environment with --env, for example `{example}`.");
    let still_recorded = recorded.first().map(|first| {
        let list = invocation.command("state list");
        let remove = invocation.command(&format!(
            "state remove {}",
            invocation::quote_argument(&first.to_string())
        ));
        let count = recorded.len();
        let destroy = invocation.command("destroy");
        if siblings.top_level.provider_count > 0 {
            format!(
                "{count} resource(s) are still recorded without --env (`{list}` lists them): \
                 delete them with `{destroy}`, or forget them without touching the real objects \
                 with `{remove}`."
            )
        } else {
            format!(
                "{count} resource(s) are still recorded without --env (`{list}` lists them) \
                 and no top-level `providers` are declared to delete them: declare those \
                 providers again and run `{destroy}`, or forget the records without touching the \
                 real objects with `{remove}`."
            )
        }
    });
    let keep = "Declare top-level `resources` if that configuration should still be managed.";
    let help = [Some(select), still_recorded, Some(keep.to_string())]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    CliError::config_with_help(
        format!(
            "this project declares infrastructure environments but no top-level `resources`, \
             so a run without --env has nothing to manage ({declared})"
        ),
        help,
    )
}

pub(super) async fn guard_named_identity(
    context: &CommandContext<'_>,
    request: &GuardRequest<'_>,
    environment: &str,
) -> Result<(), CliError> {
    let GuardRequest { action, facts, .. } = request;
    // Only creating can claim an object twice; destroying a named
    // environment that has nothing recorded changes nothing.
    if matches!(action, InfrastructureAction::Destroy { .. }) || facts.configured.is_empty() {
        return Ok(());
    }
    let unselected = recorded_addresses(context, &context.siblings.unselected_tenant).await?;
    if unselected.is_empty() {
        return Ok(());
    }
    let own = recorded_addresses(context, context.tenant).await?;
    let conflicts: Vec<&ResourceAddress> = facts
        .configured
        .iter()
        .filter(|address| !own.contains(address) && unselected.contains(address))
        .collect();
    if conflicts.is_empty() {
        return Ok(());
    }
    refuse_warn_or_allow(
        action,
        separate_state_refusal(context, environment, &conflicts),
    )
}

/// A conflict between state identities: `apply` refuses unless
/// `--allow-separate-state` was given (then it warns and goes on), `plan`
/// warns, and `destroy` is never given one.
pub(super) fn refuse_warn_or_allow(
    action: &InfrastructureAction,
    refusal: CliError,
) -> Result<(), CliError> {
    match action {
        InfrastructureAction::Apply {
            separate_state: SeparateState::Allow,
            ..
        } => {
            emit_stderr!(format!(
                "warning: {}; continuing because --allow-separate-state was given",
                refusal.message()
            ));
            Ok(())
        }
        _ => refuse_or_warn(action, refusal),
    }
}

/// How many conflicting addresses a message names.
pub(super) const LISTED_ADDRESSES: usize = 3;

/// The refusal of `apply --env NAME` that would create what the identity
/// without `--env` already records.
pub(super) fn separate_state_refusal(
    context: &CommandContext<'_>,
    environment: &str,
    conflicts: &[&ResourceAddress],
) -> CliError {
    let invocation = context.invocation;
    let unselected = invocation.with_environment(None);
    let environment = evaluation::escape_control_characters(environment);
    let mut listed: Vec<String> = conflicts
        .iter()
        .take(LISTED_ADDRESSES)
        .map(|address| strip_control_characters(&address.to_string()))
        .collect();
    if conflicts.len() > LISTED_ADDRESSES {
        listed.push(format!("{} more", conflicts.len() - LISTED_ADDRESSES));
    }
    let (are, them) = if conflicts.len() == 1 {
        ("is", "it")
    } else {
        ("are", "them")
    };
    let remove = unselected.command(&format!(
        "state remove {}",
        invocation::quote_argument(&conflicts[0].to_string())
    ));
    let deletion = if context.siblings.top_level.provider_count > 0 {
        format!(
            "deleting the objects with `{}` (they are deleted) or by ",
            unselected.command("destroy")
        )
    } else {
        String::new()
    };
    CliError::config_with_help(
        format!(
            "environment '{environment}' of {} would create {}, which {are} already recorded \
             for the same project without --env",
            tenant_label(&context.siblings.unselected_tenant),
            listed.join(", ")
        ),
        format!(
            "State is recorded separately for runs with and without --env, so creating {them} \
             under '{environment}' would manage the same real objects twice: both identities \
             would claim them, and the second create can fail or duplicate them. Moving \
             records between the two (`state move`) is not available yet. Keep these resources \
             in the configuration without --env, or free their addresses first, by {deletion}\
             forgetting the records with `{remove}` (the real objects are not touched, so \
             delete or hand them over yourself before creating them under '{environment}'). \
             If '{environment}' should manage separate objects with the same addresses, run \
             `{}`.",
            invocation.command("apply --allow-separate-state")
        ),
    )
}

/// The refusal of `apply` without `--env` that would create what declared
/// environments already record.
pub(super) fn environment_claims_refusal(
    context: &CommandContext<'_>,
    claims: &[EnvironmentClaim],
) -> CliError {
    let invocation = context.invocation;
    let mut addresses: Vec<&ResourceAddress> = Vec::new();
    for address in claims.iter().flat_map(|claim| &claim.addresses) {
        if !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    let mut listed: Vec<String> = addresses
        .iter()
        .take(LISTED_ADDRESSES)
        .map(|address| strip_control_characters(&address.to_string()))
        .collect();
    if addresses.len() > LISTED_ADDRESSES {
        listed.push(format!("{} more", addresses.len() - LISTED_ADDRESSES));
    }
    let environments: Vec<String> = claims
        .iter()
        .map(|claim| {
            format!(
                "'{}'",
                evaluation::escape_control_characters(&claim.environment)
            )
        })
        .collect();
    let (environment_noun, are, them) = match (claims.len(), addresses.len()) {
        (1, 1) => ("environment", "is", "it"),
        (1, _) => ("environment", "are", "them"),
        (_, 1) => ("environments", "is", "it"),
        _ => ("environments", "are", "them"),
    };
    // The first claim is the one the way out names: its environment, and
    // its first address.
    let first = &claims[0];
    let holder = invocation.with_environment(Some(&first.environment));
    let remove = holder.command(&format!(
        "state remove {}",
        invocation::quote_argument(&first.addresses[0].to_string())
    ));
    let environment_phrase = environments.join(", ");
    CliError::config_with_help(
        format!(
            "{} without --env would create {}, which {are} already recorded for \
             {environment_noun} {environment_phrase} of the same project",
            tenant_label(&context.siblings.unselected_tenant),
            listed.join(", "),
        ),
        format!(
            "State is recorded separately for runs with and without --env, so creating {them} \
             without --env would manage the same real objects twice: both identities would \
             claim them, and the second create can fail or duplicate them (a later \
             `destroy --env` would delete what the top-level configuration manages). Moving \
             records between the two (`state move`) is not available yet. Keep these resources \
             in the {environment_noun} {environment_phrase} (declare them there, not at the top \
             level), or free their addresses first, by deleting the objects with `{}` \
             (everything that environment records is deleted) or by forgetting the records with \
             `{remove}` (the real objects are not touched, so delete or hand them over yourself \
             before creating them without --env). If the top-level resources should manage \
             separate objects with the same addresses, run `{}`.",
            holder.command("destroy"),
            invocation.command("apply --allow-separate-state")
        ),
    )
}

/// A state-only command that selected an environment the project does not
/// declare: it may be one removed from the configuration, whose state still
/// exists and can be listed, unlocked and cleaned up, or a mistyped name. It
/// is told apart by the state: warn when the store has rows, a lock, an owner
/// or a saved change for it, refuse (naming the declared environments)
/// otherwise.
pub(super) async fn guard_environment_known(
    context: &CommandContext<'_>,
    request: &GuardRequest<'_>,
) -> Result<(), CliError> {
    let GuardRequest { facts, inputs, .. } = request;
    if facts.selection != EnvironmentSelection::NotDeclared {
        return Ok(());
    }
    let tenant = context.tenant;
    let recorded = recorded_addresses(context, tenant).await?;
    let lock = context
        .interrupts
        .until_interrupted(context.store.current_lock(tenant))
        .await?
        .map_err(|error| failure(&error, context.invocation))?;
    let owner = context
        .interrupts
        .until_interrupted(context.store.owner(tenant))
        .await?
        .map_err(|error| failure(&error, context.invocation))?;
    let saved_changes = unrecorded_store(inputs, context)
        .ok()
        .and_then(|unrecorded| unrecorded.has_pending(tenant).ok())
        .unwrap_or(false);
    let environment = evaluation::escape_control_characters(tenant.environment().unwrap_or(""));
    let declared =
        evaluation::declared_environments_phrase(&context.siblings.declared_environments);
    if !recorded.is_empty() || lock.is_some() || owner.is_some() || saved_changes {
        emit_stderr!(format!(
            "warning: project '{}' no longer declares infrastructure environment '{environment}'; \
             using the state recorded for it ({declared})",
            tenant.project()
        ));
        return Ok(());
    }
    Err(CliError::config_with_help(
        format!(
            "project '{}' has no infrastructure environment named '{environment}', and no \
             state, lock, owner or unrecorded change is recorded for that name",
            tenant.project()
        ),
        format!(
            "Environment names are case-sensitive ({declared}). State recorded for an \
             environment that was removed from the configuration can still be listed, unlocked \
             and removed by selecting it with --env."
        ),
    ))
}

/// A tenant as text for messages: its module path, project and environment,
/// with control characters escaped.
pub(super) fn tenant_label(tenant: &TenantKey) -> String {
    evaluation::escape_control_characters(&tenant.to_string())
}
