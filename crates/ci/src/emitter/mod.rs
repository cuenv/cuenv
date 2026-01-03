//! CI Pipeline Emitter Trait
//!
//! Defines the interface for emitting CI configurations from the intermediate
//! representation (IR). Implementations of this trait generate orchestrator-native
//! configurations (e.g., Buildkite, GitLab CI, Tekton).
//!
//! ## Pipeline Modes
//!
//! Emitters support two pipeline generation modes:
//!
//! - **Thin mode**: Generates a single-job workflow that delegates orchestration to cuenv.
//!   Bootstrap → `cuenv ci --pipeline <name>` → Finalizers
//!
//! - **Expanded mode**: Generates multi-job workflows with each task as a separate job,
//!   with dependencies managed by the CI orchestrator.
//!
//! ## Emitter Registry
//!
//! The [`EmitterRegistry`] provides a central registry for all available emitters,
//! enabling dynamic lookup and discovery of available formats.

mod registry;

pub use registry::{EmitterInfo, EmitterRegistry, EmitterRegistryBuilder};

use crate::ir::IntermediateRepresentation;
use cuenv_core::ci::PipelineMode;
use thiserror::Error;

/// Error types for emitter operations
#[derive(Debug, Error)]
pub enum EmitterError {
    /// YAML/JSON serialization failed
    #[error("Serialization failed: {0}")]
    Serialization(String),

    /// Invalid IR structure for this emitter
    #[error("Invalid IR: {0}")]
    InvalidIR(String),

    /// Unsupported feature in IR for this emitter
    #[error("Unsupported feature '{feature}' for {emitter} emitter")]
    UnsupportedFeature {
        feature: String,
        emitter: &'static str,
    },

    /// IO error during emission
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result type for emitter operations
pub type EmitterResult<T> = std::result::Result<T, EmitterError>;

/// Trait for CI configuration emitters
///
/// Implementations transform the IR into orchestrator-specific configurations.
/// Each emitter is responsible for mapping IR concepts to the target format.
///
/// ## Pipeline Modes
///
/// Emitters must implement both `emit_thin` and `emit_expanded` methods:
///
/// - `emit_thin`: Single-job workflow with cuenv orchestration
/// - `emit_expanded`: Multi-job workflow with orchestrator dependencies
///
/// The default `emit` method dispatches based on `ir.pipeline.mode`.
///
/// # Example
///
/// ```ignore
/// use cuenv_ci::emitter::{Emitter, EmitterResult};
/// use cuenv_ci::ir::IntermediateRepresentation;
///
/// struct MyEmitter;
///
/// impl Emitter for MyEmitter {
///     fn emit_thin(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
///         // Generate single-job workflow
///         Ok("# Thin mode config".to_string())
///     }
///
///     fn emit_expanded(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
///         // Generate multi-job workflow
///         Ok("# Expanded mode config".to_string())
///     }
///
///     fn format_name(&self) -> &'static str {
///         "my-ci"
///     }
///
///     fn file_extension(&self) -> &'static str {
///         "yml"
///     }
/// }
/// ```
pub trait Emitter: Send + Sync {
    /// Emit a thin mode CI configuration.
    ///
    /// Thin mode generates a single-job workflow that:
    /// 1. Runs bootstrap phase steps (e.g., install Nix)
    /// 2. Runs setup phase steps (e.g., build cuenv)
    /// 3. Executes `cuenv ci --pipeline <name>` for orchestration
    /// 4. Runs success/failure phase steps with conditions
    ///
    /// # Arguments
    /// * `ir` - The compiled intermediate representation
    ///
    /// # Returns
    /// The generated CI configuration as a string
    ///
    /// # Errors
    /// Returns `EmitterError` if the IR cannot be transformed or serialized
    fn emit_thin(&self, ir: &IntermediateRepresentation) -> EmitterResult<String>;

    /// Emit an expanded mode CI configuration.
    ///
    /// Expanded mode generates a multi-job workflow where:
    /// - Each task becomes a separate job
    /// - Task dependencies map to job dependencies (`needs:` in GitHub Actions)
    /// - Phase tasks are included as steps within each job
    ///
    /// # Arguments
    /// * `ir` - The compiled intermediate representation
    ///
    /// # Returns
    /// The generated CI configuration as a string
    ///
    /// # Errors
    /// Returns `EmitterError` if the IR cannot be transformed or serialized
    fn emit_expanded(&self, ir: &IntermediateRepresentation) -> EmitterResult<String>;

