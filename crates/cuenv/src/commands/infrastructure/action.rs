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
    /// Make this project's CUE instance the owner of its state.
    Adopt,
    /// List every lock held in the state database, for every project.
    Locks,
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
    },
    /// Record one provider resource that already exists.
    Import {
        /// Configured resource address, such as `cloudflare_zone.primary`.
        address: String,
        /// Opaque identifier accepted by the provider's import implementation.
        identifier: String,
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
        /// Which project's lock to act on.
        scope: UnlockScope,
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
            // Listing the locks of the whole database is listing state.
            Self::State(StateAction::List | StateAction::Locks) => {
                InfrastructurePolicyAction::StateList
            }
            Self::State(StateAction::Remove { .. }) => InfrastructurePolicyAction::StateRemove,
            Self::State(StateAction::Recover { .. }) => InfrastructurePolicyAction::StateRecover,
            Self::State(StateAction::Adopt) => InfrastructurePolicyAction::StateAdopt,
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

    /// Plans, changes and ownership transfers check the project name
    /// across the whole module; inspecting and repairing state and unlocking
    /// must keep working when a sibling instance is broken.
    pub(super) const fn name_check(&self) -> NameCheck {
        match self {
            Self::Plan
            | Self::Apply { .. }
            | Self::Import { .. }
            | Self::Destroy { .. }
            | Self::State(StateAction::Adopt) => NameCheck::WholeModule,
            Self::State(
                StateAction::List
                | StateAction::Locks
                | StateAction::Remove { .. }
                | StateAction::Recover { .. },
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
            Self::Apply { confirmation, .. } | Self::Destroy { confirmation } => *confirmation,
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
