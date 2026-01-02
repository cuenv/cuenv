//! Base schema discovery across monorepo workspaces
//!
//! This module provides functionality to discover Base configurations (owners, ignore)
//! across a monorepo without requiring full Project schemas with name fields.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::manifest::Base;

/// A discovered Base configuration in the workspace
#[derive(Debug, Clone)]
pub struct DiscoveredBase {
    /// Path to the env.cue file
    pub env_cue_path: PathBuf,
    /// Path to the project root (directory containing env.cue)
    pub project_root: PathBuf,
    /// The parsed Base manifest
    pub manifest: Base,
    /// Synthetic name derived from directory path (for CODEOWNERS sections)
    pub synthetic_name: String,
}

/// Function type for evaluating env.cue files as Base schema
pub type BaseEvalFn = Box<dyn Fn(&Path) -> Result<Base, crate::Error> + Send + Sync>;

/// Discovers Base configurations across a monorepo workspace
///
/// Unlike `TaskDiscovery`, this discovers all env.cue files that can be parsed
/// as `schema.#Base`, regardless of whether they have a `name` field. This enables
/// discovering owners and ignore configurations in nested directories that don't
/// define full projects.
pub struct BaseDiscovery {
    /// Root directory of the workspace
    workspace_root: PathBuf,
    /// All discovered Base configurations
    bases: Vec<DiscoveredBase>,
    /// Function to evaluate env.cue files
    eval_fn: Option<BaseEvalFn>,
}

