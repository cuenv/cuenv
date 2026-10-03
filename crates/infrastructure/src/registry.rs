//! Provider installation from a Terraform provider registry.
//!
//! Resolves `namespace/type` (or `hostname/namespace/type`) plus an exact
//! version to a local executable, downloading and caching it on first use.
//! The cache layout matches Terraform's plugin cache
//! (`<hostname>/<namespace>/<type>/<version>/<operating_system>_<architecture>/`).
//!
//! Every path component is validated before it touches the file system: the
//! hostname, namespace and type must be plain lowercase registry names and the
//! version must be an exact semantic version, and every directory cuenv
//! removes, renames or executes from is checked to lie lexically inside the
//! cache directory.
//!
//! Integrity: the archive's SHA-256 is checked against the checksum the
//! registry reports in the same response that names the download, so a
//! registry (or anyone able to answer as it) vouches for itself. When
//! `cuenv.lock` pins the provider (`cuenv infrastructure provider add`),
//! [`ProviderInstaller::ensure_pinned`] also requires the pinned archive
//! SHA-256, both for a download and for a cached install, so the registry
//! cannot change the release after it was pinned. The registry's GPG
//! signature over `SHA256SUMS` is not verified yet.
//!
//! On install cuenv writes a sidecar manifest (`.cuenv-provider.json`) into
//! the install directory recording the exact provider executable name, its
//! SHA-256, the archive SHA-256, the source and the version. A cached
//! directory is only reused when that manifest is present and the executable
//! still matches its recorded hash; only that exact file is ever executed.
//! Directories populated by Terraform itself (for example through a shared
//! `TF_PLUGIN_CACHE_DIR`) carry no manifest, so cuenv reinstalls them once,
//! after which Terraform and cuenv share the result.
//!
//! Known limitations of the cache check for providers `cuenv.lock` does not
//! pin:
//!
//! - The manifest vouches for itself. It lives beside the executable, so
//!   anyone who can write the cache directory can replace both and have the
//!   replacement accepted. It detects corruption and foreign installs, not
//!   tampering; keep the cache (and any shared `TF_PLUGIN_CACHE_DIR`)
//!   writable only by users you trust to run code as you.
//! - Time of check to time of use: the executable is verified by path and
//!   later started by the same path. Verification refuses symbolic links and
//!   hashes the very file it inspected, but the file can still be replaced
//!   between verification and start by someone with write access to the
//!   cache.
//!
//! Resource limits: registry JSON documents are capped at 1 MiB; downloads
//! are streamed to a staging file while hashing and capped at 1 GiB;
//! extraction is capped at 2 GiB per entry and in total. Extracted files
//! never keep special permission bits: the provider executable is `0o755` and
//! every other file is `0o644`.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::error::{InfrastructureError, Result};

/// Default provider registry host.
pub const DEFAULT_REGISTRY: &str = "registry.terraform.io";

/// Name of the sidecar manifest cuenv writes into each install directory.
const MANIFEST_FILE_NAME: &str = ".cuenv-provider.json";

/// Directory under the cache root holding in-progress installs.
const STAGING_DIRECTORY_NAME: &str = ".cuenv-staging";

/// File name of the downloaded archive inside a staging directory.
const ARCHIVE_FILE_NAME: &str = "provider.zip";

/// Directory name extracted files land in inside a staging directory.
const EXTRACTED_DIRECTORY_NAME: &str = "extracted";

/// Largest provider archive cuenv downloads (1 GiB).
const MAXIMUM_ARCHIVE_BYTES: u64 = 1 << 30;

/// Largest single entry, and largest total, cuenv extracts (2 GiB).
const MAXIMUM_EXTRACTED_BYTES: u64 = 2 << 30;

/// Largest sidecar manifest cuenv reads.
const MAXIMUM_MANIFEST_BYTES: u64 = 64 * 1024;

/// Largest registry JSON document (service discovery, download metadata)
/// cuenv reads (1 MiB).
const MAXIMUM_REGISTRY_DOCUMENT_BYTES: u64 = 1024 * 1024;

/// Permissions applied to the provider executable.
#[cfg(unix)]
const EXECUTABLE_MODE: u32 = 0o755;

/// Permissions applied to every other extracted file.
#[cfg(unix)]
const REGULAR_FILE_MODE: u32 = 0o644;

/// Fully-qualified provider source address.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderSource {
    /// Registry hostname.
    pub hostname: String,
    /// Registry namespace, for example `hashicorp`.
    pub namespace: String,
    /// Provider type, for example `random`.
    pub type_name: String,
}

impl ProviderSource {
    /// Parse `namespace/type` or `hostname/namespace/type`.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for malformed addresses,
    /// including names that are not plain registry names.
    pub fn parse(source: &str) -> Result<Self> {
        let parts: Vec<&str> = source.trim().split('/').collect();
        let (hostname, namespace, type_name) = match parts.as_slice() {
            [namespace, type_name] => (DEFAULT_REGISTRY, *namespace, *type_name),
            [hostname, namespace, type_name] => (*hostname, *namespace, *type_name),
            _ => {
                return Err(InfrastructureError::configuration(format!(
                    "invalid provider source '{source}'; expected namespace/type or hostname/namespace/type"
                )));
            }
        };
        let parsed = Self {
            hostname: hostname.to_ascii_lowercase(),
            namespace: namespace.to_ascii_lowercase(),
            type_name: type_name.to_ascii_lowercase(),
        };
        parsed.validate().map_err(|error| {
            InfrastructureError::configuration(format!(
                "invalid provider source '{source}': {error}"
            ))
        })?;
        Ok(parsed)
    }

    /// Check every component is a plain registry name that is safe to use as
    /// a path component and inside a URL.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] naming the first invalid
    /// component.
    pub fn validate(&self) -> Result<()> {
        if !is_valid_hostname(&self.hostname) {
            return Err(InfrastructureError::configuration(format!(
                "invalid registry hostname '{}'; expected lowercase letters, digits, '.' and '-' with an optional :port",
                self.hostname
            )));
        }
        for (kind, value) in [("namespace", &self.namespace), ("type", &self.type_name)] {
            if !is_valid_registry_name(value) {
                return Err(InfrastructureError::configuration(format!(
                    "invalid provider {kind} '{value}'; expected lowercase letters, digits and '-', starting with a letter or digit"
                )));
            }
        }
        Ok(())
    }
}

impl fmt::Display for ProviderSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}/{}/{}",
            self.hostname, self.namespace, self.type_name
        )
    }
}

/// `^[a-z0-9][a-z0-9-]*$`
fn is_valid_registry_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// `^[a-z0-9.-]+(:[0-9]+)?$` without `..` and without a leading or trailing `.`.
fn is_valid_hostname(value: &str) -> bool {
    let (host, port) = match value.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (value, None),
    };
    let host_is_valid = !host.is_empty()
        && !host.contains("..")
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'-'
        });
    let port_is_valid =
        port.is_none_or(|port| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()));
    host_is_valid && port_is_valid
}

