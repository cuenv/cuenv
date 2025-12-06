use crate::{Error, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use tracing;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct ResolvedInputFile {
    pub rel_path: PathBuf,
    pub source_path: PathBuf,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct ResolvedInputs {
    pub files: Vec<ResolvedInputFile>,
}

impl ResolvedInputs {
    pub fn to_summary_map(&self) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        for f in &self.files {
            map.insert(
                normalize_rel_path(&f.rel_path)
                    .to_string_lossy()
                    .to_string(),
                f.sha256.clone(),
            );
        }
        map
    }
}

fn normalize_rel_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s),
            _ => {}
        }
    }
    out
}

pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let _span = tracing::trace_span!("sha256_file", path = %path.display()).entered();
    let mut file = fs::File::open(path).map_err(|e| Error::Io {
        source: e,
        path: Some(path.into()),
        operation: "open".into(),
    })?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1024 * 64];
    let mut total: u64 = 0;
    loop {
        let n = file.read(&mut buf).map_err(|e| Error::Io {
            source: e,
            path: Some(path.into()),
            operation: "read".into(),
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    let digest = hasher.finalize();
    tracing::trace!(path = %path.display(), size = total, "Hashed file");
    Ok((hex::encode(digest), total))
}

pub struct InputResolver {
    project_root: PathBuf,
}

impl InputResolver {
    pub fn new(project_root: impl AsRef<Path>) -> Self {
        Self {
            project_root: project_root.as_ref().to_path_buf(),
        }
    }

    pub fn resolve(&self, patterns: &[String]) -> Result<ResolvedInputs> {
        let resolve_span = tracing::info_span!(
            "input_resolver.resolve",
            root = %self.project_root.display(),
            pattern_count = patterns.len()
        );
        let _resolve_guard = resolve_span.enter();

        tracing::debug!(
            patterns = ?patterns,
            "Starting input resolution"
        );

        // Categorize patterns: explicit files, directories to walk, and globs
        let mut explicit_files: Vec<String> = Vec::new();
        let mut dirs_to_walk: Vec<(String, GlobSet)> = Vec::new(); // (dir_path, globset for matching)

        let pattern_span = tracing::debug_span!("patterns.analyze");
        {
            let _g = pattern_span.enter();
            for pat in patterns {
                let p = pat.trim();
                if p.is_empty() {
                    continue;
                }

                let looks_like_glob =
                    p.contains('*') || p.contains('{') || p.contains('?') || p.contains('[');
                let abs = self.project_root.join(p);

                if looks_like_glob {
                    // Extract base directory from glob pattern
                    let base_dir = extract_glob_base(p);
                    let glob_pat = p.to_string();
                    let glob = Glob::new(&glob_pat).map_err(|e| {
                        Error::configuration(format!("Invalid glob pattern '{glob_pat}': {e}"))
                    })?;
                    let set = GlobSetBuilder::new().add(glob).build().map_err(|e| {
                        Error::configuration(format!("Failed to build glob set: {e}"))
                    })?;
                    dirs_to_walk.push((base_dir, set));
                } else if abs.is_dir() {
                    // Directory - walk it with a recursive glob
                    let glob_pat = format!("{}/**/*", p.trim_end_matches('/'));
                    let glob = Glob::new(&glob_pat).map_err(|e| {
                        Error::configuration(format!("Invalid glob pattern '{glob_pat}': {e}"))
                    })?;
                    let set = GlobSetBuilder::new().add(glob).build().map_err(|e| {
                        Error::configuration(format!("Failed to build glob set: {e}"))
                    })?;
                    dirs_to_walk.push((p.to_string(), set));
                } else {
                    // Explicit file path
                    explicit_files.push(p.to_string());
                }
            }

            tracing::debug!(
                explicit_file_count = explicit_files.len(),
                dirs_to_walk_count = dirs_to_walk.len(),
                "Categorized input patterns"
            );
        }

        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        let mut files: Vec<ResolvedInputFile> = Vec::new();

        // Resolve explicit file paths directly (no walking needed)
        let explicit_span =
            tracing::debug_span!("explicit_files.resolve", count = explicit_files.len());
        {
            let _g = explicit_span.enter();
            for raw in &explicit_files {
                let abs = self.project_root.join(raw);
                if abs.is_file() {
                    let rel = normalize_rel_path(Path::new(raw));
                    if seen.insert(rel.clone()) {
                        let (hash, size) = sha256_file(&abs)?;
                        files.push(ResolvedInputFile {
                            rel_path: rel,
                            source_path: canonical_or_abs(&abs)?,
                            sha256: hash,
                            size,
                        });
                    }
                } else {
                    tracing::warn!(path = %raw, "Explicit input file not found");
                }
            }
            tracing::debug!(
                explicit_files_found = files.len(),
                "Explicit files resolved"
            );
        }

        // Walk only the specific directories that need walking
        if !dirs_to_walk.is_empty() {
            let walkdir_span =
                tracing::info_span!("walkdir.traverse", dirs_count = dirs_to_walk.len());
            let _g = walkdir_span.enter();

            let mut total_entries_visited: u64 = 0;
            let mut total_files_matched: u64 = 0;
            let mut total_bytes_hashed: u64 = 0;

            for (base_dir, globset) in &dirs_to_walk {
                let walk_root = self.project_root.join(base_dir);
                if !walk_root.exists() {
                    tracing::debug!(dir = %base_dir, "Directory does not exist, skipping");
                    continue;
                }

                tracing::debug!(dir = %base_dir, "Walking directory for glob matches");

                for entry in WalkDir::new(&walk_root)
                    .follow_links(true)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    total_entries_visited += 1;
                    let path = entry.path();
                    if path.is_dir() {
                        continue;
                    }

                    // Relative to project root (not walk root)
                    let rel = match path.strip_prefix(&self.project_root) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    let rel_norm = normalize_rel_path(rel);

                    // Match against this specific globset
                    if globset.is_match(rel_norm.as_path()) && seen.insert(rel_norm.clone()) {
                        total_files_matched += 1;
                        let src = canonical_or_abs(path)?;
                        let (hash, size) = sha256_file(&src)?;
                        total_bytes_hashed += size;
                        files.push(ResolvedInputFile {
                            rel_path: rel_norm,
                            source_path: src,
                            sha256: hash,
                            size,
                        });
                    }
                }
            }

            tracing::info!(
                entries_visited = total_entries_visited,
                files_matched = total_files_matched,
                total_bytes_hashed,
                "WalkDir traversal complete"
            );
        } else {
            tracing::debug!("No directories to walk, skipping WalkDir");
        }

        // Deterministic ordering
        files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

        tracing::info!(total_files = files.len(), "Input resolution complete");

        Ok(ResolvedInputs { files })
    }
}

/// Extract the base directory from a glob pattern.
/// For example:
/// - `src/**/*.ts` -> `src`
/// - `**/*.ts` -> `` (empty, meaning root)
/// - `foo/bar/*.rs` -> `foo/bar`
fn extract_glob_base(pattern: &str) -> String {
    let mut base_parts = Vec::new();
    for part in pattern.split('/') {
        if part.contains('*') || part.contains('{') || part.contains('?') || part.contains('[') {
            break;
        }
        if !part.is_empty() {
            base_parts.push(part);
        }
    }
    base_parts.join("/")
}

fn canonical_or_abs(p: &Path) -> Result<PathBuf> {
    // Resolve symlinks to target content; fall back to absolute if canonicalize fails
    match fs::canonicalize(p) {
        Ok(c) => Ok(c),
        Err(_) => Ok(p.absolutize()),
    }
}

trait Absolutize {
    fn absolutize(&self) -> PathBuf;
}
impl Absolutize for &Path {
    fn absolutize(&self) -> PathBuf {
        if self.is_absolute() {
            self.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(self)
        }
    }
}

pub fn populate_hermetic_dir(resolved: &ResolvedInputs, hermetic_root: &Path) -> Result<()> {
    // Create directories and populate files preserving relative structure
    for f in &resolved.files {
        let dest = hermetic_root.join(&f.rel_path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::Io {
                source: e,
                path: Some(parent.into()),
                operation: "create_dir_all".into(),
            })?;
        }
        // Try hardlink first
        match fs::hard_link(&f.source_path, &dest) {
            Ok(_) => {}
            Err(_e) => {
                // Fall back to copy on any error creating hardlink
                fs::copy(&f.source_path, &dest).map_err(|e2| Error::Io {
                    source: e2,
                    path: Some(dest.into()),
                    operation: "copy".into(),
                })?;
            }
        }
    }
    Ok(())
}

