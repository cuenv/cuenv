//! Changelog generation and formatting.
//!
//! This module provides utilities for generating and updating CHANGELOG.md files
//! based on consumed changesets.

use crate::changeset::{BumpType, Changeset};
use crate::config::ChangelogConfig;
use crate::error::{Error, Result};
use crate::version::Version;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// A single entry in the changelog.
#[derive(Debug, Clone)]
pub struct ChangelogEntry {
    /// Version being released.
    pub version: Version,
    /// Release date.
    pub date: DateTime<Utc>,
    /// Changes organized by category.
    pub changes: Vec<ChangelogChange>,
}

/// A single change in the changelog.
#[derive(Debug, Clone)]
pub struct ChangelogChange {
    /// Type of change (major, minor, patch).
    pub bump_type: BumpType,
    /// Summary of the change.
    pub summary: String,
    /// Detailed description (optional).
    pub description: Option<String>,
    /// Packages affected by this change.
    pub packages: Vec<String>,
}

impl ChangelogEntry {
    /// Create a new changelog entry.
    #[must_use]
    pub const fn new(version: Version, date: DateTime<Utc>, changes: Vec<ChangelogChange>) -> Self {
        Self {
            version,
            date,
            changes,
        }
    }

    /// Format this entry as Markdown.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        use std::fmt::Write;
        let mut output = String::new();

        // Version header
        let date_str = self.date.format("%Y-%m-%d").to_string();
        let _ = writeln!(output, "## [{}] - {}\n", self.version, date_str);

        // Group changes by type
        let mut major_changes = Vec::new();
        let mut minor_changes = Vec::new();
        let mut patch_changes = Vec::new();

        for change in &self.changes {
            match change.bump_type {
                BumpType::Major => major_changes.push(change),
                BumpType::Minor => minor_changes.push(change),
                BumpType::Patch => patch_changes.push(change),
                BumpType::None => {} // Skip none changes
            }
        }

        // Write sections
        if !major_changes.is_empty() {
            output.push_str("### Breaking Changes\n\n");
            for change in major_changes {
                output.push_str(&format_change(change));
            }
            output.push('\n');
        }

        if !minor_changes.is_empty() {
            output.push_str("### Features\n\n");
            for change in minor_changes {
                output.push_str(&format_change(change));
            }
            output.push('\n');
        }

        if !patch_changes.is_empty() {
            output.push_str("### Fixes\n\n");
            for change in patch_changes {
                output.push_str(&format_change(change));
            }
            output.push('\n');
        }

        output
    }
}

/// Format a single change as a Markdown list item.
fn format_change(change: &ChangelogChange) -> String {
    use std::fmt::Write;
    let mut output = String::new();

    // Package prefix if multiple packages
    if change.packages.len() > 1 {
        let _ = writeln!(
            output,
            "- **[{}]** {}",
            change.packages.join(", "),
            change.summary
        );
    } else if !change.packages.is_empty() {
        let _ = writeln!(output, "- **{}**: {}", change.packages[0], change.summary);
    } else {
        let _ = writeln!(output, "- {}", change.summary);
    }

    // Add description if present
    if let Some(ref desc) = change.description {
        for line in desc.lines() {
            let _ = writeln!(output, "  {line}");
        }
    }

    output
}

/// Generator for changelog files.
pub struct ChangelogGenerator {
    /// Changelog configuration.
    config: ChangelogConfig,
}

impl ChangelogGenerator {
    /// Create a new changelog generator with the given configuration.
    #[must_use]
    pub const fn new(config: ChangelogConfig) -> Self {
        Self { config }
    }

    /// Create a changelog generator with default configuration.
    #[must_use]
    pub fn default_config() -> Self {
        Self::new(ChangelogConfig::default())
    }

