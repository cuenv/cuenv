//! Changeset creation, storage, and parsing.
//!
//! Changesets are Markdown files stored in `.cuenv/changesets/` that describe
//! pending changes for a release. They follow a format similar to Changesets
//! but with cuenv-specific extensions.
//!
//! # Changeset Format
//!
//! ```markdown
//! ---
//! "package-name": minor
//! "another-package": patch
//! ---
//!
//! Summary of the change (first line is the title)
//!
//! Optional longer description with more details.
//! ```

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// The directory name for storing changesets.
pub const CHANGESETS_DIR: &str = ".cuenv/changesets";

/// Type of version bump for a package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BumpType {
    /// No version change.
    None,
    /// Patch version bump (0.0.X).
    Patch,
    /// Minor version bump (0.X.0).
    Minor,
    /// Major version bump (X.0.0).
    Major,
}

impl BumpType {
    /// Parse a bump type from a string.
    ///
    /// # Errors
    ///
    /// Returns an error if the string is not a valid bump type.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "major" => Ok(Self::Major),
            "minor" => Ok(Self::Minor),
            "patch" => Ok(Self::Patch),
            "none" => Ok(Self::None),
            _ => Err(Error::changeset_parse(
                format!("Invalid bump type: {s}. Expected major, minor, patch, or none"),
                None,
            )),
        }
    }

    /// Get the higher of two bump types.
    #[must_use]
    pub fn max(self, other: Self) -> Self {
        if self > other { self } else { other }
    }
}

impl std::fmt::Display for BumpType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::Patch => write!(f, "patch"),
            Self::Minor => write!(f, "minor"),
            Self::Major => write!(f, "major"),
        }
    }
}

/// A change to a specific package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageChange {
    /// The package name or path.
    pub name: String,
    /// The type of version bump.
    pub bump: BumpType,
}

impl PackageChange {
    /// Create a new package change.
    #[must_use]
    pub fn new(name: impl Into<String>, bump: BumpType) -> Self {
        Self {
            name: name.into(),
            bump,
        }
    }
}

/// A changeset describing pending changes for a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changeset {
    /// Unique identifier for this changeset.
    pub id: String,
    /// Summary of the change (first line, used as title).
    pub summary: String,
    /// Packages affected by this change.
    pub packages: Vec<PackageChange>,
    /// Optional longer description.
    pub description: Option<String>,
}

impl Changeset {
    /// Create a new changeset with a generated ID.
    #[must_use]
    pub fn new(
        summary: impl Into<String>,
        packages: Vec<PackageChange>,
        description: Option<String>,
    ) -> Self {
        Self {
            id: Self::generate_id(),
            summary: summary.into(),
            packages,
            description,
        }
    }

