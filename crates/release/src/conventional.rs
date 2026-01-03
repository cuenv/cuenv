//! Conventional commit parsing and analysis.
//!
//! This module uses the `git-conventional` crate to parse commit messages
//! following the Conventional Commits specification, and `gix` for git
//! repository access.

// Git repository traversal and commit parsing involves complex iteration
#![allow(clippy::too_many_lines)]

use crate::changeset::BumpType;
use crate::config::TagType;
use crate::error::{Error, Result};
use gix::bstr::ByteSlice;
use semver::Version as SemverVersion;
use std::cmp::Ordering;
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
    /// If `since_tag` is `None`, auto-detects the latest tag matching the
    /// configured `tag_prefix` and `tag_type`.
    ///
    /// # Errors
    ///
    /// Returns an error if the repository cannot be opened or commits cannot be read.
    #[allow(clippy::default_trait_access)] // gix API requires Default::default() for sorting config
    #[allow(clippy::redundant_closure_for_method_calls)] // closures needed for type conversion from git_conventional types
    pub fn parse_since_tag(
        root: &Path,
        since_tag: Option<&str>,
        tag_prefix: &str,
        tag_type: TagType,
    ) -> Result<Vec<ConventionalCommit>> {
        // Use gix::open which handles worktree discovery internally
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
            if let Some(oid) = find_tag_oid(&repo, tag) {
                Some(oid)
            } else {
                // Collect available tags for suggestions
                let available_tags = list_tags_for_config(&repo, tag_prefix, tag_type);
                let suggestion = if available_tags.is_empty() {
                    String::new()
                } else {
                    // Find similar tags
                    let similar: Vec<_> = available_tags
                        .iter()
                        .filter(|t| {
                            t.contains(tag)
                                || tag.contains(t.as_str())
                                || levenshtein_distance(t, tag) <= 3
                        })
                        .take(3)
                        .collect();

                    if similar.is_empty() {
                        format!(
                            ". Available tags: {}",
                            available_tags
                                .iter()
                                .take(5)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    } else {
                        format!(
                            ". Did you mean: {}?",
                            similar
                                .iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                };
                return Err(Error::git(format!(
                    "Tag '{tag}' not found in repository{suggestion}"
                )));
            }
        } else {
            // Auto-detect latest tag matching the configured prefix and type
            let tags = list_tags_for_config(&repo, tag_prefix, tag_type);
            tags.first().and_then(|tag| find_tag_oid(&repo, tag))
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

        for commit in commits {
            let desc = commit.scope.as_ref().map_or_else(
                || commit.description.clone(),
                |scope| format!("**{scope}**: {}", commit.description),
            );

            if commit.breaking {
                breaking.push(desc.clone());
            }

            match commit.commit_type.as_str() {
                "feat" => features.push(desc),
                "fix" | "perf" => fixes.push(desc),
                // chore, docs, style, refactor, test, ci - not included in release summaries
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

/// A comparable version that can be either semver or calver.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ComparableVersion {
    Semver(SemverVersion),
    Calver(Vec<u32>), // e.g., [2024, 12, 23] or [2024, 12]
}

impl Ord for ComparableVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Semver(a), Self::Semver(b)) => a.cmp(b),
            (Self::Calver(a), Self::Calver(b)) => a.cmp(b),
            // Different types shouldn't happen in practice
            _ => Ordering::Equal,
        }
    }
}

impl PartialOrd for ComparableVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Parse a calver version string (e.g., "2024.12.23" or "24.04").
fn parse_calver(s: &str) -> Option<Vec<u32>> {
    let parts: std::result::Result<Vec<u32>, _> = s.split('.').map(str::parse).collect();
    let parts = parts.ok()?;
    // CalVer needs at least 2 parts (year.month or similar)
    if parts.len() >= 2 { Some(parts) } else { None }
}

/// Extract version from a tag if it matches the prefix and tag type.
fn extract_version(tag: &str, prefix: &str, tag_type: TagType) -> Option<ComparableVersion> {
    let version_str = tag.strip_prefix(prefix)?;
    match tag_type {
        TagType::Semver => SemverVersion::parse(version_str)
            .ok()
            .map(ComparableVersion::Semver),
        TagType::Calver => parse_calver(version_str).map(ComparableVersion::Calver),
    }
}

/// List all tags matching the prefix and type, sorted by version (newest first).
fn list_tags_for_config(repo: &gix::Repository, prefix: &str, tag_type: TagType) -> Vec<String> {
    let mut tags_with_versions: Vec<(String, ComparableVersion)> = Vec::new();

    if let Ok(refs) = repo.references()
        && let Ok(tag_refs) = refs.tags()
    {
        for tag_ref in tag_refs.flatten() {
            if let Ok(name) = tag_ref.name().as_bstr().to_str() {
                // Strip "refs/tags/" prefix
                let tag_name = name.strip_prefix("refs/tags/").unwrap_or(name);

                if let Some(version) = extract_version(tag_name, prefix, tag_type) {
                    tags_with_versions.push((tag_name.to_string(), version));
                }
            }
        }
    }

    // Sort by version (most recent first)
    tags_with_versions.sort_by(|a, b| b.1.cmp(&a.1));
    tags_with_versions
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// Calculate Levenshtein distance between two strings.
/// Used for fuzzy matching tag suggestions.
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();

    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    let mut matrix = vec![vec![0; b_len + 1]; a_len + 1];

    for (i, row) in matrix.iter_mut().enumerate().take(a_len + 1) {
        row[0] = i;
    }
    for (j, val) in matrix[0].iter_mut().enumerate() {
        *val = j;
    }

    for i in 1..=a_len {
        for j in 1..=b_len {
            let cost = usize::from(a_chars[i - 1] != b_chars[j - 1]);
            matrix[i][j] = std::cmp::min(
                std::cmp::min(matrix[i - 1][j] + 1, matrix[i][j - 1] + 1),
                matrix[i - 1][j - 1] + cost,
            );
        }
    }

    matrix[a_len][b_len]
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

    #[test]
    fn test_levenshtein_distance_identical() {
        assert_eq!(levenshtein_distance("hello", "hello"), 0);
    }

    #[test]
    fn test_levenshtein_distance_single_edit() {
        assert_eq!(levenshtein_distance("hello", "hallo"), 1);
        assert_eq!(levenshtein_distance("v1.0.0", "v1.0.1"), 1);
    }

    #[test]
    fn test_levenshtein_distance_prefix() {
        assert_eq!(levenshtein_distance("v1.0.0", "1.0.0"), 1);
    }

    #[test]
    fn test_levenshtein_distance_empty() {
        assert_eq!(levenshtein_distance("", "hello"), 5);
        assert_eq!(levenshtein_distance("hello", ""), 5);
        assert_eq!(levenshtein_distance("", ""), 0);
    }

    #[test]
    fn test_levenshtein_distance_multi_edit() {
        assert_eq!(levenshtein_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn test_parse_calver_valid() {
        assert_eq!(parse_calver("2024.12.23"), Some(vec![2024, 12, 23]));
        assert_eq!(parse_calver("24.04"), Some(vec![24, 4]));
        assert_eq!(parse_calver("2024.1"), Some(vec![2024, 1]));
    }

    #[test]
    fn test_parse_calver_invalid() {
        assert_eq!(parse_calver("v1.0.0"), None);
        assert_eq!(parse_calver("2024"), None);
        assert_eq!(parse_calver("invalid"), None);
    }

    #[test]
    fn test_extract_version_semver() {
        let v = extract_version("v1.2.3", "v", TagType::Semver);
        assert!(matches!(v, Some(ComparableVersion::Semver(_))));
    }

    #[test]
    fn test_extract_version_calver() {
        let v = extract_version("v2024.12.01", "v", TagType::Calver);
        assert!(matches!(v, Some(ComparableVersion::Calver(_))));
    }

    #[test]
    fn test_extract_version_wrong_prefix() {
        let v = extract_version("release-1.2.3", "v", TagType::Semver);
        assert!(v.is_none());
    }

    #[test]
    fn test_comparable_version_semver_ordering() {
        let v1 = ComparableVersion::Semver(SemverVersion::parse("1.0.0").unwrap());
        let v2 = ComparableVersion::Semver(SemverVersion::parse("2.0.0").unwrap());
        assert!(v1 < v2);
    }

    #[test]
    fn test_comparable_version_calver_ordering() {
        let v1 = ComparableVersion::Calver(vec![2024, 1, 1]);
        let v2 = ComparableVersion::Calver(vec![2024, 12, 1]);
        assert!(v1 < v2);
    }

    #[test]
    fn test_comparable_version_partial_cmp() {
        let v1 = ComparableVersion::Semver(SemverVersion::parse("1.0.0").unwrap());
        let v2 = ComparableVersion::Semver(SemverVersion::parse("1.0.0").unwrap());
        assert_eq!(v1.partial_cmp(&v2), Some(Ordering::Equal));
    }

    #[test]
    fn test_comparable_version_equality() {
        let v1 = ComparableVersion::Calver(vec![2024, 6, 15]);
        let v2 = ComparableVersion::Calver(vec![2024, 6, 15]);
        assert_eq!(v1, v2);
    }

    #[test]
    fn test_conventional_commit_clone() {
        let commit = ConventionalCommit {
            commit_type: "feat".to_string(),
            scope: Some("core".to_string()),
            breaking: false,
            description: "add feature".to_string(),
            body: Some("detailed body".to_string()),
            hash: "abc123".to_string(),
        };
        let cloned = commit.clone();
        assert_eq!(cloned.commit_type, "feat");
        assert_eq!(cloned.scope, Some("core".to_string()));
    }

    #[test]
    fn test_conventional_commit_debug() {
        let commit = ConventionalCommit {
            commit_type: "fix".to_string(),
            scope: None,
            breaking: true,
            description: "breaking fix".to_string(),
            body: None,
            hash: "def456".to_string(),
        };
        let debug = format!("{commit:?}");
        assert!(debug.contains("fix"));
        assert!(debug.contains("breaking: true"));
    }

    #[test]
    fn test_bump_type_perf() {
        let commit = ConventionalCommit {
            commit_type: "perf".to_string(),
            scope: None,
            breaking: false,
            description: "optimize query".to_string(),
            body: None,
            hash: "abc".to_string(),
        };
        assert_eq!(commit.bump_type(), BumpType::Patch);
    }

    #[test]
    fn test_aggregate_bump_empty() {
        let commits: Vec<ConventionalCommit> = vec![];
        assert_eq!(CommitParser::aggregate_bump(&commits), BumpType::None);
    }

    #[test]
    fn test_aggregate_bump_with_breaking() {
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
                breaking: true,
                description: "breaking feat".to_string(),
                body: None,
                hash: "2".to_string(),
            },
        ];
        assert_eq!(CommitParser::aggregate_bump(&commits), BumpType::Major);
    }

    #[test]
    fn test_summarize_with_breaking() {
        let commits = vec![ConventionalCommit {
            commit_type: "feat".to_string(),
            scope: Some("api".to_string()),
            breaking: true,
            description: "remove endpoint".to_string(),
            body: None,
            hash: "1".to_string(),
        }];
        let summary = CommitParser::summarize(&commits);
        assert!(summary.contains("### Breaking Changes"));
        assert!(summary.contains("**api**: remove endpoint"));
        assert!(summary.contains("### Features")); // Also listed as feature
    }

    #[test]
    fn test_summarize_empty() {
        let commits: Vec<ConventionalCommit> = vec![];
        let summary = CommitParser::summarize(&commits);
        assert_eq!(summary, "");
    }

    #[test]
    fn test_summarize_chore_not_included() {
        let commits = vec![ConventionalCommit {
            commit_type: "chore".to_string(),
            scope: None,
            breaking: false,
            description: "update deps".to_string(),
            body: None,
            hash: "1".to_string(),
        }];
        let summary = CommitParser::summarize(&commits);
        assert!(!summary.contains("### Features"));
        assert!(!summary.contains("### Bug Fixes"));
        assert!(!summary.contains("update deps"));
    }
}
