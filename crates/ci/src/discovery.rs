use cuengine::evaluate_cue_package_typed;
use cuenv_core::Result;
use cuenv_core::manifest::Cuenv;
use glob::glob;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Project {
    pub path: PathBuf,
    pub config: Cuenv,
}

/// Discover all projects in the current repository
///
/// # Errors
/// Returns an error if glob pattern matching fails
///
/// # Panics
/// Panics if the regex pattern is invalid (should not happen as it is hardcoded)
pub fn discover_projects() -> Result<Vec<Project>> {
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
        if let Ok(mut config) = evaluate_cue_package_typed::<Cuenv>(dir_path, "cuenv") {
            // Expand cross-project references and implicit dependencies
            config.expand_cross_project_references();

            projects.push(Project {
                path: entry,
                config,
            });
        }
    }

    Ok(projects)
}