    /// Emit a CI configuration based on the mode in the IR.
    ///
    /// This is the primary entry point for emission. It dispatches to
    /// `emit_thin` or `emit_expanded` based on `ir.pipeline.mode`.
    ///
    /// # Arguments
    /// * `ir` - The compiled intermediate representation
    ///
    /// # Returns
    /// The generated CI configuration as a string
    ///
    /// # Errors
    /// Returns `EmitterError` if the IR cannot be transformed or serialized
    fn emit(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
        match ir.pipeline.mode {
            PipelineMode::Thin => self.emit_thin(ir),
            PipelineMode::Expanded => self.emit_expanded(ir),
        }
    }

    /// Get the format identifier for this emitter
    ///
    /// Used for CLI flag matching (e.g., "buildkite", "gitlab", "tekton")
    fn format_name(&self) -> &'static str;

    /// Get the file extension for output files
    ///
    /// Typically "yml" or "yaml" for most CI systems
    fn file_extension(&self) -> &'static str;

    /// Get a human-readable description of this emitter
    fn description(&self) -> &'static str {
        "CI configuration emitter"
    }

    /// Validate the IR before emission
    ///
    /// Override this to perform emitter-specific validation beyond
    /// the standard IR validation.
    ///
    /// # Errors
    /// Returns `EmitterError::InvalidIR` if validation fails
    fn validate(&self, ir: &IntermediateRepresentation) -> EmitterResult<()> {
        // Default: no additional validation
        let _ = ir;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IntermediateRepresentation, PipelineMetadata};

    struct TestEmitter;

    impl Emitter for TestEmitter {
        fn emit_thin(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
            Ok(format!("# Thin Pipeline: {}", ir.pipeline.name))
        }

        fn emit_expanded(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
            Ok(format!("# Expanded Pipeline: {}", ir.pipeline.name))
        }

        fn format_name(&self) -> &'static str {
            "test"
        }

        fn file_extension(&self) -> &'static str {
            "yml"
        }
    }

    #[test]
    fn test_emitter_trait_expanded_mode() {
        let emitter = TestEmitter;
        let ir = IntermediateRepresentation {
            version: "1.5".to_string(),
            pipeline: PipelineMetadata {
                name: "my-pipeline".to_string(),
                mode: PipelineMode::Expanded,
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
                pipeline_tasks: vec![],
                pipeline_task_defs: vec![],
            },
            runtimes: vec![],
            tasks: vec![],
        };

        // emit() dispatches to emit_expanded() for Expanded mode
        let output = emitter.emit(&ir).unwrap();
        assert_eq!(output, "# Expanded Pipeline: my-pipeline");
        assert_eq!(emitter.format_name(), "test");
        assert_eq!(emitter.file_extension(), "yml");
    }

    #[test]
    fn test_emitter_trait_thin_mode() {
        let emitter = TestEmitter;
        let ir = IntermediateRepresentation {
            version: "1.5".to_string(),
            pipeline: PipelineMetadata {
                name: "my-pipeline".to_string(),
                mode: PipelineMode::Thin,
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
                pipeline_tasks: vec![],
                pipeline_task_defs: vec![],
            },
            runtimes: vec![],
            tasks: vec![],
        };

        // emit() dispatches to emit_thin() for Thin mode
        let output = emitter.emit(&ir).unwrap();
        assert_eq!(output, "# Thin Pipeline: my-pipeline");
    }

    #[test]
    fn test_default_validation() {
        let emitter = TestEmitter;
        let ir = IntermediateRepresentation {
            version: "1.5".to_string(),
            pipeline: PipelineMetadata {
                name: "test".to_string(),
                mode: PipelineMode::default(),
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
                pipeline_tasks: vec![],
                pipeline_task_defs: vec![],
            },
            runtimes: vec![],
            tasks: vec![],
        };

        assert!(emitter.validate(&ir).is_ok());
    }
}
