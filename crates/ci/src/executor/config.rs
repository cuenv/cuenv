//! CI Executor Configuration
//!
//! Configuration for the CI pipeline executor including cache settings,
//! parallelism, and secret handling.

use crate::ir::CachePolicy;
use std::path::PathBuf;

/// Configuration for the CI executor
#[derive(Debug, Clone)]
pub struct CIExecutorConfig {
    /// Project root directory
    pub project_root: PathBuf,

    /// Cache root directory (default: .cuenv/cache)
    pub cache_root: Option<PathBuf>,

    /// Maximum parallel tasks (0 = unlimited)
    pub max_parallel: usize,

    /// Capture stdout/stderr for reports
    pub capture_output: bool,

    /// Dry run mode (don't execute, just report what would run)
    pub dry_run: bool,

    /// Global cache policy override (for fork PRs -> Readonly)
    pub cache_policy_override: Option<CachePolicy>,

    /// Salt for secret fingerprinting (CUENV_SECRET_SALT)
    pub secret_salt: Option<String>,

    /// Previous salt for secret fingerprinting during rotation (CUENV_SECRET_SALT_PREV)
    pub secret_salt_prev: Option<String>,
}

impl Default for CIExecutorConfig {
    fn default() -> Self {
        Self {
            project_root: PathBuf::from("."),
            cache_root: None,
            max_parallel: 4,
            capture_output: true,
            dry_run: false,
            cache_policy_override: None,
            secret_salt: None,
            secret_salt_prev: None,
        }
    }
}

impl CIExecutorConfig {
    /// Create a new config with the given project root
    #[must_use]
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            project_root,
            ..Default::default()
        }
    }

    /// Set the cache root directory
    #[must_use]
    pub fn with_cache_root(mut self, cache_root: PathBuf) -> Self {
        self.cache_root = Some(cache_root);
        self
    }

    /// Set the maximum parallel tasks
    #[must_use]
    pub fn with_max_parallel(mut self, max_parallel: usize) -> Self {
        self.max_parallel = max_parallel;
        self
    }

    /// Enable or disable output capture
    #[must_use]
    pub fn with_capture_output(mut self, capture: bool) -> Self {
        self.capture_output = capture;
        self
    }

    /// Enable or disable dry run mode
    #[must_use]
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Set a global cache policy override
    #[must_use]
    pub fn with_cache_policy_override(mut self, policy: CachePolicy) -> Self {
        self.cache_policy_override = Some(policy);
        self
    }

    /// Set the secret salt for fingerprinting
    #[must_use]
    pub fn with_secret_salt(mut self, salt: String) -> Self {
        self.secret_salt = Some(salt);
        self
    }

    /// Set the previous secret salt for rotation
    #[must_use]
    pub fn with_secret_salt_prev(mut self, salt: String) -> Self {
        self.secret_salt_prev = Some(salt);
        self
    }

    /// Get the effective cache root path
    #[must_use]
    pub fn effective_cache_root(&self) -> PathBuf {
        self.cache_root
            .clone()
            .unwrap_or_else(|| self.project_root.join(".cuenv/cache"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = CIExecutorConfig::default();
        assert_eq!(config.max_parallel, 4);
        assert!(config.capture_output);
        assert!(!config.dry_run);
        assert!(config.cache_policy_override.is_none());
    }

    #[test]
    fn test_builder_pattern() {
        let config = CIExecutorConfig::new(PathBuf::from("/project"))
            .with_max_parallel(8)
            .with_dry_run(true)
            .with_cache_policy_override(CachePolicy::Readonly);

        assert_eq!(config.max_parallel, 8);
        assert!(config.dry_run);
        assert_eq!(config.cache_policy_override, Some(CachePolicy::Readonly));
    }

    #[test]
    fn test_effective_cache_root() {
        let config = CIExecutorConfig::new(PathBuf::from("/project"));
        assert_eq!(
            config.effective_cache_root(),
            PathBuf::from("/project/.cuenv/cache")
        );

        let config_with_override = CIExecutorConfig::new(PathBuf::from("/project"))
            .with_cache_root(PathBuf::from("/tmp/cache"));
        assert_eq!(
            config_with_override.effective_cache_root(),
            PathBuf::from("/tmp/cache")
        );
    }
}
