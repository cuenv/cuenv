//! Conventional commit parsing and analysis.
//!
//! This module uses the `git-conventional` crate to parse commit messages
//! following the Conventional Commits specification, and `gix` for git
//! repository access.

#![allow(clippy::default_trait_access)]
#![allow(clippy::redundant_closure_for_method_calls)]

use crate::changeset::BumpType;
use crate::error::{Error, Result};
use std::path::Path;

/// A parsed conventional commit with version bump information.
#[derive(Debug, Clone)]
pub struct ConventionalCommit {
    /// The commit type (feat, fix, chore, etc.)
    pub commit_type: String,
    /// Optional scope
    pub scope: Option<String>,
    /// Whether this is a breaking change
    pub breaking: bool,
    /// The commit description (first line after type)
    pub description: String,
    /// Optional commit body
    pub body: Option<String>,
    /// The full commit hash
    pub hash: String,
}

impl ConventionalCommit {
    /// Determine the bump type for this commit.
    #[must_use]
    pub fn bump_type(&self) -> BumpType {
        if self.breaking {
            return BumpType::Major;
        }

        match self.commit_type.as_str() {
            "feat" => BumpType::Minor,
            "fix" | "perf" => BumpType::Patch,
            _ => BumpType::None,
        }
    }
}

/// Parser for conventional commits from a git repository.
pub struct CommitParser;

impl CommitParser {
    /// Parse all conventional commits since the given tag.
    ///
    /// If `since_tag` is `None`, parses all commits.
    ///
    /// # Errors
    ///
    /// Returns an error if the repository cannot be opened or commits cannot be read.
    pub fn parse_since_tag(
        root: &Path,
        since_tag: Option<&str>,
    ) -> Result<Vec<ConventionalCommit>> {
        let repo =
            gix::open(root).map_err(|e| Error::git(format!("Failed to open repository: {e}")))?;

        // Get HEAD reference
        let head = repo
            .head_id()
            .map_err(|e| Error::git(format!("Failed to get HEAD: {e}")))?;

        // Set up revision walk
        let mut walk = repo
            .rev_walk([head])
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                Default::default(),
            ))
            .all()
            .map_err(|e| Error::git(format!("Failed to create rev walk: {e}")))?;

        // If we have a since_tag, find it and use as boundary
        let boundary_oid = if let Some(tag) = since_tag {
            match find_tag_oid(&repo, tag) {
                Some(oid) => Some(oid),
                None => {
                    return Err(Error::git(format!("Tag '{tag}' not found in repository")));
                }
            }
        } else {
            None
        };

        let mut commits = Vec::new();

        for info in walk.by_ref() {
            let info = info.map_err(|e| Error::git(format!("Failed to walk commits: {e}")))?;
            let oid = info.id;

            // Stop if we hit the boundary tag
            if let Some(boundary) = boundary_oid
                && oid == boundary
            {
                break;
            }

            // Get the commit object
            let commit = repo
                .find_commit(oid)
                .map_err(|e| Error::git(format!("Failed to find commit: {e}")))?;

            let message = commit.message_raw_sloppy().to_string();
            let hash = oid.to_string();

            // Try to parse as conventional commit
            if let Ok(parsed) = git_conventional::Commit::parse(&message) {
                commits.push(ConventionalCommit {
                    commit_type: parsed.type_().to_string(),
                    scope: parsed.scope().map(|s| s.to_string()),
                    breaking: parsed.breaking(),
                    description: parsed.description().to_string(),
                    body: parsed.body().map(|b| b.to_string()),
                    hash,
                });
            }
        }

        Ok(commits)
    }

    /// Calculate the aggregate bump type from a list of commits.
    ///
    /// Returns the highest bump type among all commits.
    #[must_use]
    pub fn aggregate_bump(commits: &[ConventionalCommit]) -> BumpType {
        commits
            .iter()
            .map(ConventionalCommit::bump_type)
            .fold(BumpType::None, std::cmp::max)
    }

    /// Generate a summary of commits grouped by type.
    #[must_use]
    pub fn summarize(commits: &[ConventionalCommit]) -> String {
        let mut features = Vec::new();
        let mut fixes = Vec::new();
        let mut breaking = Vec::new();
        let mut other = Vec::new();

        for commit in commits {
            let desc = if let Some(ref scope) = commit.scope {
                format!("**{}**: {}", scope, commit.description)
            } else {
                commit.description.clone()
            };

            if commit.breaking {
                breaking.push(desc.clone());
            }

            match commit.commit_type.as_str() {
                "feat" => features.push(desc),
                "fix" | "perf" => fixes.push(desc),
                "chore" | "docs" | "style" | "refactor" | "test" | "ci" => other.push(desc),
                _ => {}
            }
        }

        let mut summary = String::new();

        if !breaking.is_empty() {
            summary.push_str("### Breaking Changes\n\n");
            for item in &breaking {
                summary.push_str("- ");
                summary.push_str(item);
                summary.push('\n');
            }
            summary.push('\n');
        }

        if !features.is_empty() {
            summary.push_str("### Features\n\n");
            for item in &features {
                summary.push_str("- ");
                summary.push_str(item);
                summary.push('\n');
            }
            summary.push('\n');
        }

        if !fixes.is_empty() {
            summary.push_str("### Bug Fixes\n\n");
            for item in &fixes {
                summary.push_str("- ");
                summary.push_str(item);
                summary.push('\n');
            }
            summary.push('\n');
        }

        summary
    }
}