    /// Create a changeset with a specific ID.
    #[must_use]
    pub fn with_id(
        id: impl Into<String>,
        summary: impl Into<String>,
        packages: Vec<PackageChange>,
        description: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            summary: summary.into(),
            packages,
            description,
        }
    }

    /// Generate a unique changeset ID.
    #[must_use]
    fn generate_id() -> String {
        // Use UUID v4 for unique IDs, take first 12 hex chars (excluding hyphens) for reasonable brevity
        // while maintaining sufficient entropy to avoid collisions
        Uuid::new_v4()
            .to_string()
            .replace('-', "")
            .chars()
            .take(12)
            .collect()
    }

    /// Parse a changeset from its Markdown content.
    ///
    /// # Errors
    ///
    /// Returns an error if the content is not valid changeset format.
    pub fn parse(content: &str, id: &str) -> Result<Self> {
        let content = content.trim();

        // Must start and end with frontmatter delimiters
        if !content.starts_with("---") {
            return Err(Error::changeset_parse(
                "Changeset must start with '---' frontmatter delimiter",
                None,
            ));
        }

        // Find the end of frontmatter
        let after_first = &content[3..];
        let Some(end_idx) = after_first.find("---") else {
            return Err(Error::changeset_parse(
                "Missing closing '---' frontmatter delimiter",
                None,
            ));
        };

        let frontmatter = after_first[..end_idx].trim();
        let body = after_first[end_idx + 3..].trim();

        // Parse package bumps from frontmatter
        let packages = Self::parse_frontmatter(frontmatter)?;

        // Parse summary and description from body
        let (summary, description) = Self::parse_body(body)?;

        Ok(Self {
            id: id.to_string(),
            summary,
            packages,
            description,
        })
    }

    /// Parse the frontmatter section to extract package bumps.
    fn parse_frontmatter(frontmatter: &str) -> Result<Vec<PackageChange>> {
        let mut packages = Vec::new();

        for line in frontmatter.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Parse "package-name": bump_type
            let Some((name_part, bump_part)) = line.split_once(':') else {
                return Err(Error::changeset_parse(
                    format!("Invalid frontmatter line: {line}. Expected 'package: bump_type'"),
                    None,
                ));
            };

            // Remove quotes from package name
            let name = name_part.trim().trim_matches('"').trim_matches('\'');
            let bump = BumpType::parse(bump_part)?;

            packages.push(PackageChange::new(name, bump));
        }

        Ok(packages)
    }

    /// Parse the body section to extract summary and description.
    fn parse_body(body: &str) -> Result<(String, Option<String>)> {
        if body.is_empty() {
            return Err(Error::changeset_parse(
                "Changeset body cannot be empty",
                None,
            ));
        }

        // First non-empty line is the summary
        let mut lines = body.lines();
        let summary = lines
            .find(|l| !l.trim().is_empty())
            .ok_or_else(|| Error::changeset_parse("Missing changeset summary", None))?
            .trim()
            .to_string();

        // Rest is the description (if any non-empty content)
        let remaining_lines: Vec<&str> = lines.skip_while(|l| l.trim().is_empty()).collect();

        let description = if remaining_lines.is_empty() {
            None
        } else {
            let desc = remaining_lines.join("\n").trim().to_string();
            if desc.is_empty() { None } else { Some(desc) }
        };

        Ok((summary, description))
    }

    /// Convert the changeset to Markdown format.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        use std::fmt::Write;
        let mut output = String::from("---\n");

        // Write package bumps in frontmatter
        for pkg in &self.packages {
            let _ = writeln!(output, "\"{}\": {}", pkg.name, pkg.bump);
        }

        output.push_str("---\n\n");
        output.push_str(&self.summary);
        output.push('\n');

        if let Some(desc) = &self.description {
            output.push('\n');
            output.push_str(desc);
            output.push('\n');
        }

        output
    }

    /// Get the filename for this changeset.
    #[must_use]
    pub fn filename(&self) -> String {
        format!("{}.md", self.id)
    }
}

/// Manager for changeset operations.
pub struct ChangesetManager {
    /// Root directory of the project.
    root: PathBuf,
}

impl ChangesetManager {
    /// Create a new changeset manager for the given project root.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// Get the changesets directory path.
    #[must_use]
    pub fn changesets_dir(&self) -> PathBuf {
        self.root.join(CHANGESETS_DIR)
    }

