//! What git leaves out of a checkout, so input globs leave it out too.
//!
//! An input glob such as `crates/**` means the files of the repository under
//! `crates/`, not whatever build output, editor state or dependency tree a
//! local checkout happens to hold there. Hashing those would make cache keys
//! and exec roots differ between a developer's machine and CI.
//!
//! Only the `.gitignore` files inside the repository count. A user's global
//! excludes file and `.git/info/exclude` are deliberately not read: they
//! differ between machines, and the files an input selects must not.
//!
//! A tracked file is part of the repository even when a rule matches it, as
//! it is for git.

use crate::error::{Error, Result};
use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// The git repository a workspace belongs to: where its work tree is and
/// which files it tracks.
#[derive(Debug)]
pub struct Checkout {
    /// Canonical root of the work tree.
    root: PathBuf,
    /// Every path in the index, relative to the work tree and `/`-separated.
    tracked: BTreeSet<String>,
}

impl Checkout {
    /// The repository `workspace` lies in, or `None` when it lies in none or
    /// in a bare one. Outside a repository nothing is ignored, as for git.
    ///
    /// # Errors
    ///
    /// Fails when the repository is found but its index cannot be read.
    pub fn discover(workspace: &Path) -> Result<Option<Self>> {
        let Ok(repository) = gix::discover(workspace) else {
            return Ok(None);
        };
        let Some(workdir) = repository.workdir() else {
            return Ok(None);
        };
        let root = fs::canonicalize(workdir).map_err(|e| Error::io(e, workdir, "canonicalize"))?;
        let index = repository
            .index_or_empty()
            .map_err(|e| Error::repository(format!("read the index of {}: {e}", root.display())))?;
        let tracked = index
            .entries()
            .iter()
            .map(|entry| entry.path(&index).to_string())
            .collect();
        Ok(Some(Self { root, tracked }))
    }

    /// `path`, a canonical path, relative to the work tree.
    pub fn relative(&self, path: &Path) -> Option<PathBuf> {
        path.strip_prefix(&self.root).ok().map(Path::to_path_buf)
    }

    fn tracks(&self, relative: &Path) -> bool {
        self.tracked.contains(&slashed(relative))
    }

    fn tracks_under(&self, relative: &Path) -> bool {
        let prefix = format!("{}/", slashed(relative));
        self.tracked
            .range(prefix.clone()..)
            .next()
            .is_some_and(|path| path.starts_with(&prefix))
    }
}

/// What a walk does with one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Part of the repository.
    Keep,
    /// Ignored and untracked: not part of the repository.
    Skip,
    /// An ignored directory that still holds tracked files: enter it, but
    /// keep only what is tracked.
    TrackedOnly,
}

/// Whether an entry is a directory, as gitignore rules ending in `/` ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
}

/// The ignore rules in force inside one directory of a walk.
#[derive(Debug, Clone)]
pub struct Rules {
    checkout: Arc<Checkout>,
    /// One matcher per `.gitignore` from the work tree root down to this
    /// directory, outermost first.
    layers: Vec<Arc<Gitignore>>,
    /// This directory lies inside an ignored one.
    tracked_only: bool,
}

impl Rules {
    /// The rules for the contents of `directory`, relative to the work tree.
    ///
    /// `None` when `directory`, or a directory above it, is itself ignored.
    /// Naming an ignored location — `node_modules/**`, `target/` — is how an
    /// input asks for what git leaves out, so nothing under it is filtered.
    ///
    /// # Errors
    ///
    /// Fails when a `.gitignore` on the way cannot be read.
    pub fn at(checkout: Arc<Checkout>, directory: &Path) -> Result<Option<Self>> {
        let root = checkout.root.clone();
        let mut rules = Self {
            checkout,
            layers: Vec::new(),
            tracked_only: false,
        }
        .enter(&root, Path::new(""))?;
        let mut relative = PathBuf::new();
        for component in directory.components() {
            let Component::Normal(segment) = component else {
                continue;
            };
            relative.push(segment);
            if rules.verdict(&relative, Kind::Directory) != Verdict::Keep {
                return Ok(None);
            }
            rules = rules.enter(&root.join(&relative), &relative)?;
        }
        Ok(Some(rules))
    }