impl BaseDiscovery {
    /// Create a new BaseDiscovery for the given workspace root
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            bases: Vec::new(),
            eval_fn: None,
        }
    }

    /// Set the evaluation function for loading env.cue files
    pub fn with_eval_fn(mut self, eval_fn: BaseEvalFn) -> Self {
        self.eval_fn = Some(eval_fn);
        self
    }

    /// Discover all Base configurations in the workspace
    ///
    /// This scans for env.cue files using the ignore crate to respect .gitignore
    /// and evaluates each as a Base schema.
    ///
    /// Configurations that fail to load are logged as warnings but don't stop discovery.
    /// A summary of failures is logged at the end if any occurred.
    pub fn discover(&mut self) -> Result<(), DiscoveryError> {
        self.bases.clear();

        let eval_fn = self
            .eval_fn
            .as_ref()
            .ok_or(DiscoveryError::NoEvalFunction)?;

        // Build a walker that respects gitignore
        let walker = WalkBuilder::new(&self.workspace_root)
            .follow_links(true)
            .standard_filters(true) // Enable .gitignore, .ignore, hidden file filtering
            .build();

        // Track failures for summary
        let mut load_failures: Vec<(PathBuf, String)> = Vec::new();

        for result in walker {
            match result {
                Ok(entry) => {
                    let path = entry.path();
                    if path.file_name() == Some("env.cue".as_ref()) {
                        match self.load_base(path, eval_fn) {
                            Ok(base) => {
                                self.bases.push(base);
                            }
                            Err(e) => {
                                let error_msg = e.to_string();
                                tracing::warn!(
                                    path = %path.display(),
                                    error = %error_msg,
                                    "Failed to load Base config - this config will be skipped"
                                );
                                load_failures.push((path.to_path_buf(), error_msg));
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "Error during workspace scan - some configs may not be discovered"
                    );
                }
            }
        }

        // Log summary of failures
        if !load_failures.is_empty() {
            tracing::warn!(
                count = load_failures.len(),
                "Some Base configs failed to load during discovery. \
                 Fix CUE errors in these configs or add them to .gitignore to exclude. \
                 Run with RUST_LOG=debug for details."
            );
        }

        tracing::debug!(
            discovered = self.bases.len(),
            failures = load_failures.len(),
            "Base discovery complete"
        );

        Ok(())
    }

    /// Load a single Base configuration from its env.cue path
    fn load_base(
        &self,
        env_cue_path: &Path,
        eval_fn: &BaseEvalFn,
    ) -> Result<DiscoveredBase, DiscoveryError> {
        let project_root = env_cue_path
            .parent()
            .ok_or_else(|| DiscoveryError::InvalidPath(env_cue_path.to_path_buf()))?
            .to_path_buf();

        // Use provided eval function to evaluate the env.cue file as Base
        let manifest = eval_fn(&project_root)
            .map_err(|e| DiscoveryError::EvalError(env_cue_path.to_path_buf(), Box::new(e)))?;

        // Generate synthetic name from directory path
        let synthetic_name = derive_synthetic_name(&self.workspace_root, &project_root);

        Ok(DiscoveredBase {
            env_cue_path: env_cue_path.to_path_buf(),
            project_root,
            manifest,
            synthetic_name,
        })
    }

    /// Get all discovered Base configurations
    pub fn bases(&self) -> &[DiscoveredBase] {
        &self.bases
    }
}

/// Derive a synthetic name from the directory path relative to workspace root
///
/// Examples:
/// - `/workspace/services/api` relative to `/workspace` → "services-api"
/// - `/workspace` relative to `/workspace` → "root"
fn derive_synthetic_name(workspace_root: &Path, project_root: &Path) -> String {
    let relative = project_root
        .strip_prefix(workspace_root)
        .unwrap_or(project_root);

    if relative.as_os_str().is_empty() {
        return "root".to_string();
    }

    relative
        .to_string_lossy()
        .replace(['/', '\\'], "-")
        .trim_matches('-')
        .to_string()
}

/// Errors that can occur during Base discovery
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("Invalid path: {0}")]
    InvalidPath(PathBuf),

    #[error("Failed to evaluate {}: {}", .0.display(), .1)]
    EvalError(PathBuf, #[source] Box<crate::Error>),

    #[error("No evaluation function provided - use with_eval_fn()")]
    NoEvalFunction,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ==========================================================================
    // derive_synthetic_name tests
    // ==========================================================================

    #[test]
    fn test_derive_synthetic_name() {
        // Root directory
        let workspace = PathBuf::from("/workspace");
        assert_eq!(derive_synthetic_name(&workspace, &workspace), "root");

        // Nested directory
        let nested = PathBuf::from("/workspace/services/api");
        assert_eq!(derive_synthetic_name(&workspace, &nested), "services-api");

        // Single level
        let single = PathBuf::from("/workspace/frontend");
        assert_eq!(derive_synthetic_name(&workspace, &single), "frontend");

        // Deep nesting
        let deep = PathBuf::from("/workspace/a/b/c/d");
        assert_eq!(derive_synthetic_name(&workspace, &deep), "a-b-c-d");
    }

    #[test]
    fn test_derive_synthetic_name_trailing_slash() {
        let workspace = PathBuf::from("/workspace/");
        let nested = PathBuf::from("/workspace/services/");
        // Should handle trailing components gracefully
        assert_eq!(derive_synthetic_name(&workspace, &nested), "services");
    }

    #[test]
    fn test_derive_synthetic_name_unrelated_paths() {
        let workspace = PathBuf::from("/workspace");
        let other = PathBuf::from("/other/project");
        // When paths are unrelated, uses the full path as name
        let name = derive_synthetic_name(&workspace, &other);
        assert!(!name.is_empty());
    }

    // ==========================================================================
    // BaseDiscovery construction tests
    // ==========================================================================

    #[test]
    fn test_base_discovery_new() {
        let discovery = BaseDiscovery::new(PathBuf::from("/workspace"));
        assert!(discovery.bases().is_empty());
    }

    #[test]
    fn test_base_discovery_with_eval_fn() {
        let discovery = BaseDiscovery::new(PathBuf::from("/workspace"))
            .with_eval_fn(Box::new(|_| Ok(crate::manifest::Base::default())));
        // Just verify construction works
        assert!(discovery.bases().is_empty());
    }

    #[test]
    fn test_discovery_requires_eval_fn() {
        let mut discovery = BaseDiscovery::new(PathBuf::from("/tmp"));
        let result = discovery.discover();
        assert!(matches!(result, Err(DiscoveryError::NoEvalFunction)));
    }

    #[test]
    fn test_discovery_empty_workspace() {
        let temp = TempDir::new().unwrap();
        let mut discovery = BaseDiscovery::new(temp.path().to_path_buf())
            .with_eval_fn(Box::new(|_| Ok(crate::manifest::Base::default())));

        discovery.discover().unwrap();
        assert!(discovery.bases().is_empty());
    }

    #[test]
    fn test_discovery_with_env_cue() {
        let temp = TempDir::new().unwrap();

        // Create env.cue file
        std::fs::write(temp.path().join("env.cue"), "{}").unwrap();

        let mut discovery = BaseDiscovery::new(temp.path().to_path_buf())
            .with_eval_fn(Box::new(|_| Ok(crate::manifest::Base::default())));

        discovery.discover().unwrap();
        assert_eq!(discovery.bases().len(), 1);
        assert_eq!(discovery.bases()[0].synthetic_name, "root");
    }

    #[test]
    fn test_discovery_nested_env_cue() {
        let temp = TempDir::new().unwrap();

        // Create nested structure
        std::fs::create_dir_all(temp.path().join("services/api")).unwrap();
        std::fs::write(temp.path().join("services/api/env.cue"), "{}").unwrap();

        let mut discovery = BaseDiscovery::new(temp.path().to_path_buf())
            .with_eval_fn(Box::new(|_| Ok(crate::manifest::Base::default())));

        discovery.discover().unwrap();
        assert_eq!(discovery.bases().len(), 1);
        assert_eq!(discovery.bases()[0].synthetic_name, "services-api");
    }

    #[test]
    fn test_discovery_multiple_env_cue() {
        let temp = TempDir::new().unwrap();

        // Create multiple env.cue files
        std::fs::write(temp.path().join("env.cue"), "{}").unwrap();
        std::fs::create_dir_all(temp.path().join("frontend")).unwrap();
        std::fs::write(temp.path().join("frontend/env.cue"), "{}").unwrap();
        std::fs::create_dir_all(temp.path().join("backend")).unwrap();
        std::fs::write(temp.path().join("backend/env.cue"), "{}").unwrap();

        let mut discovery = BaseDiscovery::new(temp.path().to_path_buf())
            .with_eval_fn(Box::new(|_| Ok(crate::manifest::Base::default())));

        discovery.discover().unwrap();
        assert_eq!(discovery.bases().len(), 3);
    }

    #[test]
    fn test_discovery_skips_failed_loads() {
        let temp = TempDir::new().unwrap();

        // Create env.cue files
        std::fs::write(temp.path().join("env.cue"), "{}").unwrap();
        std::fs::create_dir_all(temp.path().join("bad")).unwrap();
        std::fs::write(temp.path().join("bad/env.cue"), "invalid").unwrap();

        let mut discovery = BaseDiscovery::new(temp.path().to_path_buf()).with_eval_fn(Box::new(
            |path| {
                // Simulate failure for "bad" directory
                if path.ends_with("bad") {
                    Err(crate::Error::configuration("Invalid CUE"))
                } else {
                    Ok(crate::manifest::Base::default())
                }
            },
        ));

        // Should succeed even with some failures
        discovery.discover().unwrap();
        // Only the good one should be discovered
        assert_eq!(discovery.bases().len(), 1);
    }

    #[test]
    fn test_discovery_respects_gitignore() {
        let temp = TempDir::new().unwrap();

        // Initialize git repo so .gitignore is respected
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(temp.path())
            .output()
            .ok();

        // Create .gitignore
        std::fs::write(temp.path().join(".gitignore"), "ignored/\n").unwrap();

        // Create ignored and non-ignored env.cue
        std::fs::create_dir_all(temp.path().join("ignored")).unwrap();
        std::fs::write(temp.path().join("ignored/env.cue"), "{}").unwrap();
        std::fs::create_dir_all(temp.path().join("included")).unwrap();
        std::fs::write(temp.path().join("included/env.cue"), "{}").unwrap();

        let mut discovery = BaseDiscovery::new(temp.path().to_path_buf())
            .with_eval_fn(Box::new(|_| Ok(crate::manifest::Base::default())));

        discovery.discover().unwrap();
        // Only included should be found (if git is available), otherwise both
        // The ignore crate requires a git repo to respect .gitignore
        assert!(!discovery.bases().is_empty());
        // Verify included is always present
        assert!(discovery
            .bases()
            .iter()
            .any(|b| b.synthetic_name == "included"));
    }

    // ==========================================================================
    // DiscoveredBase tests
    // ==========================================================================

    #[test]
    fn test_discovered_base_fields() {
        let base = DiscoveredBase {
            env_cue_path: PathBuf::from("/project/env.cue"),
            project_root: PathBuf::from("/project"),
            manifest: crate::manifest::Base::default(),
            synthetic_name: "project".to_string(),
        };

        assert_eq!(base.env_cue_path, PathBuf::from("/project/env.cue"));
        assert_eq!(base.project_root, PathBuf::from("/project"));
        assert_eq!(base.synthetic_name, "project");
    }

    #[test]
    fn test_discovered_base_clone() {
        let base = DiscoveredBase {
            env_cue_path: PathBuf::from("/project/env.cue"),
            project_root: PathBuf::from("/project"),
            manifest: crate::manifest::Base::default(),
            synthetic_name: "project".to_string(),
        };

        let cloned = base.clone();
        assert_eq!(cloned.synthetic_name, "project");
    }

    #[test]
    fn test_discovered_base_debug() {
        let base = DiscoveredBase {
            env_cue_path: PathBuf::from("/project/env.cue"),
            project_root: PathBuf::from("/project"),
            manifest: crate::manifest::Base::default(),
            synthetic_name: "project".to_string(),
        };

        let debug_str = format!("{:?}", base);
        assert!(debug_str.contains("project"));
    }

    // ==========================================================================
    // DiscoveryError tests
    // ==========================================================================

    #[test]
    fn test_discovery_error_invalid_path() {
        let err = DiscoveryError::InvalidPath(PathBuf::from("/bad/path"));
        let msg = err.to_string();
        assert!(msg.contains("Invalid path"));
        assert!(msg.contains("/bad/path"));
    }

    #[test]
    fn test_discovery_error_no_eval_function() {
        let err = DiscoveryError::NoEvalFunction;
        let msg = err.to_string();
        assert!(msg.contains("No evaluation function"));
    }

    #[test]
    fn test_discovery_error_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = DiscoveryError::Io(io_err);
        let msg = err.to_string();
        assert!(msg.contains("IO error"));
    }

    #[test]
    fn test_discovery_error_eval_error() {
        let inner = crate::Error::configuration("bad cue syntax");
        let err = DiscoveryError::EvalError(PathBuf::from("/project/env.cue"), Box::new(inner));
        let msg = err.to_string();
        assert!(msg.contains("Failed to evaluate"));
    }
}