/// Check `version` is an exact semantic version
/// (`MAJOR.MINOR.PATCH[-prerelease][+build]`), which also guarantees it is a
/// single safe path component.
///
/// # Errors
///
/// Returns [`InfrastructureError::Configuration`] for anything else, including
/// version constraints, a `v` prefix, path separators and `..`.
pub fn validate_version(version: &str) -> Result<()> {
    let (without_build, build) = match version.split_once('+') {
        Some((head, build)) => (head, Some(build)),
        None => (version, None),
    };
    let (core, prerelease) = match without_build.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (without_build, None),
    };
    let numbers: Vec<&str> = core.split('.').collect();
    let core_is_valid =
        numbers.len() == 3 && numbers.iter().all(|number| is_numeric_identifier(number));
    let suffixes_are_valid = [prerelease, build]
        .into_iter()
        .flatten()
        .all(is_semantic_version_suffix);
    if core_is_valid && suffixes_are_valid && !version.contains("..") {
        Ok(())
    } else {
        Err(InfrastructureError::configuration(format!(
            "invalid provider version '{version}'; expected an exact semantic version such as 3.7.2"
        )))
    }
}

/// `0|[1-9][0-9]*`
fn is_numeric_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

/// `[0-9A-Za-z.-]+`
fn is_semantic_version_suffix(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
}

/// Check `candidate` lies strictly inside `root`, comparing paths component
/// by component and rejecting any `..`, `.` or root component after `root`.
fn ensure_within(root: &Path, candidate: &Path) -> Result<()> {
    let escapes = || {
        InfrastructureError::install(format!(
            "refusing to use {} because it is not inside the provider cache {}",
            candidate.display(),
            root.display()
        ))
    };
    let relative = candidate.strip_prefix(root).map_err(|_| escapes())?;
    let mut components = relative.components().peekable();
    let has_components = components.peek().is_some();
    if has_components && components.all(|component| matches!(component, Component::Normal(_))) {
        Ok(())
    } else {
        Err(escapes())
    }
}

/// Terraform platform a provider build targets.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Platform {
    /// Terraform operating system name, for example `linux` or `darwin`.
    pub operating_system: String,
    /// Terraform architecture name, for example `amd64` or `arm64`.
    pub architecture: String,
}

impl fmt::Display for Platform {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}_{}", self.operating_system, self.architecture)
    }
}

/// Terraform platform of the running host, displayed as, for example, `linux_amd64`.
///
/// # Errors
///
/// Returns [`InfrastructureError::Install`] on platforms Terraform does not publish
/// providers for.
pub fn current_platform() -> Result<Platform> {
    let operating_system = match std::env::consts::OS {
        "macos" => "darwin",
        other @ ("linux" | "windows" | "freebsd" | "openbsd") => other,
        other => {
            return Err(InfrastructureError::install(format!(
                "unsupported operating system '{other}'"
            )));
        }
    };
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        "arm" => "arm",
        other => {
            return Err(InfrastructureError::install(format!(
                "unsupported architecture '{other}'"
            )));
        }
    };
    Ok(Platform {
        operating_system: operating_system.to_string(),
        architecture: architecture.to_string(),
    })
}

/// Default plugin cache directory.
#[must_use]
pub fn default_cache_directory() -> PathBuf {
    if let Some(directory) =
        std::env::var_os("TF_PLUGIN_CACHE_DIR").filter(|value| !value.is_empty())
    {
        return PathBuf::from(directory);
    }
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("cuenv")
        .join("infrastructure")
        .join("providers")
}

/// Downloads and caches provider binaries.
#[derive(Debug, Clone)]
pub struct ProviderInstaller {
    client: reqwest::Client,
    cache_directory: PathBuf,
}

/// Most redirects a registry request follows.
const MAXIMUM_REDIRECTS: usize = 5;

/// Which schemes a registry client may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegistryTransport {
    /// HTTPS only, redirects included.
    HttpsOnly,
    /// Plaintext too; only for tests against a local server.
    #[cfg(test)]
    PlaintextForTests,
}

#[derive(Debug, Deserialize)]
struct ServiceDiscovery {
    #[serde(rename = "providers.v1")]
    providers_v1: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProviderVersions {
    versions: Vec<ProviderVersion>,
}

#[derive(Debug, Deserialize)]
struct ProviderVersion {
    version: String,
    #[serde(default)]
    platforms: Vec<PublishedPlatform>,
}

#[derive(Debug, Deserialize)]
struct PublishedPlatform {
    os: String,
    arch: String,
}

#[derive(Debug, Deserialize)]
struct DownloadInformation {
    download_url: String,
    shasum: String,
    filename: String,
}

/// Sidecar manifest recording what cuenv installed into a directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct InstallationManifest {
    /// Exact file name of the provider executable.
    filename: String,
    /// Hex SHA-256 of the provider executable.
    sha256: String,
    /// Hex SHA-256 of the archive, as reported by the registry.
    archive_sha256: String,
    /// Provider source address.
    source: String,
    /// Provider version.
    version: String,
}

/// Caps applied while extracting a provider archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExtractionLimits {
    /// Largest single extracted entry, in bytes.
    entry_bytes: u64,
    /// Largest total of all extracted entries, in bytes.
    total_bytes: u64,
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            entry_bytes: MAXIMUM_EXTRACTED_BYTES,
            total_bytes: MAXIMUM_EXTRACTED_BYTES,
        }
    }
}

/// Where and how to extract a provider archive.
#[derive(Debug)]
struct ExtractionRequest<'request> {
    destination: &'request Path,
    type_name: &'request str,
    limits: ExtractionLimits,
}

/// The provider executable found while extracting an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExtractedProvider {
    filename: String,
    sha256: String,
}

/// A provider source at one exact version.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderRelease {
    source: ProviderSource,
    version: String,
}

/// Everything needed to turn a verified archive into an install directory.
#[derive(Debug)]
struct InstallationRequest {
    release: ProviderRelease,
    cache_directory: PathBuf,
    install_directory: PathBuf,
    archive_path: PathBuf,
    extraction_directory: PathBuf,
    archive_sha256: String,
    limits: ExtractionLimits,
}

/// A staging directory under the cache root, removed when dropped.
#[derive(Debug)]
struct StagingDirectory {
    path: PathBuf,
}

impl StagingDirectory {
    fn create(cache_directory: &Path) -> Result<Self> {
        let path = cache_directory
            .join(STAGING_DIRECTORY_NAME)
            .join(uuid::Uuid::new_v4().to_string());
        ensure_within(cache_directory, &path)?;
        std::fs::create_dir_all(&path).map_err(|error| {
            InfrastructureError::input_output("create provider staging directory", error)
        })?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(
                directory = %self.path.display(),
                %error,
                "failed to remove provider staging directory"
            );
        }
    }
}

/// A writer that hashes everything written through it.
struct HashingWriter<Inner> {
    inner: Inner,
    hasher: Sha256,
}

