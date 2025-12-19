//! Nix store path handling for hermetic remote execution
//!
//! This module extracts Nix store paths from environment variables,
//! resolves their full closure (dependencies), and prepares them
//! for upload to CAS as part of the input tree.

use crate::error::{RemoteError, Result};
use crate::merkle::Digest;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

/// Environment variables that may contain Nix store paths
const NIX_PATH_VARS: &[&str] = &[
    "PATH",
    "LD_LIBRARY_PATH",
    "LIBRARY_PATH",
    "PKG_CONFIG_PATH",
    "C_INCLUDE_PATH",
    "CPLUS_INCLUDE_PATH",
    "CMAKE_PREFIX_PATH",
    "ACLOCAL_PATH",
];

/// A file from the Nix store to be uploaded
#[derive(Debug, Clone)]
pub struct NixFile {
    /// Absolute path in local Nix store (e.g., /nix/store/xxx/bin/cargo)
    pub store_path: PathBuf,
    /// Relative path for remote execution (e.g., nix/store/xxx/bin/cargo)
    pub relative_path: PathBuf,
    /// File content digest
    pub digest: Digest,
    /// Whether this is a symlink
    pub is_symlink: bool,
    /// Symlink target (if is_symlink)
    pub symlink_target: Option<PathBuf>,
    /// Whether file is executable
    pub is_executable: bool,
}

/// Prepared Nix inputs for remote execution
#[derive(Debug, Default)]
pub struct NixInputs {
    /// Files to upload to CAS
    pub files: Vec<NixFile>,
    /// Mapping from local paths to remote paths for environment rewriting
    pub path_mapping: HashMap<PathBuf, PathBuf>,
    /// Total size in bytes
    pub total_size: u64,
}

