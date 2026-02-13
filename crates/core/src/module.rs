//! Module-wide CUE evaluation types
//!
//! This module provides types for representing the result of evaluating
//! an entire CUE module at once, enabling analysis across all instances.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::Error;

/// Reference metadata extracted from CUE evaluation.
/// Maps field paths (e.g., "./tasks.docs.deploy.dependsOn[0]") to their reference paths (e.g., "tasks.build").
pub type ReferenceMap = HashMap<String, String>;

/// Strip known task reference prefixes from a reference path to get the canonical task name.
///
/// The CUE bridge exports raw reference paths which may include:
/// - Direct task references: "tasks.build", "tasks.ci.deploy"
/// - Let binding aliases: "_t.build", "_tasks.deploy"
fn strip_tasks_prefix(path: &str) -> &str {
    const TASK_PREFIXES: &[&str] = &["tasks.", "_tasks.", "_t."];
    for prefix in TASK_PREFIXES {
        if let Some(stripped) = path.strip_prefix(prefix) {
            return stripped;
        }
    }
    path
}

/// Enrich task references in a JSON value with _name fields using reference metadata.
///
/// Walks the JSON structure recursively, finding task references and injecting
/// `_name` fields based on the CUE reference metadata extracted during evaluation.
///
/// Handles:
/// - `dependsOn` arrays (task dependencies)
/// - `task` fields (pipeline MatrixTask references)
/// - Pipeline task arrays (`ci.pipelines.*.tasks`)
fn enrich_task_refs(value: &mut serde_json::Value, instance_path: &str, references: &ReferenceMap) {
    enrich_task_refs_recursive(value, instance_path, "", references);
}

/// Recursively walk JSON and enrich task references
fn enrich_task_refs_recursive(
    value: &mut serde_json::Value,
    instance_path: &str,
    field_path: &str,
    references: &ReferenceMap,
) {
    match value {
        serde_json::Value::Object(obj) => {
            // Handle dependsOn arrays (task dependencies)
            if let Some(serde_json::Value::Array(deps)) = obj.get_mut("dependsOn") {
                let depends_on_path = if field_path.is_empty() {
                    "dependsOn".to_string()
                } else {
                    format!("{}.dependsOn", field_path)
                };

                enrich_task_ref_array(deps, instance_path, &depends_on_path, references);
            }

            // Handle "task" field (pipeline MatrixTask references)
            if let Some(serde_json::Value::Object(task_obj)) = obj.get_mut("task") {
                // Skip if _name already set
                if !task_obj.contains_key("_name") {
                    let task_path = if field_path.is_empty() {
                        "task".to_string()
                    } else {
                        format!("{}.task", field_path)
                    };
                    let meta_key = format!("{}/{}", instance_path, task_path);
                    if let Some(reference) = references.get(&meta_key) {
                        // Strip "tasks." prefix from the raw CUE reference path
                        let task_name = strip_tasks_prefix(reference);
                        task_obj.insert(
                            "_name".to_string(),
                            serde_json::Value::String(task_name.to_string()),
                        );
                    }
                }
            }

            // Recurse into all object fields
            for (key, child) in obj.iter_mut() {
                if key == "dependsOn" || key == "task" {
                    continue; // Already handled above
                }
                let child_path = if field_path.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", field_path, key)
                };
                enrich_task_refs_recursive(child, instance_path, &child_path, references);
            }
        }
        serde_json::Value::Array(arr) => {
            // Check if this is a pipeline tasks array (ci.pipelines.*.tasks)
            // by seeing if any element is a task ref object
            let is_pipeline_tasks =
                field_path.contains("pipelines.") && field_path.ends_with(".tasks");

            if is_pipeline_tasks {
                enrich_task_ref_array(arr, instance_path, field_path, references);
            }

            for (i, child) in arr.iter_mut().enumerate() {
                let child_path = format!("{}[{}]", field_path, i);
                enrich_task_refs_recursive(child, instance_path, &child_path, references);
            }
        }
        _ => {}
    }
}

