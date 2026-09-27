//! Provider installation from a Terraform provider registry.
//!
//! Resolves `namespace/type` (or `hostname/namespace/type`) plus an exact
//! version to a local executable, downloading and caching it on first use.
//! The cache layout matches Terraform's plugin cache
//! (`<host>/<namespace>/<type>/<version>/<os>_<arch>/`), so an existing
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

use crate::error::{InfraError, Result};

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
    /// Returns [`InfraError::Config`] for malformed addresses.
    pub fn parse(source: &str) -> Result<Self> {
        let parts: Vec<&str> = source.trim().split('/').collect();
        let (hostname, namespace, type_name) = match parts.as_slice() {
            [ns, ty] => (DEFAULT_REGISTRY, *ns, *ty),
            [host, ns, ty] => (*host, *ns, *ty),
            _ => {
                return Err(InfraError::config(format!(
                    "invalid provider source '{source}'; expected namespace/type or hostname/namespace/type"
                )));
            }
        };
        if [hostname, namespace, type_name]
            .iter()
            .any(|p| p.is_empty())
        {
            return Err(InfraError::config(format!(
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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.hostname, self.namespace, self.type_name)
    }
}

/// Terraform platform string for the running host, e.g. `linux_amd64`.
///
/// # Errors
///
/// Returns [`InfraError::Install`] on platforms Terraform does not publish
/// providers for.
pub fn current_platform() -> Result<(String, String)> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other @ ("linux" | "windows" | "freebsd" | "openbsd") => other,
        other => return Err(InfraError::install(format!("unsupported OS '{other}'"))),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        "arm" => "arm",
        other => {
            return Err(InfraError::install(format!(
                "unsupported architecture '{other}'"
            )));
        }
    };
    Ok((os.to_string(), arch.to_string()))
}

/// Default plugin cache directory.
#[must_use]
pub fn default_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("TF_PLUGIN_CACHE_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("cuenv")
        .join("infra")
        .join("providers")
}

/// Downloads and caches provider binaries.
#[derive(Debug, Clone)]
pub struct ProviderInstaller {
    client: reqwest::Client,
    cache_dir: PathBuf,
}

#[derive(Debug, Deserialize)]
struct ServiceDiscovery {
    #[serde(rename = "providers.v1")]
    providers_v1: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DownloadInfo {
    download_url: String,
    shasum: String,
    filename: String,
}

impl ProviderInstaller {
    /// Create an installer using `cache_dir`.
    ///
    /// # Errors
    ///
    /// Returns [`InfraError::Install`] if the HTTP client cannot be built.
    pub fn new(cache_dir: PathBuf) -> Result<Self> {
        crate::ensure_rustls_crypto_provider();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| InfraError::install(format!("failed to build HTTP client: {e}")))?;
        Ok(Self { client, cache_dir })
    }

    /// Directory a provider version is (or will be) installed into.
    ///
    /// # Errors
    ///
    /// Returns [`InfraError::Install`] on unsupported platforms.
    pub fn install_dir(&self, source: &ProviderSource, version: &str) -> Result<PathBuf> {
        let (os, arch) = current_platform()?;
        Ok(self
            .cache_dir
            .join(&source.hostname)
            .join(&source.namespace)
            .join(&source.type_name)
            .join(version)
            .join(format!("{os}_{arch}")))
    }

