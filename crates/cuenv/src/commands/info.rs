//! Module information command
//!
//! Displays information about a CUE module including
//! the number of Base and Project instances.

use crate::commands::convert_engine_error;
use crate::commands::env_file::find_cue_module_root;
use cuengine::ModuleEvalOptions;
use cuenv_core::{ModuleEvaluation, Result};
use serde::Serialize;
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

/// Execute the info command
///
/// Evaluates CUE instances and displays information about
/// Base and Project instances found.
///
/// # Arguments
/// * `path` - None for recursive evaluation (./...), Some(path) for specific directory only
/// * `package` - CUE package name to evaluate
/// * `json_output` - Whether to output JSON format
/// * `with_meta` - Include source location metadata for all values
pub fn execute_info(
    path: Option<&str>,
    package: &str,
    json_output: bool,
    with_meta: bool,
) -> Result<String> {
    // Determine if we should recurse based on whether a path was explicitly provided
    let recursive = path.is_none();
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

    // Build evaluation options
    let options = ModuleEvalOptions {
        with_meta,
        recursive,
        ..Default::default()
    };

    // Evaluate the entire module
    let raw_result = cuengine::evaluate_module(&module_root, package, Some(&options))
        .map_err(convert_engine_error)?;

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
}