    /// Ensure the changesets directory exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub fn ensure_dir(&self) -> Result<()> {
        let dir = self.changesets_dir();
        if !dir.exists() {
            fs::create_dir_all(&dir).map_err(|e| {
                Error::changeset_io_with_source(
                    "Failed to create changesets directory",
                    Some(dir),
                    e,
                )
            })?;
        }
        Ok(())
    }

    /// Add a new changeset.
    ///
    /// # Errors
    ///
    /// Returns an error if the changeset cannot be written.
    pub fn add(&self, changeset: &Changeset) -> Result<PathBuf> {
        self.ensure_dir()?;

        let path = self.changesets_dir().join(changeset.filename());
        let content = changeset.to_markdown();

        fs::write(&path, content).map_err(|e| {
            Error::changeset_io_with_source("Failed to write changeset", Some(path.clone()), e)
        })?;

        Ok(path)
    }

    /// List all pending changesets.
    ///
    /// # Errors
    ///
    /// Returns an error if the changesets cannot be read.
    pub fn list(&self) -> Result<Vec<Changeset>> {
        let dir = self.changesets_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut changesets = Vec::new();

        for entry in fs::read_dir(&dir).map_err(|e| {
            Error::changeset_io_with_source(
                "Failed to read changesets directory",
                Some(dir.clone()),
                e,
            )
        })? {
            let entry = entry.map_err(|e| {
                Error::changeset_io_with_source(
                    "Failed to read directory entry",
                    Some(dir.clone()),
                    e,
                )
            })?;

            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                let content = fs::read_to_string(&path).map_err(|e| {
                    Error::changeset_io_with_source(
                        "Failed to read changeset file",
                        Some(path.clone()),
                        e,
                    )
                })?;
                let changeset = Changeset::parse(&content, stem)?;
                changesets.push(changeset);
            }
        }

        Ok(changesets)
    }

    /// Get the aggregate bump type for each package from all changesets.
    ///
    /// # Errors
    ///
    /// Returns an error if the changesets cannot be read.
    pub fn get_package_bumps(&self) -> Result<HashMap<String, BumpType>> {
        let changesets = self.list()?;
        let mut bumps: HashMap<String, BumpType> = HashMap::new();

        for changeset in changesets {
            for pkg_change in changeset.packages {
                let current = bumps
                    .get(&pkg_change.name)
                    .copied()
                    .unwrap_or(BumpType::None);
                bumps.insert(pkg_change.name, current.max(pkg_change.bump));
            }
        }

        Ok(bumps)
    }

    /// Remove a changeset by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the changeset cannot be removed.
    pub fn remove(&self, id: &str) -> Result<()> {
        let path = self.changesets_dir().join(format!("{id}.md"));
        if path.exists() {
            fs::remove_file(&path).map_err(|e| {
                Error::changeset_io_with_source("Failed to remove changeset", Some(path), e)
            })?;
        }
        Ok(())
    }

    /// Remove all changesets.
    ///
    /// # Errors
    ///
    /// Returns an error if the changesets cannot be removed.
    pub fn clear(&self) -> Result<()> {
        let dir = self.changesets_dir();
        if dir.exists() {
            for entry in fs::read_dir(&dir).map_err(|e| {
                Error::changeset_io_with_source(
                    "Failed to read changesets directory",
                    Some(dir.clone()),
                    e,
                )
            })? {
                let entry = entry.map_err(|e| {
                    Error::changeset_io_with_source(
                        "Failed to read directory entry",
                        Some(dir.clone()),
                        e,
                    )
                })?;
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "md") {
                    fs::remove_file(&path).map_err(|e| {
                        Error::changeset_io_with_source("Failed to remove changeset", Some(path), e)
                    })?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_bump_type_parse() {
        assert_eq!(BumpType::parse("major").unwrap(), BumpType::Major);
        assert_eq!(BumpType::parse("Minor").unwrap(), BumpType::Minor);
        assert_eq!(BumpType::parse("PATCH").unwrap(), BumpType::Patch);
        assert_eq!(BumpType::parse("none").unwrap(), BumpType::None);
        assert!(BumpType::parse("invalid").is_err());
    }

    #[test]
    fn test_bump_type_max() {
        assert_eq!(BumpType::None.max(BumpType::Patch), BumpType::Patch);
        assert_eq!(BumpType::Patch.max(BumpType::Minor), BumpType::Minor);
        assert_eq!(BumpType::Minor.max(BumpType::Major), BumpType::Major);
        assert_eq!(BumpType::Major.max(BumpType::None), BumpType::Major);
    }

    #[test]
    fn test_bump_type_display() {
        assert_eq!(BumpType::Major.to_string(), "major");
        assert_eq!(BumpType::Minor.to_string(), "minor");
        assert_eq!(BumpType::Patch.to_string(), "patch");
        assert_eq!(BumpType::None.to_string(), "none");
    }

    #[test]
    fn test_changeset_new() {
        let changeset = Changeset::new(
            "Add feature",
            vec![PackageChange::new("my-pkg", BumpType::Minor)],
            Some("Details".to_string()),
        );

        assert_eq!(changeset.summary, "Add feature");
        assert_eq!(changeset.packages.len(), 1);
        assert!(changeset.description.is_some());
        // ID is 12 hex chars (from UUID with hyphens removed, taking first 12)
        assert_eq!(changeset.id.len(), 12);
    }

    #[test]
    fn test_changeset_parse() {
        let content = r#"---
"my-package": minor
"other-pkg": patch
---

Add a new feature

This is a longer description
with multiple lines.
"#;

        let changeset = Changeset::parse(content, "test-id").unwrap();
        assert_eq!(changeset.id, "test-id");
        assert_eq!(changeset.summary, "Add a new feature");
        assert_eq!(changeset.packages.len(), 2);
        assert_eq!(changeset.packages[0].name, "my-package");
        assert_eq!(changeset.packages[0].bump, BumpType::Minor);
        assert_eq!(changeset.packages[1].name, "other-pkg");
        assert_eq!(changeset.packages[1].bump, BumpType::Patch);
        assert!(changeset.description.is_some());
        assert!(changeset.description.unwrap().contains("multiple lines"));
    }

    #[test]
    fn test_changeset_parse_no_description() {
        let content = r#"---
"pkg": major
---

Breaking change summary
"#;

        let changeset = Changeset::parse(content, "id").unwrap();
        assert_eq!(changeset.summary, "Breaking change summary");
        assert!(changeset.description.is_none());
    }

    #[test]
    fn test_changeset_to_markdown() {
        let changeset = Changeset::with_id(
            "abc123",
            "Fix bug",
            vec![PackageChange::new("my-pkg", BumpType::Patch)],
            None,
        );

        let md = changeset.to_markdown();
        assert!(md.contains("---"));
        assert!(md.contains("\"my-pkg\": patch"));
        assert!(md.contains("Fix bug"));
    }

    #[test]
    fn test_changeset_roundtrip() {
        let original = Changeset::with_id(
            "roundtrip",
            "Test summary",
            vec![
                PackageChange::new("pkg-a", BumpType::Minor),
                PackageChange::new("pkg-b", BumpType::Patch),
            ],
            Some("Extended description".to_string()),
        );

        let markdown = original.to_markdown();
        let parsed = Changeset::parse(&markdown, "roundtrip").unwrap();

        assert_eq!(parsed.id, original.id);
        assert_eq!(parsed.summary, original.summary);
        assert_eq!(parsed.packages.len(), original.packages.len());
        assert_eq!(parsed.description, original.description);
    }

    #[test]
    fn test_changeset_manager_add_list() {
        let temp = TempDir::new().unwrap();
        let manager = ChangesetManager::new(temp.path());

        let changeset = Changeset::with_id(
            "test-cs",
            "Test change",
            vec![PackageChange::new("pkg", BumpType::Minor)],
            None,
        );

        manager.add(&changeset).unwrap();

        let list = manager.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "test-cs");
    }

    #[test]
    fn test_changeset_manager_get_package_bumps() {
        let temp = TempDir::new().unwrap();
        let manager = ChangesetManager::new(temp.path());

        // Add two changesets affecting the same package
        let cs1 = Changeset::with_id(
            "cs1",
            "Small fix",
            vec![PackageChange::new("pkg", BumpType::Patch)],
            None,
        );
        let cs2 = Changeset::with_id(
            "cs2",
            "New feature",
            vec![PackageChange::new("pkg", BumpType::Minor)],
            None,
        );

        manager.add(&cs1).unwrap();
        manager.add(&cs2).unwrap();

        let bumps = manager.get_package_bumps().unwrap();
        // Should be Minor since it's the highest
        assert_eq!(bumps.get("pkg"), Some(&BumpType::Minor));
    }

    #[test]
    fn test_changeset_manager_remove() {
        let temp = TempDir::new().unwrap();
        let manager = ChangesetManager::new(temp.path());

        let changeset = Changeset::with_id(
            "to-remove",
            "Will be removed",
            vec![PackageChange::new("pkg", BumpType::Patch)],
            None,
        );

        manager.add(&changeset).unwrap();
        assert_eq!(manager.list().unwrap().len(), 1);

        manager.remove("to-remove").unwrap();
        assert_eq!(manager.list().unwrap().len(), 0);
    }

    #[test]
    fn test_changeset_manager_clear() {
        let temp = TempDir::new().unwrap();
        let manager = ChangesetManager::new(temp.path());

        for i in 0..3 {
            let changeset = Changeset::with_id(
                format!("cs-{i}"),
                format!("Change {i}"),
                vec![PackageChange::new("pkg", BumpType::Patch)],
                None,
            );
            manager.add(&changeset).unwrap();
        }

        assert_eq!(manager.list().unwrap().len(), 3);
        manager.clear().unwrap();
        assert_eq!(manager.list().unwrap().len(), 0);
    }
}
