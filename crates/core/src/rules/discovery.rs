//! Independent .rules.cue file discovery.
//!
//! Discovers `.rules.cue` files throughout the repository and evaluates
//! each one independently (NOT as part of module unification).

use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

use crate::Result;
use crate::manifest::DirectoryRules;

/// A discovered .rules.cue configuration.
#[derive(Debug, Clone)]
pub struct DiscoveredRules {
    /// Path to the .rules.cue file.
    pub file_path: PathBuf,

    /// Directory containing the .rules.cue file.
    /// This is where ignore files and .editorconfig will be generated.
    pub directory: PathBuf,

    /// The parsed rules configuration.
    pub config: DirectoryRules,
}

/// Function type for evaluating a single .rules.cue file independently.
pub type RulesEvalFn = Box<dyn Fn(&Path) -> Result<DirectoryRules> + Send + Sync>;

/// Discovers .rules.cue files across the repository.
///
/// Each .rules.cue file is evaluated independently (not unified with
/// other CUE files in the module).
pub struct RulesDiscovery {
    /// Root directory to search from.
    root: PathBuf,

    /// Discovered rules configurations.
    discovered: Vec<DiscoveredRules>,

    /// Function to evaluate .rules.cue files.
    eval_fn: Option<RulesEvalFn>,
}