    /// Generate changelog entries from changesets for a specific package.
    #[must_use]
    pub fn generate_entries(
        &self,
        changesets: &[Changeset],
        package: &str,
        version: &Version,
    ) -> Option<ChangelogEntry> {
        let changes: Vec<ChangelogChange> = changesets
            .iter()
            .filter_map(|cs| {
                // Find if this changeset affects our package
                let pkg_change = cs.packages.iter().find(|p| p.name == package)?;

                Some(ChangelogChange {
                    bump_type: pkg_change.bump,
                    summary: cs.summary.clone(),
                    description: cs.description.clone(),
                    packages: vec![package.to_string()],
                })
            })
            .collect();

        if changes.is_empty() {
            return None;
        }

        Some(ChangelogEntry::new(version.clone(), Utc::now(), changes))
    }

    /// Generate a workspace-level changelog entry.
    #[must_use]
    pub fn generate_workspace_entry(
        &self,
        changesets: &[Changeset],
        new_versions: &HashMap<String, Version>,
    ) -> Option<ChangelogEntry> {
        if changesets.is_empty() {
            return None;
        }

        let changes: Vec<ChangelogChange> = changesets
            .iter()
            .map(|cs| {
                // Get the highest bump type from all packages
                let bump_type = cs
                    .packages
                    .iter()
                    .map(|p| p.bump)
                    .max()
                    .unwrap_or(BumpType::None);

                let packages: Vec<String> = cs.packages.iter().map(|p| p.name.clone()).collect();

                ChangelogChange {
                    bump_type,
                    summary: cs.summary.clone(),
                    description: cs.description.clone(),
                    packages,
                }
            })
            .collect();

        // Use the highest version as the workspace version
        let version = new_versions
            .values()
            .max()
            .cloned()
            .unwrap_or_else(|| Version::new(0, 1, 0));

        Some(ChangelogEntry::new(version, Utc::now(), changes))
    }

