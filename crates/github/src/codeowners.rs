//! GitHub CODEOWNERS provider.
//!
//! GitHub requires a single CODEOWNERS file at one of these locations:
//! - `.github/CODEOWNERS` (most common)
//! - `docs/CODEOWNERS`
//! - `CODEOWNERS` (repository root)
//!
//! This provider aggregates all project ownership rules into a single file
//! at `.github/CODEOWNERS`, with patterns prefixed by project paths.

use cuenv_codeowners::SectionStyle;
use cuenv_codeowners::provider::{
    CheckResult, CodeOwnersProvider, ProjectOwners, ProviderError, Result, SyncResult,
    generate_aggregated_content, write_codeowners_file,
};
use std::fs;
use std::path::Path;

/// GitHub CODEOWNERS provider.
///
/// Writes a single aggregated CODEOWNERS file to `.github/CODEOWNERS`.
/// Uses comment-style sections (`# Section Name`).
#[derive(Debug, Clone, Copy, Default)]
pub struct GitHubCodeOwnersProvider;

impl CodeOwnersProvider for GitHubCodeOwnersProvider {
    fn output_path(&self) -> &str {
        ".github/CODEOWNERS"
    }

    fn section_style(&self) -> SectionStyle {
        SectionStyle::Comment
    }

    fn sync(
        &self,
        repo_root: &Path,
        projects: &[ProjectOwners],
        dry_run: bool,
    ) -> Result<SyncResult> {
        if projects.is_empty() {
            return Err(ProviderError::Configuration(
                "No projects with ownership configuration provided".to_string(),
            ));
        }

        // Generate aggregated content
        let content = generate_aggregated_content(self.section_style(), projects, None);

        // Output path is always at repo root
        let output_path = repo_root.join(self.output_path());

        // Write the file
        let status = write_codeowners_file(&output_path, &content, dry_run)?;

        Ok(SyncResult {
            path: output_path,
            status,
            content,
        })
    }

