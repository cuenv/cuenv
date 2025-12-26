use cuengine::ModuleEvalOptions;
use cuenv_core::ModuleEvaluation;
use cuenv_core::Result;
use cuenv_core::manifest::Project;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DiscoveredCIProject {
    /// Full path to the env.cue file
    pub path: PathBuf,
    /// Relative path within the CUE module (for working-directory in CI)
    pub relative_path: PathBuf,
    pub config: Project,
}

/// Find the CUE module root by walking up from `start` looking for `cue.mod/` directory.
fn find_cue_module_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.canonicalize().ok()?;
    loop {
        if current.join("cue.mod").is_dir() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Discover all projects in the current repository
///
/// # Errors
/// Returns an error if glob pattern matching fails or if not inside a CUE module
///
/// # Panics
/// Panics if the regex pattern is invalid (should not happen as it is hardcoded)
pub fn discover_projects() -> Result<Vec<DiscoveredCIProject>> {
    // Check if we're inside a CUE module first
    let cwd = std::env::current_dir().map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: None,
        operation: "get current directory".to_string(),
    })?;

    let module_root = find_cue_module_root(&cwd).ok_or_else(|| {
        cuenv_core::Error::configuration(
            "Not inside a CUE module. Run 'cue mod init' or navigate to a directory with cue.mod/",
        )
    })?;

    // Use module-wide evaluation instead of per-project evaluation
    let options = ModuleEvalOptions {
        recursive: true,
        ..Default::default()
    };
    let raw_result = cuengine::evaluate_module(&module_root, "cuenv", Some(&options))
        .map_err(|e| cuenv_core::Error::configuration(format!("CUE evaluation failed: {e}")))?;

    let module = ModuleEvaluation::from_raw(
        module_root.clone(),
        raw_result.instances,
        raw_result.projects,
    );

    // Iterate through all Project instances (schema-verified)
    let projects: Vec<DiscoveredCIProject> = module
        .projects()
        .filter_map(|instance| {
            instance.deserialize().ok().map(|mut config: Project| {
                // Expand cross-project references and implicit dependencies
                config.expand_cross_project_references();

                // Build the path to the env.cue file
                let env_cue_path = module_root.join(&instance.path).join("env.cue");

                DiscoveredCIProject {
                    path: env_cue_path,
                    relative_path: instance.path.clone(),
                    config,
                }
            })
        })
        .collect();

    Ok(projects)
}

/// Discover projects from an already-evaluated module.
///
/// This avoids redundant CUE evaluation when the caller already has a cached module.
/// Use this when integrating with `CommandExecutor` which caches module evaluation.
#[must_use]
pub fn discover_projects_from_module(module: &ModuleEvaluation) -> Vec<DiscoveredCIProject> {
    module
        .projects()
        .filter_map(|instance| {
            instance.deserialize().ok().map(|mut config: Project| {
                // Expand cross-project references and implicit dependencies
                config.expand_cross_project_references();

                // Build the path to the env.cue file
                let env_cue_path = module.root.join(&instance.path).join("env.cue");

                DiscoveredCIProject {
                    path: env_cue_path,
                    relative_path: instance.path.clone(),
                    config,
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discovered_ci_project_has_relative_path_field() {
        // Verify the struct has the relative_path field with correct type
        let project = DiscoveredCIProject {
            path: PathBuf::from("/module/root/services/api/env.cue"),
            relative_path: PathBuf::from("services/api"),
            config: Project::default(),
        };

        assert_eq!(
            project.relative_path,
            PathBuf::from("services/api"),
            "relative_path should be set correctly"
        );
        assert_eq!(
            project.path,
            PathBuf::from("/module/root/services/api/env.cue"),
            "full path should be set correctly"
        );
    }

    #[test]
    fn test_discovered_ci_project_root_relative_path() {
        // Root projects should have "." as relative_path
        let project = DiscoveredCIProject {
            path: PathBuf::from("/module/root/env.cue"),
            relative_path: PathBuf::from("."),
            config: Project::default(),
        };

        assert_eq!(
            project.relative_path,
            PathBuf::from("."),
            "Root project should have '.' as relative_path"
        );
    }

    #[test]
    fn test_discovered_ci_project_nested_relative_path() {
        // Deeply nested projects should preserve full relative path
        let project = DiscoveredCIProject {
            path: PathBuf::from(
                "/repo/projects/rawkode.academy/platform/email-preferences/env.cue",
            ),
            relative_path: PathBuf::from("projects/rawkode.academy/platform/email-preferences"),
            config: Project::default(),
        };

        assert_eq!(
            project.relative_path,
            PathBuf::from("projects/rawkode.academy/platform/email-preferences"),
            "Nested project should have full relative path"
        );
    }

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
}