impl<Inner: Write> HashingWriter<Inner> {
    fn new(inner: Inner) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    fn finish(self) -> (Inner, String) {
        (self.inner, hex::encode(self.hasher.finalize()))
    }
}

impl<Inner: Write> Write for HashingWriter<Inner> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buffer)?;
        if let Some(accepted) = buffer.get(..written) {
            self.hasher.update(accepted);
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl ProviderInstaller {
    /// Create an installer using `cache_directory`.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Install`] if the HTTP client cannot be built.
    pub fn new(cache_directory: PathBuf) -> Result<Self> {
        Self::with_transport(cache_directory, RegistryTransport::HttpsOnly)
    }

    /// Every request, and every redirect it follows, must use HTTPS: a
    /// redirect to plaintext HTTP is refused, and at most
    /// [`MAXIMUM_REDIRECTS`] redirects are followed.
    fn with_transport(cache_directory: PathBuf, transport: RegistryTransport) -> Result<Self> {
        crate::ensure_rustls_cryptography_provider();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .redirect(reqwest::redirect::Policy::limited(MAXIMUM_REDIRECTS))
            .https_only(transport == RegistryTransport::HttpsOnly)
            .build()
            .map_err(|error| {
                InfrastructureError::install(format!("failed to build HTTP client: {error}"))
            })?;
        Ok(Self {
            client,
            cache_directory,
        })
    }

    /// Directory a provider version is (or will be) installed into.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for an invalid source or
    /// version, and [`InfrastructureError::Install`] on unsupported platforms or
    /// if the directory would fall outside the cache.
    pub fn install_directory(&self, source: &ProviderSource, version: &str) -> Result<PathBuf> {
        source.validate()?;
        validate_version(version)?;
        let platform = current_platform()?;
        let directory = self
            .cache_directory
            .join(&source.hostname)
            .join(&source.namespace)
            .join(&source.type_name)
            .join(version)
            .join(platform.to_string());
        ensure_within(&self.cache_directory, &directory)?;
        Ok(directory)
    }

    /// Return the provider executable, installing it if not cached.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for an invalid source or
    /// version, and [`InfrastructureError::Install`] if the registry lookup,
    /// download, checksum verification or extraction fails.
    pub async fn ensure(&self, source: &ProviderSource, version: &str) -> Result<PathBuf> {
        self.ensure_release(
            &ProviderRelease {
                source: source.clone(),
                version: version.to_string(),
            },
            None,
        )
        .await
    }

    /// Return the provider executable, installing it if not cached, and
    /// require the archive it came from to have the hex SHA-256
    /// `archive_sha256` (the pin `cuenv.lock` records for this platform). A
    /// cached install of a different archive is replaced; a registry that
    /// now serves a different archive is refused.
    ///
    /// # Errors
    ///
    /// As [`Self::ensure`], and [`InfrastructureError::Install`] when the
    /// archive does not match the pin.
    pub async fn ensure_pinned(
        &self,
        source: &ProviderSource,
        version: &str,
        archive_sha256: &str,
    ) -> Result<PathBuf> {
        self.ensure_release(
            &ProviderRelease {
                source: source.clone(),
                version: version.to_string(),
            },
            Some(archive_sha256),
        )
        .await
    }

    #[tracing::instrument(skip(self, release), fields(source = %release.source))]
    async fn ensure_release(
        &self,
        release: &ProviderRelease,
        pinned_archive_sha256: Option<&str>,
    ) -> Result<PathBuf> {
        let ProviderRelease { source, version } = release;
        let directory = self.install_directory(source, version)?;
        if let Some(installation) = self.cached_installation(&directory, release).await? {
            match pinned_archive_sha256 {
                Some(pin) if !installation.archive_sha256.eq_ignore_ascii_case(pin) => {
                    tracing::info!(
                        directory = %directory.display(),
                        cached = %installation.archive_sha256,
                        pinned = %pin,
                        "cached provider came from a different archive than cuenv.lock pins; reinstalling"
                    );
                }
                _ => return Ok(installation.binary),
            }
        }
        let pin_mismatch = |actual: &str, pin: &str| {
            InfrastructureError::install(format!(
                "{source} {version}: the registry's archive for this platform has SHA-256 {actual}, \
                 but cuenv.lock pins {pin}; the release changed after it was pinned"
            ))
        };

        let platform = current_platform()?;
        let base = self.providers_base_url(&source.hostname).await?;
        let download_metadata_url = format!(
            "{base}{}/{}/{version}/download/{}/{}",
            source.namespace, source.type_name, platform.operating_system, platform.architecture
        );
        let download: DownloadInformation = self
            .get_document(&download_metadata_url, "download metadata")
            .await?;
        if let Some(pin) = pinned_archive_sha256
            && !download.shasum.trim().eq_ignore_ascii_case(pin)
        {
            return Err(pin_mismatch(download.shasum.trim(), pin));
        }

        if !download.download_url.starts_with("https://") {
            return Err(InfrastructureError::install(format!(
                "registry returned a non-HTTPS download location for {}; refusing to download",
                download.filename
            )));
        }
        tracing::info!(file = %download.filename, "downloading provider");
        let staging = StagingDirectory::create(&self.cache_directory)?;
        let archive_path = staging.path().join(ARCHIVE_FILE_NAME);
        let archive_sha256 = self
            .download_archive(&download.download_url, &archive_path)
            .await?;
        if !archive_sha256.eq_ignore_ascii_case(download.shasum.trim()) {
            return Err(InfrastructureError::install(format!(
                "checksum mismatch for {}: registry says {}, downloaded {archive_sha256}",
                download.filename, download.shasum
            )));
        }

        if let Some(pin) = pinned_archive_sha256
            && !archive_sha256.eq_ignore_ascii_case(pin)
        {
            return Err(pin_mismatch(&archive_sha256, pin));
        }

        let request = InstallationRequest {
            release: release.clone(),
            cache_directory: self.cache_directory.clone(),
            install_directory: directory,
            archive_path,
            extraction_directory: staging.path().join(EXTRACTED_DIRECTORY_NAME),
            archive_sha256,
            limits: ExtractionLimits::default(),
        };
        let installation = tokio::task::spawn_blocking(move || install_from_archive(&request))
            .await
            .map_err(|error| {
                InfrastructureError::install(format!("installation task failed: {error}"))
            })??;
        drop(staging);
        // A concurrent install may have won the race with another archive.
        if let Some(pin) = pinned_archive_sha256
            && !installation.archive_sha256.eq_ignore_ascii_case(pin)
        {
            return Err(pin_mismatch(&installation.archive_sha256, pin));
        }
        Ok(installation.binary)
    }

    /// The hex archive SHA-256 the registry reports for every platform
    /// `version` is published for, keyed by Terraform platform name (such as
    /// `linux_amd64`). Nothing is downloaded but the registry's metadata.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for an invalid source
    /// or version, and [`InfrastructureError::Install`] when the registry
    /// does not list the version or a request fails.
    pub async fn archive_checksums(
        &self,
        source: &ProviderSource,
        version: &str,
    ) -> Result<BTreeMap<String, String>> {
        source.validate()?;
        validate_version(version)?;
        let base = self.providers_base_url(&source.hostname).await?;
        let versions_url = format!("{base}{}/{}/versions", source.namespace, source.type_name);
        let versions: ProviderVersions = self
            .get_document(&versions_url, "provider version list")
            .await?;
        let published = versions
            .versions
            .into_iter()
            .find(|candidate| candidate.version == version)
            .ok_or_else(|| {
                InfrastructureError::install(format!(
                    "{source} has no version {version} in its registry"
                ))
            })?;
        let mut checksums = BTreeMap::new();
        for platform in published.platforms {
            if !is_valid_registry_name(&platform.os) || !is_valid_registry_name(&platform.arch) {
                return Err(InfrastructureError::install(format!(
                    "{source} {version}: the registry lists an invalid platform '{}_{}'",
                    platform.os, platform.arch
                )));
            }
            let download_metadata_url = format!(
                "{base}{}/{}/{version}/download/{}/{}",
                source.namespace, source.type_name, platform.os, platform.arch
            );
            let download: DownloadInformation = self
                .get_document(&download_metadata_url, "download metadata")
                .await?;
            let shasum = download.shasum.trim().to_ascii_lowercase();
            if shasum.len() != 64 || !shasum.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(InfrastructureError::install(format!(
                    "{source} {version}: the registry reports an invalid SHA-256 for {}_{}",
                    platform.os, platform.arch
                )));
            }
            checksums.insert(format!("{}_{}", platform.os, platform.arch), shasum);
        }
        if checksums.is_empty() {
            return Err(InfrastructureError::install(format!(
                "{source} {version} is not published for any platform"
            )));
        }
        Ok(checksums)
    }

    /// Return the cached install if `directory` holds a verified one.
    async fn cached_installation(
        &self,
        directory: &Path,
        release: &ProviderRelease,
    ) -> Result<Option<VerifiedInstallation>> {
        let cache_directory = self.cache_directory.clone();
        let directory = directory.to_path_buf();
        let release = release.clone();
        tokio::task::spawn_blocking(move || {
            if std::fs::symlink_metadata(&directory).is_err() {
                return None;
            }
            match verified_installation(&cache_directory, &directory, &release) {
                Ok(installation) => Some(installation),
                Err(error) => {
                    tracing::info!(
                        directory = %directory.display(),
                        %error,
                        "cached provider is not a verified cuenv install; reinstalling"
                    );
                    None
                }
            }
        })
        .await
        .map_err(|error| {
            InfrastructureError::install(format!("cache verification task failed: {error}"))
        })
    }

    /// Stream `url` into `destination`, returning the hex SHA-256 of the body.
    async fn download_archive(&self, url: &str, destination: &Path) -> Result<String> {
        let too_large = || {
            InfrastructureError::install(format!(
                "provider archive at {url} exceeds the {MAXIMUM_ARCHIVE_BYTES}-byte download limit"
            ))
        };
        let mut response = self.get(url).await?;
        if response
            .content_length()
            .is_some_and(|length| length > MAXIMUM_ARCHIVE_BYTES)
        {
            return Err(too_large());
        }
        let mut file = tokio::fs::File::create(destination)
            .await
            .map_err(|error| InfrastructureError::input_output("create provider archive", error))?;
        let mut hasher = Sha256::new();
        let mut received: u64 = 0;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| InfrastructureError::install(format!("download failed: {error}")))?
        {
            received = received.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            if received > MAXIMUM_ARCHIVE_BYTES {
                return Err(too_large());
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await.map_err(|error| {
                InfrastructureError::input_output("write provider archive", error)
            })?;
        }
        file.flush()
            .await
            .map_err(|error| InfrastructureError::input_output("write provider archive", error))?;
        Ok(hex::encode(hasher.finalize()))
    }

    async fn providers_base_url(&self, hostname: &str) -> Result<String> {
        let discovery_url = format!("https://{hostname}/.well-known/terraform.json");
        let discovery: ServiceDiscovery = self
            .get_document(&discovery_url, "service discovery document")
            .await?;
        let path = discovery.providers_v1.ok_or_else(|| {
            InfrastructureError::install(format!("{hostname} does not offer a provider registry"))
        })?;
        let base = if path.starts_with("https://") {
            path
        } else {
            format!("https://{hostname}{path}")
        };
        Ok(if base.ends_with('/') {
            base
        } else {
            format!("{base}/")
        })
    }

    async fn get(&self, url: &str) -> Result<reqwest::Response> {
        let response =
            self.client.get(url).send().await.map_err(|error| {
                InfrastructureError::install(format!("GET {url} failed: {error}"))
            })?;
        if !response.status().is_success() {
            return Err(InfrastructureError::install(format!(
                "GET {url} returned HTTP {}",
                response.status()
            )));
        }
        Ok(response)
    }

    /// GET a JSON document of at most [`MAXIMUM_REGISTRY_DOCUMENT_BYTES`]
    /// and decode it; `description` names it in errors.
    async fn get_document<Document: DeserializeOwned>(
        &self,
        url: &str,
        description: &str,
    ) -> Result<Document> {
        let mut response = self.get(url).await?;
        let bytes = read_limited_body(&mut response, MAXIMUM_REGISTRY_DOCUMENT_BYTES)
            .await
            .map_err(|error| match error {
                LimitedBodyError::TooLarge => InfrastructureError::install(format!(
                    "{description} at {url} exceeds the {MAXIMUM_REGISTRY_DOCUMENT_BYTES}-byte limit"
                )),
                LimitedBodyError::Transport(error) => InfrastructureError::install(format!(
                    "GET {url} failed while reading the {description}: {error}"
                )),
            })?;
        serde_json::from_slice(&bytes).map_err(|error| {
            InfrastructureError::install(format!("invalid {description}: {error}"))
        })
    }
}

