use cuengine::evaluate_cue_package_typed;
use cuenv_core::Result;
use cuenv_core::manifest::Project;
use glob::glob;
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

    if find_cue_module_root(&cwd).is_none() {
        return Err(cuenv_core::Error::configuration(
            "Not inside a CUE module. Run 'cue mod init' or navigate to a directory with cue.mod/",
        ));
    }

    let mut projects = Vec::new();
    let package_re = regex::Regex::new(r"(?m)^package\s+cuenv\s*$").expect("Invalid regex");

    // Glob for all env.cue files
    let entries = glob("**/env.cue").map_err(|e| cuenv_core::Error::Configuration {
        src: String::new(),
        span: None,
        message: format!("Glob error: {e}"),
    })?;

    for entry in entries.flatten() {
        // Check if file declares package cuenv
        let Ok(content) = std::fs::read_to_string(&entry) else {
            continue;
        };

        if !package_re.is_match(&content) {
            continue;
        }

        let parent = entry.parent().unwrap_or_else(|| std::path::Path::new("."));
        // Fix for empty path issue (root directory)
        let dir_path = if parent.as_os_str().is_empty() {
            std::path::Path::new(".")
        } else {
            parent
        };

        // Load the configuration
        // We assume the package name is "cuenv" based on convention
        if let Ok(mut config) = evaluate_cue_package_typed::<Project>(dir_path, "cuenv") {
            // Expand cross-project references and implicit dependencies
            config.expand_cross_project_references();

            projects.push(DiscoveredCIProject {
                path: entry,
                config,
            });
        }
    }

    Ok(projects)
}
