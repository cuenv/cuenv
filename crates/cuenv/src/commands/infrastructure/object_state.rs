//! What only object-store (S3) state supports, and the checks that guard a
//! tenant there: `state pull` (every backend), `state restore`,
//! `state rename`, rename detection, the state size check and the migration
//! manifest check.

use std::path::{Path, PathBuf};

use cuenv_events::emit_stderr;
use cuenv_infrastructure::{
    ObjectStateFeatures, ObjectTenant, Plan, RestoreRequest, StateLock, StoredTenant, TenantKey,
    UnrecordedStore, strip_control_characters,
};

use super::action::{ConfirmationPolicy, Identity, RenameSource, SourceEnvironment};
use super::confirm::confirm_operation;
use super::dispatch::CommandContext;
use super::evaluation::{self, NameCheck, Needs, TargetRequest, TopLevel};
use super::failure::failure;
use super::guard::tenant_label;
use super::holder;
use super::invocation::quote_argument;
use super::output::{Output, Pulled, Renamed};
use crate::cli::CliError;

/// The object-store operations of the context's store, or the error for a
/// database backend.
fn features<'context>(
    context: &CommandContext<'context>,
    command: &str,
) -> Result<&'context dyn ObjectStateFeatures, CliError> {
    let store: &'context std::sync::Arc<dyn cuenv_infrastructure::StateStore> = context.store;
    store.object_features().ok_or_else(|| {
        CliError::config_with_help(
            format!("`{command}` needs S3 state; this project keeps its state in a database"),
            "History, restore and rename are features of `infrastructure.state.s3`.",
        )
    })
}

/// `state pull`: the tenant's records, lock and last writer as JSON, built
/// from trait reads so it works on every backend.
pub(super) async fn pull(context: &CommandContext<'_>) -> Result<(), CliError> {
    let tenant = context.tenant;
    let fail =
        |error: cuenv_infrastructure::InfrastructureError| failure(&error, context.invocation);
    let records = context
        .interrupts
        .until_interrupted(context.store.list(tenant))
        .await?
        .map_err(fail)?;
    let writer = context.store.last_writer(tenant).await.map_err(fail)?;
    let lock = context.store.current_lock(tenant).await.map_err(fail)?;
    emit_stderr!(
        "warning: this output holds provider secrets in plaintext; keep it out of tickets, \
         logs and chat"
    );
    context.output.pulled(&Pulled {
        tenant,
        location: context.store.location(tenant),
        records: &records,
        writer: writer.as_ref(),
        lock: lock.as_ref(),
    });
    Ok(())
}

/// What `state restore` restores.
pub(super) struct Restore<'restore> {
    pub(super) unrecorded: &'restore UnrecordedStore,
    pub(super) serial: Option<u64>,
    pub(super) confirmation: ConfirmationPolicy,
}

/// `state restore`: write a history snapshot back, after showing it and
/// asking (unless `--yes`). Refused while unrecorded changes are pending:
/// recording them after a rewind would apply them to the wrong version.
pub(super) async fn restore(
    context: &CommandContext<'_>,
    request: &Restore<'_>,
) -> Result<(), CliError> {
    let features = features(context, "state restore")?;
    let tenant = context.tenant;
    let fail =
        |error: cuenv_infrastructure::InfrastructureError| failure(&error, context.invocation);
    if request.unrecorded.has_pending(tenant).map_err(fail)? {
        return Err(CliError::config_with_help(
            format!(
                "{} has unrecorded changes saved on this machine; refusing to restore",
                tenant_label(tenant)
            ),
            format!(
                "Record them first with `{}`: a restore rewinds the state they were saved \
                 against.",
                context.invocation.command("state recover")
            ),
        ));
    }
    let history = context
        .interrupts
        .until_interrupted(features.history(tenant))
        .await?
        .map_err(fail)?;
    let available: Vec<String> = history
        .iter()
        .map(|entry| entry.serial.to_string())
        .collect();
    let entry = match request.serial {
        Some(serial) => history.iter().find(|entry| entry.serial == serial),
        None => history.last(),
    }
    .ok_or_else(|| {
        CliError::config_with_help(
            format!(
                "{} has no history snapshot{}",
                tenant_label(tenant),
                request
                    .serial
                    .map_or_else(String::new, |serial| format!(" with serial {serial}"))
            ),
            if available.is_empty() {
                "Snapshots are written when a run that changed the state releases its lock, \
                 unless `history: 0` is set."
                    .to_string()
            } else {
                format!("Snapshots kept: {}.", available.join(", "))
            },
        )
    })?;
    Output::restore_preview(tenant, entry, context.store.location(tenant).as_deref());
    if request.confirmation == ConfirmationPolicy::Prompt {
        confirm_operation(
            context,
            "state restore",
            "Type 'yes' to write this version back as the current state:",
        )
        .await?;
    }
    let lock = StateLock::generate();
    let holder = holder::describe("state restore");
    let restored = context
        .interrupts
        .until_interrupted(features.restore(
            tenant,
            &RestoreRequest {
                serial: Some(entry.serial),
                holder: &holder,
                lock: &lock,
            },
        ))
        .await?
        .map_err(fail)?;
    context.output.restored(tenant, &restored);
    Ok(())
}

