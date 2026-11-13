//! [`WalkHasher`]: a VCS-free [`VcsHasher`] implementation.
//!
//! `WalkHasher` resolves glob/directory/file patterns against a workspace
//! root and computes a streaming SHA-256 over every matched file. It is the
//! default fallback when a VCS-specific implementation isn't available.

use crate::error::{Error, Result};
use crate::hasher::{HashedInput, VcsHasher};
use crate::pattern;
use async_trait::async_trait;
use globset::{Glob, GlobSet, GlobSetBuilder};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use tracing::{debug, trace};

/// Name prefix of the transient directories cuenv creates inside a workspace
/// while staging, projecting or backing up task outputs.
///
/// They hold copies of other files and vanish when their owner finishes, so
/// nothing that scans a workspace — input hashing here, output collection in
/// the executor — may treat their contents as the user's files.
pub const SCRATCH_PREFIX: &str = ".cuenv-scratch-";

/// Whether `path` names one of cuenv's transient scratch directories.
#[must_use]
pub fn is_scratch_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(SCRATCH_PREFIX))
}

/// Workspace-rooted walker that streams SHA-256 over every matched file.
#[derive(Debug, Clone)]
pub struct WalkHasher {
    workspace_root: PathBuf,
    /// Directories a glob never descends into.
    excluded: Vec<PathBuf>,
}

