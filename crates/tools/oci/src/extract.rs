//! Binary extraction from OCI image layers.
//!
//! Supports:
//! - Homebrew bottles (tar.gz with `{name}/{version}/bin/{binary}` structure)
//! - Generic container images (extract specific paths from layers)
//!
//! For Homebrew bottles, this module also handles binary relocation by
//! patching `@@HOMEBREW_PREFIX@@` and `@@HOMEBREW_CELLAR@@` placeholders
//! in Mach-O binaries with actual paths.

use flate2::read::GzDecoder;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use tar::Archive;
use tracing::{debug, trace};

use crate::{Error, Result};

/// Extract a full Homebrew bottle (preserving bin/, lib/, share/, etc.).
///
/// This extracts the entire bottle contents, which is necessary for
/// dynamically linked binaries that depend on libraries in lib/.
///
/// Homebrew bottles are gzip-compressed tarballs with structure:
/// ```text
/// {name}/{version}/
/// ├── bin/
/// │   └── {binary}
/// ├── lib/
/// │   └── {libraries}
/// └── share/
/// ```
///
/// The bottle contents are extracted to `dest_dir`, stripping the
/// `{name}/{version}/` prefix from paths.
pub fn extract_homebrew_bottle(bottle_path: &Path, dest_dir: &Path) -> Result<()> {
    debug!(?bottle_path, ?dest_dir, "Extracting full Homebrew bottle");

    let file = File::open(bottle_path)?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    // Create destination directory
    std::fs::create_dir_all(dest_dir)?;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let path_str = path.to_string_lossy();

        trace!(path = %path_str, "Processing archive entry");

        // Bottles have structure: <name>/<version>/...
        // Skip the first two components and extract the rest
        let components: Vec<_> = path.components().collect();
        if components.len() <= 2 {
            // Skip the root directory entries
            continue;
        }

        // Build relative path without name/version prefix
        let relative: PathBuf = components[2..].iter().collect();

        // Validate no path traversal (e.g., "../../../etc/passwd")
        if relative
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(Error::ExtractionFailed {
                binary: path_str.to_string(),
                message: "Archive entry contains path traversal".to_string(),
            });
        }

        let dest_path = dest_dir.join(&relative);

        // Handle directories
        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&dest_path)?;
            continue;
        }

        // Handle files
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Extract the file
        entry.unpack(&dest_path)?;

        // Make binaries executable
        #[cfg(unix)]
        if relative.starts_with("bin") || relative.starts_with("libexec") {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::metadata(&dest_path) {
                let mut perms = metadata.permissions();
                let mode = perms.mode();
                // Add execute permission if it's a regular file
                if mode & 0o100 == 0 {
                    perms.set_mode(mode | 0o111);
                    if let Err(e) = std::fs::set_permissions(&dest_path, perms) {
                        tracing::warn!(
                            path = %dest_path.display(),
                            error = %e,
                            "Failed to set executable permission"
                        );
                    }
                }
            }
        }
    }

    debug!(?dest_dir, "Extracted bottle");
    Ok(())
}

