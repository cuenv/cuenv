//! Stage Contributors for CI Pipeline Generation
//!
//! This module provides the `StageContributor` trait and implementations for
//! providers that inject tasks into synthetic build stages (bootstrap, setup,
//! success, failure).
//!
//! ## Architecture
//!
//! Stage contributors are applied during IR compilation. Each contributor:
//! 1. Checks if it should be active (via `is_active`)
//! 2. Contributes tasks to appropriate stages (via `contribute`)
//!
//! The stages are then sorted by priority and included in the IR for emitters
//! to translate into platform-specific steps.
//!
//! ## Available Contributors
//!
//! - `NixContributor` - Installs Nix
//! - `CuenvContributor` - Installs or builds cuenv
//! - `CachixContributor` - Configures Cachix for Nix caching
//! - `OnePasswordContributor` - Sets up 1Password WASM SDK
//! - `GhModelsContributor` - Installs GitHub Models CLI extension

mod cachix;
mod cuenv;
mod gh_models;
mod nix;
mod onepassword;

pub use cachix::CachixContributor;
pub use cuenv::CuenvContributor;
pub use gh_models::GhModelsContributor;
pub use nix::NixContributor;
pub use onepassword::OnePasswordContributor;

use crate::ir::{BuildStage, IntermediateRepresentation, StageTask};
use cuenv_core::manifest::Project;

/// Trait for providers that contribute tasks to build stages
///
/// Implementors of this trait can inject setup/teardown tasks into the CI pipeline
/// at well-defined stages (bootstrap, setup, success, failure).
///
/// Contributors are applied with fixed-point iteration: the compilation loop
/// continues until no contributor reports modifications. This allows contributors
/// to self-detect their requirements based on the current IR state.
///
/// # Example
///
/// ```ignore
/// struct MyContributor;
///
/// impl StageContributor for MyContributor {
///     fn id(&self) -> &'static str { "my-provider" }
///
///     fn is_active(&self, ir: &IntermediateRepresentation, _: &Project) -> bool {
///         // Self-detect based on IR state (e.g., environment, existing stages)
///         ir.pipeline.environment.as_ref().is_some_and(|e| e == "production")
///     }
///
///     fn contribute(&self, ir: &IntermediateRepresentation, _: &Project) -> (Vec<(BuildStage, StageTask)>, bool) {
///         // Check if already contributed (idempotency)
///         if ir.stages.setup.iter().any(|t| t.id == "setup-my-provider") {
///             return (vec![], false);
///         }
///
///         (vec![(BuildStage::Setup, StageTask {
///             id: "setup-my-provider".into(),
///             provider: "my-provider".into(),
///             command: vec!["my-setup-command".into()],
///             priority: 15,
///             ..Default::default()
///         })], true)
///     }
/// }
/// ```
pub trait StageContributor: Send + Sync {
    /// Provider identifier (e.g., "nix", "1password", "cachix")
    fn id(&self) -> &'static str;

    /// Should this contributor be active for the given IR and project?
    ///
    /// Return `true` if the contributor should inject tasks, `false` otherwise.
    /// This is called before `contribute` to avoid unnecessary work.
    ///
    /// Contributors should self-detect their requirements based on IR state
    /// (e.g., `ir.pipeline.environment`) and project configuration.
    fn is_active(&self, ir: &IntermediateRepresentation, project: &Project) -> bool;

    /// Generate stage tasks for this provider
    ///
    /// Returns a tuple of:
    /// - A list of (stage, task) pairs to add to the IR
    /// - A boolean indicating whether any modifications were made
    ///
    /// The `modified` flag enables fixed-point iteration: compilation loops
    /// until no contributor reports modifications. Contributors must be
    /// idempotent - if already contributed, return `(vec![], false)`.
    fn contribute(
        &self,
        ir: &IntermediateRepresentation,
        project: &Project,
    ) -> (Vec<(BuildStage, StageTask)>, bool);
}

/// Returns the default set of stage contributors
///
/// These are applied in order during IR compilation. The order doesn't matter
/// much since tasks within stages are sorted by priority.
#[must_use]
pub fn default_contributors() -> Vec<Box<dyn StageContributor>> {
    vec![
        Box::new(NixContributor),
        Box::new(CuenvContributor),
        Box::new(CachixContributor),
        Box::new(OnePasswordContributor),
        Box::new(GhModelsContributor),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{PipelineMetadata, StageConfiguration};

    fn make_ir() -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.4".to_string(),
            pipeline: PipelineMetadata {
                name: "test".to_string(),
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
            },
            runtimes: vec![],
            stages: StageConfiguration::default(),
            tasks: vec![],
        }
    }

    fn make_project() -> Project {
        Project {
            name: "test".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_default_contributors() {
        let contributors = default_contributors();
        assert_eq!(contributors.len(), 5);

        let ids: Vec<_> = contributors.iter().map(|c| c.id()).collect();
        assert!(ids.contains(&"nix"));
        assert!(ids.contains(&"cuenv"));
        assert!(ids.contains(&"cachix"));
        assert!(ids.contains(&"1password"));
        assert!(ids.contains(&"gh-models"));
    }

    #[test]
    fn test_nix_contributor_inactive_without_runtimes() {
        let contributor = NixContributor;
        let ir = make_ir();
        let project = make_project();

        assert!(!contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_onepassword_contributor_inactive_by_default() {
        let contributor = OnePasswordContributor;
        let ir = make_ir();
        let project = make_project();

        assert!(!contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_cachix_contributor_inactive_without_config() {
        let contributor = CachixContributor;
        let ir = make_ir();
        let project = make_project();

        assert!(!contributor.is_active(&ir, &project));
    }
}
