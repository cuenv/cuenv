//! GitLab CODEOWNERS provider.
//!
//! GitLab supports CODEOWNERS files at:
//! - `CODEOWNERS` (repository root)
//! - `docs/CODEOWNERS`
//! - `.gitlab/CODEOWNERS`
//!
//! GitLab uses `[Section]` syntax for sections instead of `# Section`.
//!
//! This provider aggregates all project ownership rules into a single file
//! at the repository root `CODEOWNERS`.

use cuenv_codeowners::provider::{
    generate_aggregated_content, write_codeowners_file, CheckResult, CodeOwnersProvider,
    ProjectOwners, ProviderError, Result, SyncResult,
};
use cuenv_codeowners::SectionStyle;
use std::fs;
use std::path::Path;

/// GitLab CODEOWNERS provider.
///
/// Writes a single aggregated CODEOWNERS file to the repository root.
/// Uses GitLab's `[Section]` syntax for grouping rules.
#[derive(Debug, Clone, Copy, Default)]
pub struct GitLabCodeOwnersProvider;

impl CodeOwnersProvider for GitLabCodeOwnersProvider {
    fn output_path(&self) -> &str {
        "CODEOWNERS"
    }

    fn section_style(&self) -> SectionStyle {
        SectionStyle::Bracket
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

        // Generate aggregated content with Bracket style (uses [Section] syntax)
        let content = generate_aggregated_content(self.section_style(), projects, None);

        // Output path is at repo root for GitLab
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
    use cuenv_codeowners::provider::SyncStatus;
    use cuenv_codeowners::Rule;
    use tempfile::tempdir;

    #[test]
    fn test_gitlab_provider_output_path() {
        let provider = GitLabCodeOwnersProvider;
        assert_eq!(provider.output_path(), "CODEOWNERS");
    }

    #[test]
    fn test_gitlab_provider_section_style() {
        let provider = GitLabCodeOwnersProvider;
        assert_eq!(provider.section_style(), SectionStyle::Bracket);
    }

    #[test]
    fn test_gitlab_sync_creates_file() {
        let temp = tempdir().unwrap();
        let provider = GitLabCodeOwnersProvider;

        let projects = vec![ProjectOwners::new(
            "services/api",
            "services/api",
            vec![Rule::new("*.rs", ["@backend-team"])],
        )];

        let result = provider.sync(temp.path(), &projects, false).unwrap();

        assert_eq!(result.status, SyncStatus::Created);
        // GitLab uses CODEOWNERS at repo root
        assert!(result.path.ends_with("CODEOWNERS"));
        assert!(!result.path.to_string_lossy().contains(".github"));
        assert!(result.content.contains("/services/api/*.rs @backend-team"));

        // Verify file was written
        let file_content = fs::read_to_string(&result.path).unwrap();
        assert_eq!(file_content, result.content);
    }

    #[test]
    fn test_gitlab_uses_section_syntax() {
        let temp = tempdir().unwrap();
        let provider = GitLabCodeOwnersProvider;

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
        ];

        let result = provider.sync(temp.path(), &projects, false).unwrap();

        // GitLab uses [Section] syntax, not # Section
        assert!(
            result.content.contains("[services/api]"),
            "Should use [Section] syntax, got:\n{}",
            result.content
        );
        assert!(
            result.content.contains("[services/web]"),
            "Should use [Section] syntax"
        );
    }

    #[test]
    fn test_gitlab_sync_dry_run() {
        let temp = tempdir().unwrap();
        let provider = GitLabCodeOwnersProvider;

        let projects = vec![ProjectOwners::new(
            "services/api",
            "services/api",
            vec![Rule::new("*.rs", ["@backend-team"])],
        )];

        let result = provider.sync(temp.path(), &projects, true).unwrap();

        assert_eq!(result.status, SyncStatus::WouldCreate);
        assert!(!result.path.exists());
    }

    #[test]
    fn test_gitlab_check_in_sync() {
        let temp = tempdir().unwrap();
        let provider = GitLabCodeOwnersProvider;

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
    fn test_gitlab_check_out_of_sync() {
        let temp = tempdir().unwrap();
        let provider = GitLabCodeOwnersProvider;

        // Create file with different content
        fs::write(temp.path().join("CODEOWNERS"), "# Different content\n").unwrap();

        let projects = vec![ProjectOwners::new(
            "services/api",
            "services/api",
            vec![Rule::new("*.rs", ["@backend-team"])],
        )];

        let result = provider.check(temp.path(), &projects).unwrap();
        assert!(!result.in_sync);
    }

    #[test]
    fn test_gitlab_empty_projects_error() {
        let temp = tempdir().unwrap();
        let provider = GitLabCodeOwnersProvider;

        let result = provider.sync(temp.path(), &[], false);
        assert!(result.is_err());
    }
}
