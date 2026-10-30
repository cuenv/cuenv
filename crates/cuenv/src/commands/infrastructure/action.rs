//! The actions `cuenv infrastructure` performs and the options it runs with.

use cuenv_infrastructure::RecoverOverrides;
use cuenv_manifest::manifest::InfrastructurePolicyAction;

use super::evaluation::{NameCheck, Needs};
use crate::cli::OutputFormat;

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
        /// Which refusals to override: a stored record that changed since the
        /// change was saved (`--force`), and a file saved for a different
        /// state backend (`--accept-backend`).
        overrides: RecoverOverrides,
    },
    /// Retired: does nothing. State follows a project wherever its
    /// directory moves.
    Adopt,
    /// List the locks held in the state backend.
    Locks(LockListing),
    /// Print the tenant's state as JSON (plaintext provider secrets).
    Pull,
    /// Write a history snapshot back as the tenant's state.
    Restore {
        /// The snapshot serial; the newest when `None`.
        serial: Option<u64>,
        /// Whether to ask before restoring.
        confirmation: ConfirmationPolicy,
    },
    /// Copy the tenant's state to the `migrateStateTo` backend, or roll a
    /// migration back.
    Migrate(MigrateRequest),
    /// Move another identity's state to the evaluated project's identity.
    Rename(RenameSource),
}

/// Which locks `state locks` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockListing {
    /// The locks of the evaluated project's CUE module.
    Module,
    /// Every lock the backend holds (`--all`).
    All,
}

/// Whether `apply`, `destroy` and `import` check for a renamed identity
/// before creating a tenant's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identity {
    /// Refuse when state written from this directory exists under an
    /// identity the configuration no longer declares.
    Detect,
    /// Start the new identity's state anyway (`--new-identity`).
    New,
}

/// Whether `unlock` may release a migration fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceRelease {
    /// Refuse a `migrated-` lock.
    Refuse,
    /// Release it (`--migration-fence`).
    Allow,
}

/// Which identity `state rename` moves state from. Unset parts are the
/// evaluated project's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameSource {
    /// Project name the state is recorded under.
    pub project: Option<String>,
    /// CUE package the state is recorded under.
    pub package: Option<String>,
    /// Environment the state is recorded under.
    pub environment: SourceEnvironment,
}

/// The environment `state rename` moves state from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceEnvironment {
    /// The run's selected environment (or none).
    Selected,
    /// The named environment (`--from-env`).
    Named(String),
    /// The identity without `--env` (`--from-no-env`).
    Unselected,
}

/// What `state migrate` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrateRequest {
    /// Which tenants.
    pub scope: MigrationScope,
    /// Forward, or back.
    pub direction: MigrationDirection,
    /// Whether the SQL source may be upgraded to the current schema first.
    pub source_upgrade: SourceUpgrade,
}

/// The tenants `state migrate` works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationScope {
    /// The selected environment (or the identity without `--env`).
    Selected,
    /// Every environment the source holds (`--all`).
    All,
}

/// Which way `state migrate` goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationDirection {
    /// Copy the source to the target and fence the source.
    Forward,
    /// Release the source fence and tombstone the target, refusing when the
    /// target changed since the migration (`--rollback`).
    Rollback,
    /// Copy the target's changes back first (`--rollback --copy-back`).
    CopyBack,
}

/// Whether `state migrate` may upgrade an older SQL source schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceUpgrade {
    /// Refuse an older source schema.
    Refuse,
    /// Upgrade it (`--upgrade-source`).
    Allow,
}

/// Whether `apply --env NAME` may create resources whose addresses the
/// project also has recorded without `--env`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeparateState {
    /// Refuse: both identities would claim the same real objects
    /// (`--allow-separate-state` chooses [`SeparateState::Allow`]).
    Refuse,
    /// Create them as separate objects.
    Allow,
}

/// Which project's lock `unlock` acts on.
///
/// The evaluated project's, or one named by its module and project name,
/// which need not evaluate (it may be gone, broken, or belong to another
/// repository sharing the database). `--env` still selects the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnlockScope {
    /// CUE module path of the project; the evaluated project's when `None`.
    pub module_path: Option<String>,
    /// Name of the project; the evaluated project's when `None`.
    pub project: Option<String>,
}