    fn check(&self, repo_root: &Path, projects: &[ProjectOwners]) -> Result<CheckResult> {
        if projects.is_empty() {
            return Err(ProviderError::Configuration(
                "No projects with ownership configuration provided".to_string(),
            ));
        }

        // Generate expected content
        let expected = generate_aggregated_content(self.section_style(), projects, None);

        let output_path = repo_root.join(self.output_path());

        // Read actual content if file exists
        let actual = if output_path.exists() {
            Some(fs::read_to_string(&output_path)?)
        } else {
            None
        };

        // Compare (normalize line endings)
        let normalize = |s: &str| -> String {
            s.replace("\r\n", "\n")
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n")
        };

        let in_sync = actual
            .as_ref()
            .is_some_and(|a| normalize(a) == normalize(&expected));

        Ok(CheckResult {
            path: output_path,
            in_sync,
            expected,
            actual,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_codeowners::Rule;
    use cuenv_codeowners::provider::SyncStatus;
    use tempfile::tempdir;

    #[test]
    fn test_github_provider_output_path() {
        let provider = GitHubCodeOwnersProvider;
        assert_eq!(provider.output_path(), ".github/CODEOWNERS");
    }

    #[test]
    fn test_github_provider_section_style() {
        let provider = GitHubCodeOwnersProvider;
        assert_eq!(provider.section_style(), SectionStyle::Comment);
    }

    #[test]
    fn test_github_sync_creates_file() {
        let temp = tempdir().unwrap();
        let provider = GitHubCodeOwnersProvider;

        let projects = vec![ProjectOwners::new(
            "services/api",
            "services/api",
            vec![Rule::new("*.rs", ["@backend-team"])],
        )];

        let result = provider.sync(temp.path(), &projects, false).unwrap();

        assert_eq!(result.status, SyncStatus::Created);
        assert!(result.path.ends_with(".github/CODEOWNERS"));
        assert!(result.content.contains("/services/api/*.rs @backend-team"));

        // Verify file was written
        let file_content = fs::read_to_string(&result.path).unwrap();
        assert_eq!(file_content, result.content);
    }

    #[test]
    fn test_github_sync_dry_run() {
        let temp = tempdir().unwrap();
        let provider = GitHubCodeOwnersProvider;

        let projects = vec![ProjectOwners::new(
            "services/api",
            "services/api",
            vec![Rule::new("*.rs", ["@backend-team"])],
        )];

        let result = provider.sync(temp.path(), &projects, true).unwrap();

        assert_eq!(result.status, SyncStatus::WouldCreate);
        // File should NOT exist in dry-run mode
        assert!(!result.path.exists());
    }

    #[test]
    fn test_github_sync_updates_file() {
        let temp = tempdir().unwrap();
        let provider = GitHubCodeOwnersProvider;

        // Create initial file
        let github_dir = temp.path().join(".github");
        fs::create_dir_all(&github_dir).unwrap();
        fs::write(github_dir.join("CODEOWNERS"), "# Old content\n").unwrap();

        let projects = vec![ProjectOwners::new(
            "services/api",
            "services/api",
            vec![Rule::new("*.rs", ["@backend-team"])],
        )];

        let result = provider.sync(temp.path(), &projects, false).unwrap();

        assert_eq!(result.status, SyncStatus::Updated);
    }

    #[test]
    fn test_github_sync_unchanged() {
        let temp = tempdir().unwrap();
        let provider = GitHubCodeOwnersProvider;

        let projects = vec![ProjectOwners::new(
            "services/api",
            "services/api",
            vec![Rule::new("*.rs", ["@backend-team"])],
        )];

        // First sync creates the file
        let result1 = provider.sync(temp.path(), &projects, false).unwrap();
        assert_eq!(result1.status, SyncStatus::Created);

        // Second sync should be unchanged
        let result2 = provider.sync(temp.path(), &projects, false).unwrap();
        assert_eq!(result2.status, SyncStatus::Unchanged);
    }

    #[test]
    fn test_github_check_in_sync() {
        let temp = tempdir().unwrap();
        let provider = GitHubCodeOwnersProvider;

        let projects = vec![ProjectOwners::new(
            "services/api",
            "services/api",
            vec![Rule::new("*.rs", ["@backend-team"])],
        )];

        // Sync first
        provider.sync(temp.path(), &projects, false).unwrap();

        // Check should report in sync
        let result = provider.check(temp.path(), &projects).unwrap();
        assert!(result.in_sync);
    }

    #[test]
    fn test_github_check_out_of_sync() {
        let temp = tempdir().unwrap();
        let provider = GitHubCodeOwnersProvider;

        // Create file with different content
        let github_dir = temp.path().join(".github");
        fs::create_dir_all(&github_dir).unwrap();
        fs::write(github_dir.join("CODEOWNERS"), "# Different content\n").unwrap();

        let projects = vec![ProjectOwners::new(
            "services/api",
            "services/api",
            vec![Rule::new("*.rs", ["@backend-team"])],
        )];

        let result = provider.check(temp.path(), &projects).unwrap();
        assert!(!result.in_sync);
        assert!(result.actual.is_some());
    }

    #[test]
    fn test_github_check_missing_file() {
        let temp = tempdir().unwrap();
        let provider = GitHubCodeOwnersProvider;

        let projects = vec![ProjectOwners::new(
            "services/api",
            "services/api",
            vec![Rule::new("*.rs", ["@backend-team"])],
        )];

        let result = provider.check(temp.path(), &projects).unwrap();
        assert!(!result.in_sync);
        assert!(result.actual.is_none());
    }

    #[test]
    fn test_github_aggregates_multiple_projects() {
        let temp = tempdir().unwrap();
        let provider = GitHubCodeOwnersProvider;

        let projects = vec![
            ProjectOwners::new(
                "services/api",
                "services/api",
                vec![Rule::new("*.rs", ["@backend-team"])],
            ),
            ProjectOwners::new(
                "services/web",
                "services/web",
                vec![Rule::new("*.ts", ["@frontend-team"])],
            ),
            ProjectOwners::new(
                "libs/common",
                "libs/common",
                vec![Rule::new("*.rs", ["@platform-team"])],
            ),
        ];

        let result = provider.sync(temp.path(), &projects, false).unwrap();

        // All projects should be in the single file
        assert!(result.content.contains("/services/api/*.rs @backend-team"));
        assert!(result.content.contains("/services/web/*.ts @frontend-team"));
        assert!(result.content.contains("/libs/common/*.rs @platform-team"));

        // Sections should be present
        assert!(result.content.contains("# services/api"));
        assert!(result.content.contains("# services/web"));
        assert!(result.content.contains("# libs/common"));
    }

    #[test]
    fn test_github_root_project() {
        let temp = tempdir().unwrap();
        let provider = GitHubCodeOwnersProvider;

        // Project at repo root
        let projects = vec![ProjectOwners::new(
            "",
            "root",
            vec![
                Rule::new("*.rs", ["@core-team"]),
                Rule::new("/docs/**", ["@docs-team"]),
            ],
        )];

        let result = provider.sync(temp.path(), &projects, false).unwrap();

        // Root patterns should be normalized
        assert!(result.content.contains("/*.rs @core-team"));
        assert!(result.content.contains("/docs/** @docs-team"));
    }

    #[test]
    fn test_github_empty_projects_error() {
        let temp = tempdir().unwrap();
        let provider = GitHubCodeOwnersProvider;

        let result = provider.sync(temp.path(), &[], false);
        assert!(result.is_err());
    }
}
