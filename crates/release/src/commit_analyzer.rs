//! Commit analysis for per-package versioning.
//!
//! This module analyzes git commits to determine which packages are affected
//! by each commit. It uses file diffs to map changes to package boundaries,
//! enabling independent per-package version bumps in monorepos.

use crate::changeset::BumpType;
use crate::conventional::ConventionalCommit;
use crate::error::{Error, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A commit that affected a specific package.
#[derive(Debug, Clone)]
pub struct PackageAffect<'a> {
    /// The commit that caused this affect.
    pub commit: &'a ConventionalCommit,
    /// Files changed in this package by the commit.
    pub changed_files: Vec<PathBuf>,
}

impl PackageAffect<'_> {
    /// Get the bump type for this affect.
    #[must_use]
    pub fn bump_type(&self) -> BumpType {
        self.commit.bump_type()
    }
}

/// Analyzes commits to determine which packages they affect.
///
/// This analyzer uses git diffs to map commits to packages based on
/// which files were modified in each commit.
pub struct CommitAnalyzer<'a> {
    root: &'a Path,
    /// Map of package names to their root paths (relative to workspace root).
    package_paths: HashMap<String, PathBuf>,
}

impl<'a> CommitAnalyzer<'a> {
    /// Create a new commit analyzer.
    ///
    /// # Arguments
    ///
    /// * `root` - The workspace root path.
    /// * `package_paths` - Map of package names to their paths (relative to root).
    #[must_use]
    pub const fn new(root: &'a Path, package_paths: HashMap<String, PathBuf>) -> Self {
        Self {
            root,
            package_paths,
        }
    }

