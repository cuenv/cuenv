use cuengine::evaluate_cue_package_typed;
use cuenv_core::Result;
use cuenv_core::manifest::Cuenv;
use glob::glob;
use std::path::PathBuf;

pub struct Project {
    pub path: PathBuf,
    pub config: Cuenv,
}

/// Discover all projects in the current repository
///
/// # Errors
/// Returns an error if glob pattern matching fails
pub fn discover_projects() -> Result<Vec<Project>> {
    let mut projects = Vec::new();

    // Glob for all env.cue files
    let entries = glob("**/env.cue").map_err(|e| cuenv_core::Error::Configuration {
        src: String::new(),
        span: None,
        message: format!("Glob error: {e}"),
    })?;

    for entry in entries.flatten() {
        let parent_dir = entry.parent().unwrap_or(&entry);

        // Load the configuration
        // We assume the package name is "cuenv" based on convention
        match evaluate_cue_package_typed::<Cuenv>(parent_dir, "cuenv") {
            Ok(config) => {
                projects.push(Project {
                    path: entry,
                    config,
                });
            }
            Err(_e) => {
                // In discovery phase, we might want to skip invalid configs or log a warning
                // For now, we'll skip but could log if we had a logger set up here
                // println!("Warning: Failed to load config at {:?}: {}", entry, e);

                // However, if we want to be robust, we should probably return a Result with warnings?
                // But to keep signature simple, let's just push what works.
                // Or maybe we return a wrapper that includes errors?

                // Let's stick to "skip if invalid" for now, as that's common for discovery.
                // If the user explicitly runs on a path, that's different.
                continue;
            }
        }
    }

    Ok(projects)
}