/// Relocate Homebrew bottle binaries by patching placeholder paths.
///
/// Homebrew bottles contain placeholder paths:
/// - `@@HOMEBREW_PREFIX@@/opt/<dep>/lib/...`
/// - `@@HOMEBREW_CELLAR@@/<name>/<version>/lib/...`
///
/// This function patches these to point to actual cache paths.
///
/// # Arguments
///
/// * `formula_dir` - Path to the extracted formula (e.g., `~/.cache/cuenv/oci/homebrew/jq/1.8.1/`)
/// * `homebrew_cache` - Root of the homebrew cache (e.g., `~/.cache/cuenv/oci/homebrew/`)
/// * `formula_name` - Name of the formula (e.g., "jq")
/// * `formula_version` - Version of the formula (e.g., "1.8.1")
/// * `dependencies` - Map of dependency name to version
#[cfg(target_os = "macos")]
pub fn relocate_homebrew_bottle(
    formula_dir: &Path,
    homebrew_cache: &Path,
    formula_name: &str,
    formula_version: &str,
    dependencies: &std::collections::HashMap<String, String>,
) -> Result<()> {
    use std::process::Command;

    debug!(
        ?formula_dir,
        formula_name, formula_version, "Relocating Homebrew bottle"
    );

    let bin_dir = formula_dir.join("bin");
    let lib_dir = formula_dir.join("lib");

    // Collect all binaries and dylibs to process
    let mut files_to_process = Vec::new();

    if bin_dir.exists() {
        for entry in std::fs::read_dir(&bin_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                files_to_process.push(path);
            }
        }
    }

    if lib_dir.exists() {
        for entry in std::fs::read_dir(&lib_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.ends_with(".dylib") {
                    files_to_process.push(path);
                }
            }
        }
    }

    // Process each file
    for file_path in &files_to_process {
        // Get current install names
        let output = Command::new("otool").arg("-L").arg(file_path).output()?;

        if !output.status.success() {
            // Not a Mach-O binary, skip
            continue;
        }

        let output_str = String::from_utf8_lossy(&output.stdout);

        for line in output_str.lines().skip(1) {
            let line = line.trim();
            let Some(lib_path) = line.split_whitespace().next() else {
                continue;
            };

            let new_path = if lib_path.contains("@@HOMEBREW_CELLAR@@") {
                // @@HOMEBREW_CELLAR@@/jq/1.8.1/lib/libjq.1.dylib
                // -> /cache/homebrew/jq/1.8.1/lib/libjq.1.dylib
                let cellar_path =
                    lib_path.replace("@@HOMEBREW_CELLAR@@", &homebrew_cache.to_string_lossy());
                Some(cellar_path)
            } else if lib_path.contains("@@HOMEBREW_PREFIX@@") {
                // @@HOMEBREW_PREFIX@@/opt/oniguruma/lib/libonig.5.dylib
                // -> /cache/homebrew/oniguruma/<version>/lib/libonig.5.dylib

                // Extract dependency name from path
                // Format: @@HOMEBREW_PREFIX@@/opt/<name>/lib/<lib>
                let parts: Vec<&str> = lib_path.split('/').collect();
                if parts.len() >= 4 && parts[1] == "opt" {
                    let dep_name = parts[2];
                    if let Some(dep_version) = dependencies.get(dep_name) {
                        // Reconstruct path with our cache structure
                        let remainder: String = parts[3..].join("/");
                        let new = format!(
                            "{}/{}/{}/{}",
                            homebrew_cache.display(),
                            dep_name,
                            dep_version,
                            remainder
                        );
                        Some(new)
                    } else {
                        trace!(lib_path, dep_name, "Dependency version not found, skipping");
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(new_path) = new_path {
                let status = Command::new("install_name_tool")
                    .arg("-change")
                    .arg(lib_path)
                    .arg(&new_path)
                    .arg(file_path)
                    .status();

                match status {
                    Ok(s) if s.success() => {
                        trace!(
                            file = ?file_path,
                            old = lib_path,
                            new = new_path,
                            "Relocated library path"
                        );
                    }
                    Ok(_) => {
                        debug!(
                            file = ?file_path,
                            lib = lib_path,
                            "install_name_tool failed (may need codesign)"
                        );
                    }
                    Err(e) => {
                        return Err(Error::ExtractionFailed {
                            binary: file_path.display().to_string(),
                            message: format!("install_name_tool failed: {}", e),
                        });
                    }
                }
            }
        }
    }

    // Re-sign binaries (required on Apple Silicon)
    for file_path in &files_to_process {
        let status = Command::new("codesign")
            .arg("--force")
            .arg("--sign")
            .arg("-")
            .arg(file_path)
            .status();

        match status {
            Ok(s) if s.success() => {
                trace!(file = ?file_path, "Re-signed binary");
            }
            Ok(s) => {
                tracing::warn!(
                    file = ?file_path,
                    exit_code = ?s.code(),
                    "codesign failed - binary may not execute on Apple Silicon"
                );
            }
            Err(e) => {
                tracing::warn!(
                    file = ?file_path,
                    error = %e,
                    "codesign command failed to run - binary may not execute on Apple Silicon"
                );
            }
        }
    }

    debug!(?formula_dir, "Relocation complete");
    Ok(())
}

/// Stub for non-macOS platforms.
#[cfg(not(target_os = "macos"))]
pub fn relocate_homebrew_bottle(
    _formula_dir: &Path,
    _homebrew_cache: &Path,
    _formula_name: &str,
    _formula_version: &str,
    _dependencies: &std::collections::HashMap<String, String>,
) -> Result<()> {
    // Linux Homebrew bottles use different relocation (patchelf)
    // For now, we rely on LD_LIBRARY_PATH
    Ok(())
}

/// Extract a single binary from a Homebrew bottle.
///
/// Homebrew bottles are gzip-compressed tarballs with structure:
/// ```text
/// {name}/{version}/
/// ├── bin/
/// │   └── {binary}
/// ├── lib/
/// └── share/
/// ```
///
/// For example, `jq` 1.7.1:
/// ```text
/// jq/1.7.1/
/// ├── bin/
/// │   └── jq
/// └── share/
///     └── man/
/// ```
///
/// **Note**: For dynamically linked binaries, use `extract_homebrew_bottle`
/// instead to also extract lib/ dependencies.
pub fn extract_homebrew_binary(
    bottle_path: &Path,
    binary_name: &str,
    dest: &Path,
) -> Result<PathBuf> {
    debug!(
        ?bottle_path,
        binary_name,
        ?dest,
        "Extracting Homebrew binary"
    );

    let file = File::open(bottle_path)?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    // Look for the binary in any `*/bin/{binary_name}` path
    let bin_suffix = format!("/bin/{}", binary_name);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let path_str = path.to_string_lossy();

        trace!(path = %path_str, "Checking archive entry");

        if path_str.ends_with(&bin_suffix) {
            // Found the binary
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }

            // Extract to destination
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            std::fs::write(dest, &content)?;

            // Make executable
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(dest)?.permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(dest, perms)?;
            }

            debug!(?dest, "Extracted binary");
            return Ok(dest.to_path_buf());
        }
    }

    Err(Error::BinaryNotFound(binary_name.to_string()))
}

