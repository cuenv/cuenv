//! Task name and dependency normalization
//!
//! Handles normalizing task names (colons to dots), computing task FQDNs,
//! canonicalizing dependencies, and setting default project roots.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cuenv_core::manifest::Project;
use cuenv_core::manifest::TaskRef;
use cuenv_core::tasks::{Task, TaskDefinition};

/// Normalize a raw task name by replacing colons with dots.
///
/// Task names can use either colons or dots as separators, but internally
/// we use dots for consistency with CUE's namespace syntax.
#[must_use]
pub fn normalize_task_name(raw: &str) -> String {
    raw.replace(':', ".")
}

/// Create a fully-qualified domain name (FQDN) for a task.
///
/// The FQDN format is `task:{project_id}:{task_name}` where `task_name`
/// has colons normalized to dots.
#[must_use]
pub fn task_fqdn(project_id: &str, task_name: &str) -> String {
    format!("task:{project_id}:{}", normalize_task_name(task_name))
}

/// Canonicalize a dependency reference relative to the parent task's namespace.
///
/// This matches `TaskIndex` semantics:
/// - Dotted or colon-containing deps are treated as absolute paths
/// - Simple names are resolved relative to the parent namespace of `task_name`
///
/// # Example
/// ```ignore
/// // If task_name is "build.test" and dep is "lint", returns "build.lint"
/// // If dep is "deploy.prod", returns "deploy.prod" (already absolute)
/// ```
#[must_use]
pub fn canonicalize_dep_for_task_name(dep: &str, task_name: &str) -> String {
    // Match TaskIndex semantics: treat dotted/colon deps as absolute, otherwise
    // resolve relative to the parent namespace of `task_name`.
    if dep.contains('.') || dep.contains(':') {
        return normalize_task_name(dep);
    }

    let task_name_norm = normalize_task_name(task_name);
    let mut segments = task_name_norm
        .split('.')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    segments.pop();
    segments.push(dep);
    segments.join(".")
}

/// Compute a stable project identifier.
///
/// Uses the manifest's `name` field if non-empty, otherwise falls back to
/// a path-derived identifier relative to the module root.
#[must_use]
pub fn compute_project_id(manifest: &Project, project_root: &Path, module_root: &Path) -> String {
    let trimmed = manifest.name.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }

    // Fallback: stable id derived from path relative to cue.mod root.
    // Replace separators with '.' to keep the id colon-free (':' is our delimiter).
    let rel = project_root
        .strip_prefix(module_root)
        .unwrap_or(project_root)
        .to_string_lossy()
        .replace(['/', '\\'], ".");
    format!("path.{rel}")
}

/// Set default `project_root` on all tasks in a definition tree.
///
/// Recursively walks the task definition and sets `project_root` on any
/// task that doesn't already have one.
pub fn set_default_project_root(def: &mut TaskDefinition, project_root: &PathBuf) {
    match def {
        TaskDefinition::Single(task) => {
            if task.project_root.is_none() {
                task.project_root = Some(project_root.clone());
            }
        }
        TaskDefinition::Group(group) => match group {
            cuenv_core::tasks::TaskGroup::Sequential(tasks) => {
                for t in tasks {
                    set_default_project_root(t, project_root);
                }
            }
            cuenv_core::tasks::TaskGroup::Parallel(parallel) => {
                for t in parallel.tasks.values_mut() {
                    set_default_project_root(t, project_root);
                }
            }
        },
    }
}

/// Normalize a single dependency reference to FQDN format.
///
/// - If already an FQDN (starts with "task:"), returns as-is
/// - If a `TaskRef` (starts with "#"), parses and converts to FQDN
/// - Otherwise, creates FQDN using the default project ID
#[must_use]
pub fn normalize_dep(
    dep: &str,
    default_project_id: &str,
    project_id_by_name: &HashMap<String, String>,
) -> String {
    let dep = dep.trim();
    if dep.starts_with("task:") {
        return dep.to_string();
    }

    if dep.starts_with('#') {
        let parsed = TaskRef {
            ref_: dep.to_string(),
        };
        if let Some((proj, task)) = parsed.parse() {
            let proj_id = project_id_by_name.get(&proj).cloned().unwrap_or(proj);
            return task_fqdn(&proj_id, &task);
        }
    }

    task_fqdn(default_project_id, dep)
}

