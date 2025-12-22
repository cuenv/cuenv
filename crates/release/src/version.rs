//! Version calculation and bumping logic.
//!
//! This module provides semantic versioning support including:
//! - Version parsing and formatting
//! - Version bump calculation based on changesets
//! - Pre-release and build metadata handling

use crate::changeset::BumpType;
use crate::config::ReleasePackagesConfig;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// A semantic version following the `SemVer` 2.0.0 specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Version {
    /// Major version number.
    pub major: u64,
    /// Minor version number.
    pub minor: u64,
    /// Patch version number.
    pub patch: u64,
    /// Pre-release identifier (e.g., "alpha", "beta.1").
    pub prerelease: Option<String>,
    /// Build metadata (e.g., "20230101", "commit.abc123").
    pub build: Option<String>,
}

impl Version {
    /// Create a new version.
    #[must_use]
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
            prerelease: None,
            build: None,
        }
    }

    /// Create a version with a pre-release identifier.
    #[must_use]
    pub fn with_prerelease(mut self, prerelease: impl Into<String>) -> Self {
        self.prerelease = Some(prerelease.into());
        self
    }

    /// Create a version with build metadata.
    #[must_use]
    pub fn with_build(mut self, build: impl Into<String>) -> Self {
        self.build = Some(build.into());
        self
    }

    /// Apply a bump type to this version.
    #[must_use]
    pub fn bump(&self, bump_type: BumpType) -> Self {
        match bump_type {
            BumpType::Major => Self::new(self.major + 1, 0, 0),
            BumpType::Minor => Self::new(self.major, self.minor + 1, 0),
            BumpType::Patch => Self::new(self.major, self.minor, self.patch + 1),
            BumpType::None => self.clone(),
        }
    }

    /// Check if this is a pre-release version.
    #[must_use]
    pub fn is_prerelease(&self) -> bool {
        self.prerelease.is_some()
    }

    /// Check if this is the initial development version (0.x.x).
    #[must_use]
    pub fn is_initial_development(&self) -> bool {
        self.major == 0
    }

    /// Get adjusted bump type for pre-1.0 versions.
    ///
    /// In semver, 0.x.x versions are considered "initial development" where
    /// the public API is not stable. Breaking changes in 0.x.x are conventionally
    /// treated as minor bumps (0.1.0 → 0.2.0) rather than major bumps.
    ///
    /// This method remaps `BumpType::Major` to `BumpType::Minor` for pre-1.0 versions.
    #[must_use]
    pub fn adjusted_bump_type(&self, bump: BumpType) -> BumpType {
        if self.is_initial_development() && bump == BumpType::Major {
            BumpType::Minor
        } else {
            bump
        }
    }
}

impl Default for Version {
    fn default() -> Self {
        Self::new(0, 0, 0)
    }
}

impl FromStr for Version {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        // Remove leading 'v' if present
        let s = s.strip_prefix('v').unwrap_or(s);

        // Split off build metadata
        let (version_pre, build) = match s.split_once('+') {
            Some((v, b)) => (v, Some(b.to_string())),
            None => (s, None),
        };

        // Split off prerelease
        let (version, prerelease) = match version_pre.split_once('-') {
            Some((v, p)) => (v, Some(p.to_string())),
            None => (version_pre, None),
        };

        // Parse major.minor.patch
        let parts: Vec<&str> = version.split('.').collect();
        if parts.len() != 3 {
            return Err(Error::invalid_version(s));
        }

        let major = parts[0]
            .parse()
            .map_err(|_| Error::invalid_version(format!("Invalid major version: {}", parts[0])))?;
        let minor = parts[1]
            .parse()
            .map_err(|_| Error::invalid_version(format!("Invalid minor version: {}", parts[1])))?;
        let patch = parts[2]
            .parse()
            .map_err(|_| Error::invalid_version(format!("Invalid patch version: {}", parts[2])))?;

