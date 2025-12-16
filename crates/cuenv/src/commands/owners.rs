//! Code owners CLI commands.
//!
//! This module provides the implementation for:
//! - `cuenv sync codeowners` - Sync CODEOWNERS file from CUE configuration
//! - `cuenv sync codeowners --check` - Check CODEOWNERS file is in sync with CUE configuration

use crate::providers::detect_codeowners_provider;
use cuengine::CueEvaluator;
use cuenv_codeowners::Rule;
use cuenv_codeowners::provider::{ProjectOwners, SyncStatus};
use cuenv_core::Result;
use cuenv_core::manifest::Base;
use cuenv_core::owners::Owners;
use std::path::{Path, PathBuf};

/// Execute the `owners sync` command.
///
/// Generates a CODEOWNERS file from the CUE configuration and writes it to the repository.
/// Uses the provider system to write to the correct location based on platform.
///
/// # Errors
///
/// Returns an error if the configuration cannot be loaded or the file cannot be written.
#[allow(clippy::unused_async)] // Async for API consistency with other commands
pub async fn execute_owners_sync(path: &str, package: &str, dry_run: bool) -> Result<String> {
    let project_root = Path::new(path);

    // Load the CUE configuration (uses Base schema - works with or without project name)
    let owners = load_owners_config(project_root, package)?;

    if owners.rules.is_empty() && owners.default_owners.is_none() {
        return Err(cuenv_core::Error::configuration(
            "No code ownership rules defined in configuration. Add 'owners' section to your env.cue file.",
        ));
    }

    // Validate rules have at least one owner
    for (key, rule) in &owners.rules {
        if rule.owners.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "Rule '{}' (pattern '{}') has no owners defined. Each rule must have at least one owner.",
                key,
                rule.pattern
            )));
        }
    }

    // Find the repo root (cue.mod directory)
    let repo_root =
        find_cue_module_root(project_root).unwrap_or_else(|| project_root.to_path_buf());

    // Calculate relative path from repo root to project
    let relative_path = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf())
        .strip_prefix(
            repo_root
                .canonicalize()
                .unwrap_or_else(|_| repo_root.clone()),
        )
        .unwrap_or(Path::new(""))
        .to_path_buf();

    // Convert to provider types (derives project name from directory)
    let project_owners = convert_to_project_owners(project_root, &owners, relative_path);

    // Detect provider based on repo structure
    let provider = detect_codeowners_provider(&repo_root);

    // Sync using the provider
    let result = provider
        .sync(&repo_root, &[project_owners], dry_run)
        .map_err(|e| cuenv_core::Error::configuration(e.to_string()))?;

    let output = if dry_run {
        // Dry-run output format for backward compatibility with tests
        format!(
            "Would write to: {}\n\n--- Content ---\n{}",
            result.path.display(),
            result.content
        )
    } else {
        let status_msg = match result.status {
            SyncStatus::Created => "Created",
            SyncStatus::Updated => "Updated",
            SyncStatus::Unchanged => "Unchanged",
            SyncStatus::WouldCreate => "Would create",
            SyncStatus::WouldUpdate => "Would update",
        };
        format!("{} CODEOWNERS: {}", status_msg, result.path.display())
    };

    Ok(output)
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
    let project_root = Path::new(path);

    // Load the CUE configuration (uses Base schema - works with or without project name)
    let owners = load_owners_config(project_root, package)?;

    if owners.rules.is_empty() && owners.default_owners.is_none() {
        return Ok(
            "No code ownership rules defined in configuration. Nothing to check.".to_string(),
        );
    }

    // Validate rules have at least one owner
    for (key, rule) in &owners.rules {
        if rule.owners.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "Rule '{}' (pattern '{}') has no owners defined. Each rule must have at least one owner.",
                key,
                rule.pattern
            )));
        }
    }

    // Find the repo root (cue.mod directory)
    let repo_root =
        find_cue_module_root(project_root).unwrap_or_else(|| project_root.to_path_buf());

    // Calculate relative path from repo root to project
    let relative_path = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf())
        .strip_prefix(
            repo_root
                .canonicalize()
                .unwrap_or_else(|_| repo_root.clone()),
        )
        .unwrap_or(Path::new(""))
        .to_path_buf();

    // Convert to provider types (derives project name from directory)
    let project_owners = convert_to_project_owners(project_root, &owners, relative_path);

    // Detect provider based on repo structure
    let provider = detect_codeowners_provider(&repo_root);

    // Check using the provider
    let result = provider
        .check(&repo_root, &[project_owners])
        .map_err(|e| cuenv_core::Error::configuration(e.to_string()))?;

    if result.in_sync {
        Ok(format!(
            "CODEOWNERS file is in sync: {}",
            result.path.display()
        ))
    } else if result.actual.is_none() {
        Err(cuenv_core::Error::configuration(format!(
            "CODEOWNERS file not found at {}. Run 'cuenv sync codeowners' to generate it.",
            result.path.display()
        )))
    } else {
        Err(cuenv_core::Error::configuration(format!(
            "CODEOWNERS file is out of sync at {}. Run 'cuenv sync codeowners' to update it.",
            result.path.display()
        )))
    }
}

