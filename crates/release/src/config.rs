//! Release configuration types.
//!
//! This module defines the Rust representations of the release configuration
//! that can be specified in `env.cue` files.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Versioning strategy for monorepo packages.
///
/// Determines how package versions are managed when changes are detected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VersioningStrategy {
    /// All packages share the same version (lockstep versioning).
    ///
    /// When any package changes, all packages are bumped to the same
    /// new version using the maximum bump type detected.
    Fixed,

    /// Packages are bumped together but can have different versions.
    ///
    /// All packages get the same bump type applied, but each package
    /// applies it to its own current version.
    Linked,

    /// Each package is versioned independently (default).
    ///
    /// Only packages that have changes are bumped, and each package
    /// gets its own bump type based on the changes affecting it.
    #[default]
    Independent,
}

impl fmt::Display for VersioningStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixed => write!(f, "fixed"),
            Self::Linked => write!(f, "linked"),
            Self::Independent => write!(f, "independent"),
        }
    }
}

/// Version tag type for release tags.
///
/// Determines how version strings are parsed and compared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TagType {
    /// Semantic versioning (e.g., 0.19.1, 1.0.0-alpha.1).
    #[default]
    Semver,

    /// Calendar versioning (e.g., 2024.12.23, 24.04).
    Calver,
}

impl fmt::Display for TagType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Semver => write!(f, "semver"),
            Self::Calver => write!(f, "calver"),
        }
    }
}

/// Complete release configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReleaseConfig {
    /// Git-related release settings.
    pub git: ReleaseGitConfig,
    /// Package grouping configuration.
    pub packages: ReleasePackagesConfig,
    /// Changelog generation configuration.
    pub changelog: ChangelogConfig,
}

/// Git-related release configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReleaseGitConfig {
    /// Default branch for releases.
    #[serde(rename = "defaultBranch")]
    pub default_branch: String,
    /// Tag prefix for version tags (default: empty for bare versions).
    #[serde(rename = "tagPrefix")]
    pub tag_prefix: String,
    /// Version tag type (semver or calver).
    #[serde(rename = "tagType")]
    pub tag_type: TagType,
    /// Whether to create tags during release.
    #[serde(rename = "createTags")]
    pub create_tags: bool,
    /// Whether to push tags to remote.
    #[serde(rename = "pushTags")]
    pub push_tags: bool,
}

impl Default for ReleaseGitConfig {
    fn default() -> Self {
        Self {
            default_branch: "main".to_string(),
            tag_prefix: String::new(),
            tag_type: TagType::Semver,
            create_tags: true,
            push_tags: true,
        }
    }
}

impl ReleaseGitConfig {
    /// Format a tag name from a version.
    ///
    /// Combines the tag prefix with the version string.
    #[must_use]
    pub fn format_tag(&self, version: &str) -> String {
        format!("{}{}", self.tag_prefix, version)
    }
}

/// Package grouping configuration for version management.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReleasePackagesConfig {
    /// Default versioning strategy for packages not in explicit groups.
    pub strategy: VersioningStrategy,
    /// Fixed groups: packages that share the same version (lockstep versioning).
    pub fixed: Vec<Vec<String>>,
    /// Linked groups: packages that are bumped together but can have different versions.
    pub linked: Vec<Vec<String>>,
}

impl ReleasePackagesConfig {
    /// Check if a package is in a fixed group.
    #[must_use]
    pub fn is_in_fixed_group(&self, package: &str) -> bool {
        self.fixed
            .iter()
            .any(|group| group.contains(&package.to_string()))
    }

    /// Get the fixed group containing a package, if any.
    #[must_use]
    pub fn get_fixed_group(&self, package: &str) -> Option<&Vec<String>> {
        self.fixed
            .iter()
            .find(|group| group.contains(&package.to_string()))
    }

    /// Check if a package is in a linked group.
    #[must_use]
    pub fn is_in_linked_group(&self, package: &str) -> bool {
        self.linked
            .iter()
            .any(|group| group.contains(&package.to_string()))
    }

    /// Get the linked group containing a package, if any.
    #[must_use]
    pub fn get_linked_group(&self, package: &str) -> Option<&Vec<String>> {
        self.linked
            .iter()
            .find(|group| group.contains(&package.to_string()))
    }
}

/// Changelog generation configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChangelogConfig {
    /// Path to the CHANGELOG file relative to project/package root.
    pub path: String,
    /// Whether to generate changelogs for each package.
    #[serde(rename = "perPackage")]
    pub per_package: bool,
    /// Whether to generate a root changelog for the entire workspace.
    pub workspace: bool,
}

impl Default for ChangelogConfig {
    fn default() -> Self {
        Self {
            path: "CHANGELOG.md".to_string(),
            per_package: true,
            workspace: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_release_config_default() {
        let config = ReleaseConfig::default();
        assert_eq!(config.git.default_branch, "main");
        assert_eq!(config.git.tag_prefix, "");
        assert_eq!(config.git.tag_type, TagType::Semver);
        assert!(config.git.create_tags);
        assert!(config.git.push_tags);
    }

    #[test]
    fn test_git_config_format_tag() {
        // Default: empty prefix (bare semver)
        let config = ReleaseGitConfig::default();
        assert_eq!(config.format_tag("1.0.0"), "1.0.0");

        // With "v" prefix
        let v_config = ReleaseGitConfig {
            tag_prefix: "v".to_string(),
            ..Default::default()
        };
        assert_eq!(v_config.format_tag("1.0.0"), "v1.0.0");

        // With package prefix
        let pkg_config = ReleaseGitConfig {
            tag_prefix: "vscode/v".to_string(),
            ..Default::default()
        };
        assert_eq!(pkg_config.format_tag("0.1.1"), "vscode/v0.1.1");
    }

    #[test]
    fn test_tag_type_default() {
        assert_eq!(TagType::default(), TagType::Semver);
    }

    #[test]
    fn test_packages_config_fixed_groups() {
        let config = ReleasePackagesConfig {
            fixed: vec![
                vec!["pkg-a".to_string(), "pkg-b".to_string()],
                vec!["pkg-c".to_string()],
            ],
            ..Default::default()
        };

        assert!(config.is_in_fixed_group("pkg-a"));
        assert!(config.is_in_fixed_group("pkg-b"));
        assert!(config.is_in_fixed_group("pkg-c"));
        assert!(!config.is_in_fixed_group("pkg-d"));

        let group = config.get_fixed_group("pkg-a").unwrap();
        assert!(group.contains(&"pkg-a".to_string()));
        assert!(group.contains(&"pkg-b".to_string()));
    }

    #[test]
    fn test_packages_config_linked_groups() {
        let config = ReleasePackagesConfig {
            linked: vec![vec!["pkg-x".to_string(), "pkg-y".to_string()]],
            ..Default::default()
        };

        assert!(config.is_in_linked_group("pkg-x"));
        assert!(config.is_in_linked_group("pkg-y"));
        assert!(!config.is_in_linked_group("pkg-z"));
    }

    #[test]
    fn test_changelog_config_default() {
        let config = ChangelogConfig::default();
        assert_eq!(config.path, "CHANGELOG.md");
        assert!(config.per_package);
        assert!(config.workspace);
    }

    #[test]
    fn test_config_serialization() {
        let config = ReleaseConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: ReleaseConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.git.default_branch, config.git.default_branch);
    }
}