/// Why a size-limited body could not be read.
#[derive(Debug)]
enum LimitedBodyError {
    /// The body is larger than the limit.
    TooLarge,
    /// The connection failed while reading.
    Transport(reqwest::Error),
}

/// Read a whole response body, failing as soon as it exceeds `limit` bytes.
async fn read_limited_body(
    response: &mut reqwest::Response,
    limit: u64,
) -> std::result::Result<Vec<u8>, LimitedBodyError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(LimitedBodyError::TooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(LimitedBodyError::Transport)?
    {
        let received = u64::try_from(bytes.len().saturating_add(chunk.len())).unwrap_or(u64::MAX);
        if received > limit {
            return Err(LimitedBodyError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Hex SHA-256 of the rest of an open file, streamed.
fn hash_open_file(mut file: std::fs::File) -> std::io::Result<String> {
    let mut writer = HashingWriter::new(std::io::sink());
    std::io::copy(&mut file, &mut writer)?;
    Ok(writer.finish().1)
}

/// Open `path` for hashing only if it is a regular file, not a symbolic
/// link, and the file opened is the same one inspected (no swap between the
/// inspection and the open).
fn open_regular_file(path: &Path) -> Result<std::fs::File> {
    let not_regular = || {
        InfrastructureError::install(format!(
            "provider executable {} is not a regular file",
            path.display()
        ))
    };
    let inspected = std::fs::symlink_metadata(path)
        .map_err(|error| InfrastructureError::input_output("inspect provider executable", error))?;
    if !inspected.file_type().is_file() {
        return Err(not_regular());
    }
    let file = std::fs::File::open(path)
        .map_err(|error| InfrastructureError::input_output("open provider executable", error))?;
    let opened = file
        .metadata()
        .map_err(|error| InfrastructureError::input_output("inspect provider executable", error))?;
    if !opened.file_type().is_file() || !same_file(&inspected, &opened) {
        return Err(InfrastructureError::install(format!(
            "provider executable {} changed while it was being verified",
            path.display()
        )));
    }
    Ok(file)
}

/// Whether two metadata snapshots describe the same file.
#[cfg(unix)]
fn same_file(first: &std::fs::Metadata, second: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    first.dev() == second.dev() && first.ino() == second.ino()
}

/// Whether two metadata snapshots describe the same file. Without inode
/// numbers, compare what the platform offers.
#[cfg(not(unix))]
fn same_file(first: &std::fs::Metadata, second: &std::fs::Metadata) -> bool {
    first.len() == second.len() && first.modified().ok() == second.modified().ok()
}

/// Check `filename` is a single plain file name for the `type_name` provider.
fn is_provider_file_name(filename: &str, type_name: &str) -> bool {
    let mut components = Path::new(filename).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(name)), None) if name == filename
    ) && filename.starts_with(&format!("terraform-provider-{type_name}"))
}

/// A cached install whose executable matches its sidecar manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
struct VerifiedInstallation {
    /// The provider executable.
    binary: PathBuf,
    /// Hex SHA-256 of the archive it was extracted from.
    archive_sha256: String,
}

/// Verify `directory` holds a cuenv install of `release` whose executable
/// still matches the sidecar manifest; return that executable.
#[cfg(test)]
fn verify_installation(
    cache_directory: &Path,
    directory: &Path,
    release: &ProviderRelease,
) -> Result<PathBuf> {
    verified_installation(cache_directory, directory, release)
        .map(|installation| installation.binary)
}

/// Verify `directory` holds a cuenv install of `release` whose executable
/// still matches the sidecar manifest.
fn verified_installation(
    cache_directory: &Path,
    directory: &Path,
    release: &ProviderRelease,
) -> Result<VerifiedInstallation> {
    let ProviderRelease { source, version } = release;
    ensure_within(cache_directory, directory)?;
    let manifest_path = directory.join(MANIFEST_FILE_NAME);
    let mut manifest_text = String::new();
    std::fs::File::open(&manifest_path)
        .and_then(|file| {
            file.take(MAXIMUM_MANIFEST_BYTES)
                .read_to_string(&mut manifest_text)
        })
        .map_err(|error| InfrastructureError::input_output("read provider manifest", error))?;
    let manifest: InstallationManifest = serde_json::from_str(&manifest_text).map_err(|error| {
        InfrastructureError::install(format!("invalid provider manifest: {error}"))
    })?;
    if manifest.source != source.to_string() || manifest.version != *version {
        return Err(InfrastructureError::install(format!(
            "provider manifest records {} {} but {source} {version} was expected",
            manifest.source, manifest.version
        )));
    }
    if !is_provider_file_name(&manifest.filename, &source.type_name) {
        return Err(InfrastructureError::install(format!(
            "provider manifest names an invalid executable '{}'",
            manifest.filename
        )));
    }
    let binary = directory.join(&manifest.filename);
    ensure_within(cache_directory, &binary)?;
    let actual = hash_open_file(open_regular_file(&binary)?)
        .map_err(|error| InfrastructureError::input_output("hash provider executable", error))?;
    if !actual.eq_ignore_ascii_case(&manifest.sha256) {
        return Err(InfrastructureError::install(format!(
            "provider executable {} has SHA-256 {actual} but the manifest records {}",
            binary.display(),
            manifest.sha256
        )));
    }
    Ok(VerifiedInstallation {
        binary,
        archive_sha256: manifest.archive_sha256,
    })
}

/// Write the sidecar manifest into `directory`.
fn write_manifest(directory: &Path, manifest: &InstallationManifest) -> Result<()> {
    let text = serde_json::to_string_pretty(manifest).map_err(|error| {
        InfrastructureError::install(format!("failed to encode provider manifest: {error}"))
    })?;
    std::fs::write(directory.join(MANIFEST_FILE_NAME), text)
        .map_err(|error| InfrastructureError::input_output("write provider manifest", error))
}

/// Extract a verified archive, record its manifest and move it into place.
fn install_from_archive(request: &InstallationRequest) -> Result<VerifiedInstallation> {
    let cache_directory = &request.cache_directory;
    ensure_within(cache_directory, &request.extraction_directory)?;
    ensure_within(cache_directory, &request.install_directory)?;

    let archive = std::fs::File::open(&request.archive_path)
        .map_err(|error| InfrastructureError::input_output("open provider archive", error))?;
    let extracted = extract_provider_archive(
        std::io::BufReader::new(archive),
        &ExtractionRequest {
            destination: &request.extraction_directory,
            type_name: &request.release.source.type_name,
            limits: request.limits,
        },
    )?;
    write_manifest(
        &request.extraction_directory,
        &InstallationManifest {
            filename: extracted.filename,
            sha256: extracted.sha256,
            archive_sha256: request.archive_sha256.clone(),
            source: request.release.source.to_string(),
            version: request.release.version.clone(),
        },
    )?;

    let install_directory = &request.install_directory;
    if std::fs::symlink_metadata(install_directory).is_ok() {
        std::fs::remove_dir_all(install_directory).map_err(|error| {
            InfrastructureError::input_output("clear provider directory", error)
        })?;
    }
    if let Some(parent) = install_directory.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            InfrastructureError::input_output("create provider cache directory", error)
        })?;
    }
    if let Err(error) = std::fs::rename(&request.extraction_directory, install_directory) {
        // A concurrent install may have won the race; accept its result only
        // if it verifies.
        return verified_installation(cache_directory, install_directory, &request.release)
            .map_err(|_| InfrastructureError::input_output("install provider", error));
    }
    verified_installation(cache_directory, install_directory, &request.release)
}

