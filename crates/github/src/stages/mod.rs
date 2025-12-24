//! GitHub-specific Stage Contributors
//!
//! This module provides stage contributors that are specific to GitHub Actions
//! or GitHub-related CI features.
//!
//! ## Available Contributors
//!
//! - [`CachixContributor`] - Configures Cachix for Nix caching (uses GitHub secrets)
//! - [`GhModelsContributor`] - Installs GitHub Models CLI extension

mod cachix;
mod gh_models;

pub use cachix::CachixContributor;
pub use gh_models::GhModelsContributor;

use cuenv_ci::StageContributor;

/// Returns GitHub-specific stage contributors.
///
/// These should be combined with core contributors from `cuenv_ci::stages::core_contributors()`
/// when compiling for GitHub Actions.
#[must_use]
pub fn github_contributors() -> Vec<Box<dyn StageContributor>> {
    vec![
        Box::new(CachixContributor),
        Box::new(GhModelsContributor),
    ]
}
