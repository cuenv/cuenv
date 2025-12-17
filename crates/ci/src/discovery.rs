use cuengine::ModuleEvalOptions;
use cuenv_core::ModuleEvaluation;
use cuenv_core::Result;
use cuenv_core::manifest::Project;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DiscoveredCIProject {
    pub path: PathBuf,
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
    let raw_result = cuengine::evaluate_module(&module_root, "cuenv", Some(options))
        .map_err(|e| cuenv_core::Error::configuration(format!("CUE evaluation failed: {e}")))?;

    let module = ModuleEvaluation::from_raw(
        module_root.clone(),
        raw_result.instances,
        raw_result.projects,
    );

    let mut projects = Vec::new();

    // Iterate through all Project instances (schema-verified)
    for instance in module.projects() {
        let mut config: Project = match instance.deserialize() {
            Ok(c) => c,
            Err(_) => continue, // Skip instances that can't be deserialized
        };

        // Expand cross-project references and implicit dependencies
        config.expand_cross_project_references();

        // Build the path to the env.cue file
        let env_cue_path = module_root.join(&instance.path).join("env.cue");

        projects.push(DiscoveredCIProject {
            path: env_cue_path,
            config,
        });
    }

    Ok(projects)
}
