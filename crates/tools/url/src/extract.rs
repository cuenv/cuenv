//! Archive extraction helpers for URL tool downloads.

use cuenv_core::Result;
use flate2::read::GzDecoder;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use tar::Archive;
use xz2::read::XzDecoder;

/// Determine whether a filesystem path looks like a dynamic library.
pub(super) fn file_looks_like_library(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    name.ends_with(".dylib")
        || name.ends_with(".so")
        || name.contains(".so.")
        || name.ends_with(".dll")
}

/// Determine whether a path string looks like a dynamic library.
pub(super) fn path_looks_like_library(path: &str) -> bool {
    let path_lower = path.to_ascii_lowercase();
    path_lower.ends_with(".dylib")
        || path_lower.ends_with(".so")
        || path_lower.contains(".so.")
        || path_lower.ends_with(".dll")
}

/// Ensure a file is executable on Unix hosts.
pub(super) fn ensure_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)?;
    }

    Ok(())
}

fn temp_extract_dir(dest: &Path) -> PathBuf {
    dest.with_file_name(format!(
        ".{}.tmp",
        dest.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("extract")
    ))
}

fn single_root_dir(dir: &Path) -> Result<Option<PathBuf>> {
    let mut entries = std::fs::read_dir(dir)?
        .filter_map(std::result::Result::ok)
        .collect::<Vec<_>>();

    if entries.len() != 1 {
        return Ok(None);
    }

    let only_entry = entries.swap_remove(0);
    let entry_path = only_entry.path();
    if entry_path.is_dir() {
        Ok(Some(entry_path))
    } else {
        Ok(None)
    }
}

fn finalize_extracted_tree(dest: &Path, temp_dir: &Path) -> Result<()> {
    let effective_root = single_root_dir(temp_dir)?.unwrap_or_else(|| temp_dir.into());
    let normalized_dir = temp_dir.with_file_name(format!(
        ".{}.normalized",
        dest.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("extract")
    ));

    if normalized_dir.exists() {
        std::fs::remove_dir_all(&normalized_dir)?;
    }

    if effective_root == temp_dir {
        std::fs::rename(temp_dir, &normalized_dir)?;
    } else {
        std::fs::create_dir_all(&normalized_dir)?;
        for entry in std::fs::read_dir(&effective_root)? {
            let entry = entry?;
            let source_path = entry.path();
            let target_path = normalized_dir.join(entry.file_name());
            std::fs::rename(source_path, target_path)?;
        }
        std::fs::remove_dir_all(temp_dir)?;
    }

    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    std::fs::rename(normalized_dir, dest)?;
    Ok(())
}

pub(super) fn looks_like_prefix_install(dest: &Path) -> bool {
    dest.join("bin").is_dir() || dest.join("lib").is_dir() || dest.join("include").is_dir()
}

fn dir_name(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(std::borrow::ToOwned::to_owned)
}

fn looks_like_version_dir(name: &str) -> bool {
    let normalized = name.strip_prefix('v').unwrap_or(name);
    normalized
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_digit())
        && normalized
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '+'))
}

fn preferred_binary_names(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();

    if let Some(name) = dir_name(dir) {
        names.push(name.clone());

        if looks_like_version_dir(&name)
            && let Some(parent_name) = dir.parent().and_then(dir_name)
            && parent_name != name
        {
            names.push(parent_name);
        }
    }

    names
}

fn sorted_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = std::fs::read_dir(dir)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();

    files.sort_by(|left, right| {
        let left_name = left
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let right_name = right
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        left_name.cmp(right_name)
    });

    Ok(files)
}

fn executable_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let files = sorted_files(dir)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        Ok(files
            .into_iter()
            .filter(|path| {
                std::fs::metadata(path)
                    .map(|meta| meta.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false)
            })
            .collect())
    }

    #[cfg(not(unix))]
    {
        Ok(files)
    }
}

fn select_preferred_binary(
    candidates: Vec<PathBuf>,
    preferred_names: &[String],
) -> Option<PathBuf> {
    for preferred_name in preferred_names {
        if let Some(path) = candidates.iter().find(|candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some(preferred_name)
        }) {
            return Some(path.clone());
        }
    }

    candidates.into_iter().next()
}

pub(super) fn find_primary_binary_in_prefix(dest: &Path, tool_name: &str) -> Result<PathBuf> {
    let preferred = dest.join("bin").join(tool_name);
    if preferred.exists() {
        return Ok(preferred);
    }

    find_main_binary(dest)
}

