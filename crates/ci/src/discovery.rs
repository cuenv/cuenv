//! CI project discovery utilities.
//!
//! Provides functions for discovering CUE modules and projects for CI operations.
//! Projects can be discovered from the current directory or from an already-evaluated module.
//!
//! Uses discovery-based evaluation: finds all env.cue files and evaluates each directory
//! individually with `recursive: false`, avoiding CUE's `./...:package` pattern which
//! can hang when directories contain mixed packages.

use cuengine::ModuleEvalOptions;

use cuenv_core::{ModuleEvaluation, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Find the CUE module root by walking up from `start` looking for `cue.mod/` directory.
#[must_use]
pub fn find_cue_module_root(start: &Path) -> Option<PathBuf> {
    cuenv_core::cue::discovery::find_cue_module_root(start)
}

/// Evaluate the CUE module from the current working directory.
///
/// This is a convenience function that finds the module root from CWD,
/// evaluates it, and returns the `ModuleEvaluation` for further processing.
///
/// Uses discovery-based evaluation to avoid CUE's `./...:package` pattern
/// which can hang when directories contain mixed packages.
///
/// # Errors
/// Returns an error if:
/// - Current directory cannot be determined
/// - Not inside a CUE module (no `cue.mod/` found)
/// - No env.cue files with matching package found
/// - CUE evaluation fails, for any directory
/// - Two projects share a name (compared case-insensitively)
///
/// # Example
/// ```
/// use cuenv_ci::discovery;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let project = tempfile::tempdir()?;
/// # std::fs::create_dir(project.path().join("cue.mod"))?;
/// # std::fs::write(project.path().join("cue.mod/module.cue"), "module: \"example.com/example@v0\"\nlanguage: version: \"v0.14.1\"\n")?;
/// # std::fs::write(project.path().join("env.cue"), "package cuenv\nname: \"example\"\n")?;
/// # std::env::set_current_dir(project.path())?;
/// let module = discovery::evaluate_module_from_cwd()?;
/// let mut project_names = Vec::new();
/// for instance in module.projects() {
///     let project = instance.to_project()?;
///     project_names.push(project.name);
/// }
/// assert_eq!(project_names, ["example"]);
/// # Ok(())
/// # }
/// ```
pub fn evaluate_module_from_cwd() -> Result<ModuleEvaluation> {
    const PACKAGE: &str = "cuenv";

    let cwd =
        std::env::current_dir().map_err(|e| cuenv_core::Error::io("get current directory", e))?;

    let module_root = find_cue_module_root(&cwd).ok_or_else(|| {
        cuenv_core::Error::configuration(
            "Not inside a CUE module. Run 'cue mod init' or navigate to a directory with cue.mod/",
        )
    })?;

    // Discover all directories with env.cue files matching our package
    let env_cue_dirs =
        cuenv_core::cue::discovery::discover_env_cue_directories(&module_root, PACKAGE);

    if env_cue_dirs.is_empty() {
        return Err(cuenv_core::Error::configuration(format!(
            "No env.cue files with package '{PACKAGE}' found in module: {}",
            module_root.display()
        )));
    }

    // Evaluate each directory individually (non-recursive)
    let mut all_instances = HashMap::new();
    let mut all_projects = Vec::new();
    let mut all_meta = HashMap::new();
    let mut eval_errors = Vec::new();

    for dir in env_cue_dirs {
        let dir_rel_path = cuenv_core::cue::discovery::compute_relative_path(&dir, &module_root);
        // Declarations tell a project's own stamps from those of a
        // same-named project of another package.
        let options = ModuleEvalOptions {
            recursive: false,
            with_meta: true,
            with_references: true,
            target_dir: Some(dir.to_string_lossy().to_string()),
            ..Default::default()
        };

        match cuengine::evaluate_module(&module_root, PACKAGE, Some(&options)) {
            Ok(raw) => {
                // Merge instances (key by relative path from module_root)
                for (path_str, value) in raw.instances {
                    let rel_path = if path_str == "." {
                        dir_rel_path.clone()
                    } else {
                        path_str
                    };
                    all_instances.insert(rel_path.clone(), value);
                }

                for project_path in raw.projects {
                    let rel_project_path = if project_path == "." {
                        dir_rel_path.clone()
                    } else {
                        project_path
                    };
                    if !all_projects.contains(&rel_project_path) {
                        all_projects.push(rel_project_path);
                    }
                }

                // Merge meta with adjusted paths
                for (meta_key, meta_value) in raw.meta {
                    let adjusted_key =
                        cuenv_core::cue::discovery::adjust_meta_key_path(&meta_key, &dir_rel_path);
                    all_meta.insert(adjusted_key, meta_value);
                }
            }
            Err(e) => {
                tracing::warn!(
                    dir = %dir.display(),
                    error = %e,
                    "Failed to evaluate env.cue - skipping directory"
                );
                eval_errors.push((dir, e));
            }
        }
    }

    if all_instances.is_empty() {
        let error_summary = cuenv_core::cue::discovery::format_eval_errors(&eval_errors);
        return Err(cuenv_core::Error::configuration(format!(
            "No instances could be evaluated. All directories failed:\n{error_summary}"
        )));
    }
    // Fail closed: a directory that does not evaluate could declare a
    // project with a name another project uses.
    if !eval_errors.is_empty() {
        let error_summary = cuenv_core::cue::discovery::format_eval_errors(&eval_errors);
        return Err(cuenv_core::Error::configuration(format!(
            "cannot confirm that project names are unique in this module; these directories \
             fail to evaluate:\n{error_summary}"
        )));
    }

    // Convert meta to reference map for dependsOn resolution, and keep
    // where each project name and stamp is declared.
    let declarations = project_declarations(&all_meta);
    let references = if all_meta.is_empty() {
        None
    } else {
        Some(
            all_meta
                .into_iter()
                .filter_map(|(k, v)| v.reference.map(|r| (k, r)))
                .collect(),
        )
    };

    let module = ModuleEvaluation::from_raw_parts(cuenv_core::module::ModuleEvaluationInput {
        root: module_root,
        raw_instances: all_instances,
        project_paths: all_projects,
        metadata: cuenv_core::module::ModuleEvaluationMetadata {
            references,
            declarations,
            ..cuenv_core::module::ModuleEvaluationMetadata::default()
        },
    });
    module.require_unique_project_names()?;
    Ok(module)
}

/// Where every project `name` and every project a stamp names is declared,
/// by meta key: what tells a project's own stamps from those of a
/// same-named project of another package. Nothing else is kept, so tasks
/// get no source locations here.
fn project_declarations(
    meta: &HashMap<String, cuengine::FieldMeta>,
) -> Option<cuenv_core::module::SourceMap> {
    let declarations: cuenv_core::module::SourceMap = meta
        .iter()
        .filter(|(key, _)| {
            key.ends_with("/name")
                || key.ends_with(".cuenvRef.project")
                || key.ends_with(".cuenvProject")
        })
        .filter_map(|(key, field)| {
            let (file, line) = if field.definition_filename.is_empty() {
                (&field.filename, field.line)
            } else {
                (&field.definition_filename, field.definition_line)
            };
            (!file.is_empty()).then(|| {
                (
                    key.clone(),
                    cuenv_core::tasks::SourceLocation {
                        file: file.clone(),
                        line: u32::try_from(line).unwrap_or(u32::MAX),
                        column: 0,
                    },
                )
            })
        })
        .collect();
    (!declarations.is_empty()).then_some(declarations)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==========================================================================
    // find_cue_module_root tests
    // ==========================================================================

    #[test]
    fn test_find_cue_module_root_from_nested_dir() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let root = temp_dir.path();

        // Create cue.mod at root
        fs::create_dir_all(root.join("cue.mod")).expect("Failed to create cue.mod");

        // Create nested directory
        let nested = root.join("services").join("api");
        fs::create_dir_all(&nested).expect("Failed to create nested dir");

        // find_cue_module_root should find the root from nested
        let found = find_cue_module_root(&nested);
        assert!(found.is_some(), "Should find module root from nested dir");

        let found_path = found.unwrap();
        assert_eq!(
            found_path.canonicalize().unwrap(),
            root.canonicalize().unwrap(),
            "Found root should match actual root"
        );
    }

    #[test]
    fn test_find_cue_module_root_not_found() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        // No cue.mod directory

        let found = find_cue_module_root(temp_dir.path());
        assert!(
            found.is_none(),
            "Should not find module root without cue.mod"
        );
    }

    #[test]
    fn test_find_cue_module_root_from_root() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let root = temp_dir.path();

        // Create cue.mod at root
        fs::create_dir_all(root.join("cue.mod")).expect("Failed to create cue.mod");

        // Find from root itself
        let found = find_cue_module_root(root);
        assert!(found.is_some());
    }

    #[test]
    fn test_find_cue_module_root_deeply_nested() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let root = temp_dir.path();

        // Create cue.mod at root
        fs::create_dir_all(root.join("cue.mod")).expect("Failed to create cue.mod");

        // Create deeply nested directory
        let nested = root.join("a").join("b").join("c").join("d").join("e");
        fs::create_dir_all(&nested).expect("Failed to create nested dir");

        // Should still find module root
        let found = find_cue_module_root(&nested);
        assert!(found.is_some());
        assert_eq!(
            found.unwrap().canonicalize().unwrap(),
            root.canonicalize().unwrap()
        );
    }

    #[test]
    fn test_find_cue_module_root_cue_mod_file_not_dir() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let root = temp_dir.path();

        // Create cue.mod as a FILE (not directory) - should not be recognized
        fs::write(root.join("cue.mod"), "not a directory").expect("Failed to create file");

        let found = find_cue_module_root(root);
        assert!(
            found.is_none(),
            "File named cue.mod should not count as module root"
        );
    }

    #[test]
    fn test_find_cue_module_root_intermediate_cue_mod() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let root = temp_dir.path();

        // Create cue.mod in an intermediate directory
        let intermediate = root.join("project");
        fs::create_dir_all(intermediate.join("cue.mod")).expect("Failed to create cue.mod");

        // Create deeply nested directory
        let nested = intermediate.join("services").join("api");
        fs::create_dir_all(&nested).expect("Failed to create nested dir");

        // Should find the intermediate cue.mod, not walk all the way up
        let found = find_cue_module_root(&nested);
        assert!(found.is_some());
        assert_eq!(
            found.unwrap().canonicalize().unwrap(),
            intermediate.canonicalize().unwrap()
        );
    }

    #[test]
    fn test_find_cue_module_root_nonexistent_path() {
        let path = PathBuf::from("/nonexistent/path/that/does/not/exist");
        let found = find_cue_module_root(&path);
        assert!(found.is_none());
    }
}
