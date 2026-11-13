//! Affected detection utilities for tasks and files.
//!
//! This module provides utilities for determining if tasks are affected by file changes,
//! used by CI pipelines and incremental builds.
//!
//! # Design Philosophy
//!
//! - Tasks with inputs are affected if any input pattern matches changed files
//! - Tasks with NO inputs are always considered affected (we can't determine what affects them)
//! - This is the safe behavior - if we can't determine, we run the task

use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::{Path, PathBuf};

/// Why an input pattern can never be compared with changed files, which are
/// relative to the repository and lie inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnmatchablePattern {
    /// The pattern has a `..` path segment, so it leaves the project directory.
    ParentSegment,
    /// The pattern is an absolute path.
    Absolute,
    /// The pattern has glob characters but is not a valid glob.
    InvalidGlob,
}

/// Why `pattern` cannot be matched against changed files, if it cannot.
///
/// A task with such an input cannot be shown to be unaffected, so
/// [`matches_pattern`] treats the pattern as matching whenever any file
/// changed.
#[must_use]
pub fn unmatchable_pattern(pattern: &str) -> Option<UnmatchablePattern> {
    let pattern = cuenv_vcs::pattern::normalize(pattern);
    if pattern.split('/').any(|segment| segment == "..") {
        return Some(UnmatchablePattern::ParentSegment);
    }
    if Path::new(&pattern).is_absolute() {
        return Some(UnmatchablePattern::Absolute);
    }
    if cuenv_vcs::pattern::is_glob(&pattern) && Glob::new(&pattern).is_err() {
        return Some(UnmatchablePattern::InvalidGlob);
    }
    None
}

/// How an input pattern compares with the changed files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternMatch {
    /// No changed file matches the pattern.
    None,
    /// A changed file of the project matches, or the pattern cannot be
    /// compared with changed files at all and something changed.
    Matched,
    /// No changed file of the project matches, and a changed file outside
    /// the project does when the pattern is read from the root of the
    /// checkout instead of from the project.
    ///
    /// cuenv has always counted this as a match, so it still does, but a
    /// pattern is relative to its project: whether this is a root-relative
    /// input someone relies on or an accident of two directories sharing a
    /// name is for the caller to tell apart.
    OnlyFromCheckoutRoot,
}

/// Compare the changed files with one input pattern of a project.
///
/// `files` and `project_root` are in one frame: relative to the root of the
/// checkout. The project root is `.` or empty for a project at that root.
/// The pattern is relative to the project and is read as every other reader
/// of inputs reads it (see `cuenv_vcs::pattern`):
///
/// - the empty pattern is the project directory, and matches every file of
///   the project;
/// - a glob (`*`, `?`, `[`, `{`) is matched as one;
/// - anything else is a path, and matches that file and every file beneath
///   that directory.
///
/// A pattern that cannot be compared with changed files at all (see
/// [`unmatchable_pattern`]) matches whenever any file changed: reporting no
/// match would claim the task is unaffected when nothing shows that.
#[must_use]
pub fn match_pattern(files: &[PathBuf], project_root: &Path, pattern: &str) -> PatternMatch {
    if let Some(reason) = unmatchable_pattern(pattern) {
        tracing::trace!(
            pattern,
            ?reason,
            "Pattern cannot be matched; failing closed"
        );
        return if files.is_empty() {
            PatternMatch::None
        } else {
            PatternMatch::Matched
        };
    }

    let pattern = cuenv_vcs::pattern::normalize(pattern);
    let reader = PatternReader::new(&pattern);
    let mut from_checkout_root = false;

    for file in files {
        let matched = match within_project(file, project_root) {
            Some(relative) => reader.matches(relative),
            None => {
                // Outside the project. The empty pattern is the project
                // directory and nothing else.
                from_checkout_root |= !pattern.is_empty() && reader.matches(file);
                false
            }
        };

        tracing::trace!(
            file = %file.display(),
            project_root = %project_root.display(),
            pattern = %pattern,
            matched,
            "Compared changed file against affected pattern"
        );

        if matched {
            return PatternMatch::Matched;
        }
    }

    if from_checkout_root {
        PatternMatch::OnlyFromCheckoutRoot
    } else {
        PatternMatch::None
    }
}

