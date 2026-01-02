//! Artifact generation for release binaries.
//!
//! This module handles:
//! - Target platform enumeration
//! - Tarball creation with gzip compression
//! - SHA256 checksum generation
//! - Checksums manifest file creation

use crate::error::{Error, Result};
use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Supported build targets for binary distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    /// Linux `x86_64`
    LinuxX64,
    /// Linux ARM64/aarch64
    LinuxArm64,
    /// macOS ARM64 (Apple Silicon)
    DarwinArm64,
}

impl Target {
    /// Returns the Rust target triple for this target.
    #[must_use]
    pub const fn rust_triple(&self) -> &'static str {
        match self {
            Self::LinuxX64 => "x86_64-unknown-linux-gnu",
            Self::LinuxArm64 => "aarch64-unknown-linux-gnu",
            Self::DarwinArm64 => "aarch64-apple-darwin",
        }
    }

    /// Returns the OS string for archive naming.
    #[must_use]
    pub const fn os(&self) -> &'static str {
        match self {
            Self::LinuxX64 | Self::LinuxArm64 => "linux",
            Self::DarwinArm64 => "darwin",
        }
    }

    /// Returns the architecture string for archive naming.
    #[must_use]
    pub const fn arch(&self) -> &'static str {
        match self {
            Self::LinuxX64 => "x86_64",
            Self::LinuxArm64 | Self::DarwinArm64 => "arm64",
        }
    }

    /// Returns the short identifier (e.g., "linux-x64").
    #[must_use]
    pub const fn short_id(&self) -> &'static str {
        match self {
            Self::LinuxX64 => "linux-x64",
            Self::LinuxArm64 => "linux-arm64",
            Self::DarwinArm64 => "darwin-arm64",
        }
    }

    /// Returns the GitHub Actions runner for this target.
    #[must_use]
    pub const fn github_runner(&self) -> &'static str {
        match self {
            Self::LinuxX64 | Self::LinuxArm64 => "ubuntu-latest",
            Self::DarwinArm64 => "macos-14",
        }
    }

    /// Returns all supported targets.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::LinuxX64, Self::LinuxArm64, Self::DarwinArm64]
    }

    /// Parses a target from a Rust triple.
    #[must_use]
    pub fn from_rust_triple(triple: &str) -> Option<Self> {
        match triple {
            "x86_64-unknown-linux-gnu" => Some(Self::LinuxX64),
            "aarch64-unknown-linux-gnu" => Some(Self::LinuxArm64),
            "aarch64-apple-darwin" => Some(Self::DarwinArm64),
            _ => None,
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.short_id())
    }
}

impl FromStr for Target {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "linux-x64" => Ok(Self::LinuxX64),
            "linux-arm64" => Ok(Self::LinuxArm64),
            "darwin-arm64" => Ok(Self::DarwinArm64),
            _ => Err(Error::artifact(
                format!("Unknown target: {s}. Valid targets: linux-x64, linux-arm64, darwin-arm64"),
                None,
            )),
        }
    }
}

/// A built binary artifact ready for packaging.
#[derive(Debug, Clone)]
pub struct Artifact {
    /// The target platform this artifact was built for.
    pub target: Target,
    /// Path to the compiled binary.
    pub binary_path: PathBuf,
    /// Name of the binary (e.g., "cuenv").
    pub name: String,
}

/// A packaged release artifact (tarball + checksum).
#[derive(Debug, Clone)]
pub struct PackagedArtifact {
    /// The target platform.
    pub target: Target,
    /// Path to the .tar.gz archive.
    pub archive_path: PathBuf,
    /// Path to the .sha256 checksum file.
    pub checksum_path: PathBuf,
    /// Name of the archive file.
    pub archive_name: String,
    /// SHA256 checksum hex string.
    pub sha256: String,
}

/// Builder for creating release artifacts.
pub struct ArtifactBuilder {
    output_dir: PathBuf,
    version: String,
    binary_name: String,
}

impl ArtifactBuilder {
    /// Creates a new artifact builder.
    ///
    /// # Arguments
    /// * `output_dir` - Directory to write archives to
    /// * `version` - Version string (e.g., "0.16.0")
    /// * `binary_name` - Name of the binary (e.g., "cuenv")
    #[must_use]
    pub fn new(
        output_dir: impl Into<PathBuf>,
        version: impl Into<String>,
        binary_name: impl Into<String>,
    ) -> Self {
        Self {
            output_dir: output_dir.into(),
            version: version.into(),
            binary_name: binary_name.into(),
        }
    }