    /// The rules inside `relative`, a directory this walk enters, whose
    /// files are read from `physical`.
    ///
    /// # Errors
    ///
    /// Fails when the directory's `.gitignore` cannot be read.
    pub fn enter(&self, physical: &Path, relative: &Path) -> Result<Self> {
        let mut inner = self.clone();
        let file = physical.join(".gitignore");
        if file.is_file() {
            let mut builder = GitignoreBuilder::new(self.checkout.root.join(relative));
            if let Some(error) = builder.add(&file) {
                return Err(Error::repository(format!(
                    "read {}: {error}",
                    file.display()
                )));
            }
            let matcher = builder
                .build()
                .map_err(|error| Error::repository(format!("parse {}: {error}", file.display())))?;
            inner.layers.push(Arc::new(matcher));
        }
        Ok(inner)
    }

    /// The rules inside `relative`, an ignored directory that holds tracked
    /// files.
    pub fn tracked_only(&self) -> Self {
        Self {
            tracked_only: true,
            ..self.clone()
        }
    }

    /// What to do with the entry at `relative`, relative to the work tree.
    pub fn verdict(&self, relative: &Path, kind: Kind) -> Verdict {
        let ignored = self.tracked_only || self.ignores(relative, kind);
        match (ignored, kind) {
            (false, _) => Verdict::Keep,
            (true, Kind::File) if self.checkout.tracks(relative) => Verdict::Keep,
            (true, Kind::Directory) if self.checkout.tracks_under(relative) => Verdict::TrackedOnly,
            (true, _) => Verdict::Skip,
        }
    }

    fn ignores(&self, relative: &Path, kind: Kind) -> bool {
        let path = self.checkout.root.join(relative);
        self.layers
            .iter()
            .rev()
            .map(|layer| layer.matched(&path, kind == Kind::Directory))
            .find(|matched| !matched.is_none())
            .is_some_and(|matched| matches!(matched, Match::Ignore(_)))
    }
}

/// The directories below `root` that git leaves out of the checkout.
///
/// A directory is left out when the repository's `.gitignore` files ignore
/// it and it holds no tracked file. Each is relative to `root`, and only the topmost one of an ignored
/// tree is listed. Directories whose name starts with `.` or `_` are not
/// entered, and symbolic links are not followed.
///
/// Empty when `root` lies outside a repository or inside an ignored
/// directory, since a caller that names an ignored location asks for it.
///
/// # Errors
///
/// Fails when `root` cannot be resolved, or when the repository's index or a
/// `.gitignore` on the way cannot be read.
pub fn ignored_directories(root: &Path) -> Result<Vec<PathBuf>> {
    let root = fs::canonicalize(root).map_err(|e| Error::io(e, root, "canonicalize"))?;
    let Some(checkout) = Checkout::discover(&root)? else {
        return Ok(Vec::new());
    };
    let Some(base) = checkout.relative(&root) else {
        return Ok(Vec::new());
    };
    let checkout = Arc::new(checkout);
    let Some(rules) = Rules::at(Arc::clone(&checkout), &base)? else {
        return Ok(Vec::new());
    };
    let mut ignored = Vec::new();
    let mut pending = vec![(base.clone(), rules)];
    while let Some((directory, rules)) = pending.pop() {
        let physical = checkout.root.join(&directory);
        let Ok(entries) = fs::read_dir(&physical) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let name = entry.file_name();
            if name
                .to_str()
                .is_some_and(|name| name.starts_with('.') || name.starts_with('_'))
            {
                continue;
            }
            let child = directory.join(&name);
            match rules.verdict(&child, Kind::Directory) {
                Verdict::Keep => {
                    let inner = rules.enter(&physical.join(&name), &child)?;
                    pending.push((child, inner));
                }
                Verdict::TrackedOnly => pending.push((child, rules.tracked_only())),
                Verdict::Skip => {
                    if let Ok(relative) = child.strip_prefix(&base) {
                        ignored.push(relative.to_path_buf());
                    }
                }
            }
        }
    }
    ignored.sort();
    Ok(ignored)
}

