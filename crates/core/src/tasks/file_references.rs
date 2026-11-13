//! Binding the paths of file references to the files they name.
//!
//! A file reference (`content.#Files & {paths: ["articles/**"]}`,
//! `schema.#ModuleFiles & {paths: ["bun.lock"]}`) names files by paths that
//! start from an anchor: the directory of a project, or the module root.
//! The project that holds the files declares nothing and promises nothing,
//! so whoever binds a reference for hashing or for affected detection
//! checks, for every path:
//!
//! - that it follows the rules of the schema's `#AnchoredPath`, which a
//!   value the schema never saw may break;
//! - that its anchor lies inside the repository;
//! - that its literal prefix neither goes through a symbolic link nor
//!   reaches into a nested repository;
//! - that it matches at least one file, read exactly as hashing reads it.
//!
//! A rename in the other project is then an error for the caller, never a
//! silently smaller set of inputs. Trigger derivation (`cuenv sync ci`)
//! must not depend on a working tree and checks only the first rule.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cuenv_manifest::tasks::{FilesAnchor, SourceLocation, anchored_path_problem};
use cuenv_vcs::{WalkHasher, WalkOptions};

/// One path of a file reference, bound to its anchor's directory.
#[derive(Debug, Clone, Copy)]
pub struct BoundPath<'path> {
    /// Where the path starts from, as written.
    pub anchor: &'path FilesAnchor,
    /// The directory the anchor means, absolute.
    pub directory: &'path Path,
    /// The path or glob, relative to the anchor.
    pub path: &'path str,
}

/// Checks the paths of file references against the repository they lie
/// in, remembering each answer for the rest of the run.
#[derive(Debug, Clone)]
pub struct ReferenceChecks {
    /// The root of the repository, canonical: what a reference may not
    /// leave, and where links and nested repositories are looked for from.
    root: PathBuf,
    walker: WalkHasher,
    answers: HashMap<(PathBuf, String), Result<(), String>>,
}

impl ReferenceChecks {
    /// Checks against the repository (or, outside one, the directory tree)
    /// rooted at `root`.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        let root = canonical(root);
        Self {
            walker: WalkHasher::new(&root),
            root,
            answers: HashMap::new(),
        }
    }

    /// The root every bound path is placed under.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `directory` relative to the root, empty for the root itself; `None`
    /// when it lies outside.
    #[must_use]
    pub fn place(&self, directory: &Path) -> Option<PathBuf> {
        canonical(directory)
            .strip_prefix(&self.root)
            .ok()
            .map(Path::to_path_buf)
    }

    /// Check one bound path; the error says what is wrong and what to do.
    ///
    /// # Errors
    ///
    /// Returns why the path cannot be read: it breaks the rules every
    /// reference path follows, its anchor lies outside the repository, its
    /// literal prefix goes through a symbolic link or into a nested
    /// repository, or it matches no file.
    pub fn check(&mut self, bound: BoundPath<'_>) -> Result<(), String> {
        let BoundPath {
            anchor,
            directory,
            path,
        } = bound;
        if let Some(problem) = anchored_path_problem(path) {
            return Err(path_problem(anchor, path, problem));
        }
        let Some(relative) = self.place(directory) else {
            return Err(format!(
                "{anchor} lies at {}, outside the repository at {}, so its files cannot be read",
                directory.display(),
                self.root.display()
            ));
        };
        let key = (relative, path.to_string());
        if let Some(answer) = self.answers.get(&key) {
            return answer.clone();
        }
        let answer = self.read(&key.0, bound);
        self.answers.insert(key, answer.clone());
        answer
    }

    fn read(&self, relative: &Path, bound: BoundPath<'_>) -> Result<(), String> {
        let BoundPath { anchor, path, .. } = bound;
        let pattern = anchored_pattern(relative, path);
        let written = written(anchor, path);
        match self.walker.obstacle(&pattern) {
            Ok(Some(obstacle)) => return Err(format!("{written} {obstacle}")),
            Ok(None) => {}
            Err(error) => return Err(format!("{written} cannot be read: {error}")),
        }
        match self
            .walker
            .selects_any_with(&pattern, WalkOptions::real_directories())
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!(
                "`{path}` matches nothing in {}: fix or remove the path. A file reference \
                 names files that exist; a task's outputs are read through that task",
                display_directory(relative)
            )),
            Err(error) => Err(format!("{written} cannot be read: {error}")),
        }
    }
}

/// The message for a path that breaks the rules of `#AnchoredPath`.
#[must_use]
pub fn path_problem(anchor: &FilesAnchor, path: &str, problem: &str) -> String {
    format!(
        "path \"{path}\" of {} is not a path inside {anchor}: {problem}. Paths are relative to \
         the project (for `schema.#ModuleFiles`, the module root): no `..`, `.` or empty \
         segment, no leading `/`, no surrounding space; for files outside the project use \
         `schema.#ModuleFiles`",
        anchor.spelling()
    )
}

/// The message for a reference to a project no project of the module is.
#[must_use]
pub fn unknown_project(name: &str) -> String {
    format!(
        "no project of this module is named '{name}': import the project that holds the files \
         and write `<pkg>.#Files & {{paths: [...]}}` (for files outside every project, \
         `schema.#ModuleFiles & {{paths: [...]}}`)"
    )
}

/// A problem with a task's file reference, worded for the task.
#[must_use]
pub fn for_task(task: &str, source: Option<&SourceLocation>, problem: &str) -> String {
    match source {
        Some(source) if !source.file.is_empty() => {
            format!("task '{task}' ({}:{}): {problem}", source.file, source.line)
        }
        Some(_) | None => format!("task '{task}': {problem}"),
    }
}

