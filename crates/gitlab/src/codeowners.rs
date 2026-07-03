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

use cuenv_codeowners::SectionStyle;
use cuenv_codeowners::provider::CodeOwnersProvider;

/// GitLab CODEOWNERS provider.
///
/// Writes a single aggregated CODEOWNERS file to the repository root.
/// Uses GitLab's `[Section]` syntax for grouping rules. Sync and check
/// behavior comes from the [`CodeOwnersProvider`] default methods.
#[derive(Debug, Clone, Copy, Default)]
pub struct GitLabCodeOwnersProvider;

impl CodeOwnersProvider for GitLabCodeOwnersProvider {
    fn output_path(&self) -> &str {
        "CODEOWNERS"
    }

    fn section_style(&self) -> SectionStyle {
        SectionStyle::Bracket
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_codeowners::Rule;
    use cuenv_codeowners::provider::{ProjectOwners, SyncStatus};
    use std::fs;
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