impl WalkHasher {
    /// Build a walker rooted at `workspace_root`.
    #[must_use]
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
            excluded: Vec::new(),
        }
    }

    /// Never descend into `directory` while walking a glob.
    ///
    /// For cuenv's own state inside the workspace — the cache store and the
    /// exec roots under it, when the cache lives at `<project>/.cuenv-cache`
    /// or `CUENV_CACHE_DIR` points into the checkout. A `**` glob would
    /// otherwise hash the store's blobs and other tasks' exec roots as
    /// inputs. A path named explicitly is still honoured.
    #[must_use]
    pub fn excluding(mut self, directory: impl AsRef<Path>) -> Self {
        self.excluded.push(directory.as_ref().to_path_buf());
        self
    }

    /// Workspace root this walker is rooted at.
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    fn hash_file(path: &Path) -> Result<(String, u64)> {
        let mut file = fs::File::open(path).map_err(|e| Error::io(e, path, "open"))?;
        let mut hasher = Sha256::new();
        let mut buf: Box<[u8]> = vec![0u8; 64 * 1024].into_boxed_slice();
        let mut size: u64 = 0;
        loop {
            let n = file
                .read(&mut buf)
                .map_err(|e| Error::io(e, path, "read"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            size += n as u64;
        }
        Ok((hex::encode(hasher.finalize()), size))
    }

    /// Whether `pattern` selects at least one file of the workspace.
    ///
    /// The pattern is read exactly as hashing reads it, so the answer is
    /// whether hashing would find anything. Nothing is hashed, and the walk
    /// stops at the first match. A path that does not exist selects nothing.
    ///
    /// # Errors
    ///
    /// Fails when the pattern is not a valid glob, leaves the workspace, or
    /// the workspace cannot be read.
    pub fn selects_any(&self, pattern: &str) -> Result<bool> {
        self.selects_any_with(pattern, WalkOptions::default())
    }

    /// Whether `pattern` selects at least one file of the workspace when it
    /// is resolved with `options`: [`Self::selects_any`] for a walk that may
    /// be told to stay in the real directories of the repository.
    ///
    /// # Errors
    ///
    /// As [`Self::selects_any`]; with [`Reach::RealDirectories`], also when
    /// the pattern's literal prefix passes through or ends at a symbolic link
    /// or a nested repository (see [`Self::obstacle`]).
    pub fn selects_any_with(&self, pattern: &str, options: WalkOptions) -> Result<bool> {
        let found = self.resolve(
            &[pattern.to_string()],
            Resolution {
                collect: Collect::FirstMatch,
                options,
            },
        )?;
        Ok(!found.is_empty())
    }

    /// What lies in the way of `pattern` staying in the real directories of
    /// the repository: the first symbolic link or nested repository its
    /// literal prefix passes through or ends at, looked at from the
    /// workspace root down and never above it. Nothing is resolved or
    /// followed to answer, so a link is reported whatever it points at.
    ///
    /// A nested repository is a directory other than the workspace root
    /// that holds a `.git` file or directory: a submodule, a worktree or an
    /// embedded checkout.
    ///
    /// # Errors
    ///
    /// Fails when the pattern leaves the workspace or a directory on the way
    /// cannot be read.
    pub fn obstacle(&self, pattern: &str) -> Result<Option<Obstacle>> {
        let normal = pattern::normalize(pattern);
        validate_pattern(&normal)?;
        let prefix = pattern::literal_prefix(&normal);
        let mut current = self.workspace_root.clone();
        let mut relative = PathBuf::new();
        for segment in prefix.split('/').filter(|segment| !segment.is_empty()) {
            current.push(segment);
            relative.push(segment);
            let metadata = match fs::symlink_metadata(&current) {
                Ok(metadata) => metadata,
                // What does not exist selects nothing; that is for the
                // caller to report, not an obstacle.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(Error::io(error, &current, "inspect input path")),
            };
            if metadata.file_type().is_symlink() {
                return Ok(Some(Obstacle::Link(relative)));
            }
            if metadata.is_dir() && holds_repository(&current) {
                return Ok(Some(Obstacle::NestedRepository(relative)));
            }
        }
        Ok(None)
    }

    fn resolve_sync(&self, patterns: &[String]) -> Result<Vec<HashedInput>> {
        self.resolve(
            patterns,
            Resolution {
                collect: Collect::Hash,
                options: WalkOptions::default(),
            },
        )
    }

    fn resolve(&self, patterns: &[String], resolution: Resolution) -> Result<Vec<HashedInput>> {
        let Resolution { collect, options } = resolution;
        let mut explicit_files: Vec<String> = Vec::new();
        let mut dirs_to_walk: Vec<(String, GlobSet)> = Vec::new();

        for pat in patterns {
            // Every reader of an input pattern normalises it the same way;
            // see `crate::pattern`. The empty pattern is the workspace itself.
            let normal = pattern::normalize(pat);
            validate_pattern(&normal)?;
            let abs = self.workspace_root.join(&normal);

            if options.reach == Reach::RealDirectories
                && let Some(obstacle) = self.obstacle(&normal)?
            {
                return Err(Error::pattern(format!(
                    "input pattern `{normal}` {obstacle}"
                )));
            }

            if pattern::is_glob(&normal) {
                let base_dir = pattern::literal_prefix(&normal);
                dirs_to_walk.push((base_dir, single_glob(&normal)?));
            } else if abs.is_dir() {
                let glob_pat = if normal.is_empty() {
                    "**/*".to_string()
                } else {
                    format!("{normal}/**/*")
                };
                dirs_to_walk.push((normal, single_glob(&glob_pat)?));
            } else {
                explicit_files.push(normal);
            }
        }

        let workspace = fs::canonicalize(&self.workspace_root)
            .map_err(|e| Error::io(e, &self.workspace_root, "canonicalize"))?;
        let mut collected = Collected::new(collect);
        // Canonical, so a walk that reaches them through a symlink or a
        // `/var` -> `/private/var` alias still recognises them.
        let excluded: Vec<PathBuf> = self
            .excluded
            .iter()
            .filter_map(|directory| fs::canonicalize(directory).ok())
            .collect();

        for raw in &explicit_files {
            let abs = self.workspace_root.join(raw);
            // A declared path may be, or pass through, a symlink. Like a Bazel
            // source file, it stands for whatever it points at.
            let target = match fs::canonicalize(&abs) {
                Ok(target) if target.is_file() => target,
                // Asking whether a path selects anything is not an error
                // when it selects nothing.
                _ if collect == Collect::FirstMatch => continue,
                Err(error) if fs::symlink_metadata(&abs).is_ok() => {
                    return Err(Error::io(error, &abs, "follow dangling symlink input"));
                }
                Ok(_) | Err(_) => return Err(explicit_not_found(raw, &abs)),
            };
            collected.push(normalize_rel_path(Path::new(raw)), &target)?;
        }

        for (base_dir, globset) in &dirs_to_walk {
            if collected.complete() {
                break;
            }
            let walk_root = self.workspace_root.join(base_dir);
            // `exists` follows symlinks, so a dangling base is skipped too.
            if !walk_root.exists() {
                debug!(dir = %base_dir, "Directory does not exist, skipping");
                continue;
            }
            let physical = fs::canonicalize(&walk_root)
                .map_err(|e| Error::io(e, &walk_root, "canonicalize"))?;
            let mut walk = Walk {
                globset,
                workspace: &workspace,
                excluded: &excluded,
                reach: options.reach,
                descent: Vec::new(),
            };
            walk.directory(
                &mut collected,
                &physical,
                &normalize_rel_path(Path::new(base_dir)),
            )?;
        }

        let mut results = collected.results;
        // Deterministic ordering — `seen` is a BTreeSet but `results` is a Vec,
        // so we sort explicitly by relative path.
        results.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        trace!(count = results.len(), "WalkHasher resolved inputs");
        Ok(results)
    }
}

#[async_trait]
impl VcsHasher for WalkHasher {
    async fn resolve_and_hash(&self, patterns: &[String]) -> Result<Vec<HashedInput>> {
        // The walker is blocking I/O; keep it on the current task since
        // callers typically already wrap us in a spawn_blocking or a parallel
        // task executor.
        self.resolve_sync(patterns)
    }

    async fn resolve_and_hash_with(
        &self,
        patterns: &[String],
        options: WalkOptions,
    ) -> Result<Vec<HashedInput>> {
        self.resolve(
            patterns,
            Resolution {
                collect: Collect::Hash,
                options,
            },
        )
    }

    fn name(&self) -> &'static str {
        "walk"
    }
}

