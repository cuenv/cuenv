//! Code owners CLI commands.
//!
//! This module provides the implementation for:
//! - `cuenv sync codeowners` - Sync CODEOWNERS file from CUE configuration
//! - `cuenv sync codeowners --check` - Check CODEOWNERS file is in sync with CUE configuration

use cuengine::{CueEvaluator, Cuenv};
use cuenv_core::owners::Owners;
use cuenv_core::Result;
use std::fs;
use std::path::Path;

/// Execute the `owners sync` command.
///
/// Generates a CODEOWNERS file from the CUE configuration and writes it to the repository.
///
/// # Errors
///
/// Returns an error if the configuration cannot be loaded or the file cannot be written.
pub async fn execute_owners_sync(
    path: &str,
    package: &str,
    dry_run: bool,
) -> Result<String> {
    let root = Path::new(path);

    // Load the CUE configuration
    let owners = load_owners_config(root, package)?;

    if owners.rules.is_empty() && owners.default_owners.is_none() {
        return Err(cuenv_core::Error::configuration(
            "No code ownership rules defined in configuration. Add 'owners' section to your env.cue file."
        ));
    }

    // Generate CODEOWNERS content
    let content = owners.generate();
    let output_path = root.join(owners.output_path());

    if dry_run {
        let mut result = format!("Would write to: {}\n\n", output_path.display());
        result.push_str("--- Content ---\n");
        result.push_str(&content);
        return Ok(result);
    }

    // Ensure parent directory exists
    if let Some(parent) = output_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| cuenv_core::Error::Io {
                source: e,
                path: Some(parent.to_path_buf().into_boxed_path()),
                operation: "create directory".to_string(),
            })?;
        }
    }

    // Write the file
    fs::write(&output_path, &content).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(output_path.clone().into_boxed_path()),
        operation: "write CODEOWNERS file".to_string(),
    })?;

    Ok(format!("Wrote CODEOWNERS to: {}", output_path.display()))
}

/// Execute the `owners check` command.
///
/// Checks if the CODEOWNERS file is in sync with the CUE configuration.
///
/// # Errors
///
/// Returns an error if the configuration cannot be loaded or the files are out of sync.
pub async fn execute_owners_check(path: &str, package: &str) -> Result<String> {
    let root = Path::new(path);

    // Load the CUE configuration
    let owners = load_owners_config(root, package)?;

    if owners.rules.is_empty() && owners.default_owners.is_none() {
        return Ok("No code ownership rules defined in configuration. Nothing to check.".to_string());
    }

    // Generate expected CODEOWNERS content
    let expected_content = owners.generate();
    let output_path = root.join(owners.output_path());

    // Check if file exists
    if !output_path.exists() {
        return Err(cuenv_core::Error::configuration(format!(
            "CODEOWNERS file not found at {}. Run 'cuenv sync codeowners' to generate it.",
            output_path.display()
        )));
    }

    // Read current content
    let current_content = fs::read_to_string(&output_path).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(output_path.clone().into_boxed_path()),
        operation: "read CODEOWNERS file".to_string(),
    })?;

    // Compare
    if current_content == expected_content {
        Ok(format!("CODEOWNERS file is in sync: {}", output_path.display()))
    } else {
        Err(cuenv_core::Error::configuration(format!(
            "CODEOWNERS file is out of sync at {}. Run 'cuenv sync codeowners' to update it.",
            output_path.display()
        )))
    }
}

/// Load code ownership configuration from CUE.
fn load_owners_config(root: &Path, package: &str) -> Result<Owners> {
    let evaluator = CueEvaluator::builder().build()?;
    let manifest: Cuenv = evaluator.evaluate_typed(root, package)?;

    manifest.owners.ok_or_else(|| {
        cuenv_core::Error::configuration(
            "No 'owners' configuration found in env.cue. Add an 'owners' section with code ownership rules."
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::owners::{OwnerRule, OwnersOutput, Platform};

    #[test]
    fn test_owners_generate_content() {
        let owners = Owners {
            output: Some(OwnersOutput {
                platform: Some(Platform::Github),
                path: None,
                header: Some("Test Header".to_string()),
            }),
            default_owners: Some(vec!["@default-team".to_string()]),
            rules: vec![
                OwnerRule {
                    pattern: "*.rs".to_string(),
                    owners: vec!["@rust-team".to_string()],
                    description: Some("Rust files".to_string()),
                    section: Some("Backend".to_string()),
                },
            ],
        };

        let content = owners.generate();
        assert!(content.contains("# Test Header"));
        assert!(content.contains("* @default-team"));
        assert!(content.contains("# Backend"));
        assert!(content.contains("# Rust files"));
        assert!(content.contains("*.rs @rust-team"));
    }
}
