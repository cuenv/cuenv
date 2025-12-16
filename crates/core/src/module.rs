//! Module-wide CUE evaluation types
//!
//! This module provides types for representing the result of evaluating
//! an entire CUE module at once, enabling analysis across all instances.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Result of evaluating an entire CUE module
///
/// Contains all evaluated instances (directories with env.cue files)
/// from a CUE module, enabling cross-instance analysis.
#[derive(Debug, Clone)]
pub struct ModuleEvaluation {
    /// Path to the CUE module root (directory containing cue.mod/)
    pub root: PathBuf,

    /// Map of relative path to evaluated instance
    pub instances: HashMap<PathBuf, Instance>,
}

impl ModuleEvaluation {
    /// Create a new module evaluation from raw FFI result
    ///
    /// # Arguments
    /// * `root` - Path to the CUE module root
    /// * `raw_instances` - Map of relative paths to evaluated JSON values
    /// * `project_paths` - Paths verified to conform to `schema.#Project` via CUE unification
    pub fn from_raw(
        root: PathBuf,
        raw_instances: HashMap<String, serde_json::Value>,
        project_paths: Vec<String>,
    ) -> Self {
        // Convert project paths to a set for O(1) lookup
        let project_set: std::collections::HashSet<&str> =
            project_paths.iter().map(String::as_str).collect();

        let instances = raw_instances
            .into_iter()
            .map(|(path, value)| {
                let path_buf = PathBuf::from(&path);
                // Use CUE's schema verification instead of heuristic name check
                let kind = if project_set.contains(path.as_str()) {
                    InstanceKind::Project
                } else {
                    InstanceKind::Base
                };
                let instance = Instance {
                    path: path_buf.clone(),
                    kind,
                    value,
                };
                (path_buf, instance)
            })
            .collect();

        Self { root, instances }
    }

    /// Get all Base instances (directories without a `name` field)
    pub fn bases(&self) -> impl Iterator<Item = &Instance> {
        self.instances
            .values()
            .filter(|i| matches!(i.kind, InstanceKind::Base))
    }

    /// Get all Project instances (directories with a `name` field)
    pub fn projects(&self) -> impl Iterator<Item = &Instance> {
        self.instances
            .values()
            .filter(|i| matches!(i.kind, InstanceKind::Project))
    }

    /// Get the root instance (the module root directory)
    pub fn root_instance(&self) -> Option<&Instance> {
        self.instances.get(Path::new("."))
    }

    /// Get an instance by its relative path
    pub fn get(&self, path: &Path) -> Option<&Instance> {
        self.instances.get(path)
    }

    /// Count of Base instances
    pub fn base_count(&self) -> usize {
        self.bases().count()
    }

    /// Count of Project instances
    pub fn project_count(&self) -> usize {
        self.projects().count()
    }

    /// Get all ancestor paths for a given path
    ///
    /// Returns paths from immediate parent up to (and including) the root ".".
    /// Returns empty vector if path is already the root.
    pub fn ancestors(&self, path: &Path) -> Vec<PathBuf> {
        // Root has no ancestors
        if path == Path::new(".") {
            return Vec::new();
        }

        let mut ancestors = Vec::new();
        let mut current = path.to_path_buf();

        while let Some(parent) = current.parent() {
            if parent.as_os_str().is_empty() {
                // Reached filesystem root, add "." as the module root path
                ancestors.push(PathBuf::from("."));
                break;
            }
            ancestors.push(parent.to_path_buf());
            current = parent.to_path_buf();
        }

        ancestors
    }

    /// Check if a field value in a child instance is inherited from an ancestor
    ///
    /// Returns true if the field exists in both the child and an ancestor,
    /// and the values are equal (indicating inheritance via CUE unification).
    pub fn is_inherited(&self, child_path: &Path, field: &str) -> bool {
        let Some(child) = self.instances.get(child_path) else {
            return false;
        };

        let Some(child_value) = child.value.get(field) else {
            return false;
        };

        // Check each ancestor
        for ancestor_path in self.ancestors(child_path) {
            if let Some(ancestor) = self.instances.get(&ancestor_path) {
                if let Some(ancestor_value) = ancestor.value.get(field) {
                    if child_value == ancestor_value {
                        return true;
                    }
                }
            }
        }

        false
    }
}