/// Extract a binary from an archive or treat the download as a raw binary.
pub(super) fn extract_binary(
    data: &[u8],
    url: &str,
    binary_path: Option<&str>,
    dest: &Path,
) -> Result<PathBuf> {
    let url_path = url.split('?').next().unwrap_or(url);
    let is_zip = url_path.ends_with(".zip");
    let is_tar_gz = url_path.ends_with(".tar.gz") || url_path.ends_with(".tgz");
    let is_tar_xz = url_path.ends_with(".tar.xz") || url_path.ends_with(".txz");

    if is_zip {
        extract_from_zip(data, binary_path, dest)
    } else if is_tar_gz {
        extract_from_tar_gz(data, binary_path, dest)
    } else if is_tar_xz {
        extract_from_tar_xz(data, binary_path, dest)
    } else {
        std::fs::create_dir_all(dest)?;
        let binary_name = Path::new(url_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("tool");
        let binary_dest = dest.join(binary_name);
        std::fs::write(&binary_dest, data)?;
        ensure_executable(&binary_dest)?;
        Ok(binary_dest)
    }
}

/// Extract from a zip archive.
pub(super) fn extract_from_zip(
    data: &[u8],
    binary_path: Option<&str>,
    dest: &Path,
) -> Result<PathBuf> {
    let cursor = Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| cuenv_core::Error::tool_resolution(format!("Failed to open zip: {}", e)))?;

    if let Some(path) = binary_path {
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| {
                cuenv_core::Error::tool_resolution(format!("Failed to read zip entry: {}", e))
            })?;

            let name = file.name().to_string();
            if name == path || name.ends_with(&format!("/{}", path)) {
                std::fs::create_dir_all(dest)?;
                let file_name = Path::new(&name)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(path);
                let dest_path = dest.join(file_name);

                let mut content = Vec::new();
                file.read_to_end(&mut content)?;
                std::fs::write(&dest_path, &content)?;
                ensure_executable(&dest_path)?;

                return Ok(dest_path);
            }
        }

        return Err(cuenv_core::Error::tool_resolution(format!(
            "Binary '{}' not found in zip archive",
            path
        )));
    }

    let temp_dir = temp_extract_dir(dest);

    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir)?;
    }
    std::fs::create_dir_all(&temp_dir)?;

    let extract_result = (|| -> Result<()> {
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| {
                cuenv_core::Error::tool_resolution(format!("Failed to read zip entry: {}", e))
            })?;

            let outpath = match file.enclosed_name() {
                Some(path) => temp_dir.join(path),
                None => continue,
            };

            if file.is_dir() {
                std::fs::create_dir_all(&outpath)?;
            } else {
                if let Some(p) = outpath.parent() {
                    std::fs::create_dir_all(p)?;
                }
                let mut content = Vec::new();
                file.read_to_end(&mut content)?;
                std::fs::write(&outpath, &content)?;

                #[cfg(unix)]
                if let Some(mode) = file.unix_mode() {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = std::fs::metadata(&outpath)?.permissions();
                    perms.set_mode(mode);
                    std::fs::set_permissions(&outpath, perms)?;
                }
            }
        }
        Ok(())
    })();

    if let Err(e) = extract_result {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err(e);
    }

    finalize_extracted_tree(dest, &temp_dir)?;
    find_main_binary(dest)
}

/// Extract from a tar.gz archive.
pub(super) fn extract_from_tar_gz(
    data: &[u8],
    binary_path: Option<&str>,
    dest: &Path,
) -> Result<PathBuf> {
    let decoder = GzDecoder::new(Cursor::new(data));
    extract_from_tar(decoder, binary_path, dest, "tar.gz")
}

/// Extract from a tar.xz archive.
pub(super) fn extract_from_tar_xz(
    data: &[u8],
    binary_path: Option<&str>,
    dest: &Path,
) -> Result<PathBuf> {
    let decoder = XzDecoder::new(Cursor::new(data));
    extract_from_tar(decoder, binary_path, dest, "tar.xz")
}

/// Extract from a tar stream decoded by any `Read` (shared by tar.gz/tar.xz).
fn extract_from_tar(
    reader: impl Read,
    binary_path: Option<&str>,
    dest: &Path,
    archive_kind: &str,
) -> Result<PathBuf> {
    let mut archive = Archive::new(reader);

    std::fs::create_dir_all(dest)?;

    if let Some(path) = binary_path {
        for entry in archive
            .entries()
            .map_err(|e| cuenv_core::Error::tool_resolution(format!("Failed to read tar: {}", e)))?
        {
            let mut entry = entry.map_err(|e| {
                cuenv_core::Error::tool_resolution(format!("Failed to read tar entry: {}", e))
            })?;

            let entry_path = entry.path().map_err(|e| {
                cuenv_core::Error::tool_resolution(format!("Invalid path in tar: {}", e))
            })?;

            let path_str = entry_path.to_string_lossy();
            if path_str.as_ref() == path || path_str.ends_with(&format!("/{}", path)) {
                let file_name = Path::new(path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(path);
                let dest_path = dest.join(file_name);

                let mut content = Vec::new();
                entry.read_to_end(&mut content)?;
                std::fs::write(&dest_path, &content)?;
                ensure_executable(&dest_path)?;

                return Ok(dest_path);
            }
        }

        return Err(cuenv_core::Error::tool_resolution(format!(
            "Binary '{}' not found in {} archive",
            path, archive_kind
        )));
    }

    let temp_dir = temp_extract_dir(dest);
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir)?;
    }
    std::fs::create_dir_all(&temp_dir)?;

    let extract_result = archive
        .unpack(&temp_dir)
        .map_err(|e| cuenv_core::Error::tool_resolution(format!("Failed to extract tar: {}", e)));
    if let Err(err) = extract_result {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err(err);
    }

    finalize_extracted_tree(dest, &temp_dir)?;
    find_main_binary(dest)
}

/// Find the main binary in an extracted directory.
fn find_main_binary(dir: &Path) -> Result<PathBuf> {
    let preferred_names = preferred_binary_names(dir);

    let bin_dir = dir.join("bin");
    if bin_dir.exists()
        && let Some(path) = select_preferred_binary(sorted_files(&bin_dir)?, &preferred_names)
    {
        return Ok(path);
    }

    if let Some(path) = select_preferred_binary(executable_files(dir)?, &preferred_names) {
        return Ok(path);
    }

    Err(cuenv_core::Error::tool_resolution(
        "No binary found in extracted archive".to_string(),
    ))
}