/// Extract a file from OCI image layers.
///
/// Layers are applied in order (later layers override earlier).
/// The file is extracted from the first layer that contains it.
///
/// # Arguments
///
/// * `layers` - Paths to layer tar.gz files, in order
/// * `file_path` - Path to extract (e.g., "/usr/bin/nginx")
/// * `dest` - Destination path for extracted file
pub fn extract_from_layers(layers: &[PathBuf], file_path: &str, dest: &Path) -> Result<PathBuf> {
    debug!(
        ?file_path,
        ?dest,
        layer_count = layers.len(),
        "Extracting from layers"
    );

    // Normalize the path (remove leading slash for tar matching)
    let normalized_path = file_path.trim_start_matches('/');

    // Process layers in reverse order (later layers take precedence)
    for layer_path in layers.iter().rev() {
        trace!(?layer_path, "Checking layer");

        let file = File::open(layer_path)?;
        let decoder = GzDecoder::new(file);
        let mut archive = Archive::new(decoder);

        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?;
            let path_str = path.to_string_lossy();

            // Match the path (tar entries may or may not have leading ./)
            let entry_path = path_str.trim_start_matches("./").trim_start_matches('/');

            if entry_path == normalized_path {
                // Found the file
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }

                // Extract to destination
                let mut content = Vec::new();
                entry.read_to_end(&mut content)?;
                std::fs::write(dest, &content)?;

                // Preserve permissions if available
                #[cfg(unix)]
                if let Ok(mode) = entry.header().mode() {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = std::fs::metadata(dest)?.permissions();
                    perms.set_mode(mode);
                    std::fs::set_permissions(dest, perms)?;
                }

                debug!(?dest, "Extracted file from layer");
                return Ok(dest.to_path_buf());
            }
        }
    }

    Err(Error::BinaryNotFound(file_path.to_string()))
}