/// Check if any of the given files match a pattern.
///
/// See [`match_pattern`], which this reduces to a yes or no:
/// [`PatternMatch::OnlyFromCheckoutRoot`] counts as a match.
#[must_use]
pub fn matches_pattern(files: &[PathBuf], project_root: &Path, pattern: &str) -> bool {
    match_pattern(files, project_root, pattern) != PatternMatch::None
}

/// One normalised pattern, compiled once.
enum PatternReader<'pattern> {
    /// The project directory: every file of the project.
    Everything,
    /// A file, or a directory and everything beneath it.
    Path(&'pattern Path),
    /// A glob.
    Glob(GlobSet),
}

impl<'pattern> PatternReader<'pattern> {
    fn new(pattern: &'pattern str) -> Self {
        if pattern.is_empty() {
            return Self::Everything;
        }
        if !cuenv_vcs::pattern::is_glob(pattern) {
            return Self::Path(Path::new(pattern));
        }
        // `unmatchable_pattern` has refused an invalid glob by now, so a
        // failure here matches nothing rather than everything.
        Glob::new(pattern)
            .ok()
            .and_then(|glob| GlobSetBuilder::new().add(glob).build().ok())
            .map_or(Self::Path(Path::new(pattern)), Self::Glob)
    }

    fn matches(&self, relative: &Path) -> bool {
        match self {
            Self::Everything => true,
            Self::Path(path) => relative.starts_with(path),
            Self::Glob(set) => set.is_match(relative),
        }
    }
}

/// Build a GlobSet from multiple patterns for efficient batch matching.
///
/// # Arguments
///
/// * `patterns` - Iterator of glob patterns
///
/// # Returns
///
/// A compiled GlobSet, or None if no valid patterns
pub fn build_glob_set<'a>(patterns: impl Iterator<Item = &'a str>) -> Option<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    let mut has_patterns = false;

    for pattern in patterns {
        if let Ok(glob) = Glob::new(pattern) {
            builder.add(glob);
            has_patterns = true;
        } else {
            tracing::trace!(pattern, "Skipping invalid glob pattern");
        }
    }

    if !has_patterns {
        return None;
    }

    builder.build().ok()
}

/// The path of `file` inside the project, when it is a file of the project.
///
/// Both are relative to the root of the checkout, so this is an exact prefix
/// test. Guessing where an absolute project root sits in the checkout from
/// the names of its last directories used to make a checkout named like one
/// of its projects (`.../docs/docs`) read every path one level off.
fn within_project<'file>(file: &'file Path, project_root: &Path) -> Option<&'file Path> {
    if project_root == Path::new(".") || project_root.as_os_str().is_empty() {
        return Some(file);
    }
    file.strip_prefix(project_root).ok()
}

/// Trait for types that can determine if they are affected by file changes.
///
/// Implement this trait on task types to enable affected detection in
/// CI pipelines and incremental builds.
pub trait AffectedBy {
    /// Returns true if this item is affected by the given file changes.
    ///
    /// # Arguments
    ///
    /// * `changed_files` - Paths of files that have changed (typically repo-relative)
    /// * `project_root` - Root path of the project containing this item
    ///
    /// # Returns
    ///
    /// `true` if the item should be considered affected and needs to run
    fn is_affected_by(&self, changed_files: &[PathBuf], project_root: &Path) -> bool;

    /// Returns the input patterns that determine what affects this item.
    ///
    /// Used for debugging and reporting which patterns matched.
    fn input_patterns(&self) -> Vec<&str>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_pattern_simple_prefix() {
        let files = vec![PathBuf::from("crates/foo/bar.rs")];
        let root = Path::new(".");

        assert!(matches_pattern(&files, root, "crates"));
        assert!(matches_pattern(&files, root, "crates/foo"));
        assert!(matches_pattern(&files, root, "crates/foo/bar.rs"));
    }

