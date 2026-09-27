//! Provider installation from a Terraform provider registry.
//!
//! Resolves `namespace/type` (or `hostname/namespace/type`) plus an exact
//! version to a local executable, downloading and caching it on first use.
//! The cache layout matches Terraform's plugin cache
//! (`<host>/<namespace>/<type>/<version>/<operating_system>_<architecture>/`), so an existing
//! `TF_PLUGIN_CACHE_DIR` is reused as-is.
//!
//! Integrity: the archive's SHA-256 is checked against the checksum the
//! registry reports. The registry's GPG signature over `SHA256SUMS` is not
//! verified yet.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{InfrastructureError, Result};

/// Default provider registry host.
pub const DEFAULT_REGISTRY: &str = "registry.terraform.io";

/// Fully-qualified provider source address.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderSource {
    /// Registry hostname.
    pub hostname: String,
    /// Registry namespace, e.g. `hashicorp`.
    pub namespace: String,
    /// Provider type, e.g. `random`.
    pub type_name: String,
}

impl ProviderSource {
    /// Parse `namespace/type` or `hostname/namespace/type`.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for malformed addresses.
    pub fn parse(source: &str) -> Result<Self> {
        let parts: Vec<&str> = source.trim().split('/').collect();
        let (hostname, namespace, type_name) = match parts.as_slice() {
            [namespace, type_name] => (DEFAULT_REGISTRY, *namespace, *type_name),
            [host, namespace, type_name] => (*host, *namespace, *type_name),
            _ => {
                return Err(InfrastructureError::configuration(format!(
                    "invalid provider source '{source}'; expected namespace/type or hostname/namespace/type"
                )));
            }
        };
        if [hostname, namespace, type_name]
            .iter()
            .any(|part| part.is_empty())
        {
            return Err(InfrastructureError::configuration(format!(
                "invalid provider source '{source}'"
            )));
        }
        Ok(Self {
            hostname: hostname.to_ascii_lowercase(),
            namespace: namespace.to_ascii_lowercase(),
            type_name: type_name.to_ascii_lowercase(),
        })
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

/// Terraform platform string for the running host, e.g. `linux_amd64`.
///
/// # Errors
///
/// Returns [`InfrastructureError::Install`] on platforms Terraform does not publish
/// providers for.
pub fn current_platform() -> Result<(String, String)> {
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
    Ok((operating_system.to_string(), architecture.to_string()))
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

#[derive(Debug, Deserialize)]
struct ServiceDiscovery {
    #[serde(rename = "providers.v1")]
    providers_v1: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DownloadInformation {
    download_url: String,
    shasum: String,
    filename: String,
}

impl ProviderInstaller {
    /// Create an installer using `cache_directory`.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Install`] if the HTTP client cannot be built.
    pub fn new(cache_directory: PathBuf) -> Result<Self> {
        crate::ensure_rustls_cryptography_provider();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
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
    /// Returns [`InfrastructureError::Install`] on unsupported platforms.
    pub fn install_directory(&self, source: &ProviderSource, version: &str) -> Result<PathBuf> {
        let (operating_system, architecture) = current_platform()?;
        Ok(self
            .cache_directory
            .join(&source.hostname)
            .join(&source.namespace)
            .join(&source.type_name)
            .join(version)
            .join(format!("{operating_system}_{architecture}")))
    }

    /// Return the provider executable, installing it if not cached.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Install`] if the registry lookup, download,
    /// checksum verification or extraction fails.
    pub async fn ensure(&self, source: &ProviderSource, version: &str) -> Result<PathBuf> {
        let directory = self.install_directory(source, version)?;
        if let Some(binary) = find_provider_binary(&directory, &source.type_name) {
            return Ok(binary);
        }

        let (operating_system, architecture) = current_platform()?;
        let base = self.providers_base_url(&source.hostname).await?;
        let download_metadata_url = format!(
            "{base}{}/{}/{version}/download/{operating_system}/{architecture}",
            source.namespace, source.type_name
        );
        let download: DownloadInformation = self
            .get(&download_metadata_url)
            .await?
            .json()
            .await
            .map_err(|error| {
                InfrastructureError::install(format!("invalid download metadata: {error}"))
            })?;

        tracing::info!(provider = %source, version, file = %download.filename, "downloading provider");
        let archive = self
            .get(&download.download_url)
            .await?
            .bytes()
            .await
            .map_err(|error| InfrastructureError::install(format!("download failed: {error}")))?;

        let actual = hex::encode(Sha256::digest(&archive));
        if !actual.eq_ignore_ascii_case(download.shasum.trim()) {
            return Err(InfrastructureError::install(format!(
                "checksum mismatch for {}: registry says {}, downloaded {actual}",
                download.filename, download.shasum
            )));
        }

        let parent = directory
            .parent()
            .ok_or_else(|| InfrastructureError::install("invalid provider cache path"))?
            .to_path_buf();
        let staging = parent.join(format!(".staging-{}", uuid::Uuid::new_v4()));
        let final_directory = directory.clone();
        tokio::task::spawn_blocking(move || {
            extract_zip(&archive, &staging)?;
            if final_directory.exists() {
                std::fs::remove_dir_all(&final_directory)
                    .map_err(|error| InfrastructureError::io("clear provider directory", error))?;
            }
            std::fs::rename(&staging, &final_directory)
                .map_err(|error| InfrastructureError::io("install provider", error))
        })
        .await
        .map_err(|error| {
            InfrastructureError::install(format!("extraction task failed: {error}"))
        })??;

        find_provider_binary(&directory, &source.type_name).ok_or_else(|| {
            InfrastructureError::install(format!(
                "archive {} did not contain terraform-provider-{}",
                download.filename, source.type_name
            ))
        })
    }

    async fn providers_base_url(&self, hostname: &str) -> Result<String> {
        let discovery_url = format!("https://{hostname}/.well-known/terraform.json");
        let discovery: ServiceDiscovery =
            self.get(&discovery_url)
                .await?
                .json()
                .await
                .map_err(|error| {
                    InfrastructureError::install(format!(
                        "invalid service discovery document: {error}"
                    ))
                })?;
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
}

/// Find `terraform-provider-<type>*` in `directory`.
fn find_provider_binary(directory: &Path, type_name: &str) -> Option<PathBuf> {
    let prefix = format!("terraform-provider-{type_name}");
    std::fs::read_dir(directory)
        .ok()?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|file_name| file_name.to_str())
                    .is_some_and(|file_name| file_name.starts_with(&prefix))
        })
}

fn extract_zip(archive: &[u8], destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination)
        .map_err(|error| InfrastructureError::io("create provider directory", error))?;
    let mut zip_archive = zip::ZipArchive::new(std::io::Cursor::new(archive)).map_err(|error| {
        InfrastructureError::install(format!("invalid provider archive: {error}"))
    })?;
    for index in 0..zip_archive.len() {
        let mut entry = zip_archive.by_index(index).map_err(|error| {
            InfrastructureError::install(format!("invalid archive entry: {error}"))
        })?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(InfrastructureError::install(format!(
                "archive entry escapes destination: {}",
                entry.name()
            )));
        };
        let target = destination.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|error| InfrastructureError::io("create directory", error))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| InfrastructureError::io("create directory", error))?;
        }
        let mut contents = Vec::new();
        entry
            .read_to_end(&mut contents)
            .map_err(|error| InfrastructureError::io("read archive entry", error))?;
        std::fs::write(&target, contents)
            .map_err(|error| InfrastructureError::io("write provider file", error))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = entry.unix_mode().unwrap_or(0o644) | 0o755;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))
                .map_err(|error| InfrastructureError::io("set provider permissions", error))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_short_and_qualified_sources() {
        let short = ProviderSource::parse("hashicorp/random").unwrap();
        assert_eq!(short.to_string(), "registry.terraform.io/hashicorp/random");
        let qualified =
            ProviderSource::parse("registry.opentofu.org/Cloudflare/Cloudflare").unwrap();
        assert_eq!(qualified.hostname, "registry.opentofu.org");
        assert_eq!(qualified.namespace, "cloudflare");
        assert!(ProviderSource::parse("random").is_err());
        assert!(ProviderSource::parse("hashicorp//random").is_err());
    }

    #[test]
    fn install_directory_follows_terraform_cache_layout() {
        let installer = ProviderInstaller::new(PathBuf::from("/cache")).unwrap();
        let directory = installer
            .install_directory(&ProviderSource::parse("hashicorp/random").unwrap(), "3.7.2")
            .unwrap();
        let (operating_system, architecture) = current_platform().unwrap();
        assert_eq!(
            directory,
            PathBuf::from(format!(
                "/cache/registry.terraform.io/hashicorp/random/3.7.2/{operating_system}_{architecture}"
            ))
        );
    }

    #[test]
    fn extracts_zip_and_finds_binary() {
        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            writer
                .start_file::<_, ()>(
                    "terraform-provider-random_v3.7.2_x5",
                    zip::write::FileOptions::default(),
                )
                .unwrap();
            writer.write_all(b"#!/bin/sh\n").unwrap();
            writer.finish().unwrap();
        }
        let directory = tempfile::tempdir().unwrap();
        extract_zip(buffer.get_ref(), directory.path()).unwrap();
        let binary = find_provider_binary(directory.path(), "random").unwrap();
        assert!(binary.ends_with("terraform-provider-random_v3.7.2_x5"));
        assert!(find_provider_binary(directory.path(), "local").is_none());
    }
}
