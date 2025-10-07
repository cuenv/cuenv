use crate::{Error, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
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
                normalize_rel_path(&f.rel_path).to_string_lossy().to_string(),
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
        if n == 0 { break; }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    let digest = hasher.finalize();
    Ok((hex::encode(digest), total))
}

pub struct InputResolver {
    project_root: PathBuf,
}

impl InputResolver {
    pub fn new(project_root: impl AsRef<Path>) -> Self {
        Self { project_root: project_root.as_ref().to_path_buf() }
    }

    pub fn resolve(&self, patterns: &[String]) -> Result<ResolvedInputs> {
        // Build a globset for all file patterns; directories are expanded via walk
        let mut builder = GlobSetBuilder::new();
        let mut raw_patterns: Vec<(String, bool)> = Vec::new(); // (pattern, is_dir_hint)

        for pat in patterns {
            let p = pat.trim();
            if p.is_empty() { continue; }
            let abs = self.project_root.join(p);
            let is_dir_hint = abs.is_dir();
            raw_patterns.push((p.to_string(), is_dir_hint));

            // If it looks like a glob, add as-is; else if dir, add /**
            let looks_like_glob = p.contains('*') || p.contains('{') || p.contains('?') || p.contains('[');
            let glob_pat = if looks_like_glob {
                p.to_string()
            } else if is_dir_hint {
                // ensure trailing slash insensitive recursive
                format!("{}/**/*", p.trim_end_matches('/'))
            } else {
                p.to_string()
            };
            let glob = Glob::new(&glob_pat).map_err(|e| Error::configuration(format!("Invalid glob pattern '{glob_pat}': {e}")))?;
            builder.add(glob);
        }
        let set: GlobSet = builder.build().map_err(|e| Error::configuration(format!("Failed to build glob set: {e}")))?;

        // Walk project_root and pick files that match any pattern, plus explicit file paths
        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        let mut files: Vec<ResolvedInputFile> = Vec::new();

        // Fast path: also queue explicit file paths even if the globset wouldn't match due to being plain path
        for (raw, _is_dir) in &raw_patterns {
            let abs = self.project_root.join(raw);
            if abs.is_file() {
                let rel = normalize_rel_path(Path::new(raw));
                if seen.insert(rel.clone()) {
                    let (hash, size) = sha256_file(&abs)?;
                    files.push(ResolvedInputFile { rel_path: rel, source_path: canonical_or_abs(&abs)?, sha256: hash, size });
                }
            }
        }

        for entry in WalkDir::new(&self.project_root).follow_links(true).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() { continue; }
            // Relative to root
            let rel = match path.strip_prefix(&self.project_root) { Ok(p) => p, Err(_) => continue };
            let rel_norm = normalize_rel_path(rel);
            // Match globset relative path
            if set.is_match(rel_norm.as_path()) {
                if seen.insert(rel_norm.clone()) {
                    let src = canonical_or_abs(path)?;
                    let (hash, size) = sha256_file(&src)?;
                    files.push(ResolvedInputFile { rel_path: rel_norm, source_path: src, sha256: hash, size });
                }
            }
        }

        // Deterministic ordering
        files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        Ok(ResolvedInputs { files })
    }
}

fn canonical_or_abs(p: &Path) -> Result<PathBuf> {
    // Resolve symlinks to target content; fall back to absolute if canonicalize fails
    match fs::canonicalize(p) {
        Ok(c) => Ok(c),
        Err(_) => Ok(p.absolutize())
    }
}

trait Absolutize {
    fn absolutize(&self) -> PathBuf;
}
impl Absolutize for &Path {
    fn absolutize(&self) -> PathBuf {
        if self.is_absolute() { self.to_path_buf() } else { std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(self) }
    }
}

pub fn populate_hermetic_dir(resolved: &ResolvedInputs, hermetic_root: &Path) -> Result<()> {
    // Create directories and populate files preserving relative structure
    for f in &resolved.files {
        let dest = hermetic_root.join(&f.rel_path);
        if let Some(parent) = dest.parent() { fs::create_dir_all(parent).map_err(|e| Error::Io { source: e, path: Some(parent.into()), operation: "create_dir_all".into() })?; }
        // Try hardlink first
        match fs::hard_link(&f.source_path, &dest) {
            Ok(_) => {}
            Err(e) => {
                // Cross-device or unsupported: copy
                if e.kind() == std::io::ErrorKind::CrossDeviceLink {
                    fs::copy(&f.source_path, &dest).map_err(|e2| Error::Io { source: e2, path: Some(dest.into()), operation: "copy".into() })?;
                } else {
                    // Could be other FS errors; attempt copy anyway
                    fs::copy(&f.source_path, &dest).map_err(|e2| Error::Io { source: e2, path: Some(dest.into()), operation: "copy".into() })?;
                }
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
        let looks_like_glob = p.contains('*') || p.contains('{') || p.contains('?') || p.contains('[');
        let mut pat = p.clone();
        let abs = hermetic_root.join(&pat);
        if abs.is_dir() && !looks_like_glob {
            pat = format!("{}/**/*", pat.trim_end_matches('/'));
        }
        let glob = Glob::new(&pat).map_err(|e| Error::configuration(format!("Invalid output glob '{pat}': {e}")))?;
        builder.add(glob);
    }
    let set = builder.build().map_err(|e| Error::configuration(format!("Failed to build output globset: {e}")))?;

    let mut results = Vec::new();
    for entry in WalkDir::new(hermetic_root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() { continue; }
        let rel = match path.strip_prefix(hermetic_root) { Ok(p) => p, Err(_) => continue };
        if set.is_match(rel) {
            results.push(rel.to_path_buf());
        }
    }
    results.sort();
    Ok(results)
}

pub fn snapshot_workspace_tar_zst(src_root: &Path, dst_file: &Path) -> Result<()> {
    let file = fs::File::create(dst_file).map_err(|e| Error::Io { source: e, path: Some(dst_file.into()), operation: "create".into() })?;
    let enc = zstd::Encoder::new(file, 3).map_err(|e| Error::configuration(format!("zstd encoder error: {e}")))?;
    let mut builder = tar::Builder::new(enc);
    builder.append_dir_all(".", src_root).map_err(|e| Error::configuration(format!("tar append failed: {e}")))?;
    let mut enc = builder.into_inner().map_err(|e| Error::configuration(format!("tar finalize failed: {e}")))?;
    enc.finish().map_err(|e| Error::configuration(format!("zstd finish failed: {e}")))?;
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
            .resolve(&vec!["src".into(), "README.md".into(), "**/*.ts".into()])
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
        let inputs = resolver.resolve(&vec!["data/link.txt".into()]).unwrap();
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
        let resolved = resolver.resolve(&vec!["dir".into()]).unwrap();
        let herm = TempDir::new().unwrap();
        populate_hermetic_dir(&resolved, herm.path()).unwrap();
        assert!(herm.path().join("dir/x.txt").exists());
    }
}
