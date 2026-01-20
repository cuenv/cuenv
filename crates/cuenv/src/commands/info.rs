//! Module information command
//!
//! Displays information about a CUE module including
//! the number of Base and Project instances.
//!
//! Uses discovery-based evaluation when showing all projects: finds all env.cue files
//! and evaluates each directory individually with `recursive: false`, avoiding CUE's
//! `./...:package` pattern which can hang when directories contain mixed packages.

use crate::commands::convert_engine_error;
use crate::commands::env_file::{discover_env_cue_directories, find_cue_module_root};
use cuengine::ModuleEvalOptions;
use cuenv_core::{ModuleEvaluation, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;

/// Output format for JSON mode
#[derive(Debug, Serialize)]
struct InfoOutput {
    module_root: String,
    base_count: usize,
    project_count: usize,
    projects: Vec<ProjectInfo>,
}

/// Output format for --meta mode (full dump with source metadata)
#[derive(Debug, Serialize)]
struct MetaOutput {
    module_root: String,
    instances: std::collections::HashMap<String, serde_json::Value>,
    /// Source locations for all fields (key format: "path/field")
    meta: std::collections::HashMap<String, cuengine::FieldMeta>,
}

#[derive(Debug, Serialize)]
struct ProjectInfo {
    name: String,
    path: String,
}

/// Execute the info command.
///
/// Evaluates CUE instances and displays information about
/// Base and Project instances found.
///
/// # Arguments
/// * `path` - None for recursive evaluation (./...), Some(path) for specific directory only
/// * `package` - CUE package name to evaluate
/// * `json_output` - Whether to output JSON format
/// * `with_meta` - Include source location metadata for all values
///
/// # Errors
///
/// Returns an error if CUE evaluation fails or path canonicalization fails.
#[allow(clippy::too_many_lines)]
pub fn execute_info(
    path: Option<&str>,
    package: &str,
    json_output: bool,
    with_meta: bool,
) -> Result<String> {
    // Determine if we should scan all directories based on whether a path was explicitly provided
    let scan_all = path.is_none();
    let effective_path = path.unwrap_or(".");

    let start_path =
        Path::new(effective_path)
            .canonicalize()
            .map_err(|e| cuenv_core::Error::Io {
                source: e,
                path: Some(Path::new(effective_path).to_path_buf().into_boxed_path()),
                operation: "canonicalize path".to_string(),
            })?;

    // Find the CUE module root
    let module_root = find_cue_module_root(&start_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE module found (looking for cue.mod/) starting from: {}",
            start_path.display()
        ))
    })?;

    // Evaluate using discovery-based approach
    let raw_result = if scan_all {
        // Discover all directories with env.cue files matching our package
        let env_cue_dirs = discover_env_cue_directories(&module_root, package);

        if env_cue_dirs.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "No env.cue files with package '{}' found in module: {}",
                package,
                module_root.display()
            )));
        }

        // Evaluate each directory individually (non-recursive)
        let mut all_instances = HashMap::new();
        let mut all_projects = Vec::new();
        let mut all_meta = HashMap::new();

        for dir in env_cue_dirs {
            let dir_rel_path = compute_relative_path(&dir, &module_root);
            let options = ModuleEvalOptions {
                recursive: false,
                with_meta,
                target_dir: Some(dir.to_string_lossy().to_string()),
                ..Default::default()
            };

            let Ok(raw) = cuengine::evaluate_module(&module_root, package, Some(&options))
                .map_err(convert_engine_error)
            else {
                continue;
            };

            // Merge instances (key by relative path from module_root)
            for (path_str, value) in raw.instances {
                let rel_path = if path_str == "." {
                    dir_rel_path.clone()
                } else {
                    path_str
                };
                all_instances.insert(rel_path.clone(), value);

                if raw.projects.contains(&".".to_string()) {
                    all_projects.push(rel_path);
                }
            }

            // Merge meta with adjusted paths
            for (meta_key, meta_value) in raw.meta {
                let adjusted_key = if meta_key.starts_with("./") {
                    meta_key.replacen("./", &format!("{dir_rel_path}/"), 1)
                } else {
                    meta_key
                };
                all_meta.insert(adjusted_key, meta_value);
            }
        }

        if all_instances.is_empty() {
            return Err(cuenv_core::Error::configuration(
                "No instances could be evaluated. All directories failed.",
            ));
        }

        cuengine::ModuleResult {
            instances: all_instances,
            projects: all_projects,
            meta: all_meta,
        }
    } else {
        // Evaluate specific path only (non-recursive)
        let options = ModuleEvalOptions {
            with_meta,
            recursive: false,
            target_dir: Some(start_path.to_string_lossy().to_string()),
            ..Default::default()
        };

        cuengine::evaluate_module(&module_root, package, Some(&options))
            .map_err(convert_engine_error)?
    };

    // If --meta is requested, dump the full JSON with separate meta map
    if with_meta {
        let output = MetaOutput {
            module_root: module_root.display().to_string(),
            instances: raw_result.instances,
            meta: raw_result.meta,
        };
        return serde_json::to_string_pretty(&output).map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to serialize JSON: {e}"))
        });
    }

    // Convert to ModuleEvaluation (using schema-verified project list)
    let module = ModuleEvaluation::from_raw(
        module_root.clone(),
        raw_result.instances,
        raw_result.projects,
        None,
    );

    // Collect project information
    let mut projects: Vec<ProjectInfo> = module
        .projects()
        .filter_map(|instance| {
            instance.project_name().map(|name| ProjectInfo {
                name: name.to_string(),
                path: instance.path.display().to_string(),
            })
        })
        .collect();

    // Sort by name for consistent output
    projects.sort_by(|a, b| a.name.cmp(&b.name));

    if json_output {
        let output = InfoOutput {
            module_root: module_root.display().to_string(),
            base_count: module.base_count(),
            project_count: module.project_count(),
            projects,
        };
        serde_json::to_string_pretty(&output)
            .map_err(|e| cuenv_core::Error::configuration(format!("Failed to serialize JSON: {e}")))
    } else {
        // Human-readable output
        let mut output = String::new();

        let _ = writeln!(output, "Module: {}\n", module_root.display());
        let _ = writeln!(output, "Bases: {}", module.base_count());
        let _ = writeln!(output, "Projects: {}", module.project_count());

        if !projects.is_empty() {
            output.push_str("\nProjects:\n");

            // Calculate max name length for alignment
            let max_name_len = projects
                .iter()
                .map(|p| p.name.len())
                .max()
                .unwrap_or(0)
                .max(20);

            for project in &projects {
                let _ = writeln!(
                    output,
                    "  {:<width$}  {}",
                    project.name,
                    project.path,
                    width = max_name_len
                );
            }
        }

        Ok(output)
    }
}

