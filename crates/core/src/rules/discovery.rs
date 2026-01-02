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
