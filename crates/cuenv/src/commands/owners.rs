//! Code owners CLI commands.
//!
//! This module provides the implementation for:
//! - `cuenv sync codeowners` - Sync CODEOWNERS file from CUE configuration
//! - `cuenv sync codeowners --check` - Check CODEOWNERS file is in sync with CUE configuration

use cuengine::CueEvaluator;
use cuenv_core::Result;
use cuenv_core::manifest::Cuenv;
use cuenv_core::owners::Owners;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Execute the `owners sync` command.
///
/// Generates a CODEOWNERS file from the CUE configuration and writes it to the repository.
///
/// # Errors
///
/// Returns an error if the configuration cannot be loaded or the file cannot be written.
#[allow(clippy::unused_async)] // Async for API consistency with other commands
pub async fn execute_owners_sync(path: &str, package: &str, dry_run: bool) -> Result<String> {
    let root = Path::new(path);

    // Load the CUE configuration
    let owners = load_owners_config(root, package)?;

    if owners.rules.is_empty() && owners.default_owners.is_none() {
        return Err(cuenv_core::Error::configuration(
            "No code ownership rules defined in configuration. Add 'owners' section to your env.cue file.",
        ));
    }

    // Validate rules have at least one owner
    for (i, rule) in owners.rules.iter().enumerate() {
        if rule.owners.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "Rule {} (pattern '{}') has no owners defined. Each rule must have at least one owner.",
                i + 1,
                rule.pattern
            )));
        }
    }

    // Generate CODEOWNERS content
    let content = owners.generate();

    // Validate output path to prevent path traversal attacks
    let output_path = validate_output_path(root, Path::new(owners.output_path()))?;

    if dry_run {
        let mut result = format!("Would write to: {}\n\n", output_path.display());
        result.push_str("--- Content ---\n");
        result.push_str(&content);
        return Ok(result);
    }

    // Ensure parent directory exists
    if let Some(parent) = output_path.parent()
        && !parent.exists()
    {
        fs::create_dir_all(parent).map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(parent.to_path_buf().into_boxed_path()),
            operation: "create directory".to_string(),
        })?;
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
#[allow(clippy::unused_async)] // Async for API consistency with other commands
pub async fn execute_owners_check(path: &str, package: &str) -> Result<String> {
    let root = Path::new(path);

    // Load the CUE configuration
    let owners = load_owners_config(root, package)?;

    if owners.rules.is_empty() && owners.default_owners.is_none() {
        return Ok(
            "No code ownership rules defined in configuration. Nothing to check.".to_string(),
        );
    }

    // Validate rules have at least one owner
    for (i, rule) in owners.rules.iter().enumerate() {
        if rule.owners.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "Rule {} (pattern '{}') has no owners defined. Each rule must have at least one owner.",
                i + 1,
                rule.pattern
            )));
        }
    }

    // Generate expected CODEOWNERS content
    let expected_content = owners.generate();

    // Validate output path to prevent path traversal attacks
    let output_path = validate_output_path(root, Path::new(owners.output_path()))?;

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

    // Compare (normalize line endings and trailing whitespace for robust comparison)
    let normalize = |s: &str| -> String {
        s.replace("\r\n", "\n")
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    };

    if normalize(&current_content) == normalize(&expected_content) {
        Ok(format!(
            "CODEOWNERS file is in sync: {}",
            output_path.display()
        ))
    } else {
        Err(cuenv_core::Error::configuration(format!(
            "CODEOWNERS file is out of sync at {}. Run 'cuenv sync codeowners' to update it.",
            output_path.display()
        )))
    }
}

/// Load code ownership configuration from CUE.
fn load_owners_config(root: &Path, package: &str) -> Result<Owners> {
    let evaluator = CueEvaluator::builder()
        .build()
        .map_err(super::convert_engine_error)?;
    let manifest: Cuenv = evaluator
        .evaluate_typed(root, package)
        .map_err(super::convert_engine_error)?;

    manifest.owners.ok_or_else(|| {
        cuenv_core::Error::configuration(
            "No 'owners' configuration found in env.cue. Add an 'owners' section with code ownership rules."
        )
    })
}

/// Validate that the output path stays within the repository root.
/// Prevents path traversal attacks (e.g., ../../../../etc/CODEOWNERS).
fn validate_output_path(root: &Path, output_path: &Path) -> Result<PathBuf> {
    // Check for suspicious path components
    for component in output_path.components() {
        if let Component::ParentDir = component {
            return Err(cuenv_core::Error::configuration(
                "Output path cannot contain parent directory references (..). \
                 Configure a path within the repository.",
            ));
        }
    }

    let full_path = root.join(output_path);

    // Canonicalize root to compare
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    // For the output path, we need to handle non-existent directories
    // Walk up until we find an existing parent, then validate
    let mut check_path = full_path.clone();
    while !check_path.exists() {
        if let Some(parent) = check_path.parent() {
            check_path = parent.to_path_buf();
        } else {
            break;
        }
    }

    if check_path.exists() {
        let canonical_check = check_path.canonicalize().map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to resolve output path: {e}"))
        })?;

        if !canonical_check.starts_with(&canonical_root) {
            return Err(cuenv_core::Error::configuration(
                "Output path must be within the repository root. \
                 Path traversal outside repository is not allowed.",
            ));
        }
    }

    Ok(full_path)
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
            rules: vec![OwnerRule {
                pattern: "*.rs".to_string(),
                owners: vec!["@rust-team".to_string()],
                description: Some("Rust files".to_string()),
                section: Some("Backend".to_string()),
            }],
        };

        let content = owners.generate();
        assert!(content.contains("# Test Header"));
        assert!(content.contains("* @default-team"));
        assert!(content.contains("# Backend"));
        assert!(content.contains("# Rust files"));
        assert!(content.contains("*.rs @rust-team"));
    }
}
