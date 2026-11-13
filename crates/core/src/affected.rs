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

use cuenv_manifest::tasks::{FilesAnchor, InputPattern, anchored_path_problem};
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
    /// A pattern is relative to its project, so this is no match. Whether
    /// it is a root-relative input someone relied on (which is now an error
    /// for the caller to raise) or an accident of two directories sharing a
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
/// [`PatternMatch::OnlyFromCheckoutRoot`] is no match: a pattern is read
/// from its project.
#[must_use]
pub fn matches_pattern(files: &[PathBuf], project_root: &Path, pattern: &str) -> bool {
    match_pattern(files, project_root, pattern) == PatternMatch::Matched
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

/// Where the anchors of file references lie, in the frame changed files are
/// reported in: relative to the root of the checkout.
pub trait PlaceAnchor {
    /// The directory `anchor` means, relative to the root of the checkout
    /// (empty for the root itself); `None` when it cannot be placed there.
    fn place(&self, anchor: &FilesAnchor) -> Option<PathBuf>;

    /// The project whose directory holds `path`, which is relative to the
    /// root of the checkout: its name and directory. The deepest such
    /// project; `None` when no known project holds it.
    fn holder(&self, _path: &Path) -> Option<(String, PathBuf)> {
        None
    }
}

/// Anchors no file reference can be placed by: every reference then
/// counts as matching whenever anything changed.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAnchors;

impl PlaceAnchor for NoAnchors {
    fn place(&self, _anchor: &FilesAnchor) -> Option<PathBuf> {
        None
    }
}

/// Where the inputs of one project's tasks start from, in the frame of the
/// changed files.
#[derive(Clone, Copy)]
pub struct InputFrame<'frame> {
    /// The project's directory, relative to the root of the checkout; `.`
    /// or empty for a project at that root.
    pub project_root: &'frame Path,
    /// Where the anchors of file references lie.
    pub anchors: &'frame dyn PlaceAnchor,
}

/// Compare the changed files with one input pattern of a task, read from
/// where it starts.
///
/// A path of the project is read by [`match_pattern`]. A path of a file
/// reference is read from its anchor, and only files beneath the anchor are
/// compared: no other reading of it exists. A reference whose anchor cannot
/// be placed, or whose path breaks the rules every reference path follows,
/// cannot be shown to be unaffected and matches whenever anything changed;
/// the caller has bound the reference and reported why.
#[must_use]
pub fn match_input(
    files: &[PathBuf],
    frame: InputFrame<'_>,
    pattern: InputPattern<'_>,
) -> PatternMatch {
    match pattern {
        InputPattern::Project(path) => match_pattern(files, frame.project_root, path),
        InputPattern::Anchored { anchor, path } => {
            let placed = frame
                .anchors
                .place(anchor)
                .filter(|_| anchored_path_problem(path).is_none());
            let matched = match placed {
                Some(directory) => matches_anchored(files, &directory, path),
                None => !files.is_empty(),
            };
            if matched {
                PatternMatch::Matched
            } else {
                PatternMatch::None
            }
        }
    }
}

/// Whether a changed file beneath `anchor` matches `path`, read from the
/// anchor. An invalid glob cannot be compared and matches whenever anything
/// changed.
fn matches_anchored(files: &[PathBuf], anchor: &Path, path: &str) -> bool {
    let normal = cuenv_vcs::pattern::normalize(path);
    if cuenv_vcs::pattern::is_glob(&normal) && Glob::new(&normal).is_err() {
        return !files.is_empty();
    }
    let reader = PatternReader::new(&normal);
    files
        .iter()
        .any(|file| within_project(file, anchor).is_some_and(|relative| reader.matches(relative)))
}