impl NixInputs {
    /// Check if there are any Nix inputs
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Extract Nix store paths from environment variables
pub fn extract_store_paths(env: &HashMap<String, String>) -> HashSet<PathBuf> {
    let mut paths = HashSet::new();

    for var in NIX_PATH_VARS {
        if let Some(value) = env.get(*var) {
            for segment in value.split(':') {
                if segment.starts_with("/nix/store/") {
                    // Extract just the store path root (first two components after /nix/store/)
                    // e.g., /nix/store/abc123-rust-1.85/bin -> /nix/store/abc123-rust-1.85
                    if let Some(store_path) = extract_store_root(segment) {
                        paths.insert(store_path);
                    }
                }
            }
        }
    }

    debug!(
        count = paths.len(),
        "Extracted Nix store paths from environment"
    );
    paths
}

/// Extract the store path root from a full path
/// e.g., /nix/store/abc123-rust/bin/cargo -> /nix/store/abc123-rust
fn extract_store_root(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    let mut components = path.components();

    // Skip: /, nix, store
    components.next()?; // root
    let nix = components.next()?;
    let store = components.next()?;
    let hash_name = components.next()?;

    if nix.as_os_str() == "nix" && store.as_os_str() == "store" {
        Some(PathBuf::from("/nix/store").join(hash_name))
    } else {
        None
    }
}

/// Resolve the full closure of Nix store paths (all runtime dependencies)
pub async fn resolve_closure(paths: &HashSet<PathBuf>) -> Result<HashSet<PathBuf>> {
    if paths.is_empty() {
        return Ok(HashSet::new());
    }

    let path_args: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();

    debug!(
        input_count = paths.len(),
        "Resolving Nix closure with nix-store -qR"
    );

    let output = Command::new("nix-store")
        .arg("-qR")
        .args(&path_args)
        .output()
        .await
        .map_err(|e| {
            RemoteError::io_error(
                "running nix-store -qR",
                std::io::Error::new(e.kind(), format!("Failed to run nix-store: {}", e)),
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RemoteError::config_error(format!(
            "nix-store -qR failed: {}",
            stderr
        )));
    }

    let closure: HashSet<PathBuf> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();

    info!(
        input_count = paths.len(),
        closure_count = closure.len(),
        "Resolved Nix closure"
    );

    Ok(closure)
}

/// Maximum closure size before warning (100MB)
const LARGE_CLOSURE_WARNING_BYTES: u64 = 100 * 1024 * 1024;

/// Maximum closure size before skipping (10GB - too large to upload practically)
const MAX_CLOSURE_SIZE_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// Collect all files from Nix store paths
pub fn collect_files(store_paths: &HashSet<PathBuf>) -> Result<Vec<NixFile>> {
    let mut files = Vec::new();
    let total_paths = store_paths.len();
    let mut processed = 0;

    for store_path in store_paths {
        processed += 1;
        if processed % 50 == 0 || processed == total_paths {
            info!(
                processed = processed,
                total = total_paths,
                files_collected = files.len(),
                "Collecting Nix store files"
            );
        }
        let metadata = std::fs::symlink_metadata(store_path).map_err(|e| {
            RemoteError::io_error(format!("getting metadata for {:?}", store_path), e)
        })?;

        if metadata.is_dir() {
            // Recursively collect files from directory
            collect_files_recursive(store_path, store_path, &mut files)?;
        } else if metadata.is_file() {
            // Single file store path (e.g., strip.sh)
            let content = std::fs::read(store_path).map_err(|e| {
                RemoteError::io_error(format!("reading file {:?}", store_path), e)
            })?;
            let digest = Digest::from_bytes(&content);

            #[cfg(unix)]
            let is_executable = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let is_executable = false;

            files.push(NixFile {
                store_path: store_path.clone(),
                relative_path: store_path_to_relative(store_path),
                digest,
                is_symlink: false,
                symlink_target: None,
                is_executable,
            });
        } else if metadata.is_symlink() {
            // Symlink store path
            let target = std::fs::read_link(store_path).map_err(|e| {
                RemoteError::io_error(format!("reading symlink {:?}", store_path), e)
            })?;

            files.push(NixFile {
                store_path: store_path.clone(),
                relative_path: store_path_to_relative(store_path),
                digest: Digest::default(),
                is_symlink: true,
                symlink_target: Some(target),
                is_executable: false,
            });
        }
    }

    debug!(file_count = files.len(), "Collected files from Nix store");
    Ok(files)
}

/// Default parallelism for file hashing
const DEFAULT_HASH_PARALLELISM: usize = 16;

/// Metadata for a file to be hashed (collected in first pass)
#[derive(Debug)]
struct FileToHash {
    path: PathBuf,
    relative_path: PathBuf,
    is_executable: bool,
}

/// Collect file paths from store directories (fast, no I/O beyond readdir)
fn collect_file_paths(store_paths: &HashSet<PathBuf>) -> Result<Vec<FileToHash>> {
    let mut files = Vec::new();

    for store_path in store_paths {
        let metadata = std::fs::symlink_metadata(store_path).map_err(|e| {
            RemoteError::io_error(format!("getting metadata for {:?}", store_path), e)
        })?;

        if metadata.is_dir() {
            collect_paths_recursive(store_path, &mut files)?;
        } else if metadata.is_file() {
            #[cfg(unix)]
            let is_executable = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let is_executable = false;

            files.push(FileToHash {
                path: store_path.clone(),
                relative_path: store_path_to_relative(store_path),
                is_executable,
            });
        }
        // Note: symlinks are handled separately in collect_symlinks_recursive
    }

    Ok(files)
}

/// Recursively collect file paths (not symlinks, not content)
fn collect_paths_recursive(current: &Path, files: &mut Vec<FileToHash>) -> Result<()> {
    let entries = std::fs::read_dir(current).map_err(|e| {
        RemoteError::io_error(format!("reading directory {:?}", current), e)
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| {
            RemoteError::io_error(format!("reading entry in {:?}", current), e)
        })?;

        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|e| {
            RemoteError::io_error(format!("getting metadata for {:?}", path), e)
        })?;

        let file_type = metadata.file_type();

        if file_type.is_file() {
            #[cfg(unix)]
            let is_executable = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let is_executable = false;

            files.push(FileToHash {
                path: path.clone(),
                relative_path: store_path_to_relative(&path),
                is_executable,
            });
        } else if file_type.is_dir() {
            collect_paths_recursive(&path, files)?;
        }
        // Symlinks collected separately
    }

    Ok(())
}

/// Collect symlinks from store paths (no hashing needed)
fn collect_symlinks(store_paths: &HashSet<PathBuf>) -> Result<Vec<NixFile>> {
    let mut symlinks = Vec::new();

    for store_path in store_paths {
        let metadata = std::fs::symlink_metadata(store_path).map_err(|e| {
            RemoteError::io_error(format!("getting metadata for {:?}", store_path), e)
        })?;

        if metadata.is_symlink() {
            let target = std::fs::read_link(store_path).map_err(|e| {
                RemoteError::io_error(format!("reading symlink {:?}", store_path), e)
            })?;

            symlinks.push(NixFile {
                store_path: store_path.clone(),
                relative_path: store_path_to_relative(store_path),
                digest: Digest::default(),
                is_symlink: true,
                symlink_target: Some(target),
                is_executable: false,
            });
        } else if metadata.is_dir() {
            collect_symlinks_recursive(store_path, &mut symlinks)?;
        }
    }

    Ok(symlinks)
}

/// Recursively collect symlinks from a directory
fn collect_symlinks_recursive(current: &Path, symlinks: &mut Vec<NixFile>) -> Result<()> {
    let entries = std::fs::read_dir(current).map_err(|e| {
        RemoteError::io_error(format!("reading directory {:?}", current), e)
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| {
            RemoteError::io_error(format!("reading entry in {:?}", current), e)
        })?;

        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|e| {
            RemoteError::io_error(format!("getting metadata for {:?}", path), e)
        })?;

        let file_type = metadata.file_type();

        if file_type.is_symlink() {
            let target = std::fs::read_link(&path).map_err(|e| {
                RemoteError::io_error(format!("reading symlink {:?}", path), e)
            })?;

            symlinks.push(NixFile {
                store_path: path.clone(),
                relative_path: store_path_to_relative(&path),
                digest: Digest::default(),
                is_symlink: true,
                symlink_target: Some(target),
                is_executable: false,
            });
        } else if file_type.is_dir() {
            collect_symlinks_recursive(&path, symlinks)?;
        }
    }

