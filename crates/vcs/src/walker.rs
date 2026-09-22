//! [`WalkHasher`]: a VCS-free [`VcsHasher`] implementation.
//!
//! `WalkHasher` resolves glob/directory/file patterns against a workspace
//! root and computes a streaming SHA-256 over every matched file. It is the
//! default fallback when a VCS-specific implementation isn't available.

use crate::error::{Error, Result};
use crate::hasher::{HashedInput, VcsHasher};
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

    fn resolve_sync(&self, patterns: &[String]) -> Result<Vec<HashedInput>> {
        let mut explicit_files: Vec<String> = Vec::new();
        let mut dirs_to_walk: Vec<(String, GlobSet)> = Vec::new();

        for pat in patterns {
            let trimmed = pat.trim();
            if trimmed.is_empty() {
                continue;
            }
            validate_pattern(trimmed)?;
            let looks_like_glob = trimmed.contains('*')
                || trimmed.contains('{')
                || trimmed.contains('?')
                || trimmed.contains('[');
            let abs = self.workspace_root.join(trimmed);

            if looks_like_glob {
                let base_dir = extract_glob_base(trimmed);
                let glob = Glob::new(trimmed).map_err(|e| {
                    Error::pattern(format!("invalid glob pattern `{trimmed}`: {e}"))
                })?;
                let set = GlobSetBuilder::new()
                    .add(glob)
                    .build()
                    .map_err(|e| Error::pattern(format!("failed to build globset: {e}")))?;
                dirs_to_walk.push((base_dir, set));
            } else if abs.is_dir() {
                let glob_pat = format!("{}/**/*", trimmed.trim_end_matches('/'));
                let glob = Glob::new(&glob_pat).map_err(|e| {
                    Error::pattern(format!("invalid glob pattern `{glob_pat}`: {e}"))
                })?;
                let set = GlobSetBuilder::new()
                    .add(glob)
                    .build()
                    .map_err(|e| Error::pattern(format!("failed to build globset: {e}")))?;
                dirs_to_walk.push((trimmed.to_string(), set));
            } else {
                explicit_files.push(trimmed.to_string());
            }
        }

        let workspace = fs::canonicalize(&self.workspace_root)
            .map_err(|e| Error::io(e, &self.workspace_root, "canonicalize"))?;
        let mut collected = Collected::default();
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
                Ok(target) => target,
                Err(error) if fs::symlink_metadata(&abs).is_ok() => {
                    return Err(Error::io(error, &abs, "follow dangling symlink input"));
                }
                Err(_) => return Err(explicit_not_found(raw, &abs)),
            };
            if !target.is_file() {
                return Err(explicit_not_found(raw, &abs));
            }
            collected.push(normalize_rel_path(Path::new(raw)), &target)?;
        }

        for (base_dir, globset) in &dirs_to_walk {
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

/// Inputs resolved so far, deduplicated by workspace-relative path.
#[derive(Default)]
struct Collected {
    seen: BTreeSet<PathBuf>,
    results: Vec<HashedInput>,
}

impl Collected {
    /// Record the file at `target` as the input named `relative`.
    ///
    /// `target` is the resolved file, so a symlinked input contributes the
    /// bytes and mode of what it points at, and is staged as a regular copy.
    fn push(&mut self, relative: PathBuf, target: &Path) -> Result<()> {
        if !self.seen.insert(relative.clone()) {
            return Ok(());
        }
        let (sha256, size) = WalkHasher::hash_file(target)?;
        self.results.push(HashedInput {
            relative_path: relative,
            absolute_path: target.to_path_buf(),
            sha256,
            size,
            is_executable: is_executable(target)?,
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
struct Walk<'a> {
    globset: &'a GlobSet,
    /// Canonical workspace root.
    workspace: &'a Path,
    /// Canonical directories never descended into.
    excluded: &'a [PathBuf],
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
            let entry = entry.map_err(|e| Error::io(e, physical, "walk directory"))?;
            let path = entry.path();
            if is_scratch_dir(&path) {
                continue;
            }
            let logical_child = logical.join(entry.file_name());
            let file_type = entry
                .file_type()
                .map_err(|e| Error::io(e, &path, "walk directory"))?;
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

/// Extract the literal-prefix of a glob pattern.
///
/// * `src/**/*.ts` → `src`
/// * `**/*.ts` → `` (workspace root)
/// * `foo/bar/*.rs` → `foo/bar`
fn extract_glob_base(pattern: &str) -> String {
    let mut parts = Vec::new();
    for segment in pattern.split('/') {
        if segment.contains('*')
            || segment.contains('{')
            || segment.contains('?')
            || segment.contains('[')
        {
            break;
        }
        if !segment.is_empty() {
            parts.push(segment);
        }
    }
    parts.join("/")
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
    fn empty_and_whitespace_patterns_are_ignored() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "content").unwrap();
        let hasher = WalkHasher::new(tmp.path());
        let inputs = hasher.resolve_sync(&[String::new(), "  ".into()]).unwrap();
        assert!(inputs.is_empty());
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
    fn extract_glob_base_handles_common_shapes() {
        assert_eq!(extract_glob_base("src/**/*.ts"), "src");
        assert_eq!(extract_glob_base("**/*.ts"), "");
        assert_eq!(extract_glob_base("foo/bar/*.rs"), "foo/bar");
        assert_eq!(extract_glob_base("*.txt"), "");
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
        use std::os::unix::fs::symlink;

        let workspace = TempDir::new().unwrap();
        let root = workspace.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        symlink(root.join("src/main.rs"), root.join("src/alias.rs")).unwrap();

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
        use std::os::unix::fs::symlink;

        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("shared.toml"), "shared").unwrap();
        symlink(
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
        use std::os::unix::fs::symlink;

        let workspace = TempDir::new().unwrap();
        let root = workspace.path();
        fs::create_dir_all(root.join("packages/lib/src")).unwrap();
        fs::write(root.join("packages/lib/src/index.ts"), "lib").unwrap();
        fs::create_dir_all(root.join("app/node_modules")).unwrap();
        symlink(root.join("packages/lib"), root.join("app/node_modules/lib")).unwrap();

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
        use std::os::unix::fs::symlink;

        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::create_dir_all(workspace.path().join("src")).unwrap();
        fs::write(workspace.path().join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(outside.path().join("store.rs"), "not ours").unwrap();
        symlink(outside.path(), workspace.path().join("result")).unwrap();

        assert_eq!(
            resolved(workspace.path(), &["**/*.rs"]),
            vec![(PathBuf::from("src/main.rs"), "fn main() {}".to_string())]
        );
    }

    #[cfg(unix)]
    #[test]
    fn naming_a_path_through_a_directory_symlink_follows_it() {
        use std::os::unix::fs::symlink;

        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::create_dir_all(outside.path().join("bin")).unwrap();
        fs::write(outside.path().join("bin/tool"), "tool").unwrap();
        symlink(outside.path(), workspace.path().join("result")).unwrap();

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
        use std::os::unix::fs::symlink;

        let workspace = TempDir::new().unwrap();
        let root = workspace.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        symlink(root.join("gone"), root.join("src/stale.txt")).unwrap();

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
        use std::os::unix::fs::symlink;

        let workspace = TempDir::new().unwrap();
        let root = workspace.path();
        fs::create_dir_all(root.join("pkg")).unwrap();
        fs::write(root.join("pkg/index.js"), "pkg").unwrap();
        symlink(root.join("pkg"), root.join("pkg/self")).unwrap();

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
