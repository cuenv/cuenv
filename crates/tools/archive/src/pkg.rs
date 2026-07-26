//! macOS `.pkg` payload extraction via `pkgutil` and `cpio`.

use crate::{ArchiveError, Result, ensure_executable, find_main_binary};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use tempfile::Builder;
use tracing::debug;

/// Extract from a macOS `.pkg` archive.
pub fn extract_from_pkg(data: &[u8], binary_path: Option<&str>, dest: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dest)?;

    let work_dir = Builder::new().prefix("cuenv-pkg-").tempdir().map_err(|e| {
        ArchiveError::extraction(format!(
            "Failed to create temporary directory for pkg extraction: {e}"
        ))
    })?;

    let pkg_path = work_dir.path().join("asset.pkg");
    std::fs::write(&pkg_path, data)?;

    let expanded_dir = work_dir.path().join("expanded");
    run_command(
        Command::new("pkgutil")
            .arg("--expand")
            .arg(&pkg_path)
            .arg(&expanded_dir),
        "expand pkg archive",
    )?;

    let payloads = collect_payload_files(&expanded_dir)?;
    if payloads.is_empty() {
        return Err(ArchiveError::extraction(
            "No payload files found in pkg archive".to_string(),
        ));
    }

    for (index, payload_path) in payloads.iter().enumerate() {
        let payload_dir = work_dir.path().join(format!("payload-{index}"));
        std::fs::create_dir_all(&payload_dir)?;

        // Validate the payload's table of contents before extracting:
        // `cpio -idm` does not itself reject `..` or absolute entry paths,
        // so a malicious payload could otherwise write outside payload_dir.
        match scan_payload_entries(payload_path)? {
            EntryScan::Safe => {}
            EntryScan::Unlistable(reason) => {
                debug!(?payload_path, %reason, "Skipping unlistable pkg payload");
                continue;
            }
        }

        let payload_file = File::open(payload_path)?;
        let payload_extract = run_command(
            Command::new("cpio")
                .args(["-idm", "--quiet"])
                .current_dir(&payload_dir)
                .stdin(Stdio::from(payload_file)),
            "extract pkg payload",
        );

        if let Err(error) = payload_extract {
            debug!(?payload_path, %error, "Skipping unreadable pkg payload");
            continue;
        }

        if let Some(path) = binary_path {
            if let Some(found) = find_path_in_tree(&payload_dir, path)? {
                let found = ensure_contained(&found, &payload_dir)?;
                return copy_extracted_file(&found, dest, path);
            }
        } else if let Ok(found) = find_main_binary(&payload_dir) {
            let found = ensure_contained(&found, &payload_dir)?;
            return copy_extracted_file(&found, dest, "binary");
        }
    }

    if let Some(path) = binary_path {
        return Err(ArchiveError::extraction(format!(
            "Binary '{path}' not found in pkg payloads"
        )));
    }

    Err(ArchiveError::extraction(
        "No executable found in pkg payloads".to_string(),
    ))
}

/// Copy a selected extracted file into the destination directory.
fn copy_extracted_file(source: &Path, dest: &Path, fallback_name: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dest)?;
    let file_name = source
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(fallback_name);
    let dest_path = dest.join(file_name);
    std::fs::copy(source, &dest_path)?;
    ensure_executable(&dest_path)?;
    Ok(dest_path)
}

/// Cap on the bytes read from a `cpio -it` listing; a table of contents
/// larger than this is treated as hostile.
const MAX_LISTING_BYTES: u64 = 64 * 1024 * 1024;

/// Outcome of scanning a payload's table of contents.
enum EntryScan {
    /// Every listed entry is traversal-safe.
    Safe,
    /// The payload could not be listed (skip it, matching the historical
    /// behavior for unreadable payloads).
    Unlistable(String),
}