    Ok(())
}

/// Hash a single file and return NixFile
fn hash_single_file(file: FileToHash) -> Result<NixFile> {
    let content = std::fs::read(&file.path).map_err(|e| {
        RemoteError::io_error(format!("reading file {:?}", file.path), e)
    })?;

    let digest = Digest::from_bytes(&content);

    Ok(NixFile {
        store_path: file.path,
        relative_path: file.relative_path,
        digest,
        is_symlink: false,
        symlink_target: None,
        is_executable: file.is_executable,
    })
}

/// Collect all files from Nix store paths with parallel hashing
///
/// Uses bounded parallelism to avoid overwhelming the system.
/// This is significantly faster than sequential processing for large closures.
pub async fn collect_files_parallel(
    store_paths: &HashSet<PathBuf>,
    max_parallelism: usize,
) -> Result<Vec<NixFile>> {
    let total_paths = store_paths.len();
    info!(
        closure_paths = total_paths,
        parallelism = max_parallelism,
        "Starting parallel file collection from Nix closure"
    );

    // First pass: collect all file paths (fast, minimal I/O)
    let file_paths = collect_file_paths(store_paths)?;
    let symlinks = collect_symlinks(store_paths)?;

    let total_files = file_paths.len();
    let total_symlinks = symlinks.len();

    info!(
        files = total_files,
        symlinks = total_symlinks,
        "Collected paths, starting parallel hashing"
    );

    // Set up parallel processing
    let semaphore = Arc::new(Semaphore::new(max_parallelism));
    let mut join_set = JoinSet::new();
    let processed = Arc::new(AtomicUsize::new(0));

    // Second pass: hash files in parallel
    for file in file_paths {
        let permit = semaphore.clone().acquire_owned().await.map_err(|e| {
            RemoteError::io_error("acquiring semaphore", std::io::Error::other(e.to_string()))
        })?;

        let processed_clone = processed.clone();
        let total = total_files;

        join_set.spawn(async move {
            // Use spawn_blocking for CPU-bound hashing
            let result = tokio::task::spawn_blocking(move || hash_single_file(file))
                .await
                .map_err(|e| {
                    RemoteError::io_error("spawn_blocking", std::io::Error::other(e.to_string()))
                })?;

            // Progress reporting
            let count = processed_clone.fetch_add(1, Ordering::Relaxed) + 1;
            if count % 500 == 0 || count == total {
                info!(
                    processed = count,
                    total = total,
                    "Hashing Nix store files"
                );
            }

            drop(permit);
            result
        });
    }

    // Collect results
    let mut files = Vec::with_capacity(total_files + total_symlinks);

    // Add symlinks first (no hashing needed)
    files.extend(symlinks);

    // Collect hashed files
    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(nix_file)) => files.push(nix_file),
            Ok(Err(e)) => return Err(e),
            Err(e) => {
                return Err(RemoteError::io_error(
                    "parallel hashing join",
                    std::io::Error::other(e.to_string()),
                ))
            }
        }
    }

    info!(
        total_files = files.len(),
        "Parallel file collection complete"
    );
    Ok(files)
}