    /// Generates the archive filename for a target.
    ///
    /// Format: `{binary}-{version}-{os}-{arch}.tar.gz`
    #[must_use]
    pub fn archive_name(&self, target: Target) -> String {
        format!(
            "{}-{}-{}-{}.tar.gz",
            self.binary_name,
            self.version,
            target.os(),
            target.arch()
        )
    }

    /// Packages an artifact into a tarball with checksum.
    ///
    /// Creates:
    /// - `{binary}-{version}-{os}-{arch}.tar.gz`
    /// - `{binary}-{version}-{os}-{arch}.tar.gz.sha256`
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Output directory cannot be created
    /// - Tarball creation fails
    /// - Checksum computation fails
    pub fn package(&self, artifact: &Artifact) -> Result<PackagedArtifact> {
        // Ensure output directory exists
        std::fs::create_dir_all(&self.output_dir)?;

        let archive_name = self.archive_name(artifact.target);
        let archive_path = self.output_dir.join(&archive_name);
        let checksum_path = self.output_dir.join(format!("{archive_name}.sha256"));

        // Create the tarball
        self.create_tarball(artifact, &archive_path)?;

        // Compute SHA256
        let sha256 = Self::compute_sha256(&archive_path)?;

        // Write checksum file
        self.write_checksum_file(&checksum_path, &sha256, &archive_name)?;

        Ok(PackagedArtifact {
            target: artifact.target,
            archive_path,
            checksum_path,
            archive_name,
            sha256,
        })
    }

    /// Packages multiple artifacts.
    ///
    /// # Errors
    ///
    /// Returns an error if any artifact fails to package.
    pub fn package_all(&self, artifacts: &[Artifact]) -> Result<Vec<PackagedArtifact>> {
        artifacts.iter().map(|a| self.package(a)).collect()
    }

    /// Computes the SHA256 checksum of a file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or read.
    pub fn compute_sha256(path: &Path) -> Result<String> {
        let file = File::open(path).map_err(|e| {
            Error::artifact(
                format!("Failed to open file for checksum: {e}"),
                Some(path.to_path_buf()),
            )
        })?;
        let mut reader = BufReader::new(file);
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 8192];

        loop {
            let bytes_read = reader.read(&mut buffer).map_err(|e| {
                Error::artifact(
                    format!("Failed to read file for checksum: {e}"),
                    Some(path.to_path_buf()),
                )
            })?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }

        let hash = hasher.finalize();
        Ok(format!("{hash:x}"))
    }

    /// Creates a tarball containing the binary.
    #[allow(clippy::unused_self)]
    fn create_tarball(&self, artifact: &Artifact, output: &Path) -> Result<()> {
        let file = File::create(output).map_err(|e| {
            Error::artifact(
                format!("Failed to create archive: {e}"),
                Some(output.to_path_buf()),
            )
        })?;
        let encoder = GzEncoder::new(file, Compression::default());
        let mut archive = tar::Builder::new(encoder);

        // Read the binary
        let binary_file = File::open(&artifact.binary_path).map_err(|e| {
            Error::artifact(
                format!("Failed to open binary: {e}"),
                Some(artifact.binary_path.clone()),
            )
        })?;
        let metadata = binary_file.metadata().map_err(|e| {
            Error::artifact(
                format!("Failed to read binary metadata: {e}"),
                Some(artifact.binary_path.clone()),
            )
        })?;

        // Create tar header
        let mut header = tar::Header::new_gnu();
        header.set_path(&artifact.name)?;
        header.set_size(metadata.len());
        header.set_mode(0o755); // Executable
        header.set_cksum();

        // Add to archive
        archive.append(&header, &binary_file)?;
        archive.finish()?;

        Ok(())
    }

    /// Writes a checksum file in the standard format.
    #[allow(clippy::unused_self)]
    fn write_checksum_file(&self, path: &Path, sha256: &str, filename: &str) -> Result<()> {
        let content = format!("{sha256}  {filename}\n");
        std::fs::write(path, content)?;
        Ok(())
    }
}

/// Checksums manifest containing all artifact checksums.
#[derive(Debug, Default)]
pub struct ChecksumsManifest {
    /// Map of filename to SHA256 checksum.
    entries: HashMap<String, String>,
}