pub fn collect_outputs(hermetic_root: &Path, patterns: &[String]) -> Result<Vec<PathBuf>> {
    if patterns.is_empty() {
        return Ok(vec![]);
    }
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        let looks_like_glob =
            p.contains('*') || p.contains('{') || p.contains('?') || p.contains('[');
        let mut pat = p.clone();
        let abs = hermetic_root.join(&pat);
        if abs.is_dir() && !looks_like_glob {
            pat = format!("{}/**/*", pat.trim_end_matches('/'));
        }
        let glob = Glob::new(&pat)
            .map_err(|e| Error::configuration(format!("Invalid output glob '{pat}': {e}")))?;
        builder.add(glob);
    }
    let set = builder
        .build()
        .map_err(|e| Error::configuration(format!("Failed to build output globset: {e}")))?;

    let mut results = Vec::new();
    for entry in WalkDir::new(hermetic_root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let rel = match path.strip_prefix(hermetic_root) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if set.is_match(rel) {
            results.push(rel.to_path_buf());
        }
    }
    results.sort();
    Ok(results)
}

pub fn snapshot_workspace_tar_zst(src_root: &Path, dst_file: &Path) -> Result<()> {
    let file = fs::File::create(dst_file).map_err(|e| Error::Io {
        source: e,
        path: Some(dst_file.into()),
        operation: "create".into(),
    })?;
    let enc = zstd::Encoder::new(file, 3)
        .map_err(|e| Error::configuration(format!("zstd encoder error: {e}")))?;
    let mut builder = tar::Builder::new(enc);

    match builder.append_dir_all(".", src_root) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Workspace contents can legitimately disappear during a task (e.g.
            // package managers removing temp files). Skip snapshotting instead
            // of failing the whole task cache write.
            let _ = fs::remove_file(dst_file);
            tracing::warn!(
                root = %src_root.display(),
                "Skipping workspace snapshot; files disappeared during archive: {e}"
            );
            return Ok(());
        }
        Err(e) => {
            return Err(Error::configuration(format!("tar append failed: {e}")));
        }
    }

    let enc = builder
        .into_inner()
        .map_err(|e| Error::configuration(format!("tar finalize failed: {e}")))?;
    enc.finish()
        .map_err(|e| Error::configuration(format!("zstd finish failed: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn resolves_files_dirs_and_globs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // create structure
        std::fs::create_dir_all(root.join("src/sub")).unwrap();
        std::fs::write(root.join("src/a.ts"), "A").unwrap();
        std::fs::write(root.join("src/sub/b.ts"), "B").unwrap();
        std::fs::write(root.join("README.md"), "readme").unwrap();

        let resolver = InputResolver::new(root);
        let inputs = resolver
            .resolve(&["src".into(), "README.md".into(), "**/*.ts".into()])
            .unwrap();
        let rels: Vec<String> = inputs
            .files
            .iter()
            .map(|f| f.rel_path.to_string_lossy().to_string())
            .collect();
        assert!(rels.contains(&"src/a.ts".to_string()));
        assert!(rels.contains(&"src/sub/b.ts".to_string()));
        assert!(rels.contains(&"README.md".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn resolves_symlink_targets() {
        use std::os::unix::fs as unixfs;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("data/real.txt"), "hello").unwrap();
        unixfs::symlink("real.txt", root.join("data/link.txt")).unwrap();
        let resolver = InputResolver::new(root);
        let inputs = resolver.resolve(&["data/link.txt".into()]).unwrap();
        assert_eq!(inputs.files.len(), 1);
        assert!(inputs.files[0].source_path.ends_with("real.txt"));
    }

    #[test]
    fn populates_hermetic_dir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("dir")).unwrap();
        std::fs::write(root.join("dir/x.txt"), "x").unwrap();
        let resolver = InputResolver::new(root);
        let resolved = resolver.resolve(&["dir".into()]).unwrap();
        let herm = TempDir::new().unwrap();
        populate_hermetic_dir(&resolved, herm.path()).unwrap();
        assert!(herm.path().join("dir/x.txt").exists());
    }
}