/// Recursively collect files from a directory (sequential, for original collect_files)
fn collect_files_recursive(
    root: &Path,
    current: &Path,
    files: &mut Vec<NixFile>,
) -> Result<()> {
    let entries = std::fs::read_dir(current).map_err(|e| {
        RemoteError::io_error(format!("reading directory {:?}", current), e)
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| {
            RemoteError::io_error(format!("reading entry in {:?}", current), e)
        })?;

        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|e| {
            RemoteError::io_error(format!("getting metadata for {:?}", path), e)
        })?;

        let file_type = metadata.file_type();

        if file_type.is_symlink() {
            let target = std::fs::read_link(&path).map_err(|e| {
                RemoteError::io_error(format!("reading symlink {:?}", path), e)
            })?;

            files.push(NixFile {
                store_path: path.clone(),
                relative_path: store_path_to_relative(&path),
                digest: Digest::default(), // Symlinks don't need content digest
                is_symlink: true,
                symlink_target: Some(target),
                is_executable: false,
            });
        } else if file_type.is_file() {
            let content = std::fs::read(&path).map_err(|e| {
                RemoteError::io_error(format!("reading file {:?}", path), e)
            })?;

            let digest = Digest::from_bytes(&content);

            #[cfg(unix)]
            let is_executable = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let is_executable = false;

            files.push(NixFile {
                store_path: path.clone(),
                relative_path: store_path_to_relative(&path),
                digest,
                is_symlink: false,
                symlink_target: None,
                is_executable,
            });
        } else if file_type.is_dir() {
            collect_files_recursive(root, &path, files)?;
        }
    }

    Ok(())
}

/// Convert a Nix store path to a relative path for remote execution
/// /nix/store/abc123-rust/bin/cargo -> nix/store/abc123-rust/bin/cargo
fn store_path_to_relative(path: &Path) -> PathBuf {
    // Strip leading / to make relative
    path.strip_prefix("/").unwrap_or(path).to_path_buf()
}

/// Rewrite environment variables to use remote paths
pub fn rewrite_paths(
    env: &HashMap<String, String>,
    store_paths: &HashSet<PathBuf>,
) -> HashMap<String, String> {
    env.iter()
        .map(|(k, v)| {
            let new_v = store_paths.iter().fold(v.clone(), |acc, store_path| {
                let local = store_path.to_string_lossy();
                let remote = store_path_to_relative(store_path);
                acc.replace(&*local, &remote.to_string_lossy())
            });
            (k.clone(), new_v)
        })
        .collect()
}