/// Compute relative path from module_root to target directory.
fn compute_relative_path(target: &std::path::Path, module_root: &std::path::Path) -> String {
    target.strip_prefix(module_root).map_or_else(
        |_| ".".to_string(),
        |p| {
            if p.as_os_str().is_empty() {
                ".".to_string()
            } else {
                p.to_string_lossy().to_string()
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_info_serialization() {
        let info = ProjectInfo {
            name: "test-project".to_string(),
            path: "projects/test".to_string(),
        };

        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("test-project"));
        assert!(json.contains("projects/test"));
    }

    #[test]
    fn test_info_output_serialization() {
        let output = InfoOutput {
            module_root: "/test/repo".to_string(),
            base_count: 2,
            project_count: 5,
            projects: vec![ProjectInfo {
                name: "api".to_string(),
                path: "projects/api".to_string(),
            }],
        };

        let json = serde_json::to_string_pretty(&output).unwrap();
        assert!(json.contains("/test/repo"));
        assert!(json.contains("\"base_count\": 2"));
        assert!(json.contains("\"project_count\": 5"));
    }

    #[test]
    fn test_project_info_debug() {
        let info = ProjectInfo {
            name: "test-project".to_string(),
            path: "projects/test".to_string(),
        };

        let debug = format!("{info:?}");
        assert!(debug.contains("ProjectInfo"));
        assert!(debug.contains("test-project"));
    }

    #[test]
    fn test_info_output_debug() {
        let output = InfoOutput {
            module_root: "/test/repo".to_string(),
            base_count: 0,
            project_count: 0,
            projects: vec![],
        };

        let debug = format!("{output:?}");
        assert!(debug.contains("InfoOutput"));
        assert!(debug.contains("/test/repo"));
    }

    #[test]
    fn test_meta_output_serialization() {
        let mut instances = std::collections::HashMap::new();
        instances.insert("./".to_string(), serde_json::json!({"name": "test"}));

        let output = MetaOutput {
            module_root: "/test/repo".to_string(),
            instances,
            meta: std::collections::HashMap::new(),
        };

        let json = serde_json::to_string_pretty(&output).unwrap();
        assert!(json.contains("/test/repo"));
        assert!(json.contains("instances"));
    }

    #[test]
    fn test_meta_output_debug() {
        let output = MetaOutput {
            module_root: "/test".to_string(),
            instances: std::collections::HashMap::new(),
            meta: std::collections::HashMap::new(),
        };

        let debug = format!("{output:?}");
        assert!(debug.contains("MetaOutput"));
    }

    #[test]
    fn test_info_output_multiple_projects() {
        let output = InfoOutput {
            module_root: "/repo".to_string(),
            base_count: 1,
            project_count: 3,
            projects: vec![
                ProjectInfo {
                    name: "api".to_string(),
                    path: "services/api".to_string(),
                },
                ProjectInfo {
                    name: "web".to_string(),
                    path: "services/web".to_string(),
                },
                ProjectInfo {
                    name: "worker".to_string(),
                    path: "services/worker".to_string(),
                },
            ],
        };

        let json = serde_json::to_string_pretty(&output).unwrap();
        assert!(json.contains("api"));
        assert!(json.contains("web"));
        assert!(json.contains("worker"));
        assert!(json.contains("\"project_count\": 3"));
    }

    #[test]
    fn test_execute_info_invalid_path() {
        let result = execute_info(Some("/nonexistent/path"), "cuenv", false, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_execute_info_no_cue_module() {
        // Use temp directory with no cue.mod
        let temp = std::env::temp_dir();
        let result = execute_info(Some(temp.to_str().unwrap()), "cuenv", false, false);
        // Should fail with "No CUE module found"
        assert!(result.is_err());
    }
}
