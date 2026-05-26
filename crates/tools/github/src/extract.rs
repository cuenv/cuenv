use crate::GitHubToolProvider;
use cuenv_core::Result;
use flate2::read::GzDecoder;
#[cfg(target_os = "macos")]
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
use tar::Archive;
#[cfg(target_os = "macos")]
use tempfile::Builder;
#[cfg(target_os = "macos")]
use tracing::debug;
use xz2::read::XzDecoder;

impl GitHubToolProvider {
    /// Extract a binary from an archive.
    pub(super) fn extract_binary(
        &self,
        data: &[u8],
        asset_name: &str,
        binary_path: Option<&str>,
        dest: &std::path::Path,
    ) -> Result<PathBuf> {
        // Determine archive type
        let is_zip = asset_name.ends_with(".zip");
        let is_tar_gz = asset_name.ends_with(".tar.gz") || asset_name.ends_with(".tgz");
        let is_tar_xz = asset_name.ends_with(".tar.xz") || asset_name.ends_with(".txz");
        let is_pkg = asset_name.ends_with(".pkg");

        if is_zip {
            self.extract_from_zip(data, binary_path, dest)
        } else if is_tar_gz {
            self.extract_from_tar_gz(data, binary_path, dest)
        } else if is_tar_xz {
            self.extract_from_tar_xz(data, binary_path, dest)
        } else if is_pkg {
            self.extract_from_pkg(data, binary_path, dest)
        } else {
            // Assume it's a raw binary
            std::fs::create_dir_all(dest)?;
            let binary_name = std::path::Path::new(asset_name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(asset_name);
            let binary_dest = dest.join(binary_name);
            std::fs::write(&binary_dest, data)?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&binary_dest)?.permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&binary_dest, perms)?;
            }