/// Prepare Nix inputs for remote execution (sequential version)
///
/// This is the original sequential implementation. For large closures,
/// use `prepare_inputs_parallel` instead.
pub async fn prepare_inputs(env: &HashMap<String, String>) -> Result<NixInputs> {
    // Extract store paths from environment
    let direct_paths = extract_store_paths(env);

    if direct_paths.is_empty() {
        debug!("No Nix store paths in environment, skipping closure resolution");
        return Ok(NixInputs::default());
    }

    info!(
        direct_paths = direct_paths.len(),
        "Found Nix store paths in environment, resolving closure..."
    );

    // Resolve full closure
    let closure = resolve_closure(&direct_paths).await?;

    if closure.is_empty() {
        warn!("Nix closure resolution returned empty set");
        return Ok(NixInputs::default());
    }

    // Warn if closure is large
    if closure.len() > 500 {
        warn!(
            closure_size = closure.len(),
            "Large Nix closure detected. File collection may take several minutes. \
             Consider using a minimal toolchain for remote execution."
        );
    }

    // Collect all files from closure
    info!(
        closure_count = closure.len(),
        "Collecting files from Nix closure (this may take a while for large closures)..."
    );
    let files = collect_files(&closure)?;

    // Calculate total size
    let total_size: u64 = files
        .iter()
        .filter(|f| !f.is_symlink)
        .map(|f| f.digest.size_bytes as u64)
        .sum();

    // Check if closure is too large
    if total_size > MAX_CLOSURE_SIZE_BYTES {
        warn!(
            total_size_gb = total_size / (1024 * 1024 * 1024),
            max_size_gb = MAX_CLOSURE_SIZE_BYTES / (1024 * 1024 * 1024),
            "Nix closure too large for remote execution. Skipping Nix inputs. \
             Remote command will fail if it depends on Nix-provided tools."
        );
        return Ok(NixInputs::default());
    }

    // Warn about large closures
    if total_size > LARGE_CLOSURE_WARNING_BYTES {
        warn!(
            total_size_mb = total_size / (1024 * 1024),
            file_count = files.len(),
            "Large Nix closure will be uploaded. First-time upload may be slow."
        );
    }

    // Build path mapping
    let path_mapping: HashMap<PathBuf, PathBuf> = closure
        .iter()
        .map(|p| (p.clone(), store_path_to_relative(p)))
        .collect();

    info!(
        closure_count = closure.len(),
        file_count = files.len(),
        total_size_mb = total_size / (1024 * 1024),
        "Prepared Nix inputs for remote execution"
    );

    Ok(NixInputs {
        files,
        path_mapping,
        total_size,
    })
}

/// Prepare Nix inputs for remote execution with parallel file hashing
///
/// This is the main entry point that:
/// 1. Extracts Nix store paths from environment
/// 2. Resolves full closure via nix-store -qR
/// 3. Collects all files with parallel hashing
/// 4. Builds path mapping for environment rewriting
///
/// For very large closures (>10GB), returns empty to avoid excessive upload times.
pub async fn prepare_inputs_parallel(env: &HashMap<String, String>) -> Result<NixInputs> {
    prepare_inputs_parallel_with_parallelism(env, DEFAULT_HASH_PARALLELISM).await
}