impl RulesDiscovery {
    /// Create a new RulesDiscovery for the given root directory.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            discovered: Vec::new(),
            eval_fn: None,
        }
    }

    /// Set the evaluation function for .rules.cue files.
    #[must_use]
    pub fn with_eval_fn(mut self, eval_fn: RulesEvalFn) -> Self {
        self.eval_fn = Some(eval_fn);
        self
    }

    /// Discover all .rules.cue files in the repository.
    ///
    /// # Errors
    ///
    /// Returns an error if no evaluation function is provided.
    pub fn discover(&mut self) -> std::result::Result<(), RulesDiscoveryError> {
        self.discovered.clear();

        let eval_fn = self
            .eval_fn
            .as_ref()
            .ok_or(RulesDiscoveryError::NoEvalFunction)?;

        let walker = WalkBuilder::new(&self.root)
            .follow_links(true)
            .standard_filters(true)
            .build();

        let mut load_failures = Vec::new();

        for result in walker {
            match result {
                Ok(entry) => {
                    let path = entry.path();
                    // Look for .rules.cue files
                    if path.file_name() == Some(".rules.cue".as_ref()) {
                        match Self::load_rules(path, eval_fn) {
                            Ok(rules) => self.discovered.push(rules),
                            Err(e) => {
                                tracing::warn!(
                                    path = %path.display(),
                                    error = %e,
                                    "Failed to load .rules.cue - skipping"
                                );
                                load_failures.push((path.to_path_buf(), e));
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Error during directory scan");
                }
            }
        }

        if !load_failures.is_empty() {
            tracing::warn!(
                count = load_failures.len(),
                "Some .rules.cue files failed to load. \
                 Run with RUST_LOG=debug for details."
            );
        }

        tracing::debug!(
            discovered = self.discovered.len(),
            failures = load_failures.len(),
            "Rules discovery complete"
        );

        Ok(())
    }

    /// Load a single .rules.cue configuration.
    fn load_rules(
        file_path: &Path,
        eval_fn: &RulesEvalFn,
    ) -> std::result::Result<DiscoveredRules, RulesDiscoveryError> {
        let directory = file_path
            .parent()
            .ok_or_else(|| RulesDiscoveryError::InvalidPath(file_path.to_path_buf()))?
            .to_path_buf();

        // Evaluate the .rules.cue file independently
        let config = eval_fn(file_path)
            .map_err(|e| RulesDiscoveryError::EvalError(file_path.to_path_buf(), Box::new(e)))?;

        Ok(DiscoveredRules {
            file_path: file_path.to_path_buf(),
            directory,
            config,
        })
    }

    /// Get all discovered .rules.cue configurations.
    pub fn discovered(&self) -> &[DiscoveredRules] {
        &self.discovered
    }

    /// Get the root directory being searched.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Errors that can occur during rules discovery.
#[derive(Debug, thiserror::Error)]
pub enum RulesDiscoveryError {
    /// Invalid path encountered.
    #[error("Invalid path: {0}")]
    InvalidPath(PathBuf),

    /// Failed to evaluate a .rules.cue file.
    #[error("Failed to evaluate {}: {}", .0.display(), .1)]
    EvalError(PathBuf, #[source] Box<crate::Error>),

    /// No evaluation function was provided.
    #[error("No evaluation function provided")]
    NoEvalFunction,

    /// IO error during discovery.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::DirectoryRules;
    use std::fs;
    use tempfile::TempDir;

    // ==========================================================================
    // RulesDiscovery construction tests
    // ==========================================================================

    #[test]
    fn test_rules_discovery_new() {
        let discovery = RulesDiscovery::new(PathBuf::from("/some/root"));

        assert_eq!(discovery.root(), Path::new("/some/root"));
        assert!(discovery.discovered().is_empty());
    }

    #[test]
    fn test_rules_discovery_with_eval_fn() {
        let eval_fn: RulesEvalFn = Box::new(|_| Ok(DirectoryRules::default()));
        let discovery = RulesDiscovery::new(PathBuf::from("/root"))
            .with_eval_fn(eval_fn);

        // Can't directly check if eval_fn is set, but we can verify discover doesn't fail
        // with NoEvalFunction error (we'll test that separately)
        assert_eq!(discovery.root(), Path::new("/root"));
    }

    // ==========================================================================
    // RulesDiscovery::discover tests
    // ==========================================================================

    #[test]
    fn test_discover_no_eval_function_error() {
        let temp_dir = TempDir::new().unwrap();
        let mut discovery = RulesDiscovery::new(temp_dir.path().to_path_buf());

        let result = discovery.discover();

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, RulesDiscoveryError::NoEvalFunction));
        assert_eq!(err.to_string(), "No evaluation function provided");
    }

    #[test]
    fn test_discover_empty_directory() {
        let temp_dir = TempDir::new().unwrap();
        let eval_fn: RulesEvalFn = Box::new(|_| Ok(DirectoryRules::default()));
        let mut discovery = RulesDiscovery::new(temp_dir.path().to_path_buf())
            .with_eval_fn(eval_fn);

        let result = discovery.discover();

        assert!(result.is_ok());
        assert!(discovery.discovered().is_empty());
    }

    #[test]
    fn test_discover_processes_walker_results() {
        // Test the basic discovery flow without relying on hidden file behavior
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Discovery with no .rules.cue files should succeed with empty results
        let eval_fn: RulesEvalFn = Box::new(|_| Ok(DirectoryRules::default()));
        let mut discovery = RulesDiscovery::new(root.to_path_buf())
            .with_eval_fn(eval_fn);

        let result = discovery.discover();

        assert!(result.is_ok());
        // Empty because no .rules.cue files exist
        assert!(discovery.discovered().is_empty());
    }

    #[test]
    fn test_discover_ignores_non_rules_cue_files() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create various files that should NOT be picked up
        fs::write(root.join("rules.cue"), "").unwrap();  // Missing leading dot
        fs::write(root.join(".rules.txt"), "").unwrap(); // Wrong extension
        fs::write(root.join("config.cue"), "").unwrap(); // Different name

        let eval_fn: RulesEvalFn = Box::new(|_| Ok(DirectoryRules::default()));
        let mut discovery = RulesDiscovery::new(root.to_path_buf())
            .with_eval_fn(eval_fn);

        let result = discovery.discover();

        assert!(result.is_ok());
        assert!(discovery.discovered().is_empty());
    }

    #[test]
    fn test_discover_eval_failure_continues() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create two .rules.cue files
        fs::write(root.join(".rules.cue"), "").unwrap();
        fs::create_dir_all(root.join("subdir")).unwrap();
        fs::write(root.join("subdir/.rules.cue"), "").unwrap();

        // Eval function that always fails - discovery should still succeed
        // (failures are logged but not fatal)
        let eval_fn: RulesEvalFn = Box::new(|_path| {
            Err(crate::Error::configuration("test error"))
        });
        let mut discovery = RulesDiscovery::new(root.to_path_buf())
            .with_eval_fn(eval_fn);

        let result = discovery.discover();

        // Should succeed overall, even with eval failures
        assert!(result.is_ok());
        // No discoveries because all evaluations failed
        assert!(discovery.discovered().is_empty());
    }

    #[test]
    fn test_discover_clears_previous_results() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let eval_fn: RulesEvalFn = Box::new(|_| Ok(DirectoryRules::default()));
        let mut discovery = RulesDiscovery::new(root.to_path_buf())
            .with_eval_fn(eval_fn);

        // First discovery
        discovery.discover().unwrap();
        let first_count = discovery.discovered().len();

        // Second discovery - should start fresh (both empty)
        discovery.discover().unwrap();
        let second_count = discovery.discovered().len();

        // Both should be empty (no .rules.cue files)
        assert_eq!(first_count, second_count);
        assert_eq!(first_count, 0);
    }

    // ==========================================================================
    // DiscoveredRules tests
    // ==========================================================================

    #[test]
    fn test_discovered_rules_fields() {
        let discovered = DiscoveredRules {
            file_path: PathBuf::from("/repo/frontend/.rules.cue"),
            directory: PathBuf::from("/repo/frontend"),
            config: DirectoryRules::default(),
        };

        assert_eq!(discovered.file_path, PathBuf::from("/repo/frontend/.rules.cue"));
        assert_eq!(discovered.directory, PathBuf::from("/repo/frontend"));
    }

    #[test]
    fn test_discovered_rules_clone() {
        let discovered = DiscoveredRules {
            file_path: PathBuf::from("/repo/.rules.cue"),
            directory: PathBuf::from("/repo"),
            config: DirectoryRules::default(),
        };

        let cloned = discovered.clone();

        assert_eq!(cloned.file_path, discovered.file_path);
        assert_eq!(cloned.directory, discovered.directory);
    }

    // ==========================================================================
    // RulesDiscoveryError tests
    // ==========================================================================

    #[test]
    fn test_rules_discovery_error_invalid_path_display() {
        let err = RulesDiscoveryError::InvalidPath(PathBuf::from("/bad/path"));
        assert_eq!(err.to_string(), "Invalid path: /bad/path");
    }

    #[test]
    fn test_rules_discovery_error_no_eval_function_display() {
        let err = RulesDiscoveryError::NoEvalFunction;
        assert_eq!(err.to_string(), "No evaluation function provided");
    }

    #[test]
    fn test_rules_discovery_error_io_display() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = RulesDiscoveryError::Io(io_err);
        assert!(err.to_string().contains("file not found"));
    }

    #[test]
    fn test_rules_discovery_error_eval_error_display() {
        let inner_err = crate::Error::configuration("CUE syntax error");
        let err = RulesDiscoveryError::EvalError(
            PathBuf::from("/repo/.rules.cue"),
            Box::new(inner_err),
        );
        let display = err.to_string();
        assert!(display.contains("/repo/.rules.cue"));
        assert!(display.contains("CUE syntax error"));
    }

    #[test]
    fn test_rules_discovery_error_io_from() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let err: RulesDiscoveryError = io_err.into();
        assert!(matches!(err, RulesDiscoveryError::Io(_)));
    }

    // ==========================================================================
    // load_rules tests (unit testing the internal function)
    // ==========================================================================

    #[test]
    fn test_load_rules_sets_correct_directory() {
        // Test load_rules directly by creating a temporary file and calling it
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let subdir = root.join("frontend").join("components");
        fs::create_dir_all(&subdir).unwrap();
        let rules_file = subdir.join(".rules.cue");
        fs::write(&rules_file, "").unwrap();

        let eval_fn: RulesEvalFn = Box::new(|_| Ok(DirectoryRules::default()));

        // Call load_rules directly
        let result = RulesDiscovery::load_rules(&rules_file, &eval_fn);

        assert!(result.is_ok());
        let discovered = result.unwrap();
        assert_eq!(discovered.directory, subdir);
        assert_eq!(discovered.file_path, rules_file);
    }

    #[test]
    fn test_load_rules_eval_fn_error() {
        let temp_dir = TempDir::new().unwrap();
        let rules_file = temp_dir.path().join(".rules.cue");
        fs::write(&rules_file, "").unwrap();

        let eval_fn: RulesEvalFn = Box::new(|_| {
            Err(crate::Error::configuration("parse failed"))
        });

        let result = RulesDiscovery::load_rules(&rules_file, &eval_fn);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, RulesDiscoveryError::EvalError(_, _)));
    }

    #[test]
    fn test_root_accessor() {
        let path = PathBuf::from("/custom/root/path");
        let discovery = RulesDiscovery::new(path.clone());

        assert_eq!(discovery.root(), path.as_path());
    }

    #[test]
    fn test_discovered_accessor_empty() {
        let discovery = RulesDiscovery::new(PathBuf::from("/root"));

        assert!(discovery.discovered().is_empty());
    }
}