    /// Return the provider executable, installing it if not cached.
    ///
    /// # Errors
    ///
    /// Returns [`InfraError::Install`] if the registry lookup, download,
    /// checksum verification or extraction fails.
    pub async fn ensure(&self, source: &ProviderSource, version: &str) -> Result<PathBuf> {
        let dir = self.install_dir(source, version)?;
        if let Some(binary) = find_provider_binary(&dir, &source.type_name) {
            return Ok(binary);
        }

        let (os, arch) = current_platform()?;
        let base = self.providers_base_url(&source.hostname).await?;
        let info_url = format!(
            "{base}{}/{}/{version}/download/{os}/{arch}",
            source.namespace, source.type_name
        );
        let info: DownloadInfo = self
            .get(&info_url)
            .await?
            .json()
            .await
            .map_err(|e| InfraError::install(format!("invalid download metadata: {e}")))?;

        tracing::info!(provider = %source, version, file = %info.filename, "downloading provider");
        let archive = self
            .get(&info.download_url)
            .await?
            .bytes()
            .await
            .map_err(|e| InfraError::install(format!("download failed: {e}")))?;

        let actual = hex::encode(Sha256::digest(&archive));
        if !actual.eq_ignore_ascii_case(info.shasum.trim()) {
            return Err(InfraError::install(format!(
                "checksum mismatch for {}: registry says {}, downloaded {actual}",
                info.filename, info.shasum
            )));
        }

        let parent = dir
            .parent()
            .ok_or_else(|| InfraError::install("invalid provider cache path"))?
            .to_path_buf();
        let staging = parent.join(format!(".staging-{}", uuid::Uuid::new_v4()));
        let final_dir = dir.clone();
        tokio::task::spawn_blocking(move || {
            extract_zip(&archive, &staging)?;
            if final_dir.exists() {
                std::fs::remove_dir_all(&final_dir)
                    .map_err(|e| InfraError::io("clear provider dir", e))?;
            }
            std::fs::rename(&staging, &final_dir).map_err(|e| InfraError::io("install provider", e))
        })
        .await
        .map_err(|e| InfraError::install(format!("extraction task failed: {e}")))??;

        find_provider_binary(&dir, &source.type_name).ok_or_else(|| {
            InfraError::install(format!(
                "archive {} did not contain terraform-provider-{}",
                info.filename, source.type_name
            ))
        })
    }

    async fn providers_base_url(&self, hostname: &str) -> Result<String> {
        let discovery_url = format!("https://{hostname}/.well-known/terraform.json");
        let discovery: ServiceDiscovery =
            self.get(&discovery_url).await?.json().await.map_err(|e| {
                InfraError::install(format!("invalid service discovery document: {e}"))
            })?;
        let path = discovery.providers_v1.ok_or_else(|| {
            InfraError::install(format!("{hostname} does not offer a provider registry"))
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
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| InfraError::install(format!("GET {url} failed: {e}")))?;
        if !response.status().is_success() {
            return Err(InfraError::install(format!(
                "GET {url} returned HTTP {}",
                response.status()
            )));
        }
        Ok(response)
    }
}

/// Find `terraform-provider-<type>*` in `dir`.
fn find_provider_binary(dir: &Path, type_name: &str) -> Option<PathBuf> {
    let prefix = format!("terraform-provider-{type_name}");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix))
        })
}

fn extract_zip(archive: &[u8], dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest).map_err(|e| InfraError::io("create provider dir", e))?;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))
        .map_err(|e| InfraError::install(format!("invalid provider archive: {e}")))?;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| InfraError::install(format!("invalid archive entry: {e}")))?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(InfraError::install(format!(
                "archive entry escapes destination: {}",
                entry.name()
            )));
        };
        let target = dest.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| InfraError::io("create dir", e))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| InfraError::io("create dir", e))?;
        }
        let mut contents = Vec::new();
        entry
            .read_to_end(&mut contents)
            .map_err(|e| InfraError::io("read archive entry", e))?;
        std::fs::write(&target, contents).map_err(|e| InfraError::io("write provider file", e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = entry.unix_mode().unwrap_or(0o644) | 0o755;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))
                .map_err(|e| InfraError::io("chmod provider", e))?;
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
        assert!(ProviderSource::parse("a//b").is_err());
    }

    #[test]
    fn install_dir_follows_terraform_cache_layout() {
        let installer = ProviderInstaller::new(PathBuf::from("/cache")).unwrap();
        let dir = installer
            .install_dir(&ProviderSource::parse("hashicorp/random").unwrap(), "3.7.2")
            .unwrap();
        let (os, arch) = current_platform().unwrap();
        assert_eq!(
            dir,
            PathBuf::from(format!(
                "/cache/registry.terraform.io/hashicorp/random/3.7.2/{os}_{arch}"
            ))
        );
    }

    #[test]
    fn extracts_zip_and_finds_binary() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buf);
            writer
                .start_file::<_, ()>(
                    "terraform-provider-random_v3.7.2_x5",
                    zip::write::FileOptions::default(),
                )
                .unwrap();
            writer.write_all(b"#!/bin/sh\n").unwrap();
            writer.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        extract_zip(buf.get_ref(), dir.path()).unwrap();
        let binary = find_provider_binary(dir.path(), "random").unwrap();
        assert!(binary.ends_with("terraform-provider-random_v3.7.2_x5"));
        assert!(find_provider_binary(dir.path(), "local").is_none());
    }
}
