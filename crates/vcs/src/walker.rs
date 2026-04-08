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
use walkdir::WalkDir;

/// Workspace-rooted walker that streams SHA-256 over every matched file.
#[derive(Debug, Clone)]
pub struct WalkHasher {
    workspace_root: PathBuf,
}

impl WalkHasher {
    /// Build a walker rooted at `workspace_root`.
    #[must_use]
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
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

        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        let mut results: Vec<HashedInput> = Vec::new();

        for raw in &explicit_files {
            let abs = self.workspace_root.join(raw);
            if abs.is_file() {
                let rel = normalize_rel_path(Path::new(raw));
                if seen.insert(rel.clone()) {
                    let (hash, size) = Self::hash_file(&abs)?;
                    results.push(HashedInput {
                        relative_path: rel,
                        absolute_path: canonical_or_abs(&abs),
                        sha256: hash,
                        size,
                        is_executable: is_executable(&abs)?,
                    });
                }
            } else {
                return Err(Error::io(
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("explicit input file '{raw}' not found"),
                    ),
                    &abs,
                    "open",
                ));
            }
        }

        for (base_dir, globset) in &dirs_to_walk {
            let walk_root = self.workspace_root.join(base_dir);
            if !walk_root.exists() {
                debug!(dir = %base_dir, "Directory does not exist, skipping");
                continue;
            }
            for entry in WalkDir::new(&walk_root).follow_links(true) {
                let entry = entry.map_err(|e| {
                    let path = e.path().unwrap_or(walk_root.as_path());
                    Error::io(
                        std::io::Error::new(
                            e.io_error()
                                .map_or(std::io::ErrorKind::Other, std::io::Error::kind),
                            format!("walkdir error under {}: {e}", walk_root.display()),
                        ),
                        path,
                        "walkdir",
                    )
                })?;
                let path = entry.path();
                if path.is_dir() {
                    continue;
                }
                let Ok(rel) = path.strip_prefix(&self.workspace_root) else {
                    continue;
                };
                let rel_norm = normalize_rel_path(rel);
                if globset.is_match(rel_norm.as_path()) && seen.insert(rel_norm.clone()) {
                    let (hash, size) = Self::hash_file(path)?;
                    results.push(HashedInput {
                        relative_path: rel_norm,
                        absolute_path: canonical_or_abs(path),
                        sha256: hash,
                        size,
                        is_executable: is_executable(path)?,
                    });
                }
            }
        }

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

/// Strip `.` / `..` components from a relative path so the result is a clean
/// workspace-relative identifier.
fn normalize_rel_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s),
            _ => {}
        }
    }
    out
}

/// Canonicalize a path, falling back to the absolute form when canonicalize fails.
fn canonical_or_abs(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(p)
        }
    })
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

        assert!(err.to_string().contains("walkdir"));
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
        assert_eq!(normalize_rel_path(Path::new("a/../b")), PathBuf::from("b"));
    }
}