/// List all files in an archive (for debugging).
#[allow(dead_code)]
pub fn list_archive_contents(archive_path: &Path) -> Result<Vec<String>> {
    let file = File::open(archive_path)?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    let mut paths = Vec::new();
    for entry in archive.entries()? {
        let entry = entry?;
        paths.push(entry.path()?.to_string_lossy().to_string());
    }

    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use tar::Builder;
    use tempfile::TempDir;

    fn create_test_tarball(dir: &Path, files: &[(&str, &[u8])]) -> PathBuf {
        let tarball_path = dir.join("test.tar.gz");
        let file = File::create(&tarball_path).unwrap();
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = Builder::new(encoder);

        for (path, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append(&header, &content[..]).unwrap();
        }

        builder.into_inner().unwrap().finish().unwrap();
        tarball_path
    }

    #[test]
    fn test_extract_homebrew_binary() -> Result<()> {
        let temp = TempDir::new()?;

        // Create a mock Homebrew bottle
        let bottle = create_test_tarball(
            temp.path(),
            &[
                ("jq/1.7.1/.brew/jq.rb", b"formula"),
                ("jq/1.7.1/bin/jq", b"#!/bin/sh\necho jq"),
                ("jq/1.7.1/share/man/man1/jq.1", b"manpage"),
            ],
        );

        let dest = temp.path().join("extracted").join("jq");
        let result = extract_homebrew_binary(&bottle, "jq", &dest)?;

        assert_eq!(result, dest);
        assert!(dest.exists());

        let content = std::fs::read_to_string(&dest)?;
        assert_eq!(content, "#!/bin/sh\necho jq");

        Ok(())
    }

    #[test]
    fn test_extract_from_layers() -> Result<()> {
        let temp = TempDir::new()?;

        // Create two layers
        let layer1 = create_test_tarball(
            temp.path(),
            &[("usr/bin/app", b"version 1"), ("etc/config", b"config 1")],
        );

        // Rename to layer2 path
        let layer2_path = temp.path().join("layer2.tar.gz");
        let layer2 = create_test_tarball(
            temp.path(),
            &[("usr/bin/app", b"version 2")], // Override
        );
        std::fs::rename(&layer2, &layer2_path)?;

        let dest = temp.path().join("extracted").join("app");
        let layers = vec![layer1, layer2_path];

        let result = extract_from_layers(&layers, "/usr/bin/app", &dest)?;

        assert_eq!(result, dest);

        // Should get version 2 (from later layer)
        let content = std::fs::read_to_string(&dest)?;
        assert_eq!(content, "version 2");

        Ok(())
    }

    #[test]
    fn test_binary_not_found() {
        let temp = TempDir::new().unwrap();
        let bottle = create_test_tarball(temp.path(), &[("other/file", b"content")]);

        let dest = temp.path().join("missing");
        let result = extract_homebrew_binary(&bottle, "jq", &dest);

        assert!(matches!(result, Err(Error::BinaryNotFound(_))));
    }

    /// Helper to create a tarball with raw path bytes (bypassing tar's path validation).
    fn create_tarball_with_raw_path(dir: &Path, path_bytes: &[u8], content: &[u8]) -> PathBuf {
        use std::io::Write;

        let tarball_path = dir.join("malicious.tar.gz");
        let file = File::create(&tarball_path).unwrap();
        let mut encoder = GzEncoder::new(file, Compression::default());

        // Create a GNU tar header manually
        let mut header = [0u8; 512];

        // Name field (first 100 bytes)
        let name_len = std::cmp::min(path_bytes.len(), 100);
        header[..name_len].copy_from_slice(&path_bytes[..name_len]);

        // Mode (octal string, 8 bytes at offset 100)
        header[100..107].copy_from_slice(b"0000755");

        // UID (octal string, 8 bytes at offset 108)
        header[108..115].copy_from_slice(b"0000000");

        // GID (octal string, 8 bytes at offset 116)
        header[116..123].copy_from_slice(b"0000000");

        // Size (octal string, 12 bytes at offset 124)
        let size_str = format!("{:011o}", content.len());
        header[124..135].copy_from_slice(size_str.as_bytes());

        // Mtime (octal string, 12 bytes at offset 136)
        header[136..147].copy_from_slice(b"00000000000");

        // Typeflag (1 byte at offset 156): '0' for regular file
        header[156] = b'0';

        // Magic (6 bytes at offset 257): "ustar\0"
        header[257..263].copy_from_slice(b"ustar\0");

        // Version (2 bytes at offset 263)
        header[263..265].copy_from_slice(b"00");

        // Calculate and set checksum (8 bytes at offset 148)
        // First, fill checksum field with spaces for calculation
        header[148..156].copy_from_slice(b"        ");
        let checksum: u32 = header.iter().map(|&b| b as u32).sum();
        let checksum_str = format!("{:06o}\0 ", checksum);
        header[148..156].copy_from_slice(checksum_str.as_bytes());

        encoder.write_all(&header).unwrap();

        // Write content (padded to 512 bytes)
        encoder.write_all(content).unwrap();
        let padding = 512 - (content.len() % 512);
        if padding < 512 {
            encoder.write_all(&vec![0u8; padding]).unwrap();
        }

        // Write two empty blocks to end the archive
        encoder.write_all(&[0u8; 1024]).unwrap();

        encoder.finish().unwrap();
        tarball_path
    }

    #[test]
    fn test_path_traversal_rejected() {
        let temp = TempDir::new().unwrap();

        // Create a tarball with a path traversal attempt using raw bytes
        // Path: "jq/1.7.1/../../../etc/passwd" - this bypasses tar's validation
        let malicious_path = b"jq/1.7.1/../../../etc/passwd";
        let bottle = create_tarball_with_raw_path(temp.path(), malicious_path, b"malicious");

        let dest_dir = temp.path().join("output");
        let result = extract_homebrew_bottle(&bottle, &dest_dir);

        // Should fail due to path traversal
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("path traversal"),
            "Expected path traversal error, got: {}",
            err
        );

        // Ensure the malicious file was not created outside dest_dir
        assert!(!temp.path().join("etc/passwd").exists());
    }

    #[test]
    fn test_extract_bottle_normal_paths() -> Result<()> {
        let temp = TempDir::new()?;

        // Create a normal bottle structure
        let bottle = create_test_tarball(
            temp.path(),
            &[
                ("jq/1.7.1/bin/jq", b"binary"),
                ("jq/1.7.1/lib/libjq.dylib", b"library"),
                ("jq/1.7.1/share/doc/README", b"docs"),
            ],
        );

        let dest_dir = temp.path().join("output");
        extract_homebrew_bottle(&bottle, &dest_dir)?;

        // Verify files were extracted correctly
        assert!(dest_dir.join("bin/jq").exists());
        assert!(dest_dir.join("lib/libjq.dylib").exists());
        assert!(dest_dir.join("share/doc/README").exists());

        Ok(())
    }
}