/// Stream a cpio payload's table of contents (`cpio -it`) and validate each
/// entry as it is read, without buffering the whole listing.
///
/// Returns a hard error for traversal attempts and oversized listings;
/// returns [`EntryScan::Unlistable`] for payloads cpio cannot read.
fn scan_payload_entries(payload_path: &Path) -> Result<EntryScan> {
    let payload_file = File::open(payload_path)?;
    let mut child = match Command::new("cpio")
        .args(["-it", "--quiet"])
        .stdin(Stdio::from(payload_file))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return Ok(EntryScan::Unlistable(format!(
                "failed to run cpio -it: {e}"
            )));
        }
    };

    let Some(stdout) = child.stdout.take() else {
        reap(&mut child);
        return Ok(EntryScan::Unlistable("cpio stdout unavailable".to_string()));
    };

    let mut read_bytes: u64 = 0;
    for line in BufReader::new(stdout).split(b'\n') {
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                reap(&mut child);
                return Ok(EntryScan::Unlistable(format!(
                    "failed to read listing: {e}"
                )));
            }
        };

        read_bytes += line.len() as u64 + 1;
        if read_bytes > MAX_LISTING_BYTES {
            reap(&mut child);
            return Err(ArchiveError::extraction(
                "pkg payload listing exceeds the size limit".to_string(),
            ));
        }

        // Lossy conversion preserves the ASCII bytes traversal detection
        // relies on (`.`, `/`), so non-UTF-8 names are still validated.
        let entry = String::from_utf8_lossy(&line);
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if !crate::entry_paths::is_safe_entry(entry) {
            reap(&mut child);
            return Err(ArchiveError::extraction(format!(
                "pkg payload contains unsafe path '{entry}'"
            )));
        }
    }

    match child.wait() {
        Ok(status) if status.success() => Ok(EntryScan::Safe),
        Ok(status) => Ok(EntryScan::Unlistable(format!(
            "cpio -it exited with {status}"
        ))),
        Err(e) => Ok(EntryScan::Unlistable(format!(
            "failed to wait for cpio -it: {e}"
        ))),
    }
}

/// Kill and reap a child process, ignoring errors (used on early abort).
fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Require a located file to be canonically contained in the payload
/// directory, guarding against symlinks that point outside it.
fn ensure_contained(found: &Path, payload_dir: &Path) -> Result<PathBuf> {
    let root = payload_dir.canonicalize()?;
    let resolved = found.canonicalize()?;
    if resolved.starts_with(&root) {
        Ok(resolved)
    } else {
        Err(ArchiveError::extraction(format!(
            "pkg payload entry '{}' escapes the extraction directory",
            found.display()
        )))
    }
}

/// Run a process and map non-zero exits to extraction errors.
fn run_command(command: &mut Command, action: &str) -> Result<()> {
    let status = command
        .status()
        .map_err(|e| ArchiveError::extraction(format!("Failed to {action}: {e}")))?;

    if status.success() {
        Ok(())
    } else {
        Err(ArchiveError::extraction(format!(
            "Failed to {action}: {status}"
        )))
    }
}

/// Recursively collect all `Payload` files from an expanded pkg directory.
fn collect_payload_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut stack = vec![root.to_path_buf()];
    let mut payloads = Vec::new();

    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }

            if path.is_file() && path.file_name().and_then(|n| n.to_str()) == Some("Payload") {
                payloads.push(path);
            }
        }
    }

    Ok(payloads)
}

/// Find a file in a directory tree matching the requested pkg path.
fn find_path_in_tree(root: &Path, path: &str) -> Result<Option<PathBuf>> {
    let requested = normalize_lookup_path(path);
    let mut stack = vec![root.to_path_buf()];

    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current)? {
            let entry = entry?;
            let entry_path = entry.path();

            if entry_path.is_dir() {
                stack.push(entry_path);
                continue;
            }

            if !entry_path.is_file() {
                continue;
            }

            let Ok(relative) = entry_path.strip_prefix(root) else {
                continue;
            };
            let candidate = relative.to_string_lossy().replace('\\', "/");
            let candidate = candidate.trim_start_matches("./");

            if candidate == requested || candidate.ends_with(&format!("/{requested}")) {
                return Ok(Some(entry_path));
            }
        }
    }

    Ok(None)
}

/// Normalize lookup paths for suffix matching.
fn normalize_lookup_path(path: &str) -> String {
    path.trim_start_matches('/')
        .trim_start_matches("./")
        .to_string()
}
