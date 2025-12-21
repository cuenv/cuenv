//! CI Pipeline Emitter Trait
//!
//! Defines the interface for emitting CI configurations from the intermediate
//! representation (IR). Implementations of this trait generate orchestrator-native
//! configurations (e.g., Buildkite, GitLab CI, Tekton).

use crate::ir::IntermediateRepresentation;
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
/// # Example
///
/// ```ignore
/// use cuenv_ci::emitter::{Emitter, EmitterResult};
/// use cuenv_ci::ir::IntermediateRepresentation;
///
/// struct MyEmitter;
///
/// impl Emitter for MyEmitter {
///     fn emit(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
///         // Transform IR to target format
///         Ok("# Generated CI config".to_string())
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
    /// Emit a CI configuration from the intermediate representation
    ///
    /// # Arguments
    /// * `ir` - The compiled intermediate representation
    ///
    /// # Returns
    /// The generated CI configuration as a string (typically YAML or JSON)
    ///
    /// # Errors
    /// Returns `EmitterError` if the IR cannot be transformed or serialized
    fn emit(&self, ir: &IntermediateRepresentation) -> EmitterResult<String>;

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
    use crate::ir::{IntermediateRepresentation, PipelineMetadata, StageConfiguration};

    struct TestEmitter;

    impl Emitter for TestEmitter {
        fn emit(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
            Ok(format!("# Pipeline: {}", ir.pipeline.name))
        }

        fn format_name(&self) -> &'static str {
            "test"
        }

        fn file_extension(&self) -> &'static str {
            "yml"
        }
    }

    #[test]
    fn test_emitter_trait() {
        let emitter = TestEmitter;
        let ir = IntermediateRepresentation {
            version: "1.4".to_string(),
            pipeline: PipelineMetadata {
                name: "my-pipeline".to_string(),
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
            },
            runtimes: vec![],
            stages: StageConfiguration::default(),
            tasks: vec![],
        };

        let output = emitter.emit(&ir).unwrap();
        assert_eq!(output, "# Pipeline: my-pipeline");
        assert_eq!(emitter.format_name(), "test");
        assert_eq!(emitter.file_extension(), "yml");
    }

    #[test]
    fn test_default_validation() {
        let emitter = TestEmitter;
        let ir = IntermediateRepresentation {
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
        };

        assert!(emitter.validate(&ir).is_ok());
    }
}
