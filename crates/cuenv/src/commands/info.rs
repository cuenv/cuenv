//! Module information command
//!
//! Displays information about a CUE module including
//! the number of Base and Project instances.
//!
//! Without an explicit path, evaluates the selected package recursively across
//! the module in one CUE evaluation. With a path, evaluates only that directory.

use crate::commands::convert_engine_error;
use crate::commands::env_file::find_cue_module_root;
use cuengine::ModuleEvalOptions;
use cuenv_core::{ModuleEvaluation, Result};
use serde::Serialize;
use std::fmt::Write;
use std::path::{Path, PathBuf};

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

/// Options for executing the info command.
#[derive(Clone, Copy, Debug)]
pub struct InfoOptions<'a> {
    /// None for recursive evaluation (./...), Some(path) for specific directory only.
    pub path: Option<&'a str>,
    /// CUE package name to evaluate.
    pub package: &'a str,
    /// Whether to output JSON format.
    pub json_output: bool,
    /// Include source location metadata for all values.
    pub with_meta: bool,
}

struct InfoContext {
    scan_all: bool,
    start_path: PathBuf,
    module_root: PathBuf,
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
pub fn execute_info(options: InfoOptions<'_>) -> Result<String> {
    let context = resolve_info_context(options)?;
    let raw_result = evaluate_info_module(&context, options)?;

    if options.with_meta {
        return render_meta_output(&context.module_root, raw_result);
    }

    render_module_summary(&context.module_root, raw_result, options.json_output)
}

fn resolve_info_context(options: InfoOptions<'_>) -> Result<InfoContext> {
    let scan_all = options.path.is_none();
    let effective_path = options.path.unwrap_or(".");

    let start_path = Path::new(effective_path).canonicalize().map_err(|e| {
        cuenv_core::Error::io_with_path(
            "canonicalize path",
            Path::new(effective_path).to_path_buf(),
            e,
        )
    })?;

    // Find the CUE module root
    let module_root = find_cue_module_root(&start_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE module found (looking for cue.mod/) starting from: {}",
            start_path.display()
        ))
    })?;

    Ok(InfoContext {
        scan_all,
        start_path,
        module_root,
    })
}

fn evaluate_info_module(
    context: &InfoContext,
    options: InfoOptions<'_>,
) -> Result<cuengine::ModuleResult> {
    if context.scan_all {
        evaluate_recursive_info_module(context, options)
    } else {
        evaluate_specific_info_path(context, options)
    }
}

fn evaluate_recursive_info_module(
    context: &InfoContext,
    options: InfoOptions<'_>,
) -> Result<cuengine::ModuleResult> {
    let eval_options = ModuleEvalOptions {
        recursive: true,
        with_meta: options.with_meta,
        ..Default::default()
    };

    cuengine::evaluate_module(&context.module_root, options.package, Some(&eval_options))
        .map_err(convert_engine_error)
}

fn evaluate_specific_info_path(
    context: &InfoContext,
    options: InfoOptions<'_>,
) -> Result<cuengine::ModuleResult> {
    let eval_options = ModuleEvalOptions {
        with_meta: options.with_meta,
        recursive: false,
        target_dir: Some(context.start_path.to_string_lossy().to_string()),
        ..Default::default()
    };

    cuengine::evaluate_module(&context.module_root, options.package, Some(&eval_options))
        .map_err(convert_engine_error)
}

fn render_meta_output(module_root: &Path, raw_result: cuengine::ModuleResult) -> Result<String> {
    let output = MetaOutput {
        module_root: module_root.display().to_string(),
        instances: raw_result.instances,
        meta: raw_result.meta,
    };
    serde_json::to_string_pretty(&output)
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to serialize JSON: {e}")))
}

fn render_module_summary(
    module_root: &Path,
    raw_result: cuengine::ModuleResult,
    json_output: bool,
) -> Result<String> {
    let module = ModuleEvaluation::from_raw(
        module_root.to_path_buf(),
        raw_result.instances,
        raw_result.projects,
        None,
    );

    let mut projects: Vec<ProjectInfo> = module
        .projects()
        .filter_map(|instance| {
            instance.project_name().map(|name| ProjectInfo {
                name: name.to_string(),
                path: instance.path.display().to_string(),
            })
        })
        .collect();
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
    use std::error::Error;
    use std::fs;
    use tempfile::TempDir;

    type TestResult<T = ()> = std::result::Result<T, Box<dyn Error>>;

    fn temp_module() -> TestResult<TempDir> {
        Ok(tempfile::Builder::new()
            .prefix("cuenv-info-recursive-")
            .tempdir()?)
    }

    fn write_module(root: &Path) -> TestResult {
        fs::create_dir_all(root.join("cue.mod"))?;
        fs::write(
            root.join("cue.mod/module.cue"),
            "module: \"example.com/info-recursive-test\"\nlanguage: {version: \"v0.9.0\"}\n",
        )?;
        Ok(())
    }

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
        let result = execute_info(InfoOptions {
            path: Some("/nonexistent/path"),
            package: "cuenv",
            json_output: false,
            with_meta: false,
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_execute_info_no_cue_module() {
        // Use temp directory with no cue.mod
        let temp = std::env::temp_dir();
        let result = execute_info(InfoOptions {
            path: Some(temp.to_str().unwrap()),
            package: "cuenv",
            json_output: false,
            with_meta: false,
        });
        // Should fail with "No CUE module found"
        assert!(result.is_err());
    }

    #[test]
    fn unscoped_info_discovers_an_arbitrarily_named_cue_file() -> TestResult {
        let temp = temp_module()?;
        write_module(temp.path())?;
        fs::write(
            temp.path().join("project.cue"),
            "package cuenv\nname: \"arbitrary-filename\"\n",
        )?;
        let context = InfoContext {
            scan_all: true,
            start_path: temp.path().to_path_buf(),
            module_root: temp.path().to_path_buf(),
        };
        let result = evaluate_info_module(
            &context,
            InfoOptions {
                path: None,
                package: "cuenv",
                json_output: true,
                with_meta: false,
            },
        )?;

        assert_eq!(result.projects, ["."]);
        assert_eq!(result.instances["."]["name"], "arbitrary-filename");

        Ok(())
    }

    #[test]
    fn unscoped_info_propagates_a_selected_package_failure() -> TestResult {
        let temp = temp_module()?;
        write_module(temp.path())?;
        fs::write(
            temp.path().join("defaults.cue"),
            "package cuenv\nenv: {ROOT: \"yes\"}\n",
        )?;
        fs::create_dir_all(temp.path().join("valid"))?;
        fs::write(
            temp.path().join("valid/project.cue"),
            "package cuenv\nname: \"valid\"\n",
        )?;
        fs::create_dir_all(temp.path().join("broken"))?;
        fs::write(
            temp.path().join("broken/configuration.cue"),
            "package cuenv\nname: \"broken\"\nimpossible: 1 & 2\n",
        )?;
        let context = InfoContext {
            scan_all: true,
            start_path: temp.path().to_path_buf(),
            module_root: temp.path().to_path_buf(),
        };

        let error = evaluate_info_module(
            &context,
            InfoOptions {
                path: None,
                package: "cuenv",
                json_output: true,
                with_meta: false,
            },
        )
        .expect_err("a broken selected-package instance must fail unscoped info");

        assert!(error.to_string().contains("broken"), "{error}");

        Ok(())
    }
}