fn validate_pattern(pattern: &str) -> Result<()> {
    if Path::new(pattern).components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(Error::pattern(format!(
            "input pattern must stay within the workspace: {pattern}"
        )));
    }
    Ok(())
}

/// Strip `.` components from a relative path so the result is a clean
/// workspace-relative identifier.
fn normalize_rel_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        if let Component::Normal(s) = comp {
            out.push(s);
        }
    }
    out
}

/// A set that matches one glob.
fn single_glob(glob: &str) -> Result<GlobSet> {
    let compiled = Glob::new(glob)
        .map_err(|e| Error::pattern(format!("invalid glob pattern `{glob}`: {e}")))?;
    GlobSetBuilder::new()
        .add(compiled)
        .build()
        .map_err(|e| Error::pattern(format!("failed to build globset: {e}")))
}

/// What a walk may pass through on its way to files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reach {
    /// Follow symbolic links, as a plain input always has: a linked file
    /// stands for what it points at, and a linked directory inside the
    /// workspace is descended into.
    #[default]
    ThroughLinks,
    /// Stay in the real directories of this repository. A symbolic link,
    /// to a file or to a directory, and a nested repository (a directory
    /// other than the workspace root holding `.git`) are neither entered
    /// nor included, and neither is the repository's own `.git`. A pattern
    /// whose literal prefix passes through or ends at one of them is an
    /// error. The files selected are then the same in any checkout of the
    /// repository, whatever untracked links it holds.
    RealDirectories,
}

/// How [`WalkHasher`] resolves patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WalkOptions {
    /// What the walk may pass through.
    pub reach: Reach,
}

impl WalkOptions {
    /// The options for the paths of a file reference: stay in the real
    /// directories of the repository.
    #[must_use]
    pub const fn real_directories() -> Self {
        Self {
            reach: Reach::RealDirectories,
        }
    }
}

/// What keeps a pattern from staying in the real directories of the
/// repository (see [`WalkHasher::obstacle`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Obstacle {
    /// A symbolic link, relative to the workspace root.
    Link(PathBuf),
    /// A nested repository, relative to the workspace root.
    NestedRepository(PathBuf),
}

impl std::fmt::Display for Obstacle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Link(path) => write!(
                formatter,
                "goes through the symbolic link `{}`; reference the target directly",
                path.display()
            ),
            Self::NestedRepository(path) => write!(
                formatter,
                "reaches into the nested repository `{}`; a file reference cannot reach into a \
                 submodule",
                path.display()
            ),
        }
    }
}

/// Whether `directory` is the root of a repository of its own.
fn holds_repository(directory: &Path) -> bool {
    fs::symlink_metadata(directory.join(".git")).is_ok()
}

/// What one resolution collects, and how it walks.
#[derive(Debug, Clone, Copy)]
struct Resolution {
    collect: Collect,
    options: WalkOptions,
}

/// What a resolution is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Collect {
    /// Every selected file, hashed.
    Hash,
    /// Whether anything is selected: stop at the first file and hash nothing.
    FirstMatch,
}

/// Inputs resolved so far, deduplicated by workspace-relative path.
struct Collected {
    collect: Collect,
    seen: BTreeSet<PathBuf>,
    results: Vec<HashedInput>,
}

impl Collected {
    fn new(collect: Collect) -> Self {
        Self {
            collect,
            seen: BTreeSet::new(),
            results: Vec::new(),
        }
    }

    /// Whether nothing more needs to be found.
    fn complete(&self) -> bool {
        self.collect == Collect::FirstMatch && !self.results.is_empty()
    }