    /// Update a changelog file with a new entry.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or written.
    pub fn update_file(&self, path: &Path, entry: &ChangelogEntry) -> Result<()> {
        let new_content = entry.to_markdown();

        let existing = if path.exists() {
            fs::read_to_string(path).map_err(|e| {
                Error::changeset_io_with_source(
                    format!("Failed to read changelog: {}", path.display()),
                    Some(path.to_path_buf()),
                    e,
                )
            })?
        } else {
            // Create initial changelog structure
            "# Changelog\n\nAll notable changes to this project will be documented in this file.\n\n".to_string()
        };

        // Insert new content after the header
        let content = existing.find("\n## ").map_or_else(
            // No existing entries, append to end
            || format!("{}\n{}", existing.trim_end(), new_content),
            |idx| {
                format!("{}{}{}", &existing[..idx], "\n", new_content.trim_end()) + &existing[idx..]
            },
        );

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                Error::changeset_io_with_source(
                    format!("Failed to create directory: {}", parent.display()),
                    Some(parent.to_path_buf()),
                    e,
                )
            })?;
        }

        fs::write(path, content).map_err(|e| {
            Error::changeset_io_with_source(
                format!("Failed to write changelog: {}", path.display()),
                Some(path.to_path_buf()),
                e,
            )
        })?;

        Ok(())
    }

    /// Get the changelog file path for a package.
    #[must_use]
    pub fn get_changelog_path(&self, package_root: &Path) -> std::path::PathBuf {
        package_root.join(&self.config.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changeset::PackageChange;
    use tempfile::TempDir;

    #[test]
    fn test_changelog_entry_to_markdown() {
        let version = Version::new(1, 2, 0);
        let changes = vec![
            ChangelogChange {
                bump_type: BumpType::Minor,
                summary: "Add new feature".to_string(),
                description: None,
                packages: vec!["my-pkg".to_string()],
            },
            ChangelogChange {
                bump_type: BumpType::Patch,
                summary: "Fix bug".to_string(),
                description: Some("Detailed description".to_string()),
                packages: vec!["my-pkg".to_string()],
            },
        ];

        let entry = ChangelogEntry::new(version, Utc::now(), changes);
        let md = entry.to_markdown();

        assert!(md.contains("## [1.2.0]"));
        assert!(md.contains("### Features"));
        assert!(md.contains("Add new feature"));
        assert!(md.contains("### Fixes"));
        assert!(md.contains("Fix bug"));
        assert!(md.contains("Detailed description"));
    }

    #[test]
    fn test_changelog_entry_breaking_changes() {
        let version = Version::new(2, 0, 0);
        let changes = vec![ChangelogChange {
            bump_type: BumpType::Major,
            summary: "Breaking API change".to_string(),
            description: None,
            packages: vec!["my-pkg".to_string()],
        }];

        let entry = ChangelogEntry::new(version, Utc::now(), changes);
        let md = entry.to_markdown();

        assert!(md.contains("### Breaking Changes"));
        assert!(md.contains("Breaking API change"));
    }

    #[test]
    fn test_format_change_multiple_packages() {
        let change = ChangelogChange {
            bump_type: BumpType::Minor,
            summary: "Shared feature".to_string(),
            description: None,
            packages: vec!["pkg-a".to_string(), "pkg-b".to_string()],
        };

        let formatted = format_change(&change);
        assert!(formatted.contains("[pkg-a, pkg-b]"));
    }

    #[test]
    fn test_changelog_generator_generate_entries() {
        let generator = ChangelogGenerator::default_config();

        let changesets = vec![
            Changeset::with_id(
                "cs1",
                "Add feature A",
                vec![PackageChange::new("my-pkg", BumpType::Minor)],
                None,
            ),
            Changeset::with_id(
                "cs2",
                "Fix bug B",
                vec![PackageChange::new("my-pkg", BumpType::Patch)],
                None,
            ),
            Changeset::with_id(
                "cs3",
                "Other package change",
                vec![PackageChange::new("other-pkg", BumpType::Minor)],
                None,
            ),
        ];

        let version = Version::new(1, 1, 0);
        let entry = generator.generate_entries(&changesets, "my-pkg", &version);

        assert!(entry.is_some());
        let entry = entry.unwrap();
        assert_eq!(entry.version, version);
        assert_eq!(entry.changes.len(), 2);
    }

    #[test]
    fn test_changelog_generator_no_changes_for_package() {
        let generator = ChangelogGenerator::default_config();

        let changesets = vec![Changeset::with_id(
            "cs1",
            "Other change",
            vec![PackageChange::new("other-pkg", BumpType::Minor)],
            None,
        )];

        let version = Version::new(1, 0, 0);
        let entry = generator.generate_entries(&changesets, "my-pkg", &version);

        assert!(entry.is_none());
    }

    #[test]
    fn test_changelog_generator_update_file_new() {
        let temp = TempDir::new().unwrap();
        let changelog_path = temp.path().join("CHANGELOG.md");

        let generator = ChangelogGenerator::default_config();
        let entry = ChangelogEntry::new(
            Version::new(1, 0, 0),
            Utc::now(),
            vec![ChangelogChange {
                bump_type: BumpType::Minor,
                summary: "Initial release".to_string(),
                description: None,
                packages: vec!["pkg".to_string()],
            }],
        );

        generator.update_file(&changelog_path, &entry).unwrap();

        let content = fs::read_to_string(&changelog_path).unwrap();
        assert!(content.contains("# Changelog"));
        assert!(content.contains("## [1.0.0]"));
        assert!(content.contains("Initial release"));
    }

    #[test]
    fn test_changelog_generator_update_file_existing() {
        let temp = TempDir::new().unwrap();
        let changelog_path = temp.path().join("CHANGELOG.md");

        // Create initial changelog
        let initial = r"# Changelog

All notable changes to this project will be documented in this file.

## [0.1.0] - 2023-01-01

### Features

- Initial version
";
        fs::write(&changelog_path, initial).unwrap();

        let generator = ChangelogGenerator::default_config();
        let entry = ChangelogEntry::new(
            Version::new(0, 2, 0),
            Utc::now(),
            vec![ChangelogChange {
                bump_type: BumpType::Minor,
                summary: "New feature".to_string(),
                description: None,
                packages: vec!["pkg".to_string()],
            }],
        );

        generator.update_file(&changelog_path, &entry).unwrap();

        let content = fs::read_to_string(&changelog_path).unwrap();
        assert!(content.contains("## [0.2.0]"));
        assert!(content.contains("## [0.1.0]"));
        // New entry should come before old one
        assert!(content.find("## [0.2.0]").unwrap() < content.find("## [0.1.0]").unwrap());
    }
}