/// The identity `state rename` moves state from.
fn rename_source(
    context: &CommandContext<'_>,
    source: &RenameSource,
) -> Result<ObjectTenant, CliError> {
    let tenant = context.tenant;
    let environment = match &source.environment {
        SourceEnvironment::Selected => tenant.environment().map(str::to_string),
        SourceEnvironment::Named(name) => Some(name.clone()),
        SourceEnvironment::Unselected => None,
    };
    let project = source
        .project
        .clone()
        .unwrap_or_else(|| tenant.project().to_string());
    let from = match environment {
        Some(environment) => {
            TenantKey::with_environment(tenant.module_path(), project, environment)
        }
        None => TenantKey::try_new(tenant.module_path(), project),
    }
    .map_err(|error| failure(&error, context.invocation))?;
    Ok(ObjectTenant {
        package: source
            .package
            .clone()
            .unwrap_or_else(|| context.instance.package().to_string()),
        tenant: from,
    })
}

/// `state rename`: move the state recorded under another identity of this
/// module to the evaluated project's identity, leaving a tombstone.
pub(super) async fn rename(
    context: &CommandContext<'_>,
    source: &RenameSource,
) -> Result<(), CliError> {
    let features = features(context, "state rename")?;
    let from = rename_source(context, source)?;
    let to = ObjectTenant {
        package: context.instance.package().to_string(),
        tenant: context.tenant.clone(),
    };
    if from == to {
        return Err(CliError::config_with_help(
            format!(
                "nothing to rename: {} is this run's own identity",
                tenant_label(context.tenant)
            ),
            "Name the identity the state is recorded under with --from-project, \
             --from-package, --from-env or --from-no-env.",
        ));
    }
    let holder = holder::describe("state rename");
    context
        .interrupts
        .until_interrupted(features.rename(&from, &to, &holder))
        .await?
        .map_err(|error| failure(&error, context.invocation))?;
    context.output.renamed(&Renamed {
        from: &from,
        to: &to,
        location: context.store.location(context.tenant),
    });
    Ok(())
}

/// What a command does on a probable rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RenameResponse {
    /// Say so and carry on (`plan` only reads).
    Report,
    /// Refuse (`apply`, `destroy`, `import`).
    Refuse,
}

/// The inputs of [`check_rename`].
pub(super) struct RenameCheck<'check> {
    pub(super) project_directory: &'check Path,
    pub(super) identity: Identity,
    pub(super) response: RenameResponse,
}

/// Before a tenant's state is created: look for state written from this
/// directory under an identity the configuration no longer declares, which
/// is most likely this project renamed (its project name, package or
/// environment). A new identity would start empty and create every resource
/// again. Only object stores can list a module's tenants; other backends
/// are not checked.
pub(super) async fn check_rename(
    context: &CommandContext<'_>,
    check: &RenameCheck<'_>,
) -> Result<(), CliError> {
    if check.identity == Identity::New {
        return Ok(());
    }
    let Some(features) = context.store.object_features() else {
        return Ok(());
    };
    let tenant = context.tenant;
    let fail =
        |error: cuenv_infrastructure::InfrastructureError| failure(&error, context.invocation);
    // The common case, an existing tenant, costs one read.
    if features.size(tenant).await.map_err(fail)?.is_some() {
        return Ok(());
    }
    let tenants = context
        .interrupts
        .until_interrupted(features.tenants(tenant.module_path()))
        .await?
        .map_err(fail)?;
    let package = context.instance.package();
    let exists = tenants.iter().any(|stored| {
        stored.package == package
            && stored.project == tenant.project()
            && stored.environment.as_deref() == tenant.environment()
    });
    if exists {
        return Ok(());
    }
    let directory = context.instance.directory();
    let mut renamed = Vec::new();
    for stored in tenants.iter().filter(|stored| {
        (stored.records > 0 || stored.locked)
            && stored
                .last_writer
                .as_ref()
                .is_some_and(|writer| writer.directory() == directory)
    }) {
        if !declared(context, check.project_directory, stored).await? {
            renamed.push(stored);
        }
    }
    let Some(first) = renamed.first() else {
        return Ok(());
    };
    match check.response {
        RenameResponse::Report => {
            for stored in &renamed {
                emit_stderr!(format!(
                    "note: probable rename: {}; `apply` will refuse to start {} empty",
                    describe_stored(stored),
                    tenant_label(tenant)
                ));
            }
            Ok(())
        }
        RenameResponse::Refuse => Err(rename_refusal(context, first, renamed.len())),
    }
}

