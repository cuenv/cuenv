//! Provider detection and registration.
//!
//! This module provides functions to detect and instantiate the appropriate
//! providers based on the repository structure and CI environment.

use cuenv_codeowners::provider::CodeownersProvider;
use std::path::Path;

#[cfg(feature = "bitbucket")]
use cuenv_bitbucket::BitbucketCodeownersProvider;
#[cfg(feature = "buildkite")]
use cuenv_buildkite::BuildkiteCIProvider;
#[cfg(feature = "github")]
use cuenv_github::GitHubCodeownersProvider;
#[cfg(feature = "gitlab")]
use cuenv_gitlab::GitLabCodeownersProvider;

/// Detect the appropriate CODEOWNERS provider based on repository structure.
///
/// Checks for platform-specific files/directories:
/// - `.github/` directory -> GitHub
/// - `.gitlab-ci.yml` file -> GitLab
/// - `bitbucket-pipelines.yml` file -> Bitbucket
/// - Falls back to GitHub if none detected (when github feature is enabled)
///
/// # Panics
///
/// Panics at compile time if no platform features are enabled.
#[must_use]
pub fn detect_codeowners_provider(repo_root: &Path) -> Box<dyn CodeownersProvider> {
    #[cfg(feature = "github")]
    if repo_root.join(".github").is_dir() {
        return Box::new(GitHubCodeownersProvider);
    }

    #[cfg(feature = "gitlab")]
    if repo_root.join(".gitlab-ci.yml").exists() {
        return Box::new(GitLabCodeownersProvider);
    }

    #[cfg(feature = "bitbucket")]
    if repo_root.join("bitbucket-pipelines.yml").exists() {
        return Box::new(BitbucketCodeownersProvider);
    }

    // Fallback to first available provider
    #[cfg(feature = "github")]
    return Box::new(GitHubCodeownersProvider);

    #[cfg(all(not(feature = "github"), feature = "gitlab"))]
    return Box::new(GitLabCodeownersProvider);

    #[cfg(all(
        not(feature = "github"),
        not(feature = "gitlab"),
        feature = "bitbucket"
    ))]
    return Box::new(BitbucketCodeownersProvider);

    #[cfg(not(any(feature = "github", feature = "gitlab", feature = "bitbucket")))]
    compile_error!("At least one platform feature must be enabled (github, gitlab, or bitbucket)");
}

/// Detect the appropriate CI provider.
///
/// Tries to detect the CI environment in order:
/// 1. Buildkite (if buildkite feature is enabled)
/// 2. GitHub Actions (if github feature is enabled)
/// 3. Falls back to `LocalProvider` (always available in cuenv-ci)
///
/// # Arguments
///
/// * `from_ref` - Optional base ref for local provider (e.g., "main" to compare against)
#[cfg(feature = "github")]
pub fn detect_ci_provider(
    from_ref: Option<String>,
) -> std::sync::Arc<dyn cuenv_ci::provider::CIProvider> {
    use cuenv_ci::provider::CIProvider;
    use std::sync::Arc;

    // Try Buildkite first (most specific)
    #[cfg(feature = "buildkite")]
    if let Some(provider) = BuildkiteCIProvider::detect() {
        return Arc::new(provider);
    }

    // Try GitHub
    if let Some(provider) = cuenv_github::GitHubCIProvider::detect() {
        return Arc::new(provider);
    }

    // Fall back to local provider
    if let Some(base_ref) = from_ref {
        Arc::new(cuenv_ci::provider::LocalProvider::with_base_ref(base_ref))
    } else {
        // LocalProvider::detect() should always succeed for local environments,
        // but fall back to a default with "main" as base ref if it somehow fails
        Arc::new(
            cuenv_ci::provider::LocalProvider::detect().unwrap_or_else(|| {
                cuenv_ci::provider::LocalProvider::with_base_ref("main".to_string())
            }),
        )
    }
}

/// Detect the appropriate CI provider (no GitHub support).
///
/// # Arguments
///
/// * `from_ref` - Optional base ref for local provider (e.g., "main" to compare against)
#[cfg(not(feature = "github"))]
pub fn detect_ci_provider(
    from_ref: Option<String>,
) -> std::sync::Arc<dyn cuenv_ci::provider::CIProvider> {
    use cuenv_ci::provider::CIProvider;
    use std::sync::Arc;

    // Try Buildkite first
    #[cfg(feature = "buildkite")]
    if let Some(provider) = BuildkiteCIProvider::detect() {
        return Arc::new(provider);
    }

    // Fall back to local provider
    if let Some(base_ref) = from_ref {
        Arc::new(cuenv_ci::provider::LocalProvider::with_base_ref(base_ref))
    } else {
        // LocalProvider::detect() should always succeed for local environments,
        // but fall back to a default with "main" as base ref if it somehow fails
        Arc::new(
            cuenv_ci::provider::LocalProvider::detect().unwrap_or_else(|| {
                cuenv_ci::provider::LocalProvider::with_base_ref("main".to_string())
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    #[cfg(feature = "github")]
    fn test_detect_github() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join(".github")).unwrap();

        let provider = detect_codeowners_provider(temp.path());
        assert_eq!(provider.platform(), cuenv_codeowners::Platform::Github);
    }

    #[test]
    #[cfg(feature = "gitlab")]
    fn test_detect_gitlab() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join(".gitlab-ci.yml"), "").unwrap();

        let provider = detect_codeowners_provider(temp.path());
        assert_eq!(provider.platform(), cuenv_codeowners::Platform::Gitlab);
    }

    #[test]
    #[cfg(feature = "bitbucket")]
    fn test_detect_bitbucket() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("bitbucket-pipelines.yml"), "").unwrap();

        let provider = detect_codeowners_provider(temp.path());
        assert_eq!(provider.platform(), cuenv_codeowners::Platform::Bitbucket);
    }

    #[test]
    #[cfg(feature = "github")]
    fn test_detect_default_github() {
        let temp = tempdir().unwrap();
        let provider = detect_codeowners_provider(temp.path());
        // Should fall back to GitHub when no platform indicators found
        assert_eq!(provider.platform(), cuenv_codeowners::Platform::Github);
    }
}