        Ok(Self {
            major,
            minor,
            patch,
            prerelease,
            build,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(ref pre) = self.prerelease {
            write!(f, "-{pre}")?;
        }
        if let Some(ref build) = self.build {
            write!(f, "+{build}")?;
        }
        Ok(())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        // Compare major, minor, patch
        match self.major.cmp(&other.major) {
            Ordering::Equal => {}
            ord => return ord,
        }
        match self.minor.cmp(&other.minor) {
            Ordering::Equal => {}
            ord => return ord,
        }
        match self.patch.cmp(&other.patch) {
            Ordering::Equal => {}
            ord => return ord,
        }

        // Pre-release versions have lower precedence
        match (&self.prerelease, &other.prerelease) {
            (None, None) => Ordering::Equal,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(a), Some(b)) => a.cmp(b),
        }
        // Build metadata is ignored in comparison
    }
}

/// Calculator for determining new versions based on changesets.
pub struct VersionCalculator {
    /// Current versions of packages.
    current_versions: HashMap<String, Version>,
    /// Package grouping configuration.
    packages_config: ReleasePackagesConfig,
}

impl VersionCalculator {
    /// Create a new version calculator.
    #[must_use]
    pub fn new(
        current_versions: HashMap<String, Version>,
        packages_config: ReleasePackagesConfig,
    ) -> Self {
        Self {
            current_versions,
            packages_config,
        }
    }

    /// Calculate new versions based on bump types.
    ///
    /// This applies the package grouping rules:
    /// - Fixed groups: all packages get the same version (highest bump)
    /// - Linked groups: all packages are bumped together
    /// - Independent: packages are bumped individually
    #[must_use]
    pub fn calculate(&self, bumps: &HashMap<String, BumpType>) -> HashMap<String, Version> {
        let mut new_versions = HashMap::new();
        let mut processed: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Process each package with a bump
        for (package, &bump) in bumps {
            if processed.contains(package) || bump == BumpType::None {
                continue;
            }

            // Check if in a fixed group
            if let Some(group) = self.packages_config.get_fixed_group(package) {
                self.process_fixed_group(group, bumps, &mut new_versions);
                for p in group {
                    processed.insert(p.clone());
                }
            }
            // Check if in a linked group
            else if let Some(group) = self.packages_config.get_linked_group(package) {
                self.process_linked_group(group, bumps, &mut new_versions);
                for p in group {
                    processed.insert(p.clone());
                }
            }
            // Independent package
            else {
                self.process_independent(package, bump, &mut new_versions);
                processed.insert(package.clone());
            }
        }

        new_versions
    }

    /// Process a fixed group (all packages get the same version).
    fn process_fixed_group(
        &self,
        group: &[String],
        bumps: &HashMap<String, BumpType>,
        new_versions: &mut HashMap<String, Version>,
    ) {
        // Find the highest bump in the group
        let max_bump = group
            .iter()
            .filter_map(|p| bumps.get(p))
            .fold(BumpType::None, |acc, &b| acc.max(b));

        if max_bump == BumpType::None {
            return;
        }

        // Find the highest current version in the group
        let max_version = group
            .iter()
            .filter_map(|p| self.current_versions.get(p))
            .max()
            .cloned()
            .unwrap_or_default();

        // Apply the bump and set for all packages
        let new_version = max_version.bump(max_bump);
        for package in group {
            new_versions.insert(package.clone(), new_version.clone());
        }
    }

    /// Process a linked group (all packages are bumped together but can have different versions).
    fn process_linked_group(
        &self,
        group: &[String],
        bumps: &HashMap<String, BumpType>,
        new_versions: &mut HashMap<String, Version>,
    ) {
        // Find the highest bump in the group
        let max_bump = group
            .iter()
            .filter_map(|p| bumps.get(p))
            .fold(BumpType::None, |acc, &b| acc.max(b));

        if max_bump == BumpType::None {
            return;
        }

        // Each package is bumped by the max bump from its own current version
        for package in group {
            let current = self
                .current_versions
                .get(package)
                .cloned()
                .unwrap_or_default();
            new_versions.insert(package.clone(), current.bump(max_bump));
        }
    }

    /// Process an independent package.
    fn process_independent(
        &self,
        package: &str,
        bump: BumpType,
        new_versions: &mut HashMap<String, Version>,
    ) {
        let current = self
            .current_versions
            .get(package)
            .cloned()
            .unwrap_or_default();
        new_versions.insert(package.to_string(), current.bump(bump));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_new() {
        let v = Version::new(1, 2, 3);
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);
        assert!(v.prerelease.is_none());
        assert!(v.build.is_none());
    }