/// A stored tenant for messages.
fn describe_stored(stored: &StoredTenant) -> String {
    format!(
        "{} record(s){} of project '{}' (package '{}', {}) at {}, last written from this \
         directory",
        stored.records,
        if stored.locked {
            " and a held lock"
        } else {
            ""
        },
        strip_control_characters(&stored.project),
        strip_control_characters(&stored.package),
        stored.environment.as_deref().map_or_else(
            || "without --env".to_string(),
            |environment| format!("environment '{}'", strip_control_characters(environment))
        ),
        strip_control_characters(&stored.location)
    )
}

/// The refusal for a probable rename, with the ways out.
fn rename_refusal(context: &CommandContext<'_>, stored: &StoredTenant, count: usize) -> CliError {
    let mut flags = Vec::new();
    if stored.project != context.tenant.project() {
        flags.push(format!(
            "--from-project {}",
            quote_argument(&stored.project)
        ));
    }
    if stored.package != context.instance.package() {
        flags.push(format!(
            "--from-package {}",
            quote_argument(&stored.package)
        ));
    }
    if stored.environment.as_deref() != context.tenant.environment() {
        flags.push(stored.environment.as_deref().map_or_else(
            || "--from-no-env".to_string(),
            |environment| format!("--from-env {}", quote_argument(environment)),
        ));
    }
    let rename = context
        .invocation
        .command(&format!("state rename {}", flags.join(" ")));
    let more = if count > 1 {
        format!(" ({} such identities; the first is shown)", count)
    } else {
        String::new()
    };
    CliError::config_with_help(
        format!(
            "refusing to start {} empty: this directory's state is recorded under an identity \
             the configuration no longer declares: {}{more}",
            tenant_label(context.tenant),
            describe_stored(stored)
        ),
        format!(
            "If this is the same project renamed, move its state: `{rename}`. To retire the old \
             identity, forget its records with `state remove <address>` selecting it, or declare \
             it again and destroy it{}. If this is a separate project on purpose, run once with \
             --new-identity.",
            if stored.locked {
                "; release its lock with `unlock`"
            } else {
                ""
            }
        ),
    )
}

/// Whether the configuration still declares `stored`. In this run's
/// package that is answered by this run's own evaluation; another package
/// in the same directory is evaluated, and one that fails to evaluate is a
/// refusal, never taken as an answer.
async fn declared(
    context: &CommandContext<'_>,
    project_directory: &Path,
    stored: &StoredTenant,
) -> Result<bool, CliError> {
    if stored.package == context.instance.package() {
        return Ok(stored.project == context.tenant.project()
            && environment_declared(
                &context.siblings.declared_environments,
                context.siblings.top_level,
                stored.environment.as_deref(),
            ));
    }
    if !declares_package(project_directory, &stored.package) {
        return Ok(false);
    }
    let directory = project_directory.to_path_buf();
    let package = stored.package.clone();
    let evaluated = tokio::task::spawn_blocking(move || evaluate_package(&directory, &package))
        .await
        .map_err(|error| CliError::other(format!("evaluating another package failed: {error}")))?;
    match evaluated {
        Ok(Some(target)) => Ok(target.tenant.project() == stored.project
            && environment_declared(
                &target.declared_environments,
                target.top_level,
                stored.environment.as_deref(),
            )),
        Ok(None) => Ok(false),
        Err(error) => Err(CliError::config_with_help(
            format!(
                "cannot tell whether package '{}' in this directory still declares project '{}': \
                 it does not evaluate: {}",
                strip_control_characters(&stored.package),
                strip_control_characters(&stored.project),
                error.message()
            ),
            "Fix that package, or run once with --new-identity if this project is a separate \
             one on purpose.",
        )),
    }
}