/// `path` as a pattern relative to the root, given its anchor's place. An
/// empty path is the anchor itself.
#[must_use]
pub fn anchored_pattern(anchor: &Path, path: &str) -> String {
    let anchor = anchor.to_string_lossy().replace('\\', "/");
    match (anchor.is_empty(), path.is_empty()) {
        (true, _) => path.to_string(),
        (false, true) => anchor,
        (false, false) => format!("{anchor}/{path}"),
    }
}

/// How a reference path is written, for messages.
fn written(anchor: &FilesAnchor, path: &str) -> String {
    format!("`{} & {{paths: [\"{path}\"]}}`", anchor.spelling())
}

fn display_directory(relative: &Path) -> String {
    if relative.as_os_str().is_empty() {
        "the module root".to_string()
    } else {
        format!("{}/", relative.display())
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn repository() -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let root = directory.path();
        for (path, content) in [
            ("content/articles/one.md", "one"),
            ("content/readme.md", "readme"),
            ("bun.lock", "lock"),
        ] {
            let path = root.join(path);
            fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
            fs::write(path, content).expect("a file");
        }
        directory
    }

    #[test]
    fn a_path_that_selects_files_under_its_anchor_binds() {
        let directory = repository();
        let mut checks = ReferenceChecks::new(directory.path());
        let content = FilesAnchor::Project("content".to_string());
        let content_directory = directory.path().join("content");

        for path in ["articles/**", "articles", "readme.md", "*.md", "**/one.md"] {
            assert_eq!(
                checks.check(BoundPath {
                    anchor: &content,
                    directory: &content_directory,
                    path,
                }),
                Ok(()),
                "{path}"
            );
        }
        assert_eq!(
            checks.check(BoundPath {
                anchor: &FilesAnchor::Module,
                directory: directory.path(),
                path: "bun.lock",
            }),
            Ok(())
        );
    }

    #[test]
    fn a_path_that_matches_nothing_is_an_error_naming_the_anchor() {
        let directory = repository();
        let mut checks = ReferenceChecks::new(directory.path());
        let content = FilesAnchor::Project("content".to_string());
        let content_directory = directory.path().join("content");

        let error = checks
            .check(BoundPath {
                anchor: &content,
                directory: &content_directory,
                path: "posts/**",
            })
            .expect_err("a renamed directory matches nothing");
        assert!(
            error.contains("`posts/**` matches nothing in content/"),
            "{error}"
        );
        // The module root's `bun.lock` is not content's.
        assert!(
            checks
                .check(BoundPath {
                    anchor: &content,
                    directory: &content_directory,
                    path: "bun.lock",
                })
                .is_err()
        );
    }

    #[test]
    fn a_path_the_schema_rejects_is_rejected_here_too() {
        let directory = repository();
        let mut checks = ReferenceChecks::new(directory.path());
        let content = FilesAnchor::Project("content".to_string());
        let content_directory = directory.path().join("content");

        for path in ["../bun.lock", "/etc/hosts", "./readme.md", "readme.md ", ""] {
            let error = checks
                .check(BoundPath {
                    anchor: &content,
                    directory: &content_directory,
                    path,
                })
                .expect_err("not a path inside the anchor");
            assert!(
                error.contains("is not a path inside project 'content'"),
                "{error}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_path_through_a_link_is_an_error_and_a_glob_leaves_links_out() {
        let directory = repository();
        let root = directory.path();
        fs::create_dir_all(root.join("shared")).expect("a directory");
        fs::write(root.join("shared/a.md"), "a").expect("a file");
        std::os::unix::fs::symlink("../shared", root.join("content/vendor")).expect("a link");
        let mut checks = ReferenceChecks::new(root);
        let content = FilesAnchor::Project("content".to_string());
        let content_directory = root.join("content");

        let error = checks
            .check(BoundPath {
                anchor: &content,
                directory: &content_directory,
                path: "vendor/**",
            })
            .expect_err("a reference through a link");
        assert!(error.contains("symbolic link `content/vendor`"), "{error}");
        assert_eq!(
            checks
                .check(BoundPath {
                    anchor: &content,
                    directory: &content_directory,
                    path: "**/a.md",
                })
                .map_err(|error| error.contains("matches nothing")),
            Err(true),
            "the only a.md lies behind a link"
        );
    }

    #[test]
    fn an_anchor_outside_the_repository_cannot_be_read() {
        let directory = repository();
        let elsewhere = tempfile::tempdir().expect("a temporary directory");
        let mut checks = ReferenceChecks::new(&directory.path().join("content"));

        let error = checks
            .check(BoundPath {
                anchor: &FilesAnchor::Module,
                directory: elsewhere.path(),
                path: "bun.lock",
            })
            .expect_err("outside the repository");
        assert!(error.contains("outside the repository"), "{error}");
    }

    #[test]
    fn a_problem_names_the_task_and_where_it_is_declared() {
        let source = SourceLocation {
            file: "app/env.cue".to_string(),
            line: 9,
            column: 2,
        };
        assert_eq!(
            for_task("build", Some(&source), "broken"),
            "task 'build' (app/env.cue:9): broken"
        );
        assert_eq!(for_task("build", None, "broken"), "task 'build': broken");
        assert_eq!(anchored_pattern(Path::new(""), "bun.lock"), "bun.lock");
        assert_eq!(
            anchored_pattern(Path::new("content"), "articles/**"),
            "content/articles/**"
        );
        assert_eq!(anchored_pattern(Path::new("content"), ""), "content");
        assert_eq!(anchored_pattern(Path::new(""), ""), "");
    }
}