    #[test]
    fn test_version_with_prerelease() {
        let v = Version::new(1, 0, 0).with_prerelease("alpha.1");
        assert_eq!(v.prerelease, Some("alpha.1".to_string()));
    }

    #[test]
    fn test_version_with_build() {
        let v = Version::new(1, 0, 0).with_build("commit.abc123");
        assert_eq!(v.build, Some("commit.abc123".to_string()));
    }

    #[test]
    fn test_version_parse() {
        let v: Version = "1.2.3".parse().unwrap();
        assert_eq!(v, Version::new(1, 2, 3));

        let v: Version = "v1.2.3".parse().unwrap();
        assert_eq!(v, Version::new(1, 2, 3));

        let v: Version = "1.2.3-beta.1".parse().unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.prerelease, Some("beta.1".to_string()));

        let v: Version = "1.2.3+build.123".parse().unwrap();
        assert_eq!(v.build, Some("build.123".to_string()));

        let v: Version = "1.2.3-rc.1+build.456".parse().unwrap();
        assert_eq!(v.prerelease, Some("rc.1".to_string()));
        assert_eq!(v.build, Some("build.456".to_string()));
    }

    #[test]
    fn test_version_parse_invalid() {
        assert!("1.2".parse::<Version>().is_err());
        assert!("1.2.3.4".parse::<Version>().is_err());
        assert!("a.b.c".parse::<Version>().is_err());
    }

    #[test]
    fn test_version_display() {
        assert_eq!(Version::new(1, 2, 3).to_string(), "1.2.3");
        assert_eq!(
            Version::new(1, 2, 3).with_prerelease("alpha").to_string(),
            "1.2.3-alpha"
        );
        assert_eq!(
            Version::new(1, 2, 3).with_build("123").to_string(),
            "1.2.3+123"
        );
        assert_eq!(
            Version::new(1, 2, 3)
                .with_prerelease("beta")
                .with_build("456")
                .to_string(),
            "1.2.3-beta+456"
        );
    }

    #[test]
    fn test_version_bump() {
        let v = Version::new(1, 2, 3);
        assert_eq!(v.bump(BumpType::Patch), Version::new(1, 2, 4));
        assert_eq!(v.bump(BumpType::Minor), Version::new(1, 3, 0));
        assert_eq!(v.bump(BumpType::Major), Version::new(2, 0, 0));
        assert_eq!(v.bump(BumpType::None), Version::new(1, 2, 3));
    }

    #[test]
    fn test_version_ordering() {
        assert!(Version::new(2, 0, 0) > Version::new(1, 0, 0));
        assert!(Version::new(1, 1, 0) > Version::new(1, 0, 0));
        assert!(Version::new(1, 0, 1) > Version::new(1, 0, 0));

        // Pre-release has lower precedence
        assert!(Version::new(1, 0, 0) > Version::new(1, 0, 0).with_prerelease("alpha"));
    }

    #[test]
    fn test_version_is_prerelease() {
        assert!(!Version::new(1, 0, 0).is_prerelease());
        assert!(
            Version::new(1, 0, 0)
                .with_prerelease("alpha")
                .is_prerelease()
        );
    }

    #[test]
    fn test_version_is_initial_development() {
        assert!(Version::new(0, 1, 0).is_initial_development());
        assert!(!Version::new(1, 0, 0).is_initial_development());
    }

    #[test]
    fn test_adjusted_bump_type_pre_1_0() {
        // In pre-1.0 (0.x.x), Major bumps become Minor (breaking changes are minor bumps)
        let v = Version::new(0, 16, 0);
        assert_eq!(v.adjusted_bump_type(BumpType::Major), BumpType::Minor);
        assert_eq!(v.adjusted_bump_type(BumpType::Minor), BumpType::Minor);
        assert_eq!(v.adjusted_bump_type(BumpType::Patch), BumpType::Patch);
        assert_eq!(v.adjusted_bump_type(BumpType::None), BumpType::None);
    }

    #[test]
    fn test_adjusted_bump_type_post_1_0() {
        // In post-1.0 (1.x.x+), Major bumps stay Major
        let v = Version::new(1, 0, 0);
        assert_eq!(v.adjusted_bump_type(BumpType::Major), BumpType::Major);
        assert_eq!(v.adjusted_bump_type(BumpType::Minor), BumpType::Minor);
        assert_eq!(v.adjusted_bump_type(BumpType::Patch), BumpType::Patch);
        assert_eq!(v.adjusted_bump_type(BumpType::None), BumpType::None);

        let v2 = Version::new(2, 5, 3);
        assert_eq!(v2.adjusted_bump_type(BumpType::Major), BumpType::Major);
    }

    #[test]
    fn test_version_calculator_independent() {
        let current = HashMap::from([
            ("pkg-a".to_string(), Version::new(1, 0, 0)),
            ("pkg-b".to_string(), Version::new(2, 0, 0)),
        ]);
        let config = ReleasePackagesConfig::default();
        let calc = VersionCalculator::new(current, config);

        let bumps = HashMap::from([
            ("pkg-a".to_string(), BumpType::Minor),
            ("pkg-b".to_string(), BumpType::Patch),
        ]);

        let new_versions = calc.calculate(&bumps);
        assert_eq!(new_versions.get("pkg-a"), Some(&Version::new(1, 1, 0)));
        assert_eq!(new_versions.get("pkg-b"), Some(&Version::new(2, 0, 1)));
    }

    #[test]
    fn test_version_calculator_fixed_group() {
        let current = HashMap::from([
            ("pkg-a".to_string(), Version::new(1, 0, 0)),
            ("pkg-b".to_string(), Version::new(1, 0, 0)),
        ]);
        let config = ReleasePackagesConfig {
            fixed: vec![vec!["pkg-a".to_string(), "pkg-b".to_string()]],
            ..Default::default()
        };
        let calc = VersionCalculator::new(current, config);

        // Only pkg-a has a bump, but both should be updated
        let bumps = HashMap::from([("pkg-a".to_string(), BumpType::Minor)]);

        let new_versions = calc.calculate(&bumps);
        assert_eq!(new_versions.get("pkg-a"), Some(&Version::new(1, 1, 0)));
        assert_eq!(new_versions.get("pkg-b"), Some(&Version::new(1, 1, 0)));
    }

    #[test]
    fn test_version_calculator_fixed_group_max_bump() {
        let current = HashMap::from([
            ("pkg-a".to_string(), Version::new(1, 0, 0)),
            ("pkg-b".to_string(), Version::new(1, 0, 0)),
        ]);
        let config = ReleasePackagesConfig {
            fixed: vec![vec!["pkg-a".to_string(), "pkg-b".to_string()]],
            ..Default::default()
        };
        let calc = VersionCalculator::new(current, config);

        // Different bumps - should use the highest
        let bumps = HashMap::from([
            ("pkg-a".to_string(), BumpType::Patch),
            ("pkg-b".to_string(), BumpType::Minor),
        ]);

        let new_versions = calc.calculate(&bumps);
        // Both get Minor bump (the higher one)
        assert_eq!(new_versions.get("pkg-a"), Some(&Version::new(1, 1, 0)));
        assert_eq!(new_versions.get("pkg-b"), Some(&Version::new(1, 1, 0)));
    }

    #[test]
    fn test_version_calculator_linked_group() {
        let current = HashMap::from([
            ("pkg-a".to_string(), Version::new(1, 0, 0)),
            ("pkg-b".to_string(), Version::new(2, 0, 0)),
        ]);
        let config = ReleasePackagesConfig {
            linked: vec![vec!["pkg-a".to_string(), "pkg-b".to_string()]],
            ..Default::default()
        };
        let calc = VersionCalculator::new(current, config);

        let bumps = HashMap::from([("pkg-a".to_string(), BumpType::Minor)]);

        let new_versions = calc.calculate(&bumps);
        // Both are bumped by minor, but from their own versions
        assert_eq!(new_versions.get("pkg-a"), Some(&Version::new(1, 1, 0)));
        assert_eq!(new_versions.get("pkg-b"), Some(&Version::new(2, 1, 0)));
    }
}