            Ok(binary_dest)
        }
    }

    /// Extract from a zip archive.
    ///
    /// Uses a temporary directory for atomic extraction - if extraction fails
    /// partway through, no partial files are left in the destination.
    fn extract_from_zip(
        &self,
        data: &[u8],
        binary_path: Option<&str>,
        dest: &std::path::Path,
    ) -> Result<PathBuf> {
        let cursor = Cursor::new(data);
        let mut archive = zip::ZipArchive::new(cursor).map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to open zip: {}", e))
        })?;

        // If a specific path is requested, extract just that file (no temp dir needed)
        if let Some(path) = binary_path {
            for i in 0..archive.len() {
                let mut file = archive.by_index(i).map_err(|e| {
                    cuenv_core::Error::tool_resolution(format!("Failed to read zip entry: {}", e))
                })?;

                let name = file.name().to_string();
                if name == path || name.ends_with(&format!("/{}", path)) {
                    std::fs::create_dir_all(dest)?;
                    let file_name = std::path::Path::new(&name)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(path);
                    let dest_path = dest.join(file_name);

                    let mut content = Vec::new();
                    file.read_to_end(&mut content)?;
                    std::fs::write(&dest_path, &content)?;

                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let mut perms = std::fs::metadata(&dest_path)?.permissions();
                        perms.set_mode(0o755);
                        std::fs::set_permissions(&dest_path, perms)?;
                    }

                    return Ok(dest_path);
                }
            }

            return Err(cuenv_core::Error::tool_resolution(format!(
                "Binary '{}' not found in archive",
                path
            )));
        }

        // Extract all files to a temp directory first for atomic operation
        let temp_dir = dest.with_file_name(format!(
            ".{}.tmp",
            dest.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("extract")
        ));

        // Clean up any previous failed extraction
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir)?;
        }
        std::fs::create_dir_all(&temp_dir)?;

        // Extract to temp directory
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

        // On failure, clean up temp directory
        if let Err(e) = extract_result {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(e);
        }

        // Atomic move: remove destination if exists, then rename temp to dest
        if dest.exists() {
            std::fs::remove_dir_all(dest)?;
        }
        std::fs::rename(&temp_dir, dest)?;

        // Find the main binary (first executable in bin/ or root)
        self.find_main_binary(dest)
    }

    /// Extract from a tar.gz archive.
    pub(super) fn extract_from_tar_gz(
        &self,
        data: &[u8],
        binary_path: Option<&str>,
        dest: &std::path::Path,
    ) -> Result<PathBuf> {
        let decoder = GzDecoder::new(Cursor::new(data));
        self.extract_from_tar(decoder, binary_path, dest)
    }

    /// Extract from a tar.xz archive.
    pub(super) fn extract_from_tar_xz(
        &self,
        data: &[u8],
        binary_path: Option<&str>,
        dest: &std::path::Path,
    ) -> Result<PathBuf> {
        let decoder = XzDecoder::new(Cursor::new(data));
        self.extract_from_tar(decoder, binary_path, dest)
    }

    /// Extract from a tar stream decoded by any `Read` (shared by tar.gz/tar.xz).
    fn extract_from_tar(
        &self,
        reader: impl Read,
        binary_path: Option<&str>,
        dest: &std::path::Path,
    ) -> Result<PathBuf> {
        let mut archive = Archive::new(reader);

        std::fs::create_dir_all(dest)?;

        if let Some(path) = binary_path {
            // Look for specific file
            for entry in archive.entries().map_err(|e| {
                cuenv_core::Error::tool_resolution(format!("Failed to read tar: {}", e))
            })? {
                let mut entry = entry.map_err(|e| {
                    cuenv_core::Error::tool_resolution(format!("Failed to read tar entry: {}", e))
                })?;

                let entry_path = entry.path().map_err(|e| {
                    cuenv_core::Error::tool_resolution(format!("Invalid path in tar: {}", e))
                })?;

                let path_str = entry_path.to_string_lossy();
                if path_str.as_ref() == path || path_str.ends_with(&format!("/{}", path)) {
                    let file_name = std::path::Path::new(path)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(path);
                    let dest_path = dest.join(file_name);

                    let mut content = Vec::new();
                    entry.read_to_end(&mut content)?;
                    std::fs::write(&dest_path, &content)?;

                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let mut perms = std::fs::metadata(&dest_path)?.permissions();
                        perms.set_mode(0o755);
                        std::fs::set_permissions(&dest_path, perms)?;
                    }

                    return Ok(dest_path);
                }
            }

            return Err(cuenv_core::Error::tool_resolution(format!(
                "Binary '{}' not found in archive",
                path
            )));
        }

        // Extract all files
        archive.unpack(dest).map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to extract tar: {}", e))
        })?;

        // Find the main binary
        self.find_main_binary(dest)
    }

    /// Extract from a macOS .pkg archive.
    #[cfg(target_os = "macos")]
    fn extract_from_pkg(
        &self,
        data: &[u8],
        binary_path: Option<&str>,
        dest: &std::path::Path,
    ) -> Result<PathBuf> {
        std::fs::create_dir_all(dest)?;

        let work_dir = Builder::new().prefix("cuenv-pkg-").tempdir().map_err(|e| {
            cuenv_core::Error::tool_resolution(format!(
                "Failed to create temporary directory for pkg extraction: {}",
                e
            ))
        })?;

        let pkg_path = work_dir.path().join("asset.pkg");
        std::fs::write(&pkg_path, data)?;

        let expanded_dir = work_dir.path().join("expanded");
        Self::run_command(
            Command::new("pkgutil")
                .arg("--expand")
                .arg(&pkg_path)
                .arg(&expanded_dir),
            "expand pkg archive",
        )?;

        let payloads = Self::collect_payload_files(&expanded_dir)?;
        if payloads.is_empty() {
            return Err(cuenv_core::Error::tool_resolution(
                "No payload files found in pkg archive".to_string(),
            ));
        }

        for (index, payload_path) in payloads.iter().enumerate() {
            let payload_dir = work_dir.path().join(format!("payload-{index}"));
            std::fs::create_dir_all(&payload_dir)?;

            let payload_file = File::open(payload_path)?;
            let payload_extract = Self::run_command(
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
                if let Some(found) = Self::find_path_in_tree(&payload_dir, path)? {
                    return Self::copy_extracted_file(&found, dest, path);
                }
            } else if let Ok(found) = self.find_main_binary(&payload_dir) {
                return Self::copy_extracted_file(&found, dest, "binary");
            }
        }

        if let Some(path) = binary_path {
            return Err(cuenv_core::Error::tool_resolution(format!(
                "Binary '{}' not found in pkg payloads",
                path
            )));
        }

        Err(cuenv_core::Error::tool_resolution(
            "No executable found in pkg payloads".to_string(),
        ))
    }

    /// Extract from a macOS .pkg archive (unsupported on non-macOS hosts).
    #[cfg(not(target_os = "macos"))]
    fn extract_from_pkg(
        &self,
        _data: &[u8],
        _binary_path: Option<&str>,
        _dest: &std::path::Path,
    ) -> Result<PathBuf> {
        Err(cuenv_core::Error::tool_resolution(
            ".pkg extraction is only supported on macOS hosts".to_string(),
        ))
    }

    /// Copy a selected extracted file into the destination directory.
    #[cfg(target_os = "macos")]
    fn copy_extracted_file(source: &Path, dest: &Path, fallback_name: &str) -> Result<PathBuf> {
        std::fs::create_dir_all(dest)?;
        let file_name = source
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(fallback_name);
        let dest_path = dest.join(file_name);
        std::fs::copy(source, &dest_path)?;
        Self::ensure_executable(&dest_path)?;
        Ok(dest_path)
    }

    /// Run a process and map non-zero exits to tool-resolution errors.
    #[cfg(target_os = "macos")]
    fn run_command(command: &mut Command, action: &str) -> Result<()> {
        let status = command.status().map_err(|e| {
            cuenv_core::Error::tool_resolution(format!("Failed to {}: {}", action, e))
        })?;

        if status.success() {
            Ok(())
        } else {
            Err(cuenv_core::Error::tool_resolution(format!(
                "Failed to {}: {}",
                action, status
            )))
        }
    }

    /// Recursively collect all `Payload` files from an expanded pkg directory.
    #[cfg(target_os = "macos")]
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
    #[cfg(target_os = "macos")]
    fn find_path_in_tree(root: &Path, path: &str) -> Result<Option<PathBuf>> {
        let requested = Self::normalize_lookup_path(path);
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
    #[cfg(target_os = "macos")]
    fn normalize_lookup_path(path: &str) -> String {
        path.trim_start_matches('/')
            .trim_start_matches("./")
            .to_string()
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

    /// Find the main binary in an extracted directory.
    fn find_main_binary(&self, dir: &std::path::Path) -> Result<PathBuf> {
        // First, look for binaries in bin/
        let bin_dir = dir.join("bin");
        if bin_dir.exists() {
            for entry in std::fs::read_dir(&bin_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() {
                    return Ok(path);
                }
            }
        }

        // Then look for executables in root
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = std::fs::metadata(&path)
                        && meta.permissions().mode() & 0o111 != 0
                    {
                        return Ok(path);
                    }
                }
                #[cfg(not(unix))]
                {
                    // On non-Unix, just return the first file
                    return Ok(path);
                }
            }
        }

        Err(cuenv_core::Error::tool_resolution(
            "No binary found in extracted archive".to_string(),
        ))
    }
}