    /// Record the file at `target` as the input named `relative`.
    ///
    /// `target` is the resolved file, so a symlinked input contributes the
    /// bytes and mode of what it points at, and is staged as a regular copy.
    fn push(&mut self, relative: PathBuf, target: &Path) -> Result<()> {
        if !self.seen.insert(relative.clone()) {
            return Ok(());
        }
        let (sha256, size, is_executable) = match self.collect {
            Collect::Hash => {
                let (sha256, size) = WalkHasher::hash_file(target)?;
                (sha256, size, is_executable(target)?)
            }
            Collect::FirstMatch => (String::new(), 0, false),
        };
        self.results.push(HashedInput {
            relative_path: relative,
            absolute_path: target.to_path_buf(),
            sha256,
            size,
            is_executable,
        });
        Ok(())
    }
}

/// One glob's walk, following symlinks the way Bazel treats source files.
///
/// Every path is matched by its *logical* name — where it sits in the
/// workspace, including through symlinks — and hashed from its resolved
/// target. A symlinked file is followed wherever it points. A symlinked
/// directory is descended into only when it stays inside the workspace:
/// walking past a `result` link from `nix build` into the store, or a
/// `.direnv` profile, would hash a toolchain nobody declared. Naming such a
/// directory explicitly in a pattern still follows it.
struct Walk<'walk> {
    globset: &'walk GlobSet,
    /// Canonical workspace root.
    workspace: &'walk Path,
    /// Canonical directories never descended into.
    excluded: &'walk [PathBuf],
    /// What the walk may pass through.
    reach: Reach,
    /// Canonical directories on the current descent, so a symlink back to an
    /// ancestor is not followed forever.
    descent: Vec<PathBuf>,
}

impl Walk<'_> {
    fn directory(
        &mut self,
        collected: &mut Collected,
        physical: &Path,
        logical: &Path,
    ) -> Result<()> {
        if self
            .excluded
            .iter()
            .any(|excluded| physical.starts_with(excluded))
        {
            return Ok(());
        }
        if self.descent.iter().any(|ancestor| ancestor == physical) {
            debug!(path = %logical.display(), "not following a symlink cycle");
            return Ok(());
        }
        self.descent.push(physical.to_path_buf());

        let entries =
            fs::read_dir(physical).map_err(|e| Error::io(e, physical, "walk directory"))?;
        for entry in entries {
            if collected.complete() {
                break;
            }
            let entry = entry.map_err(|e| Error::io(e, physical, "walk directory"))?;
            let path = entry.path();
            if is_scratch_dir(&path) {
                continue;
            }
            let logical_child = logical.join(entry.file_name());
            let file_type = entry
                .file_type()
                .map_err(|e| Error::io(e, &path, "walk directory"))?;
            if self.reach == Reach::RealDirectories && self.leaves_repository(&entry, file_type) {
                trace!(path = %logical_child.display(), "not entering what lies outside the repository's real directories");
                continue;
            }
            if file_type.is_symlink() {
                self.symlink(collected, &path, &logical_child)?;
            } else if file_type.is_dir() {
                self.directory(collected, &path, &logical_child)?;
            } else if file_type.is_file() && self.globset.is_match(&logical_child) {
                collected.push(logical_child, &path)?;
            }
        }

        self.descent.pop();
        Ok(())
    }

    /// Whether `entry` lies outside the real directories of the repository:
    /// a symbolic link, the repository's `.git`, or a nested repository.
    fn leaves_repository(&self, entry: &fs::DirEntry, file_type: fs::FileType) -> bool {
        if file_type.is_symlink() || entry.file_name() == ".git" {
            return true;
        }
        file_type.is_dir() && {
            let path = entry.path();
            path != self.workspace && holds_repository(&path)
        }
    }

    fn symlink(&mut self, collected: &mut Collected, link: &Path, logical: &Path) -> Result<()> {
        let selected = self.globset.is_match(logical);
        let target = match fs::canonicalize(link) {
            Ok(target) => target,
            // A dangling link the pattern selects is a missing input; one it
            // does not select is none of this action's business.
            Err(error) if selected => {
                return Err(Error::io(error, link, "follow dangling symlink input"));
            }
            Err(_) => return Ok(()),
        };

        if target.is_dir() {
            if !target.starts_with(self.workspace) {
                debug!(
                    link = %logical.display(),
                    target = %target.display(),
                    "not descending into a directory symlink that leaves the workspace"
                );
                return Ok(());
            }
            return self.directory(collected, &target, logical);
        }
        if selected && target.is_file() {
            collected.push(logical.to_path_buf(), &target)?;
        }
        Ok(())
    }
}

fn explicit_not_found(raw: &str, abs: &Path) -> Error {
    Error::io(
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("explicit input file '{raw}' not found"),
        ),
        abs,
        "open",
    )
}