    #[test]
    fn test_matches_pattern_no_match() {
        let files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");

        assert!(!matches_pattern(&files, root, "crates"));
        assert!(!matches_pattern(&files, root, "tests"));
    }

    #[test]
    fn test_matches_pattern_glob() {
        let files = vec![PathBuf::from("src/lib.rs"), PathBuf::from("src/main.rs")];
        let root = Path::new(".");

        assert!(matches_pattern(&files, root, "src/*.rs"));
        assert!(!matches_pattern(&files, root, "*.txt"));
    }

    #[test]
    fn test_matches_pattern_with_project_root() {
        // File is repo-relative, project root is a subdirectory
        let files = vec![PathBuf::from("projects/website/src/app.rs")];
        let root = Path::new("projects/website");

        // Pattern is project-relative
        assert!(matches_pattern(&files, root, "src"));
        assert!(matches_pattern(&files, root, "src/app.rs"));
    }

    #[test]
    fn test_matches_pattern_different_project() {
        // File is in a different project
        let files = vec![PathBuf::from("projects/api/src/main.rs")];
        let root = Path::new("projects/website");

        // Should not match - file is in api, not website
        assert!(!matches_pattern(&files, root, "src"));
    }

    #[test]
    fn a_file_is_placed_in_its_project_by_an_exact_prefix() {
        assert_eq!(
            within_project(
                Path::new("projects/website/src/lib.rs"),
                Path::new("projects/website")
            ),
            Some(Path::new("src/lib.rs"))
        );
        assert_eq!(
            within_project(Path::new("src/lib.rs"), Path::new(".")),
            Some(Path::new("src/lib.rs"))
        );
        assert_eq!(
            within_project(Path::new("src/lib.rs"), Path::new("")),
            Some(Path::new("src/lib.rs"))
        );
        assert_eq!(
            within_project(
                Path::new("projects/api/src/lib.rs"),
                Path::new("projects/website")
            ),
            None
        );
    }

    #[test]
    fn a_checkout_named_like_its_project_is_not_read_one_level_off() {
        // The checkout is `.../docs/docs` and the project is `docs`. Placing
        // the project by the names of its last directories stripped `docs/`
        // twice and read `docs/index.md` as `index.md` of a project `docs/docs`.
        let files = vec![PathBuf::from("docs/index.md")];

        assert!(matches_pattern(&files, Path::new("docs"), "index.md"));
        assert!(matches_pattern(&files, Path::new("docs"), "**/*.md"));
        assert!(!matches_pattern(&files, Path::new("docs"), "docs/**"));
    }

    #[test]
    fn a_pattern_is_matched_however_it_is_spelled() {
        let root = Path::new("app");
        let lock = vec![PathBuf::from("app/bun.lock")];
        let nested = vec![PathBuf::from("app/src/deep/c.txt")];

        for spelling in ["bun.lock", "./bun.lock", " bun.lock ", "bun.lock\u{a0}"] {
            assert!(matches_pattern(&lock, root, spelling), "{spelling:?}");
        }
        for spelling in [
            "src",
            "./src",
            "src/",
            "src/.",
            "src//deep",
            "src/./deep/c.txt",
        ] {
            assert!(matches_pattern(&nested, root, spelling), "{spelling:?}");
        }
        for spelling in ["src/**", "./src/**", "src//**", "src/** "] {
            assert!(matches_pattern(&nested, root, spelling), "{spelling:?}");
        }
    }

    #[test]
    fn a_brace_pattern_is_a_glob() {
        let root = Path::new(".");
        let files = vec![PathBuf::from("src/a.txt")];

        assert!(matches_pattern(&files, root, "src/{a,b}.txt"));
        assert!(!matches_pattern(&files, root, "src/{b,c}.txt"));
        assert!(matches_pattern(
            &[PathBuf::from("config.yaml")],
            root,
            "config.{json,yaml}"
        ));
    }