/// Extract `archive` into `request.destination`, enforcing size limits and
/// normalised permissions, and return the provider executable it contained.
fn extract_provider_archive(
    archive: impl Read + Seek,
    request: &ExtractionRequest<'_>,
) -> Result<ExtractedProvider> {
    let destination = request.destination;
    let limits = request.limits;
    std::fs::create_dir_all(destination)
        .map_err(|error| InfrastructureError::input_output("create provider directory", error))?;
    let mut zip_archive = zip::ZipArchive::new(archive).map_err(|error| {
        InfrastructureError::install(format!("invalid provider archive: {error}"))
    })?;
    let mut remaining_bytes = limits.total_bytes;
    let mut provider: Option<ExtractedProvider> = None;
    for index in 0..zip_archive.len() {
        let mut entry = zip_archive.by_index(index).map_err(|error| {
            InfrastructureError::install(format!("invalid archive entry: {error}"))
        })?;
        let name = entry.name().to_string();
        let Some(relative) = entry.enclosed_name() else {
            return Err(InfrastructureError::install(format!(
                "archive entry escapes destination: {name}"
            )));
        };
        let target = destination.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|error| InfrastructureError::input_output("create directory", error))?;
            continue;
        }
        if entry.is_symlink() {
            return Err(InfrastructureError::install(format!(
                "archive entry {name} is a symbolic link, which provider archives must not contain"
            )));
        }
        let provider_file_name = relative
            .to_str()
            .filter(|file_name| is_provider_file_name(file_name, request.type_name))
            .map(str::to_string);
        if provider_file_name.is_some() && provider.is_some() {
            return Err(InfrastructureError::install(format!(
                "archive contains more than one terraform-provider-{} executable",
                request.type_name
            )));
        }

        let entry_limit = limits.entry_bytes.min(remaining_bytes);
        let too_large = || {
            InfrastructureError::install(format!(
                "archive entry {name} exceeds the extraction limit ({} bytes per entry, {} bytes in total)",
                limits.entry_bytes, limits.total_bytes
            ))
        };
        if entry.size() > entry_limit {
            return Err(too_large());
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| InfrastructureError::input_output("create directory", error))?;
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|error| InfrastructureError::input_output("create provider file", error))?;
        let mut writer = HashingWriter::new(file);
        let copied = std::io::copy(
            &mut entry.by_ref().take(entry_limit.saturating_add(1)),
            &mut writer,
        )
        .map_err(|error| InfrastructureError::input_output("extract archive entry", error))?;
        if copied > entry_limit {
            return Err(too_large());
        }
        remaining_bytes -= copied;
        let (mut file, sha256) = writer.finish();
        file.flush()
            .map_err(|error| InfrastructureError::input_output("write provider file", error))?;
        drop(file);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if provider_file_name.is_some() {
                EXECUTABLE_MODE
            } else {
                REGULAR_FILE_MODE
            };
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode)).map_err(
                |error| InfrastructureError::input_output("set provider permissions", error),
            )?;
        }
        if let Some(filename) = provider_file_name {
            provider = Some(ExtractedProvider { filename, sha256 });
        }
    }
    provider.ok_or_else(|| {
        InfrastructureError::install(format!(
            "archive did not contain a terraform-provider-{} executable",
            request.type_name
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROVIDER_FILE: &str = "terraform-provider-random_v3.7.2_x5";

    /// Hex SHA-256 of the file at `path`, streamed.
    fn hash_file(path: &Path) -> std::io::Result<String> {
        hash_open_file(std::fs::File::open(path)?)
    }

    fn random_source() -> ProviderSource {
        ProviderSource::parse("hashicorp/random").unwrap()
    }

    /// Build a zip archive from `(name, contents)` entries.
    fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            for (name, contents) in entries {
                writer
                    .start_file(
                        *name,
                        zip::write::SimpleFileOptions::default().unix_permissions(0o777),
                    )
                    .unwrap();
                writer.write_all(contents).unwrap();
            }
            writer.finish().unwrap();
        }
        buffer.into_inner()
    }

    /// Rewrite every central directory entry's Unix mode to `mode`.
    ///
    /// `SimpleFileOptions::unix_permissions` masks the mode to `0o777`, so
    /// special bits have to be patched into the external attributes directly.
    fn force_unix_mode(archive: &mut [u8], mode: u32) {
        const CENTRAL_DIRECTORY_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
        let attributes = ((0o100_000 | mode) << 16).to_le_bytes();
        let offsets: Vec<usize> = archive
            .windows(4)
            .enumerate()
            .filter(|(_, window)| *window == CENTRAL_DIRECTORY_SIGNATURE)
            .map(|(offset, _)| offset)
            .collect();
        assert!(!offsets.is_empty());
        for offset in offsets {
            // "Version made by" high byte: 3 = Unix.
            archive[offset + 5] = 3;
            archive[offset + 38..offset + 42].copy_from_slice(&attributes);
        }
    }

    fn extract(
        archive: &[u8],
        destination: &Path,
        limits: ExtractionLimits,
    ) -> Result<ExtractedProvider> {
        extract_provider_archive(
            std::io::Cursor::new(archive),
            &ExtractionRequest {
                destination,
                type_name: "random",
                limits,
            },
        )
    }

    fn write_installation(directory: &Path, binary_contents: &[u8]) -> PathBuf {
        std::fs::create_dir_all(directory).unwrap();
        let binary = directory.join(PROVIDER_FILE);
        std::fs::write(&binary, binary_contents).unwrap();
        write_manifest(
            directory,
            &InstallationManifest {
                filename: PROVIDER_FILE.to_string(),
                sha256: hash_file(&binary).unwrap(),
                archive_sha256: "00".repeat(32),
                source: random_source().to_string(),
                version: "3.7.2".to_string(),
            },
        )
        .unwrap();
        binary
    }

    fn release(version: &str) -> ProviderRelease {
        ProviderRelease {
            source: random_source(),
            version: version.to_string(),
        }
    }

    /// A verified install of `source` 3.7.2 from an archive with hex
    /// SHA-256 `"00" * 32`, placed where `installer` looks for it.
    fn cached_install(installer: &ProviderInstaller, source: &ProviderSource) -> PathBuf {
        let directory = installer.install_directory(source, "3.7.2").unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        let binary = directory.join(PROVIDER_FILE);
        std::fs::write(&binary, b"#!/bin/sh\n").unwrap();
        write_manifest(
            &directory,
            &InstallationManifest {
                filename: PROVIDER_FILE.to_string(),
                sha256: hash_file(&binary).unwrap(),
                archive_sha256: "00".repeat(32),
                source: source.to_string(),
                version: "3.7.2".to_string(),
            },
        )
        .unwrap();
        binary
    }

    #[tokio::test]
    async fn a_cached_install_of_the_pinned_archive_is_reused() {
        let cache = tempfile::tempdir().unwrap();
        let installer = ProviderInstaller::new(cache.path().to_path_buf()).unwrap();
        let binary = cached_install(&installer, &random_source());
        let pinned = installer
            .ensure_pinned(&random_source(), "3.7.2", &"00".repeat(32))
            .await
            .unwrap();
        assert_eq!(pinned, binary);
    }

    #[tokio::test]
    async fn a_cached_install_of_another_archive_is_not_reused_when_pinned() {
        let cache = tempfile::tempdir().unwrap();
        let installer = ProviderInstaller::new(cache.path().to_path_buf()).unwrap();
        // Port 1 refuses connections: reinstalling fails without the network.
        let source = ProviderSource::parse("localhost:1/hashicorp/random").unwrap();
        cached_install(&installer, &source);
        assert!(installer.ensure(&source, "3.7.2").await.is_ok());
        let error = installer
            .ensure_pinned(&source, "3.7.2", &"11".repeat(32))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("localhost:1"), "{error}");
    }

    #[test]
    fn parses_short_and_qualified_sources() {
        let short = random_source();
        assert_eq!(short.to_string(), "registry.terraform.io/hashicorp/random");
        let qualified =
            ProviderSource::parse("registry.opentofu.org/Cloudflare/Cloudflare").unwrap();
        assert_eq!(qualified.hostname, "registry.opentofu.org");
        assert_eq!(qualified.namespace, "cloudflare");
        assert!(ProviderSource::parse("localhost:8443/acme/widget-2").is_ok());
        assert!(ProviderSource::parse("random").is_err());
        assert!(ProviderSource::parse("hashicorp//random").is_err());
    }

    #[test]
    fn rejects_unsafe_source_components() {
        for source in [
            "../random",
            "hashicorp/..",
            "hashicorp/.",
            "-hashicorp/random",
            "hashicorp/-random",
            "hashicorp/ran_dom",
            "hashicorp/ran#dom",
            "hashicorp/random?x",
            "has hicorp/random",
            "../hashicorp/random",
            "./hashicorp/random",
            "registry..terraform.io/hashicorp/random",
            ".registry.terraform.io/hashicorp/random",
            "registry.terraform.io./hashicorp/random",
            "registry.terraform.io:/hashicorp/random",
            "registry.terraform.io:80a/hashicorp/random",
            "registry_terraform.io/hashicorp/random",
            "user@registry.terraform.io/hashicorp/random",
        ] {
            let error = ProviderSource::parse(source).unwrap_err();
            assert!(
                matches!(error, InfrastructureError::Configuration(_)),
                "{source}: {error}"
            );
        }
    }

    #[test]
    fn accepts_exact_semantic_versions() {
        for version in [
            "3.7.2",
            "0.0.0",
            "10.20.30",
            "1.2.3-alpha.1",
            "1.2.3-rc-1",
            "1.2.3+build.5",
            "1.2.3-rc.1+build-7",
        ] {
            assert!(validate_version(version).is_ok(), "{version}");
        }
    }

    #[test]
    fn rejects_traversal_and_malformed_versions() {
        for version in [
            "3.7.2-a/../3.7.2/download/linux/amd64#/../../x",
            "",
            "3.7",
            "3.7.2.1",
            "03.7.2",
            "v3.7.2",
            "~> 3.7",
            "3.7.2-",
            "3.7.2+",
            "..",
            "../3.7.2",
            "3.7.2/..",
            "3.7.2-a/b",
            "3.7.2-a\\b",
            "3.7.2#x",
            "3.7.2?x",
            "3.7.2-a..b",
            "3.7.2+a..b",
            "3.7.2 ",
        ] {
            let error = validate_version(version).unwrap_err();
            assert!(
                matches!(error, InfrastructureError::Configuration(_)),
                "{version}: {error}"
            );
        }
    }

    #[test]
    fn install_directory_follows_terraform_cache_layout() {
        let installer = ProviderInstaller::new(PathBuf::from("/cache")).unwrap();
        let directory = installer
            .install_directory(&random_source(), "3.7.2")
            .unwrap();
        let platform = current_platform().unwrap();
        assert_eq!(
            directory,
            PathBuf::from(format!(
                "/cache/registry.terraform.io/hashicorp/random/3.7.2/{platform}"
            ))
        );
    }

    #[test]
    fn install_directory_rejects_unvalidated_components() {
        let installer = ProviderInstaller::new(PathBuf::from("/cache")).unwrap();
        assert!(
            installer
                .install_directory(
                    &random_source(),
                    "3.7.2-a/../3.7.2/download/linux/amd64#/../../x"
                )
                .is_err()
        );
        let constructed = ProviderSource {
            hostname: "registry.terraform.io".to_string(),
            namespace: "..".to_string(),
            type_name: "random".to_string(),
        };
        assert!(installer.install_directory(&constructed, "3.7.2").is_err());
    }

    #[test]
    fn containment_is_checked_component_wise() {
        let root = Path::new("/cache");
        assert!(ensure_within(root, Path::new("/cache/a/b")).is_ok());
        assert!(ensure_within(root, Path::new("/cache")).is_err());
        assert!(ensure_within(root, Path::new("/cache/../etc")).is_err());
        assert!(ensure_within(root, Path::new("/cache/a/../../etc")).is_err());
        assert!(ensure_within(root, Path::new("/cachet/a")).is_err());
        assert!(ensure_within(root, Path::new("/other/a")).is_err());
    }

    #[test]
    fn platform_displays_as_terraform_platform_string() {
        let platform = Platform {
            operating_system: "linux".to_string(),
            architecture: "amd64".to_string(),
        };
        assert_eq!(platform.to_string(), "linux_amd64");
    }

    #[test]
    fn verified_installation_round_trips() {
        let cache = tempfile::tempdir().unwrap();
        let directory = cache
            .path()
            .join("registry.terraform.io/hashicorp/random/3.7.2/x");
        let binary = write_installation(&directory, b"#!/bin/sh\n");
        assert_eq!(
            verify_installation(cache.path(), &directory, &release("3.7.2")).unwrap(),
            binary
        );
        assert!(verify_installation(cache.path(), &directory, &release("3.7.3")).is_err());
    }

    #[test]
    fn tampered_binary_is_not_accepted() {
        let cache = tempfile::tempdir().unwrap();
        let directory = cache.path().join("provider");
        let binary = write_installation(&directory, b"#!/bin/sh\n");
        std::fs::write(&binary, b"#!/bin/sh\nrm -rf /\n").unwrap();
        assert!(verify_installation(cache.path(), &directory, &release("3.7.2")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_executable_is_not_accepted() {
        let cache = tempfile::tempdir().unwrap();
        let directory = cache.path().join("provider");
        let binary = write_installation(&directory, b"#!/bin/sh\n");
        // Same contents, so only the link itself can be the reason.
        let target = cache.path().join("elsewhere");
        std::fs::write(&target, b"#!/bin/sh\n").unwrap();
        std::fs::remove_file(&binary).unwrap();
        std::os::unix::fs::symlink(&target, &binary).unwrap();
        let error = verify_installation(cache.path(), &directory, &release("3.7.2")).unwrap_err();
        assert!(error.to_string().contains("not a regular file"), "{error}");
    }

    /// Serve `body` once per connection to any GET, with a `content-length`
    /// header only when `declared_length` is set.
    async fn document_server(body: String, declared_length: Option<usize>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/document.json", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut chunk).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                }
                let length = declared_length
                    .map(|length| format!("content-length: {length}\r\n"))
                    .unwrap_or_default();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n{length}\
                     connection: close\r\n\r\n{body}"
                );
                // The client may hang up early on an oversized declaration.
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        url
    }

    #[tokio::test]
    async fn registry_requests_refuse_plaintext_http() {
        let installer = ProviderInstaller::new(PathBuf::from("/cache")).unwrap();
        let small = r#"{"providers.v1": "/v1/providers/"}"#.to_string();
        let url = document_server(small.clone(), Some(small.len())).await;
        let error = installer
            .get_document::<ServiceDiscovery>(&url, "document")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("failed"), "{error}");
    }

    #[tokio::test]
    async fn registry_documents_are_bounded() {
        let installer = ProviderInstaller::with_transport(
            PathBuf::from("/cache"),
            RegistryTransport::PlaintextForTests,
        )
        .unwrap();
        let small = r#"{"providers.v1": "/v1/providers/"}"#.to_string();
        let url = document_server(small.clone(), Some(small.len())).await;
        let discovery: ServiceDiscovery = installer.get_document(&url, "document").await.unwrap();
        assert_eq!(discovery.providers_v1.as_deref(), Some("/v1/providers/"));

        let limit = usize::try_from(MAXIMUM_REGISTRY_DOCUMENT_BYTES).unwrap();
        let oversized = format!(r#"{{"providers.v1": "{}"}}"#, "x".repeat(limit));
        for declared_length in [Some(oversized.len()), None] {
            let url = document_server(oversized.clone(), declared_length).await;
            let error = installer
                .get_document::<ServiceDiscovery>(&url, "document")
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("exceeds the {limit}-byte limit")),
                "{declared_length:?}: {error}"
            );
        }
    }

    #[test]
    fn directory_without_manifest_is_not_accepted() {
        let cache = tempfile::tempdir().unwrap();
        let directory = cache.path().join("provider");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("terraform-provider-random-evil"), b"evil").unwrap();
        assert!(verify_installation(cache.path(), &directory, &release("3.7.2")).is_err());
    }

    #[test]
    fn manifest_naming_a_path_outside_the_directory_is_not_accepted() {
        let cache = tempfile::tempdir().unwrap();
        let directory = cache.path().join("provider");
        write_installation(&directory, b"#!/bin/sh\n");
        let outside = cache.path().join("terraform-provider-random");
        std::fs::write(&outside, b"evil").unwrap();
        write_manifest(
            &directory,
            &InstallationManifest {
                filename: "../terraform-provider-random".to_string(),
                sha256: hash_file(&outside).unwrap(),
                archive_sha256: "00".repeat(32),
                source: random_source().to_string(),
                version: "3.7.2".to_string(),
            },
        )
        .unwrap();
        assert!(verify_installation(cache.path(), &directory, &release("3.7.2")).is_err());
    }

    #[test]
    fn extracts_zip_and_records_provider_executable() {
        let archive = build_zip(&[(PROVIDER_FILE, b"#!/bin/sh\n"), ("LICENSE", b"MPL")]);
        let directory = tempfile::tempdir().unwrap();
        let extracted = extract(&archive, directory.path(), ExtractionLimits::default()).unwrap();
        assert_eq!(extracted.filename, PROVIDER_FILE);
        assert_eq!(
            extracted.sha256,
            hash_file(&directory.path().join(PROVIDER_FILE)).unwrap()
        );
        let missing = build_zip(&[("terraform-provider-local_v2.9.1_x5", b"x")]);
        let other = tempfile::tempdir().unwrap();
        assert!(extract(&missing, other.path(), ExtractionLimits::default()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn extraction_normalises_permissions_and_strips_special_bits() {
        use std::os::unix::fs::PermissionsExt;
        let mut archive = build_zip(&[(PROVIDER_FILE, b"#!/bin/sh\n"), ("LICENSE", b"MPL")]);
        force_unix_mode(&mut archive, 0o4777);
        let mut reader = zip::ZipArchive::new(std::io::Cursor::new(archive.as_slice())).unwrap();
        let mode = reader.by_index(0).unwrap().unix_mode().unwrap();
        assert_eq!(mode & 0o7777, 0o4777, "fixture must carry the setuid bit");

        let directory = tempfile::tempdir().unwrap();
        extract(&archive, directory.path(), ExtractionLimits::default()).unwrap();
        let mode_of = |name: &str| {
            std::fs::metadata(directory.path().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777
        };
        assert_eq!(mode_of(PROVIDER_FILE), 0o755);
        assert_eq!(mode_of("LICENSE"), 0o644);
    }

    #[test]
    fn oversized_entry_is_rejected() {
        let archive = build_zip(&[(PROVIDER_FILE, &[b'x'; 100])]);
        let directory = tempfile::tempdir().unwrap();
        let limits = ExtractionLimits {
            entry_bytes: 10,
            total_bytes: 1_000,
        };
        let error = extract(&archive, directory.path(), limits).unwrap_err();
        assert!(error.to_string().contains("extraction limit"), "{error}");
    }

    #[test]
    fn oversized_total_is_rejected() {
        let archive = build_zip(&[(PROVIDER_FILE, &[b'x'; 8]), ("LICENSE", &[b'y'; 8])]);
        let directory = tempfile::tempdir().unwrap();
        let limits = ExtractionLimits {
            entry_bytes: 10,
            total_bytes: 12,
        };
        let error = extract(&archive, directory.path(), limits).unwrap_err();
        assert!(error.to_string().contains("extraction limit"), "{error}");
    }

    #[test]
    fn installs_from_archive_into_verified_cache_directory() {
        let cache = tempfile::tempdir().unwrap();
        let installer = ProviderInstaller::new(cache.path().to_path_buf()).unwrap();
        let install_directory = installer
            .install_directory(&random_source(), "3.7.2")
            .unwrap();
        // A stale, unverified directory is replaced.
        std::fs::create_dir_all(&install_directory).unwrap();
        std::fs::write(
            install_directory.join("terraform-provider-random-evil"),
            b"evil",
        )
        .unwrap();

        let staging = StagingDirectory::create(cache.path()).unwrap();
        let archive_path = staging.path().join(ARCHIVE_FILE_NAME);
        std::fs::write(&archive_path, build_zip(&[(PROVIDER_FILE, b"#!/bin/sh\n")])).unwrap();
        let request = InstallationRequest {
            release: release("3.7.2"),
            cache_directory: cache.path().to_path_buf(),
            install_directory: install_directory.clone(),
            archive_path: archive_path.clone(),
            extraction_directory: staging.path().join(EXTRACTED_DIRECTORY_NAME),
            archive_sha256: hash_file(&archive_path).unwrap(),
            limits: ExtractionLimits::default(),
        };
        let installation = install_from_archive(&request).unwrap();
        assert_eq!(installation.archive_sha256, request.archive_sha256);
        let binary = installation.binary;
        assert_eq!(binary, install_directory.join(PROVIDER_FILE));
        assert!(
            !install_directory
                .join("terraform-provider-random-evil")
                .exists()
        );
        assert!(install_directory.join(MANIFEST_FILE_NAME).is_file());
        assert_eq!(
            verify_installation(cache.path(), &install_directory, &release("3.7.2")).unwrap(),
            binary
        );

        let staging_path = staging.path().to_path_buf();
        drop(staging);
        assert!(!staging_path.exists());
    }
}