/// Find the OID for a given tag name.
fn find_tag_oid(repo: &gix::Repository, tag_name: &str) -> Option<gix::ObjectId> {
    // Try various tag formats
    let tag_refs = [
        format!("refs/tags/{tag_name}"),
        format!("refs/tags/v{tag_name}"),
        tag_name.to_string(),
    ];

    for tag_ref in &tag_refs {
        if let Ok(reference) = repo.find_reference(tag_ref.as_str())
            && let Ok(id) = reference.into_fully_peeled_id()
        {
            return Some(id.detach());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bump_type_feat() {
        let commit = ConventionalCommit {
            commit_type: "feat".to_string(),
            scope: None,
            breaking: false,
            description: "add feature".to_string(),
            body: None,
            hash: "abc123".to_string(),
        };
        assert_eq!(commit.bump_type(), BumpType::Minor);
    }

    #[test]
    fn test_bump_type_fix() {
        let commit = ConventionalCommit {
            commit_type: "fix".to_string(),
            scope: None,
            breaking: false,
            description: "fix bug".to_string(),
            body: None,
            hash: "abc123".to_string(),
        };
        assert_eq!(commit.bump_type(), BumpType::Patch);
    }

    #[test]
    fn test_bump_type_breaking() {
        let commit = ConventionalCommit {
            commit_type: "feat".to_string(),
            scope: None,
            breaking: true,
            description: "breaking change".to_string(),
            body: None,
            hash: "abc123".to_string(),
        };
        assert_eq!(commit.bump_type(), BumpType::Major);
    }

    #[test]
    fn test_bump_type_chore() {
        let commit = ConventionalCommit {
            commit_type: "chore".to_string(),
            scope: None,
            breaking: false,
            description: "update deps".to_string(),
            body: None,
            hash: "abc123".to_string(),
        };
        assert_eq!(commit.bump_type(), BumpType::None);
    }

    #[test]
    fn test_aggregate_bump() {
        let commits = vec![
            ConventionalCommit {
                commit_type: "fix".to_string(),
                scope: None,
                breaking: false,
                description: "fix".to_string(),
                body: None,
                hash: "1".to_string(),
            },
            ConventionalCommit {
                commit_type: "feat".to_string(),
                scope: None,
                breaking: false,
                description: "feat".to_string(),
                body: None,
                hash: "2".to_string(),
            },
        ];
        assert_eq!(CommitParser::aggregate_bump(&commits), BumpType::Minor);
    }

    #[test]
    fn test_summarize() {
        let commits = vec![
            ConventionalCommit {
                commit_type: "feat".to_string(),
                scope: Some("api".to_string()),
                breaking: false,
                description: "add endpoint".to_string(),
                body: None,
                hash: "1".to_string(),
            },
            ConventionalCommit {
                commit_type: "fix".to_string(),
                scope: None,
                breaking: false,
                description: "fix crash".to_string(),
                body: None,
                hash: "2".to_string(),
            },
        ];

        let summary = CommitParser::summarize(&commits);
        assert!(summary.contains("### Features"));
        assert!(summary.contains("**api**: add endpoint"));
        assert!(summary.contains("### Bug Fixes"));
        assert!(summary.contains("fix crash"));
    }
}