    /// Analyze commits and map them to affected packages.
    ///
    /// Returns a map of package names to the commits that affected them,
    /// along with the specific files changed in that package.
    ///
    /// # Errors
    ///
    /// Returns an error if git operations fail when analyzing commits.
    pub fn analyze<'c>(
        &self,
        commits: &'c [ConventionalCommit],
    ) -> Result<HashMap<String, Vec<PackageAffect<'c>>>> {
        let mut package_commits: HashMap<String, Vec<PackageAffect<'c>>> = HashMap::new();

        for commit in commits {
            let changed_files = self.get_changed_files(&commit.hash)?;
            let affected = self.map_files_to_packages(&changed_files);

            for (pkg_name, pkg_files) in affected {
                let affect = PackageAffect {
                    commit,
                    changed_files: pkg_files,
                };
                package_commits.entry(pkg_name).or_default().push(affect);
            }
        }

        Ok(package_commits)
    }

    /// Calculate the aggregate bump type per package from analyzed commits.
    ///
    /// Returns a map of package names to their maximum bump type.
    ///
    /// # Errors
    ///
    /// Returns an error if git operations fail when analyzing commits.
    pub fn calculate_bumps(
        &self,
        commits: &[ConventionalCommit],
    ) -> Result<HashMap<String, BumpType>> {
        let package_affects = self.analyze(commits)?;

        let mut bumps = HashMap::new();
        for (pkg_name, affects) in package_affects {
            let max_bump = affects
                .iter()
                .map(PackageAffect::bump_type)
                .max()
                .unwrap_or(BumpType::None);

            if max_bump != BumpType::None {
                bumps.insert(pkg_name, max_bump);
            }
        }

        Ok(bumps)
    }

    /// Get the files changed in a specific commit.
    ///
    /// Uses `git diff-tree` to list files changed in the commit.
    /// For root commits (no parent), uses `--root` flag to show all added files.
    fn get_changed_files(&self, commit_hash: &str) -> Result<Vec<PathBuf>> {
        // Use git diff-tree to get changed files
        // --no-commit-id: Don't print commit hash
        // --name-only: Only print file names
        // -r: Recurse into subtrees
        // --root: For root commits (no parent), show files as additions
        let output = Command::new("git")
            .args([
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-r",
                "--root",
                commit_hash,
            ])
            .current_dir(self.root)
            .output()
            .map_err(|e| Error::git(format!("Failed to run git diff-tree: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::git(format!(
                "git diff-tree failed for {commit_hash}: {stderr}"
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let files: Vec<PathBuf> = stdout
            .lines()
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .collect();

        Ok(files)
    }

    /// Map changed files to packages.
    ///
    /// Returns a map of package names to the files that changed in that package.
    fn map_files_to_packages(&self, files: &[PathBuf]) -> HashMap<String, Vec<PathBuf>> {
        let mut package_files: HashMap<String, Vec<PathBuf>> = HashMap::new();

        for file in files {
            if let Some(pkg_name) = self.file_to_package(file) {
                package_files
                    .entry(pkg_name)
                    .or_default()
                    .push(file.clone());
            }
        }

        package_files
    }

    /// Determine which package owns a file path.
    ///
    /// Returns `None` if the file doesn't belong to any package
    /// (e.g., root-level config files).
    fn file_to_package(&self, file_path: &Path) -> Option<String> {
        // Find the package whose path is a prefix of this file path
        // Use longest match to handle nested packages correctly
        let mut best_match: Option<(&String, usize)> = None;

        for (pkg_name, pkg_path) in &self.package_paths {
            // Normalize the package path (make it relative if absolute)
            let relative_pkg_path = if pkg_path.is_absolute() {
                pkg_path.strip_prefix(self.root).unwrap_or(pkg_path)
            } else {
                pkg_path.as_path()
            };

            if file_path.starts_with(relative_pkg_path) {
                let path_len = relative_pkg_path.components().count();
                if best_match.as_ref().map_or(true, |(_, prev_len)| path_len > *prev_len) {
                    best_match = Some((pkg_name, path_len));
                }
            }
        }

        best_match.map(|(name, _)| name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn create_test_workspace(temp: &TempDir) -> PathBuf {
        let root = temp.path().to_path_buf();

        // Create workspace structure
        fs::create_dir_all(root.join("crates/foo/src")).unwrap();
        fs::create_dir_all(root.join("crates/bar/src")).unwrap();

        // Create root Cargo.toml
        let root_manifest = r#"[workspace]
resolver = "2"
members = ["crates/foo", "crates/bar"]

[workspace.package]
version = "1.0.0"
"#;
        fs::write(root.join("Cargo.toml"), root_manifest).unwrap();

        // Create package manifests
        fs::write(
            root.join("crates/foo/Cargo.toml"),
            "[package]\nname = \"foo\"\nversion.workspace = true\n",
        )
        .unwrap();
        fs::write(
            root.join("crates/bar/Cargo.toml"),
            "[package]\nname = \"bar\"\nversion.workspace = true\n",
        )
        .unwrap();

        // Create source files
        fs::write(root.join("crates/foo/src/lib.rs"), "// foo lib").unwrap();
        fs::write(root.join("crates/bar/src/lib.rs"), "// bar lib").unwrap();

        root
    }

    fn init_git_repo(path: &Path) {
        Command::new("git")
            .args(["init", "--ref-format=files"])
            .current_dir(path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(path)
            .output()
            .unwrap();
    }

    fn create_commit(path: &Path, message: &str) -> String {
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["commit", "--no-gpg-sign", "-m", message])
            .current_dir(path)
            .output()
            .unwrap();

        // Get the commit hash
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(path)
            .output()
            .unwrap();

        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn test_file_to_package() {
        let temp = TempDir::new().unwrap();
        let root = create_test_workspace(&temp);

        let package_paths = HashMap::from([
            ("foo".to_string(), PathBuf::from("crates/foo")),
            ("bar".to_string(), PathBuf::from("crates/bar")),
        ]);

        let analyzer = CommitAnalyzer::new(&root, package_paths);

        // Test files in packages
        assert_eq!(
            analyzer.file_to_package(Path::new("crates/foo/src/lib.rs")),
            Some("foo".to_string())
        );
        assert_eq!(
            analyzer.file_to_package(Path::new("crates/bar/Cargo.toml")),
            Some("bar".to_string())
        );

        // Test root-level files (not in any package)
        assert_eq!(analyzer.file_to_package(Path::new("Cargo.toml")), None);
        assert_eq!(analyzer.file_to_package(Path::new("README.md")), None);
    }

    #[test]
    fn test_analyze_commits_per_package() {
        let temp = TempDir::new().unwrap();
        let root = create_test_workspace(&temp);
        init_git_repo(&root);

        // Initial commit
        let _hash1 = create_commit(&root, "feat: initial commit");

        // Modify only foo
        fs::write(root.join("crates/foo/src/lib.rs"), "// foo updated").unwrap();
        let hash2 = create_commit(&root, "fix: update foo");

        // Modify only bar
        fs::write(root.join("crates/bar/src/lib.rs"), "// bar updated").unwrap();
        let hash3 = create_commit(&root, "feat: update bar");

        let package_paths = HashMap::from([
            ("foo".to_string(), PathBuf::from("crates/foo")),
            ("bar".to_string(), PathBuf::from("crates/bar")),
        ]);

        let commits = vec![
            ConventionalCommit {
                commit_type: "fix".to_string(),
                scope: None,
                breaking: false,
                description: "update foo".to_string(),
                body: None,
                hash: hash2,
            },
            ConventionalCommit {
                commit_type: "feat".to_string(),
                scope: None,
                breaking: false,
                description: "update bar".to_string(),
                body: None,
                hash: hash3,
            },
        ];

        let analyzer = CommitAnalyzer::new(&root, package_paths);
        let bumps = analyzer.calculate_bumps(&commits).unwrap();

        // foo should get patch (from fix)
        assert_eq!(bumps.get("foo"), Some(&BumpType::Patch));
        // bar should get minor (from feat)
        assert_eq!(bumps.get("bar"), Some(&BumpType::Minor));
    }

    #[test]
    fn test_analyze_commit_affecting_multiple_packages() {
        let temp = TempDir::new().unwrap();
        let root = create_test_workspace(&temp);
        init_git_repo(&root);

        // Initial commit
        create_commit(&root, "chore: initial");

        // Modify both packages in one commit
        fs::write(root.join("crates/foo/src/lib.rs"), "// foo v2").unwrap();
        fs::write(root.join("crates/bar/src/lib.rs"), "// bar v2").unwrap();
        let hash = create_commit(&root, "feat: update both");

        let package_paths = HashMap::from([
            ("foo".to_string(), PathBuf::from("crates/foo")),
            ("bar".to_string(), PathBuf::from("crates/bar")),
        ]);

        let commits = vec![ConventionalCommit {
            commit_type: "feat".to_string(),
            scope: None,
            breaking: false,
            description: "update both".to_string(),
            body: None,
            hash,
        }];

        let analyzer = CommitAnalyzer::new(&root, package_paths);
        let result = analyzer.analyze(&commits).unwrap();

        // Both packages should be affected
        assert!(result.contains_key("foo"));
        assert!(result.contains_key("bar"));
    }

    #[test]
    fn test_root_files_not_mapped() {
        let temp = TempDir::new().unwrap();
        let root = create_test_workspace(&temp);
        init_git_repo(&root);

        // Initial commit
        create_commit(&root, "chore: initial");

        // Modify only root-level file
        fs::write(root.join("README.md"), "# Updated").unwrap();
        let hash = create_commit(&root, "docs: update readme");

        let package_paths = HashMap::from([
            ("foo".to_string(), PathBuf::from("crates/foo")),
            ("bar".to_string(), PathBuf::from("crates/bar")),
        ]);

        let commits = vec![ConventionalCommit {
            commit_type: "docs".to_string(),
            scope: None,
            breaking: false,
            description: "update readme".to_string(),
            body: None,
            hash,
        }];

        let analyzer = CommitAnalyzer::new(&root, package_paths);
        let result = analyzer.analyze(&commits).unwrap();

        // No packages should be affected
        assert!(result.is_empty());
    }
}