/// Convert manifest owners configuration to provider `ProjectOwners` type.
/// For Base configs (without a name), the project name is derived from the directory.
fn convert_to_project_owners(
    project_root: &Path,
    owners: &Owners,
    relative_path: PathBuf,
) -> ProjectOwners {
    // Sort rules by order then by key for determinism
    let mut rule_entries: Vec<_> = owners.rules.iter().collect();
    rule_entries.sort_by(|a, b| {
        let order_a = a.1.order.unwrap_or(i32::MAX);
        let order_b = b.1.order.unwrap_or(i32::MAX);
        order_a.cmp(&order_b).then_with(|| a.0.cmp(b.0))
    });

    let rules: Vec<Rule> = rule_entries
        .iter()
        .map(|(_key, r)| {
            let mut rule = Rule::new(&r.pattern, r.owners.clone());
            if let Some(ref desc) = r.description {
                rule = rule.description(desc.clone());
            }
            if let Some(ref section) = r.section {
                rule = rule.section(section.clone());
            }
            rule
        })
        .collect();

    // Use directory name as project name for Base configs
    let project_name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string();

    let mut project_owners = ProjectOwners::new(relative_path, project_name, rules);

    if let Some(ref default_owners) = owners.default_owners {
        project_owners = project_owners.with_default_owners(default_owners.clone());
    }

    project_owners
}

/// Find the CUE module root by walking up from the given path.
fn find_cue_module_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join("cue.mod").is_dir() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Load code ownership configuration from CUE using Base schema.
/// Works with both schema.#Base and schema.#Project configurations.
fn load_owners_config(root: &Path, package: &str) -> Result<Owners> {
    let evaluator = CueEvaluator::builder()
        .build()
        .map_err(super::convert_engine_error)?;
    let manifest: Base = evaluator
        .evaluate_typed(root, package)
        .map_err(super::convert_engine_error)?;

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
    use std::collections::HashMap;

    #[test]
    fn test_owners_generate_content() {
        let mut rules = HashMap::new();
        rules.insert(
            "rust-files".to_string(),
            OwnerRule {
                pattern: "*.rs".to_string(),
                owners: vec!["@rust-team".to_string()],
                description: Some("Rust files".to_string()),
                section: Some("Backend".to_string()),
                order: None,
            },
        );

        let owners = Owners {
            output: Some(OwnersOutput {
                platform: Some(Platform::Github),
                path: None,
                header: Some("Test Header".to_string()),
            }),
            default_owners: Some(vec!["@default-team".to_string()]),
            rules,
        };

        let content = owners.generate();
        assert!(content.contains("# Test Header"));
        assert!(content.contains("* @default-team"));
        assert!(content.contains("# Backend"));
        assert!(content.contains("# Rust files"));
        assert!(content.contains("*.rs @rust-team"));
    }
}