fn slashed(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(segment) => Some(segment.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout(root: &Path, tracked: &[&str]) -> Arc<Checkout> {
        Arc::new(Checkout {
            root: root.to_path_buf(),
            tracked: tracked.iter().map(|path| (*path).to_owned()).collect(),
        })
    }

    #[test]
    fn nested_gitignore_files_and_negations_decide_in_git_order()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::TempDir::new()?;
        let root = fs::canonicalize(tmp.path())?;
        fs::create_dir_all(root.join("crates/app/target"))?;
        fs::write(root.join(".gitignore"), "target/\n*.log\n")?;
        fs::write(root.join("crates/app/.gitignore"), "!keep.log\n")?;

        let top = Rules::at(checkout(&root, &[]), Path::new("crates/app"))?
            .ok_or("crates/app is not ignored")?;
        let app = Path::new("crates/app");
        assert_eq!(
            top.verdict(&app.join("target"), Kind::Directory),
            Verdict::Skip
        );
        assert_eq!(
            top.verdict(&app.join("debug.log"), Kind::File),
            Verdict::Skip
        );
        assert_eq!(
            top.verdict(&app.join("keep.log"), Kind::File),
            Verdict::Keep
        );
        assert_eq!(top.verdict(&app.join("lib.rs"), Kind::File), Verdict::Keep);
        Ok(())
    }

    #[test]
    fn naming_an_ignored_directory_turns_filtering_off()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::TempDir::new()?;
        let root = fs::canonicalize(tmp.path())?;
        fs::create_dir_all(root.join("node_modules/pkg"))?;
        fs::write(root.join(".gitignore"), "node_modules/\n")?;

        let checkout = checkout(&root, &[]);
        assert!(Rules::at(Arc::clone(&checkout), Path::new("node_modules"))?.is_none());
        assert!(Rules::at(checkout, Path::new("node_modules/pkg"))?.is_none());
        Ok(())
    }

    #[test]
    fn tracked_files_survive_the_rules_that_match_them()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::TempDir::new()?;
        let root = fs::canonicalize(tmp.path())?;
        fs::write(root.join(".gitignore"), "vendor/\n*.bin\n")?;

        let rules = Rules::at(
            checkout(&root, &["vendor/lib/a.c", "fixture.bin"]),
            Path::new(""),
        )?
        .ok_or("the root is not ignored")?;
        assert_eq!(
            rules.verdict(Path::new("fixture.bin"), Kind::File),
            Verdict::Keep
        );
        assert_eq!(
            rules.verdict(Path::new("vendor"), Kind::Directory),
            Verdict::TrackedOnly
        );
        let inside = rules.tracked_only();
        assert_eq!(
            inside.verdict(Path::new("vendor/lib/a.c"), Kind::File),
            Verdict::Keep
        );
        assert_eq!(
            inside.verdict(Path::new("vendor/lib/b.c"), Kind::File),
            Verdict::Skip
        );
        Ok(())
    }

    #[test]
    fn ignored_directories_lists_topmost_untracked_ignored_trees() -> Result<()> {
        let tmp = tempfile::tempdir().map_err(|e| Error::io(e, "tmp", "tempdir"))?;
        let root = tmp.path();
        gix::init(root).map_err(|e| Error::repository(e.to_string()))?;
        for directory in [
            "target/debug",
            "crates/app/target",
            "crates/app/src",
            "node_modules/pkg",
            ".cache/inner",
            "_scratch/target",
        ] {
            fs::create_dir_all(root.join(directory)).map_err(|e| Error::io(e, root, "mkdir"))?;
        }
        fs::write(root.join(".gitignore"), "target/\nnode_modules/\n")
            .map_err(|e| Error::io(e, root, "write"))?;

        assert_eq!(
            ignored_directories(root)?,
            [
                PathBuf::from("crates/app/target"),
                PathBuf::from("node_modules"),
                PathBuf::from("target"),
            ]
        );
        assert_eq!(
            ignored_directories(&root.join("crates"))?,
            [PathBuf::from("app/target")]
        );
        assert_eq!(
            ignored_directories(&root.join("target"))?,
            Vec::<PathBuf>::new()
        );
        Ok(())
    }

    #[test]
    fn ignored_directories_outside_a_repository_is_empty() -> Result<()> {
        let tmp = tempfile::tempdir().map_err(|e| Error::io(e, "tmp", "tempdir"))?;
        fs::create_dir_all(tmp.path().join("target")).map_err(|e| Error::io(e, "tmp", "mkdir"))?;
        fs::write(tmp.path().join(".gitignore"), "target/\n")
            .map_err(|e| Error::io(e, "tmp", "write"))?;
        assert_eq!(ignored_directories(tmp.path())?, Vec::<PathBuf>::new());
        Ok(())
    }
}