impl ChecksumsManifest {
    /// Creates a new empty manifest.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a checksum entry.
    pub fn add(&mut self, filename: impl Into<String>, sha256: impl Into<String>) {
        self.entries.insert(filename.into(), sha256.into());
    }

    /// Creates a manifest from packaged artifacts.
    #[must_use]
    pub fn from_artifacts(artifacts: &[PackagedArtifact]) -> Self {
        let mut manifest = Self::new();
        for artifact in artifacts {
            manifest.add(artifact.archive_name.clone(), artifact.sha256.clone());
        }
        manifest
    }

    /// Returns the checksums in the standard format.
    ///
    /// Format: `{sha256}  {filename}\n` (note: two spaces, matching sha256sum output)
    #[must_use]
    pub fn to_checksums_format(&self) -> String {
        let mut lines: Vec<_> = self
            .entries
            .iter()
            .map(|(filename, sha256)| format!("{sha256}  {filename}"))
            .collect();
        lines.sort(); // Deterministic ordering
        lines.join("\n") + "\n"
    }

    /// Writes the manifest to a CHECKSUMS.txt file.
    ///
    /// # Errors
    ///
    /// Returns an error if writing to the file fails.
    pub fn write(&self, path: &Path) -> Result<()> {
        let content = self.to_checksums_format();
        std::fs::write(path, content)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_target_rust_triple() {
        assert_eq!(Target::LinuxX64.rust_triple(), "x86_64-unknown-linux-gnu");
        assert_eq!(
            Target::LinuxArm64.rust_triple(),
            "aarch64-unknown-linux-gnu"
        );
        assert_eq!(Target::DarwinArm64.rust_triple(), "aarch64-apple-darwin");
    }

    #[test]
    fn test_target_short_id() {
        assert_eq!(Target::LinuxX64.short_id(), "linux-x64");
        assert_eq!(Target::LinuxArm64.short_id(), "linux-arm64");
        assert_eq!(Target::DarwinArm64.short_id(), "darwin-arm64");
    }

    #[test]
    fn test_target_from_str() {
        assert_eq!(Target::from_str("linux-x64").unwrap(), Target::LinuxX64);
        assert_eq!(
            Target::from_str("darwin-arm64").unwrap(),
            Target::DarwinArm64
        );
        assert!(Target::from_str("unknown").is_err());
    }

    #[test]
    fn test_target_from_rust_triple() {
        assert_eq!(
            Target::from_rust_triple("x86_64-unknown-linux-gnu"),
            Some(Target::LinuxX64)
        );
        assert_eq!(Target::from_rust_triple("unknown-triple"), None);
    }

    #[test]
    fn test_archive_name() {
        let builder = ArtifactBuilder::new("/tmp", "0.16.0", "cuenv");
        assert_eq!(
            builder.archive_name(Target::LinuxX64),
            "cuenv-0.16.0-linux-x86_64.tar.gz"
        );
        assert_eq!(
            builder.archive_name(Target::DarwinArm64),
            "cuenv-0.16.0-darwin-arm64.tar.gz"
        );
    }

    #[test]
    fn test_compute_sha256() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        let hash = ArtifactBuilder::compute_sha256(&file_path).unwrap();
        assert_eq!(hash.len(), 64); // SHA256 hex length

        // Known hash for "hello world"
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_package_artifact() {
        let temp = TempDir::new().unwrap();
        let binary_path = temp.path().join("cuenv");
        std::fs::write(&binary_path, "#!/bin/bash\necho hello").unwrap();

        let output_dir = temp.path().join("dist");
        let builder = ArtifactBuilder::new(&output_dir, "0.16.0", "cuenv");

        let artifact = Artifact {
            target: Target::LinuxX64,
            binary_path,
            name: "cuenv".to_string(),
        };

        let packaged = builder.package(&artifact).unwrap();

        assert!(packaged.archive_path.exists());
        assert!(packaged.checksum_path.exists());
        assert_eq!(packaged.archive_name, "cuenv-0.16.0-linux-x86_64.tar.gz");
        assert_eq!(packaged.sha256.len(), 64);
    }

    #[test]
    fn test_checksums_manifest() {
        let mut manifest = ChecksumsManifest::new();
        manifest.add("file1.tar.gz", "abc123");
        manifest.add("file2.tar.gz", "def456");

        let output = manifest.to_checksums_format();
        assert!(output.contains("abc123  file1.tar.gz"));
        assert!(output.contains("def456  file2.tar.gz"));
    }
}