/// Evaluate `package` in `directory`; `None` when it declares no project
/// with infrastructure.
fn evaluate_package(
    directory: &Path,
    package: &str,
) -> Result<Option<evaluation::Target>, CliError> {
    let path = directory.to_string_lossy().to_string();
    match evaluation::evaluate(TargetRequest {
        path: &path,
        package,
        name_check: NameCheck::TargetOnly,
        needs: Needs::StateOnly,
        environment: None,
    }) {
        Ok(target) => Ok(Some(target)),
        Err(error) if error.message().contains("has no `infrastructure` block") => Ok(None),
        Err(error) => Err(error),
    }
}

/// Whether a CUE file in `directory` declares `package`.
fn declares_package(directory: &Path, package: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return true;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path: &PathBuf| path.extension().is_some_and(|extension| extension == "cue"))
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .any(|text| package_clause(&text) == Some(package))
}

/// The package clause of a CUE file: the first `package` line outside
/// comments.
fn package_clause(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//") && !line.starts_with('@'))
        .find_map(|line| line.strip_prefix("package "))
        .map(str::trim)
}

/// Whether `environment` is declared: a named one by
/// `infrastructure.environments`, the identity without `--env` by a
/// top-level configuration.
fn environment_declared(
    declared: &[String],
    top_level: TopLevel,
    environment: Option<&str>,
) -> bool {
    environment.map_or_else(
        || top_level.providers + top_level.resources + top_level.data_sources > 0,
        |name| declared.iter().any(|declared| declared == name),
    )
}

/// Refuse an apply that creates resources when the tenant's state object
/// is at 90% of the size an object store writes: the next writes would
/// fail half way through.
pub(super) async fn refuse_full_state(
    context: &CommandContext<'_>,
    plan: &Plan,
) -> Result<(), CliError> {
    let Some(features) = context.store.object_features() else {
        return Ok(());
    };
    let summary = plan.summary();
    if summary.create + summary.replace == 0 {
        return Ok(());
    }
    let Some(size) = features
        .size(context.tenant)
        .await
        .map_err(|error| failure(&error, context.invocation))?
    else {
        return Ok(());
    };
    if size.bytes.saturating_mul(10) < size.limit.saturating_mul(9) {
        return Ok(());
    }
    Err(CliError::config_with_help(
        format!(
            "the state of {} is {} bytes, within 10% of the {}-byte limit; refusing a run that \
             creates resources",
            tenant_label(context.tenant),
            size.bytes,
            size.limit
        ),
        "Split the project's resources across projects or environments, or remove records \
         that are no longer needed with `state remove`.",
    ))
}

/// Refuse a run whose state a migration has not copied yet: its tenant is
/// still in the source backend, listed in the project's migration manifest.
pub(super) async fn refuse_unmigrated(context: &CommandContext<'_>) -> Result<(), CliError> {
    let Some(features) = context.store.object_features() else {
        return Ok(());
    };
    let awaiting = features
        .awaiting_migration(context.tenant)
        .await
        .map_err(|error| failure(&error, context.invocation))?;
    if !awaiting {
        return Ok(());
    }
    Err(CliError::config_with_help(
        format!(
            "the state of {} has not been migrated to {} yet",
            tenant_label(context.tenant),
            context
                .store
                .location(context.tenant)
                .unwrap_or_else(|| "this backend".to_string())
        ),
        format!(
            "Put the previous backend back in `infrastructure.state`, name this one in \
             `migrateStateTo`, and run `{}`.",
            context.invocation.command("state migrate")
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_package_clause_skips_comments_and_attributes() {
        assert_eq!(
            package_clause("// header\n@extern(embed)\n\npackage ops\n\nname: \"x\""),
            Some("ops")
        );
        assert_eq!(package_clause("name: \"x\""), None);
    }

    #[test]
    fn the_identity_without_env_is_declared_by_a_top_level_configuration() {
        let top = |resources| TopLevel {
            providers: 0,
            resources,
            data_sources: 0,
        };
        assert!(environment_declared(&[], top(1), None));
        assert!(!environment_declared(&[], top(0), None));
        assert!(environment_declared(&["Prod".into()], top(0), Some("Prod")));
        assert!(!environment_declared(
            &["Prod".into()],
            top(1),
            Some("prod")
        ));
    }
}