impl UnlockScope {
    /// Whether the scope names another project than the evaluated one.
    pub(super) fn is_named(&self) -> bool {
        self.module_path.is_some() || self.project.is_some()
    }
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
        /// Whether to create resources the project also has recorded
        /// without `--env`.
        separate_state: SeparateState,
        /// Whether to check for a renamed identity.
        identity: Identity,
    },
    /// Record one provider resource that already exists.
    Import {
        /// Configured resource address, such as `cloudflare_zone.primary`.
        address: String,
        /// Opaque identifier accepted by the provider's import implementation.
        identifier: String,
        /// Whether to check for a renamed identity.
        identity: Identity,
    },
    /// Delete every managed resource of the project.
    Destroy {
        /// Whether to ask before destroying.
        confirmation: ConfirmationPolicy,
        /// Whether to check for a renamed identity.
        identity: Identity,
    },
    /// Inspect or repair recorded state.
    State(StateAction),
    /// Show the project's state lock, or release it when its identifier is given.
    Unlock {
        /// Identifier of the lock to release.
        lock_identifier: Option<String>,
        /// Which project's lock to act on.
        scope: UnlockScope,
        /// Whether a migration fence may be released.
        fence: FenceRelease,
    },
}

impl InfrastructureAction {
    /// The `allowInfrastructure` policy action that decides which project
    /// variables this action may resolve and pass to providers.
    pub(super) const fn policy_name(&self) -> InfrastructurePolicyAction {
        match self {
            Self::Plan => InfrastructurePolicyAction::Plan,
            Self::Apply { .. } => InfrastructurePolicyAction::Apply,
            Self::Import { .. } => InfrastructurePolicyAction::Import,
            Self::Destroy { .. } => InfrastructurePolicyAction::Destroy,
            // Listing the locks of the backend is listing state.
            Self::State(StateAction::List | StateAction::Locks(_)) => {
                InfrastructurePolicyAction::StateList
            }
            Self::State(StateAction::Remove { .. }) => InfrastructurePolicyAction::StateRemove,
            Self::State(StateAction::Recover { .. }) => InfrastructurePolicyAction::StateRecover,
            Self::State(StateAction::Adopt) => InfrastructurePolicyAction::StateAdopt,
            Self::State(StateAction::Pull) => InfrastructurePolicyAction::StatePull,
            Self::State(StateAction::Restore { .. }) => InfrastructurePolicyAction::StateRestore,
            Self::State(StateAction::Migrate(_)) => InfrastructurePolicyAction::StateMigrate,
            Self::State(StateAction::Rename(_)) => InfrastructurePolicyAction::StateRename,
            Self::Unlock { .. } => InfrastructurePolicyAction::Unlock,
        }
    }

    /// Whether this action starts providers and needs the full project
    /// environment. State-only commands resolve just the backend token.
    pub(super) const fn uses_provider_environment(&self) -> bool {
        match self {
            Self::Plan | Self::Apply { .. } | Self::Import { .. } | Self::Destroy { .. } => true,
            Self::State(_) | Self::Unlock { .. } => false,
        }
    }

    /// Plans and changes check the project name across the whole module;
    /// inspecting and repairing state and unlocking must keep working when
    /// a sibling instance is broken, but repairs refuse a project copy in a
    /// directory the module-wide loader skips.
    pub(super) const fn name_check(&self) -> NameCheck {
        match self {
            Self::Plan
            | Self::Apply { .. }
            | Self::Import { .. }
            | Self::Destroy { .. }
            | Self::State(StateAction::Migrate(_) | StateAction::Rename(_)) => {
                NameCheck::WholeModule
            }
            Self::State(
                StateAction::Remove { .. }
                | StateAction::Recover { .. }
                | StateAction::Restore { .. },
            ) => NameCheck::TargetNotSkipped,
            Self::State(
                StateAction::List | StateAction::Locks(_) | StateAction::Adopt | StateAction::Pull,
            )
            | Self::Unlock { .. } => NameCheck::TargetOnly,
        }
    }

    /// How much of the infrastructure configuration the action needs:
    /// state-only commands need only the state backend, so state can always
    /// be listed, unlocked and removed.
    pub(super) const fn needs(&self) -> Needs {
        if self.uses_provider_environment() {
            Needs::Configuration
        } else {
            Needs::StateOnly
        }
    }

    /// Whether the action asks for confirmation.
    pub(super) const fn confirmation(&self) -> ConfirmationPolicy {
        match self {
            Self::Apply { confirmation, .. }
            | Self::Destroy { confirmation, .. }
            | Self::State(StateAction::Restore { confirmation, .. }) => *confirmation,
            Self::Plan | Self::Import { .. } | Self::State(_) | Self::Unlock { .. } => {
                ConfirmationPolicy::AssumeYes
            }
        }
    }

    /// A safe action label for traces; arguments such as provider import
    /// identifiers are never included.
    pub(super) const fn trace_name(&self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Apply { .. } => "apply",
            Self::Import { .. } => "import",
            Self::Destroy { .. } => "destroy",
            Self::State(_) => "state",
            Self::Unlock { .. } => "unlock",
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
    /// Named infrastructure and project environment selected with `--env`.
    pub environment: Option<String>,
    /// Action to perform.
    pub action: InfrastructureAction,
    /// Text or JSON output.
    pub output: OutputFormat,
}
