//! Release configuration types.
//!
//! This module defines the Rust representations of the release configuration
//! that can be specified in `env.cue` files.

use serde::{Deserialize, Serialize};

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
    /// Tag format template with ${package} and ${version} placeholders.
    #[serde(rename = "tagFormat")]
    pub tag_format: String,
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
            tag_format: "v${version}".to_string(),
            create_tags: true,
            push_tags: true,
        }
    }
}

impl ReleaseGitConfig {
    /// Format a tag name for a package and version.
    ///
    /// Replaces `${package}` and `${version}` placeholders in the tag format.
    #[must_use]
    pub fn format_tag(&self, package: &str, version: &str) -> String {
        self.tag_format
            .replace("${package}", package)
            .replace("${version}", version)
    }
}

/// Package grouping configuration for version management.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReleasePackagesConfig {
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
        assert_eq!(config.git.tag_format, "v${version}");
        assert!(config.git.create_tags);
        assert!(config.git.push_tags);
    }

    #[test]
    fn test_git_config_format_tag() {
        let config = ReleaseGitConfig::default();
        assert_eq!(config.format_tag("my-pkg", "1.0.0"), "v1.0.0");

        let monorepo_config = ReleaseGitConfig {
            tag_format: "${package}-v${version}".to_string(),
            ..Default::default()
        };
        assert_eq!(
            monorepo_config.format_tag("my-pkg", "1.0.0"),
            "my-pkg-v1.0.0"
        );
    }

    #[test]
    fn test_packages_config_fixed_groups() {
        let config = ReleasePackagesConfig {
            fixed: vec![
                vec!["pkg-a".to_string(), "pkg-b".to_string()],
                vec!["pkg-c".to_string()],
            ],
            linked: vec![],
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
            fixed: vec![],
            linked: vec![vec!["pkg-x".to_string(), "pkg-y".to_string()]],
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
