//! Binary extraction from OCI image layers.
//!
//! Supports:
//! - Homebrew bottles (tar.gz with `{name}/{version}/bin/{binary}` structure)
//! - Generic container images (extract specific paths from layers)

use flate2::read::GzDecoder;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use tar::Archive;
use tracing::{debug, trace};

use crate::{Error, Result};

/// Extract a binary from a Homebrew bottle.
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
    debug!(?file_path, ?dest, layer_count = layers.len(), "Extracting from layers");

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
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
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
            &[
                ("usr/bin/app", b"version 1"),
                ("etc/config", b"config 1"),
            ],
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
}