/// Prepare Nix inputs with configurable parallelism
pub async fn prepare_inputs_parallel_with_parallelism(
    env: &HashMap<String, String>,
    parallelism: usize,
) -> Result<NixInputs> {
    // Extract store paths from environment
    let direct_paths = extract_store_paths(env);

    if direct_paths.is_empty() {
        debug!("No Nix store paths in environment, skipping closure resolution");
        return Ok(NixInputs::default());
    }

    info!(
        direct_paths = direct_paths.len(),
        "Found Nix store paths in environment, resolving closure..."
    );

    // Resolve full closure
    let closure = resolve_closure(&direct_paths).await?;

    if closure.is_empty() {
        warn!("Nix closure resolution returned empty set");
        return Ok(NixInputs::default());
    }

    info!(
        closure_count = closure.len(),
        parallelism = parallelism,
        "Collecting files from Nix closure with parallel hashing..."
    );

    // Collect all files from closure with parallel hashing
    let files = collect_files_parallel(&closure, parallelism).await?;

    // Calculate total size
    let total_size: u64 = files
        .iter()
        .filter(|f| !f.is_symlink)
        .map(|f| f.digest.size_bytes as u64)
        .sum();

    // Check if closure is too large
    if total_size > MAX_CLOSURE_SIZE_BYTES {
        warn!(
            total_size_gb = total_size / (1024 * 1024 * 1024),
            max_size_gb = MAX_CLOSURE_SIZE_BYTES / (1024 * 1024 * 1024),
            "Nix closure too large for remote execution. Skipping Nix inputs. \
             Remote command will fail if it depends on Nix-provided tools."
        );
        return Ok(NixInputs::default());
    }

    // Warn about large closures
    if total_size > LARGE_CLOSURE_WARNING_BYTES {
        info!(
            total_size_mb = total_size / (1024 * 1024),
            file_count = files.len(),
            "Large Nix closure will be uploaded. First-time upload may be slow."
        );
    }

    // Build path mapping
    let path_mapping: HashMap<PathBuf, PathBuf> = closure
        .iter()
        .map(|p| (p.clone(), store_path_to_relative(p)))
        .collect();

    info!(
        closure_count = closure.len(),
        file_count = files.len(),
        total_size_mb = total_size / (1024 * 1024),
        "Prepared Nix inputs for remote execution"
    );

    Ok(NixInputs {
        files,
        path_mapping,
        total_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_store_root() {
        assert_eq!(
            extract_store_root("/nix/store/abc123-rust-1.85.0/bin/cargo"),
            Some(PathBuf::from("/nix/store/abc123-rust-1.85.0"))
        );

        assert_eq!(
            extract_store_root("/nix/store/xyz789-glibc/lib"),
            Some(PathBuf::from("/nix/store/xyz789-glibc"))
        );

        assert_eq!(extract_store_root("/usr/bin/cargo"), None);
        assert_eq!(extract_store_root("/home/user/.cargo/bin/cargo"), None);
    }

    #[test]
    fn test_store_path_to_relative() {
        assert_eq!(
            store_path_to_relative(Path::new("/nix/store/abc123/bin/cargo")),
            PathBuf::from("nix/store/abc123/bin/cargo")
        );
    }

    #[test]
    fn test_extract_store_paths() {
        let mut env = HashMap::new();
        env.insert(
            "PATH".to_string(),
            "/nix/store/abc-rust/bin:/nix/store/xyz-go/bin:/usr/bin".to_string(),
        );
        env.insert(
            "LD_LIBRARY_PATH".to_string(),
            "/nix/store/glibc-123/lib".to_string(),
        );

        let paths = extract_store_paths(&env);

        assert!(paths.contains(&PathBuf::from("/nix/store/abc-rust")));
        assert!(paths.contains(&PathBuf::from("/nix/store/xyz-go")));
        assert!(paths.contains(&PathBuf::from("/nix/store/glibc-123")));
        assert_eq!(paths.len(), 3);
    }

    #[test]
    fn test_rewrite_paths() {
        let mut env = HashMap::new();
        env.insert(
            "PATH".to_string(),
            "/nix/store/abc-rust/bin:/usr/bin".to_string(),
        );

        let mut store_paths = HashSet::new();
        store_paths.insert(PathBuf::from("/nix/store/abc-rust"));

        let rewritten = rewrite_paths(&env, &store_paths);

        assert_eq!(
            rewritten.get("PATH"),
            Some(&"nix/store/abc-rust/bin:/usr/bin".to_string())
        );
    }
}