#[cfg(unix)]
fn is_executable(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path).map_err(|e| Error::io(e, path, "metadata"))?;
    Ok(metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> Result<bool> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn resolves_explicit_files_dirs_and_globs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src/sub")).unwrap();
        fs::write(root.join("src/a.ts"), "A").unwrap();
        fs::write(root.join("src/sub/b.ts"), "B").unwrap();
        fs::write(root.join("README.md"), "readme").unwrap();

        let hasher = WalkHasher::new(root);
        let inputs = hasher
            .resolve_sync(&["src".into(), "README.md".into(), "**/*.ts".into()])
            .unwrap();
        let rels: Vec<String> = inputs
            .iter()
            .map(|f| f.relative_path.to_string_lossy().into_owned())
            .collect();
        assert!(rels.contains(&"src/a.ts".to_string()));
        assert!(rels.contains(&"src/sub/b.ts".to_string()));
        assert!(rels.contains(&"README.md".to_string()));
    }

    #[test]
    fn deduplicates_overlapping_patterns() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "content").unwrap();
        let hasher = WalkHasher::new(tmp.path());
        let inputs = hasher
            .resolve_sync(&["a.txt".into(), "*.txt".into()])
            .unwrap();
        assert_eq!(inputs.len(), 1);
    }

    #[test]
    fn the_empty_pattern_and_a_lone_dot_mean_the_workspace_itself() {
        // Affected detection reads these as "anything in the project", so
        // hashing has to as well: a pattern hashed as nothing and matched as
        // everything would never invalidate the cache.
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("a.txt"), "content").unwrap();
        fs::write(tmp.path().join("src/b.txt"), "content").unwrap();
        let hasher = WalkHasher::new(tmp.path());
        for pattern in ["", "  ", ".", "./"] {
            let inputs = hasher.resolve_sync(&[pattern.into()]).unwrap();
            let paths: Vec<_> = inputs.iter().map(|i| i.relative_path.clone()).collect();
            assert_eq!(
                paths,
                [PathBuf::from("a.txt"), PathBuf::from("src/b.txt")],
                "{pattern:?}"
            );
        }
    }

    #[test]
    fn a_pattern_is_read_the_same_however_it_is_spelled() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("src/deep")).unwrap();
        fs::write(tmp.path().join("bun.lock"), "lock").unwrap();
        fs::write(tmp.path().join("src/a.txt"), "a").unwrap();
        fs::write(tmp.path().join("src/b.txt"), "b").unwrap();
        fs::write(tmp.path().join("src/deep/c.txt"), "c").unwrap();
        for (plain, spellings) in [
            (
                "bun.lock",
                vec!["./bun.lock", " bun.lock ", "bun.lock\u{a0}"],
            ),
            ("src", vec!["./src", "src/", "src/.", "src//", " src"]),
            (
                "src/deep/c.txt",
                vec!["src/./deep/c.txt", "src//deep/c.txt"],
            ),
            ("src/*.txt", vec!["./src/*.txt", "src//*.txt", "src/*.txt "]),
            ("src/{a,b}.txt", vec!["./src/{a,b}.txt"]),
        ] {
            let expected = resolved(tmp.path(), &[plain]);
            assert!(!expected.is_empty(), "{plain:?} selects something");
            for spelling in spellings {
                assert_eq!(resolved(tmp.path(), &[spelling]), expected, "{spelling:?}");
            }
        }
    }

    #[test]
    fn selects_any_answers_without_hashing_and_reads_patterns_as_hashing_does() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("src/deep")).unwrap();
        fs::create_dir_all(tmp.path().join("empty")).unwrap();
        fs::write(tmp.path().join("src/deep/c.txt"), "c").unwrap();
        let hasher = WalkHasher::new(tmp.path());

        for selected in [
            "src",
            "./src/",
            "src/**",
            "src/deep/c.txt",
            "**/*.txt",
            "",
            "src/{deep,x}/*",
        ] {
            assert!(hasher.selects_any(selected).unwrap(), "{selected:?}");
        }
        for nothing in [
            "shared/**",
            "shared",
            "missing.txt",
            "empty",
            "empty/**",
            "src/*.md",
        ] {
            assert!(!hasher.selects_any(nothing).unwrap(), "{nothing:?}");
        }
        assert!(hasher.selects_any("../outside").is_err());
    }

    /// The paths `patterns` select in the real directories of the repository
    /// at `root`.
    fn resolved_in_real_directories(root: &Path, patterns: &[&str]) -> Result<Vec<String>> {
        let patterns: Vec<String> = patterns.iter().map(ToString::to_string).collect();
        let inputs = WalkHasher::new(root).resolve(
            &patterns,
            Resolution {
                collect: Collect::Hash,
                options: WalkOptions::real_directories(),
            },
        )?;
        Ok(inputs
            .iter()
            .map(|input| input.relative_path.to_string_lossy().into_owned())
            .collect())
    }

    #[cfg(unix)]
    #[test]
    fn a_walk_in_real_directories_leaves_out_links_nested_repositories_and_git() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/HEAD"), "ref").unwrap();
        fs::create_dir_all(root.join("shared")).unwrap();
        fs::write(root.join("shared/a.txt"), "a").unwrap();
        fs::create_dir_all(root.join("content/articles")).unwrap();
        fs::write(root.join("content/articles/one.md"), "one").unwrap();
        std::os::unix::fs::symlink("../../shared", root.join("content/articles/vendor")).unwrap();
        std::os::unix::fs::symlink("one.md", root.join("content/articles/current.md")).unwrap();
        fs::create_dir_all(root.join("content/themes/.git")).unwrap();
        fs::write(root.join("content/themes/theme.css"), "css").unwrap();
        fs::write(root.join("content/drafts.git"), "not a repository").unwrap();

        assert_eq!(
            resolved_in_real_directories(root, &["content/**"]).unwrap(),
            ["content/articles/one.md", "content/drafts.git"]
        );
        assert_eq!(
            resolved_in_real_directories(root, &["**"]).unwrap(),
            [
                "content/articles/one.md",
                "content/drafts.git",
                "shared/a.txt"
            ],
            "no file of .git is an input"
        );
        // A plain input still follows them.
        let followed: Vec<PathBuf> = resolved(root, &["content/articles/**"])
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        assert_eq!(
            followed,
            [
                PathBuf::from("content/articles/current.md"),
                PathBuf::from("content/articles/one.md"),
                PathBuf::from("content/articles/vendor/a.txt")
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_pattern_through_a_link_or_into_a_nested_repository_is_an_obstacle() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("shared")).unwrap();
        fs::write(root.join("shared/a.txt"), "a").unwrap();
        fs::create_dir_all(root.join("content")).unwrap();
        std::os::unix::fs::symlink("../shared", root.join("content/vendor")).unwrap();
        std::os::unix::fs::symlink("../shared/a.txt", root.join("content/a.txt")).unwrap();
        fs::create_dir_all(root.join("content/themes")).unwrap();
        fs::write(root.join("content/themes/.git"), "gitdir: elsewhere").unwrap();
        let hasher = WalkHasher::new(root);

        for (pattern, obstacle) in [
            (
                "content/vendor/**",
                Obstacle::Link(PathBuf::from("content/vendor")),
            ),
            (
                "content/vendor/a.txt",
                Obstacle::Link(PathBuf::from("content/vendor")),
            ),
            (
                "content/a.txt",
                Obstacle::Link(PathBuf::from("content/a.txt")),
            ),
            (
                "content/themes/**",
                Obstacle::NestedRepository(PathBuf::from("content/themes")),
            ),
        ] {
            assert_eq!(
                hasher.obstacle(pattern).unwrap(),
                Some(obstacle),
                "{pattern}"
            );
            let error = resolved_in_real_directories(root, &[pattern]).unwrap_err();
            assert!(error.to_string().contains(pattern), "{error}");
        }
        for clear in ["content/**", "shared/a.txt", "missing/**", "**/*.txt"] {
            assert_eq!(hasher.obstacle(clear).unwrap(), None, "{clear}");
        }
        assert!(
            !hasher
                .selects_any_with("content/**", WalkOptions::real_directories())
                .unwrap(),
            "only links and a nested repository lie below content"
        );
    }

    #[test]
    fn missing_file_errors() {
        let tmp = TempDir::new().unwrap();
        let hasher = WalkHasher::new(tmp.path());
        let err = hasher
            .resolve_sync(&["nonexistent.txt".into()])
            .unwrap_err();
        assert!(matches!(
            err,
            Error::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound
        ));
    }

    #[test]
    fn same_content_yields_same_hash() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "payload").unwrap();
        fs::write(tmp.path().join("b.txt"), "payload").unwrap();
        let hasher = WalkHasher::new(tmp.path());
        let inputs = hasher.resolve_sync(&["*.txt".into()]).unwrap();
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].sha256, inputs[1].sha256);
    }

    #[test]
    fn different_content_yields_different_hash() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "one").unwrap();
        fs::write(tmp.path().join("b.txt"), "two").unwrap();
        let hasher = WalkHasher::new(tmp.path());
        let inputs = hasher.resolve_sync(&["*.txt".into()]).unwrap();
        assert_eq!(inputs.len(), 2);
        assert_ne!(inputs[0].sha256, inputs[1].sha256);
    }

    #[test]
    fn results_are_sorted_by_relative_path() {
        let tmp = TempDir::new().unwrap();
        for name in ["c.txt", "a.txt", "b.txt"] {
            fs::write(tmp.path().join(name), name).unwrap();
        }
        let hasher = WalkHasher::new(tmp.path());
        let inputs = hasher.resolve_sync(&["*.txt".into()]).unwrap();
        let names: Vec<String> = inputs
            .iter()
            .map(|i| i.relative_path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn nested_directory_walks_recursively() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("a/b/c")).unwrap();
        fs::write(tmp.path().join("a/b/c/deep.txt"), "deep").unwrap();
        let hasher = WalkHasher::new(tmp.path());
        let inputs = hasher.resolve_sync(&["a".into()]).unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].relative_path, PathBuf::from("a/b/c/deep.txt"));
    }

    #[test]
    fn glob_brackets_work() {
        let tmp = TempDir::new().unwrap();
        for name in ["a1.txt", "a2.txt", "b1.txt"] {
            fs::write(tmp.path().join(name), name).unwrap();
        }
        let hasher = WalkHasher::new(tmp.path());
        let inputs = hasher.resolve_sync(&["a[12].txt".into()]).unwrap();
        assert_eq!(inputs.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn walkdir_errors_are_not_silently_dropped() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let unreadable = tmp.path().join("restricted");
        fs::create_dir_all(&unreadable).unwrap();
        fs::write(unreadable.join("secret.txt"), "secret").unwrap();

        let mut permissions = fs::metadata(&unreadable).unwrap().permissions();
        permissions.set_mode(0o000);
        fs::set_permissions(&unreadable, permissions).unwrap();

        let hasher = WalkHasher::new(tmp.path());
        let err = hasher.resolve_sync(&["restricted".into()]).unwrap_err();

        let mut cleanup_permissions = fs::metadata(&unreadable).unwrap().permissions();
        cleanup_permissions.set_mode(0o755);
        fs::set_permissions(&unreadable, cleanup_permissions).unwrap();

        assert!(err.to_string().contains("walk directory"));
    }

    #[test]
    fn walker_name_is_walk() {
        let tmp = TempDir::new().unwrap();
        let hasher = WalkHasher::new(tmp.path());
        assert_eq!(hasher.name(), "walk");
    }

    #[tokio::test]
    async fn async_trait_method_works() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("x.txt"), "x").unwrap();
        let hasher = WalkHasher::new(tmp.path());
        let inputs = hasher.resolve_and_hash(&["*.txt".into()]).await.unwrap();
        assert_eq!(inputs.len(), 1);
    }

    #[test]
    fn normalize_rel_path_strips_dots() {
        assert_eq!(normalize_rel_path(Path::new("./a/b")), PathBuf::from("a/b"));
    }

    #[test]
    fn scratch_directories_are_never_inputs() {
        // A concurrent task projecting outputs leaves copies here for a
        // moment; hashing them would make keys depend on scheduling.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.ts"), "A").unwrap();
        let scratch = root.join(format!("{SCRATCH_PREFIX}123-0"));
        fs::create_dir_all(scratch.join("src")).unwrap();
        fs::write(scratch.join("src/a.ts"), "copy").unwrap();

        let inputs = WalkHasher::new(root)
            .resolve_sync(&["**/*.ts".to_string()])
            .unwrap();
        let rels: Vec<_> = inputs.iter().map(|f| f.relative_path.clone()).collect();
        assert_eq!(rels, vec![PathBuf::from("src/a.ts")]);
    }

    #[test]
    fn traversal_patterns_are_rejected() {
        let tmp = TempDir::new().unwrap();
        let hasher = WalkHasher::new(tmp.path());
        let error = hasher.resolve_sync(&["a/../b".to_string()]).unwrap_err();
        assert!(error.to_string().contains("must stay within the workspace"));
    }

    /// Resolve `patterns` and return `(relative path, file contents)` pairs.
    #[cfg(unix)]
    fn resolved(root: &Path, patterns: &[&str]) -> Vec<(PathBuf, String)> {
        let patterns: Vec<String> = patterns.iter().map(ToString::to_string).collect();
        WalkHasher::new(root)
            .resolve_sync(&patterns)
            .unwrap()
            .into_iter()
            .map(|input| {
                let contents = fs::read_to_string(&input.absolute_path).unwrap();
                (input.relative_path, contents)
            })
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn a_selected_file_symlink_is_hashed_as_its_target() {
        let workspace = TempDir::new().unwrap();
        let root = workspace.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::os::unix::fs::symlink(root.join("src/main.rs"), root.join("src/alias.rs")).unwrap();

        assert_eq!(
            resolved(root, &["**/*.rs"]),
            vec![
                (PathBuf::from("src/alias.rs"), "fn main() {}".to_string()),
                (PathBuf::from("src/main.rs"), "fn main() {}".to_string()),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_symlink_may_point_outside_the_workspace() {
        // Bazel treats a symlinked source file as whatever it points at.

        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("shared.toml"), "shared").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("shared.toml"),
            workspace.path().join("config.toml"),
        )
        .unwrap();

        assert_eq!(
            resolved(workspace.path(), &["*.toml"]),
            vec![(PathBuf::from("config.toml"), "shared".to_string())]
        );
        assert_eq!(
            resolved(workspace.path(), &["config.toml"]),
            vec![(PathBuf::from("config.toml"), "shared".to_string())]
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_in_workspace_directory_symlink_is_walked_under_its_own_name() {
        // pnpm links workspace packages this way.

        let workspace = TempDir::new().unwrap();
        let root = workspace.path();
        fs::create_dir_all(root.join("packages/lib/src")).unwrap();
        fs::write(root.join("packages/lib/src/index.ts"), "lib").unwrap();
        fs::create_dir_all(root.join("app/node_modules")).unwrap();
        std::os::unix::fs::symlink(root.join("packages/lib"), root.join("app/node_modules/lib"))
            .unwrap();

        assert_eq!(
            resolved(root, &["app/**/*.ts"]),
            vec![(
                PathBuf::from("app/node_modules/lib/src/index.ts"),
                "lib".to_string()
            )]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_symlink_leaving_the_workspace_is_not_walked() {
        // The shape `nix build` leaves behind. Walking it would hash a store
        // path no declaration named.

        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::create_dir_all(workspace.path().join("src")).unwrap();
        fs::write(workspace.path().join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(outside.path().join("store.rs"), "not ours").unwrap();
        std::os::unix::fs::symlink(outside.path(), workspace.path().join("result")).unwrap();

        assert_eq!(
            resolved(workspace.path(), &["**/*.rs"]),
            vec![(PathBuf::from("src/main.rs"), "fn main() {}".to_string())]
        );
    }

    #[cfg(unix)]
    #[test]
    fn naming_a_path_through_a_directory_symlink_follows_it() {
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::create_dir_all(outside.path().join("bin")).unwrap();
        fs::write(outside.path().join("bin/tool"), "tool").unwrap();
        std::os::unix::fs::symlink(outside.path(), workspace.path().join("result")).unwrap();

        assert_eq!(
            resolved(workspace.path(), &["result/bin/tool"]),
            vec![(PathBuf::from("result/bin/tool"), "tool".to_string())]
        );
        assert_eq!(
            resolved(workspace.path(), &["result/bin"]),
            vec![(PathBuf::from("result/bin/tool"), "tool".to_string())]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_fails_only_when_selected() {
        let workspace = TempDir::new().unwrap();
        let root = workspace.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::os::unix::fs::symlink(root.join("gone"), root.join("src/stale.txt")).unwrap();

        assert_eq!(
            resolved(root, &["src/**/*.rs"]),
            vec![(PathBuf::from("src/main.rs"), "fn main() {}".to_string())]
        );
        let error = WalkHasher::new(root)
            .resolve_sync(&["src/**/*.txt".to_string()])
            .unwrap_err();
        assert!(error.to_string().contains("dangling symlink"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_cycle_is_not_followed_forever() {
        let workspace = TempDir::new().unwrap();
        let root = workspace.path();
        fs::create_dir_all(root.join("pkg")).unwrap();
        fs::write(root.join("pkg/index.js"), "pkg").unwrap();
        std::os::unix::fs::symlink(root.join("pkg"), root.join("pkg/self")).unwrap();

        assert_eq!(
            resolved(root, &["**/*.js"]),
            vec![(PathBuf::from("pkg/index.js"), "pkg".to_string())]
        );
    }

    #[test]
    fn an_excluded_directory_is_never_walked() {
        let workspace = TempDir::new().unwrap();
        let root = workspace.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::create_dir_all(root.join(".cuenv-cache/exec/abc/src")).unwrap();
        fs::write(root.join(".cuenv-cache/exec/abc/src/main.rs"), "copy").unwrap();

        let inputs = WalkHasher::new(root)
            .excluding(root.join(".cuenv-cache"))
            .resolve_sync(&["**/*.rs".to_string()])
            .unwrap();

        let rels: Vec<_> = inputs.iter().map(|f| f.relative_path.clone()).collect();
        assert_eq!(rels, vec![PathBuf::from("src/main.rs")]);
    }
}