/// Trait for types that can determine if they are affected by file changes.
///
/// Implement this trait on task types to enable affected detection in
/// CI pipelines and incremental builds.
pub trait AffectedBy {
    /// Returns true if this item is affected by the given file changes.
    ///
    /// `changed_files` are relative to the root of the checkout, and
    /// `frame` says where the item's inputs start from in that frame.
    fn is_affected_by(&self, changed_files: &[PathBuf], frame: InputFrame<'_>) -> bool;

    /// Returns the input patterns that determine what affects this item.
    ///
    /// Used for debugging and reporting which patterns matched.
    fn input_patterns(&self) -> Vec<InputPattern<'_>>;
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
        // A pattern is read from its project: the root file is no match.
        assert!(!matches_pattern(&shared, root, "shared/**"));
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

    /// `content` lives in `content/`, the module at the checkout root.
    struct Anchors;

    impl PlaceAnchor for Anchors {
        fn place(&self, anchor: &FilesAnchor) -> Option<PathBuf> {
            match anchor {
                FilesAnchor::Project(name) if name == "content" => Some(PathBuf::from("content")),
                FilesAnchor::Project(_) => None,
                FilesAnchor::Module => Some(PathBuf::new()),
            }
        }
    }

    fn anchored<'a>(anchor: &'a FilesAnchor, path: &'a str) -> InputPattern<'a> {
        InputPattern::Anchored { anchor, path }
    }

    #[test]
    fn a_file_reference_is_read_from_its_anchor() {
        let frame = InputFrame {
            project_root: Path::new("app"),
            anchors: &Anchors,
        };
        let content = FilesAnchor::Project("content".to_string());
        let article = vec![PathBuf::from("content/articles/one.md")];

        assert_eq!(
            match_input(&article, frame, anchored(&content, "articles/**")),
            PatternMatch::Matched
        );
        assert_eq!(
            match_input(&article, frame, anchored(&content, "articles")),
            PatternMatch::Matched
        );
        assert_eq!(
            match_input(&article, frame, anchored(&content, "drafts/**")),
            PatternMatch::None
        );
        // The same path of the consumer's own project is another file.
        assert_eq!(
            match_input(&article, frame, InputPattern::Project("articles/**")),
            PatternMatch::None
        );
        // A first-segment glob stays beneath its anchor.
        assert_eq!(
            match_input(
                &[PathBuf::from("docs/one.md")],
                frame,
                anchored(&content, "*/one.md")
            ),
            PatternMatch::None
        );
    }

    #[test]
    fn a_module_reference_is_read_from_the_module_root() {
        let frame = InputFrame {
            project_root: Path::new("services/api"),
            anchors: &Anchors,
        };
        let module = FilesAnchor::Module;

        for (changed, path, expected) in [
            ("bun.lock", "bun.lock", PatternMatch::Matched),
            (
                "services/shared/a.ts",
                "services/shared/**",
                PatternMatch::Matched,
            ),
            ("protos/a/b.proto", "**/*.proto", PatternMatch::Matched),
            ("services/api/bun.lock", "bun.lock", PatternMatch::None),
        ] {
            assert_eq!(
                match_input(&[PathBuf::from(changed)], frame, anchored(&module, path)),
                expected,
                "{changed} against {path}"
            );
        }
    }

    #[test]
    fn a_reference_that_cannot_be_placed_matches_whenever_anything_changed() {
        let frame = InputFrame {
            project_root: Path::new("app"),
            anchors: &Anchors,
        };
        let unknown = FilesAnchor::Project("elsewhere".to_string());
        let content = FilesAnchor::Project("content".to_string());
        let unrelated = vec![PathBuf::from("docs/readme.md")];

        assert_eq!(
            match_input(&unrelated, frame, anchored(&unknown, "articles/**")),
            PatternMatch::Matched
        );
        assert_eq!(
            match_input(&unrelated, frame, anchored(&content, "../docs/**")),
            PatternMatch::Matched
        );
        assert_eq!(
            match_input(&[], frame, anchored(&unknown, "articles/**")),
            PatternMatch::None
        );
        let nowhere = InputFrame {
            project_root: Path::new("app"),
            anchors: &NoAnchors,
        };
        assert_eq!(
            match_input(&unrelated, nowhere, anchored(&content, "articles/**")),
            PatternMatch::Matched
        );
    }
}
