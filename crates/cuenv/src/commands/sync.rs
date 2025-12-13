//! Sync command implementation for generating files from CUE configuration.
//!
//! Currently supports generating ignore files (.gitignore, .dockerignore, etc.)
//! from the `ignore` field in the CUE configuration.

use cuengine::{CueEvaluator, Cuenv};
use cuenv_core::Result;
use cuenv_ignore::{FileStatus, IgnoreConfig};
use std::path::Path;
use tracing::instrument;

/// Convert manifest ignore configuration to cuenv_ignore configs.
fn convert_to_ignore_configs(manifest: &Cuenv) -> Vec<IgnoreConfig> {
    let Some(ignore) = &manifest.ignore else {
        return Vec::new();
    };

    ignore
        .iter()
        .map(|(tool, value)| IgnoreConfig {
            tool: tool.clone(),
            patterns: value.patterns().to_vec(),
            filename: value.filename().map(String::from),
        })
        .collect()
}

/// Execute the sync command.
///
/// Reads the CUE configuration and generates ignore files based on the `ignore` field.
#[instrument(name = "sync")]
pub async fn execute_sync(path: &str, package: &str, dry_run: bool) -> Result<String> {
    tracing::info!("Starting sync command");

    // Create CUE evaluator
    let evaluator = CueEvaluator::builder().build()?;

    // Convert path string to Path
    let dir_path = Path::new(path);

    // Evaluate the CUE package
    tracing::debug!("Evaluating CUE package '{}' at path '{}'", package, path);
    let manifest: Cuenv = evaluator.evaluate_typed(dir_path, package)?;

    // Convert to ignore configs
    let configs = convert_to_ignore_configs(&manifest);

    if configs.is_empty() {
        tracing::info!("No ignore patterns configured");
        return Ok(
            "No ignore patterns configured. Add an `ignore` field to your env.cue.".to_string(),
        );
    }

    // Check if all configs have empty patterns
    let all_empty = configs.iter().all(|c| c.patterns.is_empty());
    if all_empty {
        return Ok("No ignore files to generate (all pattern lists are empty).".to_string());
    }

    // Generate ignore files using the cuenv-ignore crate
    let result = cuenv_ignore::generate_ignore_files(dir_path, configs, dry_run).map_err(|e| {
        match e {
            cuenv_ignore::Error::NotInGitRepo => {
                cuenv_core::Error::configuration("cuenv sync must be run within a Git repository")
            }
            cuenv_ignore::Error::BareRepository => {
                cuenv_core::Error::configuration("Cannot sync in a bare Git repository")
            }
            cuenv_ignore::Error::OutsideGitRepo => cuenv_core::Error::configuration(
                "Target directory must be within the Git repository",
            ),
            cuenv_ignore::Error::InvalidToolName { name, reason } => {
                cuenv_core::Error::configuration(format!("Invalid tool name '{name}': {reason}"))
            }
            cuenv_ignore::Error::Io(io_err) => cuenv_core::Error::Io {
                source: io_err,
                path: Some(dir_path.to_path_buf().into_boxed_path()),
                operation: "sync ignore files".to_string(),
            },
        }
    })?;

    // Format output
    let mut output_lines = Vec::new();

    for file in &result.files {
        let status_str = match file.status {
            FileStatus::Created => format!("Created {} ({} patterns)", file.filename, file.pattern_count),
            FileStatus::Updated => format!("Updated {} ({} patterns)", file.filename, file.pattern_count),
            FileStatus::Unchanged => format!("Unchanged {}", file.filename),
            FileStatus::WouldCreate => format!("Would create {} ({} patterns)", file.filename, file.pattern_count),
            FileStatus::WouldUpdate => format!("Would update {} ({} patterns)", file.filename, file.pattern_count),
        };
        output_lines.push(status_str);
    }

    if output_lines.is_empty() {
        return Ok("No ignore files to generate (all pattern lists are empty).".to_string());
    }

    let output = output_lines.join("\n");
    tracing::info!("Sync command completed successfully");
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::manifest::{IgnoreEntry, IgnoreValue};
    use std::collections::HashMap;

    #[test]
    fn test_convert_to_ignore_configs_empty() {
        let manifest = Cuenv::new("test");
        let configs = convert_to_ignore_configs(&manifest);
        assert!(configs.is_empty());
    }

    #[test]
    fn test_convert_to_ignore_configs_simple_patterns() {
        let mut manifest = Cuenv::new("test");
        let mut ignore = HashMap::new();
        ignore.insert(
            "git".to_string(),
            IgnoreValue::Patterns(vec!["node_modules/".to_string(), ".env".to_string()]),
        );
        manifest.ignore = Some(ignore);

        let configs = convert_to_ignore_configs(&manifest);
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].tool, "git");
        assert_eq!(configs[0].patterns, vec!["node_modules/", ".env"]);
        assert!(configs[0].filename.is_none());
    }

    #[test]
    fn test_convert_to_ignore_configs_extended_with_filename() {
        let mut manifest = Cuenv::new("test");
        let mut ignore = HashMap::new();
        ignore.insert(
            "custom".to_string(),
            IgnoreValue::Extended(IgnoreEntry {
                patterns: vec!["*.tmp".to_string()],
                filename: Some(".myignore".to_string()),
            }),
        );
        manifest.ignore = Some(ignore);

        let configs = convert_to_ignore_configs(&manifest);
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].tool, "custom");
        assert_eq!(configs[0].patterns, vec!["*.tmp"]);
        assert_eq!(configs[0].filename, Some(".myignore".to_string()));
    }
}
