//! Code owners CLI commands.
//!
//! This module provides the implementation for:
//! - `cuenv sync codeowners` - Sync CODEOWNERS file from CUE configuration
//! - `cuenv sync codeowners --check` - Check CODEOWNERS file is in sync with CUE configuration

use crate::commands::CommandExecutor;
use crate::commands::env_file::find_cue_module_root;
use crate::providers::detect_code_owners_provider;
use cuenv_codeowners::Rule;
use cuenv_codeowners::provider::{ProjectOwners, SyncStatus};
use cuenv_core::Result;
use cuenv_core::owners::Owners;
use std::path::{Path, PathBuf};

/// Execute the `owners sync` command.
///
/// Generates a CODEOWNERS file from the CUE configuration and writes it to the repository.
/// Uses the provider system to write to the correct location based on platform.
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
///
/// # Errors
///
/// Returns an error if the configuration cannot be loaded or the file cannot be written.
#[allow(clippy::unused_async)] // Async for API consistency with other commands
#[allow(dead_code)]
pub async fn execute_owners_sync(
    path: &str,
    package: &str,
    dry_run: bool,
    executor: Option<&CommandExecutor>,
) -> Result<String> {
    let project_root = Path::new(path);

    // Load the CUE configuration (uses Base schema - works with or without project name)
    let owners = load_owners_config(project_root, package, executor)?;

    if owners.rules.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "No code ownership rules defined in configuration. Add 'owners.rules' section to your env.cue file.",
        ));
    }

    // Validate rules have at least one owner
    for (key, rule) in &owners.rules {
        if rule.owners.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "Rule '{}' (pattern '{}') has no owners defined. Each rule must have at least one owner.",
                key, rule.pattern
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
    let provider = detect_code_owners_provider(&repo_root);

    // Sync using the provider
    let result = provider
        .sync(&repo_root, &[project_owners], dry_run)
        .map_err(|e| cuenv_core::Error::configuration(e.to_string()))?;

    let display_path = result.path.strip_prefix(&repo_root).unwrap_or(&result.path);
    let output = if dry_run {
        // Dry-run output format for backward compatibility with tests
        format!(
            "Would write to: {}\n\n--- Content ---\n{}",
            display_path.display(),
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
        format!("{} CODEOWNERS: {}", status_msg, display_path.display())
    };

    Ok(output)
}

/// Execute the `owners check` command.
///
/// Checks if the CODEOWNERS file is in sync with the CUE configuration.
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
///
/// # Errors
///
/// Returns an error if the configuration cannot be loaded or the files are out of sync.
#[allow(clippy::unused_async)] // Async for API consistency with other commands
#[allow(dead_code)]
pub async fn execute_owners_check(
    path: &str,
    package: &str,
    executor: Option<&CommandExecutor>,
) -> Result<String> {
    let project_root = Path::new(path);

    // Load the CUE configuration (uses Base schema - works with or without project name)
    let owners = load_owners_config(project_root, package, executor)?;

    if owners.rules.is_empty() {
        return Ok(
            "No code ownership rules defined in configuration. Nothing to check.".to_string(),
        );
    }

    // Validate rules have at least one owner
    for (key, rule) in &owners.rules {
        if rule.owners.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "Rule '{}' (pattern '{}') has no owners defined. Each rule must have at least one owner.",
                key, rule.pattern
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
    let provider = detect_code_owners_provider(&repo_root);

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
#[allow(dead_code)]
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

    ProjectOwners::new(relative_path, project_name, rules)
}

/// Load code ownership configuration from CUE using module-wide evaluation.
///
/// **DEPRECATED**: CODEOWNERS configuration has moved to .rules.cue files.
/// Use `cuenv sync rules` instead.
#[allow(dead_code)]
fn load_owners_config(
    _root: &Path,
    _package: &str,
    _executor: Option<&CommandExecutor>,
) -> Result<Owners> {
    // Owners configuration has moved to .rules.cue files
    Err(cuenv_core::Error::configuration(
        "CODEOWNERS configuration has moved to .rules.cue files. Use 'cuenv sync rules' instead.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_codeowners::{CodeOwners, SectionStyle};
    use cuenv_core::owners::{OwnerRule, OwnersOutput};
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
                platform: Some("github".to_string()),
                path: None,
                header: Some("Test Header".to_string()),
            }),
            rules,
        };

        // Build CodeOwners using the builder pattern
        let codeowners = CodeOwners::builder()
            .section_style(SectionStyle::Comment)
            .header(owners.header().unwrap_or(""))
            .rules(owners.sorted_rules().into_iter().map(|(_key, r)| {
                let mut rule = Rule::new(&r.pattern, r.owners.clone());
                if let Some(ref desc) = r.description {
                    rule = rule.description(desc.clone());
                }
                if let Some(ref section) = r.section {
                    rule = rule.section(section.clone());
                }
                rule
            }))
            .build();

        let content = codeowners.generate();
        assert!(content.contains("# Test Header"));
        assert!(content.contains("# Backend"));
        assert!(content.contains("# Rust files"));
        assert!(content.contains("*.rs @rust-team"));
    }
}
