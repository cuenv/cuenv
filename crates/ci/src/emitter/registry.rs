//! Emitter Registry
//!
//! Provides a registry for CI configuration emitters, allowing dynamic
//! lookup and discovery of available formats.

use std::collections::HashMap;
use std::sync::Arc;

use super::{Emitter, EmitterError, EmitterResult};
use crate::ir::IntermediateRepresentation;

/// Registry for CI configuration emitters.
///
/// Provides a central registry for all available emitters, enabling:
/// - Dynamic lookup by format name
/// - Enumeration of available formats
/// - Default emitter configuration
///
/// # Example
///
/// ```ignore
/// use cuenv_ci::emitter::{EmitterRegistry, Emitter};
///
/// let mut registry = EmitterRegistry::new();
/// registry.register(Box::new(MyEmitter));
///
/// // Look up by name
/// let emitter = registry.get("my-format").unwrap();
/// let output = emitter.emit(&ir)?;
/// ```
#[derive(Default)]
pub struct EmitterRegistry {
    emitters: HashMap<&'static str, Arc<dyn Emitter>>,
}

impl EmitterRegistry {
    /// Create a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            emitters: HashMap::new(),
        }
    }

    /// Register an emitter.
    ///
    /// The emitter's `format_name()` is used as the key.
    /// If an emitter with the same name already exists, it is replaced.
    pub fn register(&mut self, emitter: impl Emitter + 'static) {
        let name = emitter.format_name();
        self.emitters.insert(name, Arc::new(emitter));
    }

    /// Register an Arc-wrapped emitter.
    ///
    /// Useful when the emitter is already shared.
    pub fn register_arc(&mut self, emitter: Arc<dyn Emitter>) {
        let name = emitter.format_name();
        self.emitters.insert(name, emitter);
    }

    /// Get an emitter by format name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Emitter>> {
        self.emitters.get(name).cloned()
    }

    /// Check if an emitter is registered.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.emitters.contains_key(name)
    }

    /// Get all registered format names.
    #[must_use]
    pub fn formats(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.emitters.keys().copied().collect();
        names.sort_unstable();
        names
    }

    /// Get all registered emitters.
    #[must_use]
    pub fn all(&self) -> Vec<Arc<dyn Emitter>> {
        self.emitters.values().cloned().collect()
    }

    /// Get the number of registered emitters.
    #[must_use]
    pub fn len(&self) -> usize {
        self.emitters.len()
    }

    /// Check if the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.emitters.is_empty()
    }

    /// Emit using a specific format.
    ///
    /// # Errors
    /// Returns error if the format is not found or emission fails.
    pub fn emit(&self, format: &str, ir: &IntermediateRepresentation) -> EmitterResult<String> {
        let emitter = self.get(format).ok_or_else(|| {
            EmitterError::InvalidIR(format!(
                "Unknown format '{}'. Available: {}",
                format,
                self.formats().join(", ")
            ))
        })?;

        emitter.emit(ir)
    }
}

/// Information about a registered emitter.
#[derive(Debug, Clone)]
pub struct EmitterInfo {
    /// Format name (CLI flag value).
    pub format: &'static str,
    /// File extension.
    pub extension: &'static str,
    /// Human-readable description.
    pub description: &'static str,
}

impl EmitterInfo {
    /// Create emitter info from an emitter.
    #[must_use]
    pub fn from_emitter(emitter: &dyn Emitter) -> Self {
        Self {
            format: emitter.format_name(),
            extension: emitter.file_extension(),
            description: emitter.description(),
        }
    }
}

impl EmitterRegistry {
    /// Get information about all registered emitters.
    #[must_use]
    pub fn info(&self) -> Vec<EmitterInfo> {
        let mut infos: Vec<_> = self
            .emitters
            .values()
            .map(|e| EmitterInfo::from_emitter(e.as_ref()))
            .collect();
        infos.sort_by_key(|i| i.format);
        infos
    }
}

/// Builder for creating an emitter registry with default emitters.
#[derive(Default)]
pub struct EmitterRegistryBuilder {
    registry: EmitterRegistry,
}

impl EmitterRegistryBuilder {
    /// Create a new builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a custom emitter.
    #[must_use]
    pub fn with_emitter(mut self, emitter: impl Emitter + 'static) -> Self {
        self.registry.register(emitter);
        self
    }

    /// Build the registry.
    #[must_use]
    pub fn build(self) -> EmitterRegistry {
        self.registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::ci::PipelineMode;

    struct TestEmitter {
        name: &'static str,
    }

    impl Emitter for TestEmitter {
        fn emit_thin(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
            Ok(format!("# {} thin - {}", self.name, ir.pipeline.name))
        }

        fn emit_expanded(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
            Ok(format!("# {} expanded - {}", self.name, ir.pipeline.name))
        }

        fn format_name(&self) -> &'static str {
            self.name
        }

        fn file_extension(&self) -> &'static str {
            "yml"
        }

        fn description(&self) -> &'static str {
            "Test emitter"
        }
    }

    #[test]
    fn test_registry_new() {
        let registry = EmitterRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_registry_register() {
        let mut registry = EmitterRegistry::new();
        registry.register(TestEmitter { name: "test" });

        assert!(!registry.is_empty());
        assert_eq!(registry.len(), 1);
        assert!(registry.has("test"));
    }

    #[test]
    fn test_registry_get() {
        let mut registry = EmitterRegistry::new();
        registry.register(TestEmitter { name: "test" });

        let emitter = registry.get("test");
        assert!(emitter.is_some());
        assert_eq!(emitter.unwrap().format_name(), "test");

        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_registry_formats() {
        let mut registry = EmitterRegistry::new();
        registry.register(TestEmitter { name: "buildkite" });
        registry.register(TestEmitter { name: "gitlab" });
        registry.register(TestEmitter { name: "circleci" });

        let formats = registry.formats();
        assert_eq!(formats, vec!["buildkite", "circleci", "gitlab"]);
    }

    #[test]
    fn test_registry_emit() {
        let mut registry = EmitterRegistry::new();
        registry.register(TestEmitter { name: "test" });

        // Default mode is Thin, so emit() dispatches to emit_thin()
        let ir = IntermediateRepresentation {
            version: "1.5".to_string(),
            pipeline: crate::ir::PipelineMetadata {
                name: "my-pipeline".to_string(),
                mode: PipelineMode::default(),
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
                pipeline_tasks: vec![],
            },
            runtimes: vec![],
            tasks: vec![],
        };

        let output = registry.emit("test", &ir).unwrap();
        assert_eq!(output, "# test thin - my-pipeline");
    }

    #[test]
    fn test_registry_emit_unknown_format() {
        let registry = EmitterRegistry::new();
        let ir = IntermediateRepresentation {
            version: "1.5".to_string(),
            pipeline: crate::ir::PipelineMetadata {
                name: "test".to_string(),
                mode: PipelineMode::default(),
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
                pipeline_tasks: vec![],
            },
            runtimes: vec![],
            tasks: vec![],
        };

        let result = registry.emit("unknown", &ir);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown format"));
    }

    #[test]
    fn test_registry_info() {
        let mut registry = EmitterRegistry::new();
        registry.register(TestEmitter { name: "test" });

        let infos = registry.info();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].format, "test");
        assert_eq!(infos[0].extension, "yml");
        assert_eq!(infos[0].description, "Test emitter");
    }

    #[test]
    fn test_registry_register_replaces() {
        let mut registry = EmitterRegistry::new();
        registry.register(TestEmitter { name: "test" });
        registry.register(TestEmitter { name: "test" });

        assert_eq!(registry.len(), 1);
    }
}