/// Enrich task reference objects in an array with _name from reference metadata
fn enrich_task_ref_array(
    arr: &mut [serde_json::Value],
    instance_path: &str,
    array_path: &str,
    references: &ReferenceMap,
) {
    for (i, element) in arr.iter_mut().enumerate() {
        if let serde_json::Value::Object(obj) = element {
            // Skip if _name already set
            if obj.contains_key("_name") {
                continue;
            }

            // Look up the reference in metadata
            let meta_key = format!("{}/{}[{}]", instance_path, array_path, i);
            if let Some(reference) = references.get(&meta_key) {
                // CUE ReferencePath already provides canonical path - just strip prefix
                let task_name = strip_tasks_prefix(reference);
                obj.insert(
                    "_name".to_string(),
                    serde_json::Value::String(task_name.to_string()),
                );
            }
        }
    }
}

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
    /// * `references` - Optional reference map for dependsOn resolution (extracted from CUE metadata)
    pub fn from_raw(
        root: PathBuf,
        raw_instances: HashMap<String, serde_json::Value>,
        project_paths: Vec<String>,
        references: Option<ReferenceMap>,
    ) -> Self {
        // Convert project paths to a set for O(1) lookup
        let project_set: std::collections::HashSet<&str> =
            project_paths.iter().map(String::as_str).collect();

        let instances = raw_instances
            .into_iter()
            .map(|(path, mut value)| {
                let path_buf = PathBuf::from(&path);
                // Use CUE's schema verification instead of heuristic name check
                let kind = if project_set.contains(path.as_str()) {
                    InstanceKind::Project
                } else {
                    InstanceKind::Base
                };

                // Enrich dependsOn arrays with _name using reference metadata
                if let Some(ref refs) = references {
                    enrich_task_refs(&mut value, &path, refs);
                }

                // Process task output references: replace ref objects with
                // placeholder strings and collect auto-dependency pairs.
                // Must happen before Task deserialization (ref objects would
                // fail Vec<String> deserialization in Task.args).
                let output_ref_deps = crate::tasks::output_refs::process_output_refs(&mut value);

                let instance = Instance {
                    path: path_buf.clone(),
                    kind,
                    value,
                    output_ref_deps,
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
            if let Some(ancestor) = self.instances.get(&ancestor_path)
                && let Some(ancestor_value) = ancestor.value.get(field)
                && child_value == ancestor_value
            {
                return true;
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

    /// Auto-inferred dependencies from task output references.
    /// Each pair is (task_that_references, task_being_referenced).
    #[serde(default, skip_serializing)]
    pub output_ref_deps: Vec<crate::tasks::output_refs::OutputRefDep>,
}

impl Instance {
    /// Deserialize this instance's value into a typed struct
    ///
    /// This enables commands to extract strongly-typed configuration
    /// from the evaluated CUE without re-evaluating.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let instance = module.get(path)?;
    /// let project: Project = instance.deserialize()?;
    /// ```
    pub fn deserialize<T: DeserializeOwned>(&self) -> crate::Result<T> {
        serde_json::from_value(self.value.clone()).map_err(|e| {
            Error::configuration(format!(
                "Failed to deserialize {} as {}: {}",
                self.path.display(),
                std::any::type_name::<T>(),
                e
            ))
        })
    }

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
        let project_paths = vec!["projects/api".to_string(), "projects/web".to_string()];

        ModuleEvaluation::from_raw(PathBuf::from("/test/repo"), raw, project_paths, None)
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

    #[test]
    fn test_instance_deserialize() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct TestConfig {
            name: String,
            env: std::collections::HashMap<String, String>,
        }

        let instance = Instance {
            path: PathBuf::from("test/path"),
            kind: InstanceKind::Project,
            value: json!({
                "name": "my-project",
                "env": { "FOO": "bar" }
            }),
            output_ref_deps: vec![],
        };

        let config: TestConfig = instance.deserialize().unwrap();
        assert_eq!(config.name, "my-project");
        assert_eq!(config.env.get("FOO"), Some(&"bar".to_string()));
    }

    #[test]
    fn test_instance_deserialize_error() {
        #[derive(Debug, Deserialize)]
        #[allow(dead_code)] // Test struct for deserialization error testing
        struct RequiredFields {
            required_field: String,
        }

        let instance = Instance {
            path: PathBuf::from("test/path"),
            kind: InstanceKind::Base,
            value: json!({}), // Missing required field
            output_ref_deps: vec![],
        };

        let result: crate::Result<RequiredFields> = instance.deserialize();
        assert!(result.is_err());

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("test/path"),
            "Error should mention path: {}",
            msg
        );
        assert!(
            msg.contains("RequiredFields"),
            "Error should mention target type: {}",
            msg
        );
    }

    // ==========================================================================
    // Additional ModuleEvaluation tests
    // ==========================================================================

    #[test]
    fn test_module_evaluation_empty() {
        let module =
            ModuleEvaluation::from_raw(PathBuf::from("/test"), HashMap::new(), vec![], None);

        assert_eq!(module.base_count(), 0);
        assert_eq!(module.project_count(), 0);
        assert!(module.root_instance().is_none());
    }

    #[test]
    fn test_module_evaluation_root_only() {
        let mut raw = HashMap::new();
        raw.insert(".".to_string(), json!({"key": "value"}));

        let module = ModuleEvaluation::from_raw(PathBuf::from("/test"), raw, vec![], None);

        assert_eq!(module.base_count(), 1);
        assert_eq!(module.project_count(), 0);
        assert!(module.root_instance().is_some());
    }

    #[test]
    fn test_module_evaluation_get_nonexistent() {
        let module =
            ModuleEvaluation::from_raw(PathBuf::from("/test"), HashMap::new(), vec![], None);

        assert!(module.get(Path::new("nonexistent")).is_none());
    }

    #[test]
    fn test_module_evaluation_multiple_projects() {
        let mut raw = HashMap::new();
        raw.insert("proj1".to_string(), json!({"name": "proj1"}));
        raw.insert("proj2".to_string(), json!({"name": "proj2"}));
        raw.insert("proj3".to_string(), json!({"name": "proj3"}));

        let project_paths = vec![
            "proj1".to_string(),
            "proj2".to_string(),
            "proj3".to_string(),
        ];

        let module = ModuleEvaluation::from_raw(PathBuf::from("/test"), raw, project_paths, None);

        assert_eq!(module.project_count(), 3);
        assert_eq!(module.base_count(), 0);
    }

    #[test]
    fn test_module_evaluation_ancestors_deep_path() {
        let module =
            ModuleEvaluation::from_raw(PathBuf::from("/test"), HashMap::new(), vec![], None);

        let ancestors = module.ancestors(Path::new("a/b/c/d"));
        assert_eq!(ancestors.len(), 4);
        assert_eq!(ancestors[0], PathBuf::from("a/b/c"));
        assert_eq!(ancestors[1], PathBuf::from("a/b"));
        assert_eq!(ancestors[2], PathBuf::from("a"));
        assert_eq!(ancestors[3], PathBuf::from("."));
    }

    #[test]
    fn test_module_evaluation_is_inherited_no_child() {
        let module =
            ModuleEvaluation::from_raw(PathBuf::from("/test"), HashMap::new(), vec![], None);

        // Non-existent child should return false
        assert!(!module.is_inherited(Path::new("nonexistent"), "field"));
    }

    #[test]
    fn test_module_evaluation_is_inherited_no_field() {
        let mut raw = HashMap::new();
        raw.insert("child".to_string(), json!({"other": "value"}));

        let module = ModuleEvaluation::from_raw(PathBuf::from("/test"), raw, vec![], None);

        // Child exists but doesn't have the field
        assert!(!module.is_inherited(Path::new("child"), "missing_field"));
    }

    // ==========================================================================
    // Instance tests
    // ==========================================================================

    #[test]
    fn test_instance_get_field() {
        let instance = Instance {
            path: PathBuf::from("test"),
            kind: InstanceKind::Project,
            value: json!({
                "name": "my-project",
                "version": "1.0.0"
            }),
            output_ref_deps: vec![],
        };

        assert_eq!(instance.get_field("name"), Some(&json!("my-project")));
        assert_eq!(instance.get_field("version"), Some(&json!("1.0.0")));
        assert!(instance.get_field("nonexistent").is_none());
    }

    #[test]
    fn test_instance_has_field() {
        let instance = Instance {
            path: PathBuf::from("test"),
            kind: InstanceKind::Project,
            value: json!({"name": "test", "env": {}}),
            output_ref_deps: vec![],
        };

        assert!(instance.has_field("name"));
        assert!(instance.has_field("env"));
        assert!(!instance.has_field("missing"));
    }

    #[test]
    fn test_instance_project_name_base() {
        let instance = Instance {
            path: PathBuf::from("test"),
            kind: InstanceKind::Base,
            value: json!({"name": "should-be-ignored"}),
            output_ref_deps: vec![],
        };

        // Base instances don't return project name even if they have one
        assert!(instance.project_name().is_none());
    }

    #[test]
    fn test_instance_project_name_missing() {
        let instance = Instance {
            path: PathBuf::from("test"),
            kind: InstanceKind::Project,
            value: json!({}),
            output_ref_deps: vec![],
        };

        assert!(instance.project_name().is_none());
    }

    #[test]
    fn test_instance_clone() {
        let instance = Instance {
            path: PathBuf::from("original"),
            kind: InstanceKind::Project,
            value: json!({"name": "test"}),
            output_ref_deps: vec![],
        };

        let cloned = instance.clone();
        assert_eq!(cloned.path, instance.path);
        assert_eq!(cloned.kind, instance.kind);
        assert_eq!(cloned.value, instance.value);
    }

    #[test]
    fn test_instance_serialize() {
        let instance = Instance {
            path: PathBuf::from("test/path"),
            kind: InstanceKind::Project,
            value: json!({"name": "my-project"}),
            output_ref_deps: vec![],
        };

        let json = serde_json::to_string(&instance).unwrap();
        assert!(json.contains("test/path"));
        assert!(json.contains("Project"));
        assert!(json.contains("my-project"));
    }

    // ==========================================================================
    // InstanceKind tests
    // ==========================================================================

    #[test]
    fn test_instance_kind_equality() {
        assert_eq!(InstanceKind::Base, InstanceKind::Base);
        assert_eq!(InstanceKind::Project, InstanceKind::Project);
        assert_ne!(InstanceKind::Base, InstanceKind::Project);
    }

    #[test]
    fn test_instance_kind_copy() {
        let kind = InstanceKind::Project;
        let copied = kind;
        assert_eq!(kind, copied);
    }

    #[test]
    fn test_instance_kind_serialize() {
        let base_json = serde_json::to_string(&InstanceKind::Base).unwrap();
        let project_json = serde_json::to_string(&InstanceKind::Project).unwrap();

        assert!(base_json.contains("Base"));
        assert!(project_json.contains("Project"));
    }

    #[test]
    fn test_instance_kind_deserialize() {
        let base: InstanceKind = serde_json::from_str("\"Base\"").unwrap();
        let project: InstanceKind = serde_json::from_str("\"Project\"").unwrap();

        assert_eq!(base, InstanceKind::Base);
        assert_eq!(project, InstanceKind::Project);
    }

    // ==========================================================================
    // strip_tasks_prefix tests
    // ==========================================================================

    #[test]
    fn test_strip_tasks_prefix() {
        // Standard tasks prefix
        assert_eq!(strip_tasks_prefix("tasks.build"), "build");
        assert_eq!(strip_tasks_prefix("tasks.ci.deploy"), "ci.deploy");

        // Common _t alias (used in env.cue for scope conflict avoidance)
        assert_eq!(strip_tasks_prefix("_t.cargo.build"), "cargo.build");
        assert_eq!(strip_tasks_prefix("_t.release.publish"), "release.publish");

        // Hidden _tasks alias
        assert_eq!(strip_tasks_prefix("_tasks.internal"), "internal");

        // No prefix (already canonical)
        assert_eq!(strip_tasks_prefix("build"), "build");
        assert_eq!(strip_tasks_prefix("ci.deploy"), "ci.deploy");
    }
}