/// Normalize all dependencies in a task definition tree to FQDN format.
///
/// Recursively walks the definition and converts all `depends_on` entries
/// to fully-qualified task references.
pub fn normalize_definition_deps(
    def: &mut TaskDefinition,
    project_id_by_root: &HashMap<PathBuf, String>,
    project_id_by_name: &HashMap<String, String>,
    default_project_id: &str,
) {
    fn scope_project_id_for_task(
        task: &Task,
        project_id_by_root: &HashMap<PathBuf, String>,
        fallback: &str,
    ) -> String {
        if let Some(root) = &task.project_root
            && let Some(id) = project_id_by_root.get(root)
        {
            return id.clone();
        }
        fallback.to_string()
    }

    match def {
        TaskDefinition::Single(task) => {
            let scope_id = scope_project_id_for_task(task, project_id_by_root, default_project_id);
            let deps = std::mem::take(&mut task.depends_on);
            task.depends_on = deps
                .into_iter()
                .map(|d| normalize_dep(&d, &scope_id, project_id_by_name))
                .collect();
        }
        TaskDefinition::Group(group) => match group {
            cuenv_core::tasks::TaskGroup::Sequential(tasks) => {
                for t in tasks {
                    normalize_definition_deps(
                        t,
                        project_id_by_root,
                        project_id_by_name,
                        default_project_id,
                    );
                }
            }
            cuenv_core::tasks::TaskGroup::Parallel(parallel) => {
                // Normalize group-level depends_on too
                let group_deps = std::mem::take(&mut parallel.depends_on);
                parallel.depends_on = group_deps
                    .into_iter()
                    .map(|d| normalize_dep(&d, default_project_id, project_id_by_name))
                    .collect();
                for t in parallel.tasks.values_mut() {
                    normalize_definition_deps(
                        t,
                        project_id_by_root,
                        project_id_by_name,
                        default_project_id,
                    );
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_task_name() {
        assert_eq!(normalize_task_name("build:test"), "build.test");
        assert_eq!(normalize_task_name("build.test"), "build.test");
        assert_eq!(normalize_task_name("simple"), "simple");
    }

    #[test]
    fn test_task_fqdn() {
        assert_eq!(task_fqdn("myproject", "build"), "task:myproject:build");
        assert_eq!(task_fqdn("proj", "test:unit"), "task:proj:test.unit");
    }

    #[test]
    fn test_canonicalize_dep_absolute() {
        // Deps with dots or colons are treated as absolute
        assert_eq!(
            canonicalize_dep_for_task_name("deploy.prod", "build.test"),
            "deploy.prod"
        );
        assert_eq!(
            canonicalize_dep_for_task_name("deploy:prod", "build.test"),
            "deploy.prod"
        );
    }

    #[test]
    fn test_canonicalize_dep_relative() {
        // Simple names are resolved relative to parent namespace
        assert_eq!(
            canonicalize_dep_for_task_name("lint", "build.test"),
            "build.lint"
        );
        assert_eq!(
            canonicalize_dep_for_task_name("check", "fmt.fix"),
            "fmt.check"
        );
    }

    #[test]
    fn test_canonicalize_dep_top_level() {
        // Top-level task deps stay top-level
        assert_eq!(canonicalize_dep_for_task_name("other", "build"), "other");
    }

    #[test]
    fn test_compute_project_id_with_name() {
        let manifest = Project {
            name: "my-project".to_string(),
            ..Default::default()
        };
        let id = compute_project_id(&manifest, Path::new("/root/sub"), Path::new("/root"));
        assert_eq!(id, "my-project");
    }

    #[test]
    fn test_compute_project_id_fallback() {
        let manifest = Project {
            name: String::new(),
            ..Default::default()
        };
        let id = compute_project_id(&manifest, Path::new("/root/sub/proj"), Path::new("/root"));
        assert_eq!(id, "path.sub.proj");
    }

    #[test]
    fn test_normalize_dep_fqdn_passthrough() {
        let map = HashMap::new();
        assert_eq!(
            normalize_dep("task:proj:build", "default", &map),
            "task:proj:build"
        );
    }

    #[test]
    fn test_normalize_dep_simple() {
        let map = HashMap::new();
        assert_eq!(normalize_dep("build", "myproj", &map), "task:myproj:build");
    }

    #[test]
    fn test_normalize_dep_taskref() {
        let mut map = HashMap::new();
        map.insert("other".to_string(), "other-id".to_string());
        assert_eq!(
            normalize_dep("#other:build", "myproj", &map),
            "task:other-id:build"
        );
    }
}
