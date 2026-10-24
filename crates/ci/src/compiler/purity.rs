//! Flake purity analysis and runtime digests for the [`Compiler`].

use super::digest::DigestBuilder;
use super::{Compiler, CompilerError};
use crate::flake::{FlakeLockAnalyzer, FlakeLockError, PurityAnalysis};
use crate::ir::{PurityMode, Runtime};
use std::path::{Path, PathBuf};
use uuid::Uuid;

impl Compiler {
    /// Analyze flake.lock for purity and compute runtime digest
    ///
    /// If a flake.lock file is found, analyzes it for unlocked inputs
    /// and computes a deterministic digest based on the locked content.
    ///
    /// # Returns
    /// - `Some(Ok((digest, purity)))` if analysis succeeded
    /// - `Some(Err(e))` if analysis failed
    /// - `None` if no flake.lock was found (not a flake-based project)
    #[must_use]
    pub fn analyze_flake_purity(&self) -> Option<Result<(String, PurityMode), CompilerError>> {
        let lock_path = self.resolve_flake_lock_path();

        if !lock_path.exists() {
            return None;
        }

        Some(self.perform_flake_analysis(&lock_path))
    }

    /// Resolve the path to flake.lock
    pub(super) fn resolve_flake_lock_path(&self) -> PathBuf {
        // Use explicit path if provided
        if let Some(path) = &self.options.flake_lock_path {
            return path.clone();
        }

        // Otherwise, look in project root
        if let Some(root) = &self.options.project_root {
            return root.join("flake.lock");
        }

        // Default: current directory
        PathBuf::from("flake.lock")
    }

    /// Perform flake purity analysis and apply purity mode
    pub(super) fn perform_flake_analysis(
        &self,
        lock_path: &Path,
    ) -> Result<(String, PurityMode), CompilerError> {
        let analyzer = FlakeLockAnalyzer::from_path(lock_path)?;
        let analysis = analyzer.analyze();

        self.apply_purity_mode(&analysis)
    }

    /// Apply purity mode enforcement based on analysis results
    ///
    /// - **Strict**: Reject unlocked flakes with an error
    /// - **Warning**: Log warnings and inject UUID into digest (non-deterministic)
    /// - **Override**: Apply manual input overrides for deterministic builds
    pub(super) fn apply_purity_mode(
        &self,
        analysis: &PurityAnalysis,
    ) -> Result<(String, PurityMode), CompilerError> {
        match self.options.purity_mode {
            PurityMode::Strict => {
                if !analysis.is_pure {
                    let inputs: Vec<String> = analysis
                        .unlocked_inputs
                        .iter()
                        .map(|u| format!("{}: {}", u.name, u.reason))
                        .collect();
                    return Err(CompilerError::FlakeLock(FlakeLockError::strict_violation(
                        inputs,
                    )));
                }
                Ok((analysis.locked_digest.clone(), PurityMode::Strict))
            }

            PurityMode::Warning => {
                if analysis.is_pure {
                    Ok((analysis.locked_digest.clone(), PurityMode::Warning))
                } else {
                    // Log warnings for each unlocked input
                    for input in &analysis.unlocked_inputs {
                        tracing::warn!(
                            input = %input.name,
                            reason = %input.reason,
                            "Unlocked flake input detected - cache key will be non-deterministic"
                        );
                    }

                    // Inject UUID v4 into digest to force cache miss
                    let uuid = Uuid::new_v4().to_string();
                    let mut digest_builder = DigestBuilder::new();
                    digest_builder.add_inputs(std::slice::from_ref(&analysis.locked_digest));
                    digest_builder.add_impurity_uuid(&uuid);

                    Ok((digest_builder.finalize(), PurityMode::Warning))
                }
            }

            PurityMode::Override => {
                // In override mode, apply manual input overrides
                let mut effective_digest = analysis.locked_digest.clone();

                if !self.options.input_overrides.is_empty() {
                    let mut digest_builder = DigestBuilder::new();
                    digest_builder.add_inputs(&[effective_digest]);

                    // Add overrides to digest in deterministic order
                    let mut sorted_overrides: Vec<_> =
                        self.options.input_overrides.iter().collect();
                    sorted_overrides.sort_by_key(|(k, _)| *k);

                    for (key, value) in sorted_overrides {
                        digest_builder.add_inputs(&[format!("override:{key}={value}")]);
                    }

                    effective_digest = digest_builder.finalize();
                }

                Ok((effective_digest, PurityMode::Override))
            }
        }
    }

    /// Compute a runtime configuration from the flake analysis
    ///
    /// This method creates a `Runtime` IR type with the computed digest
    /// based on flake purity analysis.
    ///
    /// # Errors
    ///
    /// Returns `CompilerError` if flake purity analysis fails.
    pub fn compute_runtime(
        &self,
        id: impl Into<String>,
        flake_ref: impl Into<String>,
        output: impl Into<String>,
        system: impl Into<String>,
    ) -> Result<Runtime, CompilerError> {
        let (digest, purity) = match self.analyze_flake_purity() {
            Some(result) => result?,
            None => {
                // No flake.lock found - use placeholder digest
                // This handles non-flake projects gracefully
                ("sha256:no-flake-lock".to_string(), self.options.purity_mode)
            }
        };

        Ok(Runtime {
            id: id.into(),
            flake: flake_ref.into(),
            output: output.into(),
            system: system.into(),
            digest,
            purity,
        })
    }
}