    #[test]
    fn the_empty_pattern_is_the_project_directory() {
        let root = Path::new("app");

        for spelling in ["", ".", "./", " "] {
            assert!(
                matches_pattern(&[PathBuf::from("app/any/file.txt")], root, spelling),
                "{spelling:?}"
            );
            assert_eq!(
                match_pattern(&[PathBuf::from("docs/readme.md")], root, spelling),
                PatternMatch::None,
                "{spelling:?}"
            );
        }
    }

    #[test]
    fn a_match_only_from_the_checkout_root_is_told_apart() {
        let root = Path::new("app");
        let shared = vec![PathBuf::from("shared/x.txt")];

        assert_eq!(
            match_pattern(&shared, root, "shared/**"),
            PatternMatch::OnlyFromCheckoutRoot
        );
        assert_eq!(
            match_pattern(&shared, root, "shared"),
            PatternMatch::OnlyFromCheckoutRoot
        );
        assert!(matches_pattern(&shared, root, "shared/**"));
        assert_eq!(match_pattern(&shared, root, "src/**"), PatternMatch::None);

        // A file of the project wins over a root namesake.
        let both = vec![
            PathBuf::from("package.json"),
            PathBuf::from("app/package.json"),
        ];
        assert_eq!(
            match_pattern(&both, root, "package.json"),
            PatternMatch::Matched
        );
        assert_eq!(
            match_pattern(&both[..1], root, "package.json"),
            PatternMatch::OnlyFromCheckoutRoot
        );
    }

    #[test]
    fn a_parent_segment_matches_whenever_any_file_changed() {
        let root = Path::new("projects/app");
        let unrelated = vec![PathBuf::from("docs/readme.md")];

        assert!(matches_pattern(&unrelated, root, "../content/articles/**"));
        assert!(matches_pattern(&unrelated, root, "src/../../shared"));
        assert!(matches_pattern(&unrelated, root, ".."));
        assert!(!matches_pattern(&[], root, "../content/articles/**"));
    }

    #[test]
    fn dots_inside_a_segment_are_not_a_parent_segment() {
        let root = Path::new(".");
        let unrelated = vec![PathBuf::from("docs/readme.md")];

        assert_eq!(unmatchable_pattern("foo..bar/**"), None);
        assert_eq!(unmatchable_pattern(".../x"), None);
        assert!(!matches_pattern(&unrelated, root, "foo..bar/**"));
    }

    #[test]
    fn an_absolute_pattern_matches_whenever_any_file_changed() {
        let root = Path::new("projects/app");
        let unrelated = vec![PathBuf::from("docs/readme.md")];

        assert_eq!(
            unmatchable_pattern("/etc/hosts"),
            Some(UnmatchablePattern::Absolute)
        );
        assert!(matches_pattern(&unrelated, root, "/etc/hosts"));
        assert!(!matches_pattern(&[], root, "/etc/hosts"));
    }

    #[test]
    fn an_invalid_glob_matches_whenever_any_file_changed() {
        let root = Path::new(".");
        let unrelated = vec![PathBuf::from("docs/readme.md")];

        assert_eq!(
            unmatchable_pattern("src/[unclosed"),
            Some(UnmatchablePattern::InvalidGlob)
        );
        assert!(matches_pattern(&unrelated, root, "src/[unclosed"));
        assert!(!matches_pattern(&[], root, "src/[unclosed"));
    }

    #[test]
    fn test_build_glob_set() {
        let patterns = ["src/**/*.rs", "tests/*.rs"];
        let set = build_glob_set(patterns.iter().copied()).unwrap();

        assert!(set.is_match("src/lib.rs"));
        assert!(set.is_match("src/foo/bar.rs"));
        assert!(set.is_match("tests/test.rs"));
        assert!(!set.is_match("docs/readme.md"));
    }

    #[test]
    fn test_build_glob_set_invalid_patterns() {
        let patterns = ["[invalid", "src/**"];
        let set = build_glob_set(patterns.iter().copied()).unwrap();

        // Invalid pattern is skipped, valid one still works
        assert!(set.is_match("src/lib.rs"));
    }

    #[test]
    fn test_build_glob_set_empty() {
        let patterns: [&str; 0] = [];
        let set = build_glob_set(patterns.iter().copied());
        assert!(set.is_none());
    }
}