/// A single evaluated CUE instance (directory with env.cue)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    /// Relative path from module root to this instance
    pub path: PathBuf,

    /// Whether this is a Base or Project instance
    pub kind: InstanceKind,

    /// The raw evaluated JSON value
    pub value: serde_json::Value,
}

impl Instance {
    /// Get the project name if this is a Project instance
    pub fn project_name(&self) -> Option<&str> {
        if matches!(self.kind, InstanceKind::Project) {
            self.value.get("name").and_then(|v| v.as_str())
        } else {
            None
        }
    }

    /// Get a field value from the evaluated config
    pub fn get_field(&self, field: &str) -> Option<&serde_json::Value> {
        self.value.get(field)
    }

    /// Check if a field exists in the evaluated config
    pub fn has_field(&self, field: &str) -> bool {
        self.value.get(field).is_some()
    }
}

/// The kind of CUE instance
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceKind {
    /// A Base instance (no `name` field) - typically intermediate/root config
    Base,
    /// A Project instance (has `name` field) - a leaf node with full features
    Project,
}

impl std::fmt::Display for InstanceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Base => write!(f, "Base"),
            Self::Project => write!(f, "Project"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_test_module() -> ModuleEvaluation {
        let mut raw = HashMap::new();

        // Root (Base)
        raw.insert(
            ".".to_string(),
            json!({
                "env": { "SHARED": "value" },
                "owners": { "rules": { "default": { "pattern": "**", "owners": ["@owner"] } } }
            }),
        );

        // Project with inherited owners
        raw.insert(
            "projects/api".to_string(),
            json!({
                "name": "api",
                "env": { "SHARED": "value" },
                "owners": { "rules": { "default": { "pattern": "**", "owners": ["@owner"] } } }
            }),
        );

        // Project with local owners
        raw.insert(
            "projects/web".to_string(),
            json!({
                "name": "web",
                "env": { "SHARED": "value" },
                "owners": { "rules": { "local": { "pattern": "**", "owners": ["@web-team"] } } }
            }),
        );

        // Specify which paths are projects (simulating CUE schema verification)
        let project_paths = vec![
            "projects/api".to_string(),
            "projects/web".to_string(),
        ];

        ModuleEvaluation::from_raw(PathBuf::from("/test/repo"), raw, project_paths)
    }

    #[test]
    fn test_instance_kind_detection() {
        let module = create_test_module();

        assert_eq!(module.base_count(), 1);
        assert_eq!(module.project_count(), 2);

        let root = module.root_instance().unwrap();
        assert!(matches!(root.kind, InstanceKind::Base));

        let api = module.get(Path::new("projects/api")).unwrap();
        assert!(matches!(api.kind, InstanceKind::Project));
        assert_eq!(api.project_name(), Some("api"));
    }

    #[test]
    fn test_ancestors() {
        let module = create_test_module();

        let ancestors = module.ancestors(Path::new("projects/api"));
        assert_eq!(ancestors.len(), 2);
        assert_eq!(ancestors[0], PathBuf::from("projects"));
        assert_eq!(ancestors[1], PathBuf::from("."));

        let root_ancestors = module.ancestors(Path::new("."));
        assert!(root_ancestors.is_empty());
    }

    #[test]
    fn test_is_inherited() {
        let module = create_test_module();

        // api's owners should be inherited (same as root)
        assert!(module.is_inherited(Path::new("projects/api"), "owners"));

        // web's owners should NOT be inherited (different from root)
        assert!(!module.is_inherited(Path::new("projects/web"), "owners"));

        // env is the same, so should be inherited
        assert!(module.is_inherited(Path::new("projects/api"), "env"));
    }

    #[test]
    fn test_instance_kind_display() {
        assert_eq!(InstanceKind::Base.to_string(), "Base");
        assert_eq!(InstanceKind::Project.to_string(), "Project");
    }
}
