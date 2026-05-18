//! OCI registry client for resolving and pulling images.
//!
//! Uses `oci-distribution` for registry operations.

use oci_distribution::client::{ClientConfig, ClientProtocol};
use oci_distribution::manifest::{ImageIndexEntry, OciDescriptor, OciManifest};
use oci_distribution::secrets::RegistryAuth;
use oci_distribution::{Client, Reference};
use sha2::{Digest, Sha256};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::OnceLock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, info, trace, warn};

use crate::cache::OciCache;
use crate::platform::Platform;
use crate::{Error, Result};

/// OCI registry client for image resolution and blob pulling.
pub struct OciClient {
    client: OnceLock<std::result::Result<Client, String>>,
}

impl Default for OciClient {
    fn default() -> Self {
        Self::new()
    }
}

impl OciClient {
    fn create_client() -> std::result::Result<Client, String> {
        let config = ClientConfig {
            protocol: ClientProtocol::Https,
            ..Default::default()
        };

        catch_unwind(AssertUnwindSafe(|| Client::new(config))).map_err(|_| {
            "Failed to initialize OCI client because system proxy discovery panicked".to_string()
        })
    }

    fn client(&self) -> Result<&Client> {
        match self.client.get_or_init(Self::create_client) {
            Ok(client) => Ok(client),
            Err(err) => Err(Error::Oci(err.clone())),
        }
    }

    /// Create a new OCI client with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: OnceLock::new(),
        }
    }

    /// Resolve an image reference to a digest for a specific platform.
    ///
    /// Returns the manifest digest for the platform-specific image. For
    /// multi-arch image indexes, walks the entries and selects the one whose
    /// `platform.os`/`platform.architecture` match the requested platform.
    /// For single-arch manifests, returns the layers without further filtering.
    pub async fn resolve_digest(&self, image: &str, platform: &Platform) -> Result<ResolvedImage> {
        let reference = parse_reference(image)?;
        info!(%image, %platform, "Resolving image digest");

        let auth = self.get_auth(&reference);
        let client = self.client()?;

        // Pull the raw manifest so we can dispatch on image-index vs image-manifest
        // without relying on the client's default (host-based) platform resolver.
        let (manifest, top_digest) = client
            .pull_manifest(&reference, &auth)
            .await
            .map_err(|e| Error::Oci(e.to_string()))?;

        match manifest {
            OciManifest::Image(image_manifest) => {
                trace!(?image_manifest, "Got single-arch image manifest");
                let layer_descriptors: Vec<OciDescriptor> = image_manifest.layers.clone();
                let layers: Vec<String> =
                    layer_descriptors.iter().map(|l| l.digest.clone()).collect();

                debug!(
                    %image,
                    %platform,
                    digest = %top_digest,
                    layer_count = layers.len(),
                    "Resolved single-arch image"
                );

                Ok(ResolvedImage {
                    reference,
                    digest: top_digest,
                    layers,
                    layer_descriptors,
                })
            }
            OciManifest::ImageIndex(index) => {
                debug!(
                    %image,
                    entries = index.manifests.len(),
                    "Got multi-arch image index"
                );

                let expected_oci_platform = platform.to_oci_platform();
                let (expected_os, expected_arch) = expected_oci_platform
                    .split_once('/')
                    .unwrap_or((platform.os.as_str(), platform.arch.as_str()));

                let entry = select_index_entry(&index.manifests, expected_os, expected_arch)
                    .ok_or_else(|| {
                        let available = available_platforms(&index.manifests);
                        warn!(
                            %image,
                            requested = %expected_oci_platform,
                            available = %available,
                            "Requested platform not found in image index"
                        );
                        Error::platform_not_available(
                            format!("{image} (available: {available})"),
                            expected_oci_platform.clone(),
                        )
                    })?;

                let child_reference = Reference::with_digest(
                    reference.registry().to_string(),
                    reference.repository().to_string(),
                    entry.digest.clone(),
                );

                let (child_manifest, child_digest) = client
                    .pull_manifest(&child_reference, &auth)
                    .await
                    .map_err(|e| Error::Oci(e.to_string()))?;

                let image_manifest = match child_manifest {
                    OciManifest::Image(manifest) => manifest,
                    OciManifest::ImageIndex(_) => {
                        return Err(Error::Oci(format!(
                            "Child manifest for platform '{expected_oci_platform}' in '{image}' was unexpectedly an image index"
                        )));
                    }
                };

                let layer_descriptors: Vec<OciDescriptor> = image_manifest.layers.clone();
                let layers: Vec<String> =
                    layer_descriptors.iter().map(|l| l.digest.clone()).collect();

                debug!(
                    %image,
                    %platform,
                    digest = %child_digest,
                    layer_count = layers.len(),
                    "Resolved multi-arch image"
                );

                Ok(ResolvedImage {
                    reference,
                    digest: child_digest,
                    layers,
                    layer_descriptors,
                })
            }
        }
    }

    /// Pull a blob (layer) to a file using its descriptor.
    ///
    /// After downloading, the blob's SHA256 digest is verified against the
    /// expected digest from the descriptor. If verification fails, the file
    /// is deleted and an error is returned.
    pub async fn pull_blob_by_descriptor(
        &self,
        reference: &Reference,
        descriptor: &OciDescriptor,
        dest: &Path,
    ) -> Result<()> {
        debug!(digest = %descriptor.digest, ?dest, "Pulling blob");

        // Create parent directories
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Pull the blob
        let mut file = tokio::fs::File::create(dest).await?;
        let client = self.client()?;

        client
            .pull_blob(reference, descriptor, &mut file)
            .await
            .map_err(|e| Error::blob_pull_failed(&descriptor.digest, e.to_string()))?;

        file.flush().await?;

        // Verify the digest matches
        let computed_digest = compute_file_digest(dest).await?;
        if computed_digest != descriptor.digest {
            // Remove the corrupted/invalid file
            tokio::fs::remove_file(dest).await.ok();
            return Err(Error::digest_mismatch(&descriptor.digest, &computed_digest));
        }

        debug!(digest = %descriptor.digest, ?dest, "Pulled and verified blob");
        Ok(())
    }

    /// Pull all layers for an image and cache them.
    pub async fn pull_layers(
        &self,
        resolved: &ResolvedImage,
        cache: &OciCache,
    ) -> Result<Vec<std::path::PathBuf>> {
        let mut paths = Vec::new();

        for descriptor in &resolved.layer_descriptors {
            let path = cache.blob_path(&descriptor.digest);

            if path.exists() {
                trace!(digest = %descriptor.digest, "Layer already cached");
            } else {
                self.pull_blob_by_descriptor(&resolved.reference, descriptor, &path)
                    .await?;
            }

            paths.push(path);
        }

        Ok(paths)
    }

    /// Get authentication for a registry.
    ///
    /// Currently returns anonymous auth. Can be extended to support:
    /// - Docker config credentials
    /// - Environment variables
    /// - Keychain integration
    fn get_auth(&self, reference: &Reference) -> RegistryAuth {
        // Check for GHCR token in environment
        if reference.registry() == "ghcr.io" {
            if let Ok(token) = std::env::var("GITHUB_TOKEN") {
                return RegistryAuth::Basic("".to_string(), token);
            }
            if let Ok(token) = std::env::var("GH_TOKEN") {
                return RegistryAuth::Basic("".to_string(), token);
            }
        }

        RegistryAuth::Anonymous
    }
}

/// A resolved OCI image with digest and layer information.
#[derive(Debug, Clone)]
pub struct ResolvedImage {
    /// The parsed reference.
    pub reference: Reference,
    /// Content-addressable digest of the manifest.
    pub digest: String,
    /// Layer digests (for reference).
    pub layers: Vec<String>,
    /// Layer descriptors (for pulling blobs).
    pub layer_descriptors: Vec<OciDescriptor>,
}

/// Parse an image reference string.
fn parse_reference(image: &str) -> Result<Reference> {
    image
        .parse()
        .map_err(|e: oci_distribution::ParseError| Error::invalid_reference(image, e.to_string()))
}

/// Select the manifest entry matching the requested OS and architecture.
///
/// `expected_os` and `expected_arch` use OCI/GOOS-GOARCH conventions
/// (e.g., `linux`/`amd64`, `darwin`/`arm64`).
fn select_index_entry<'a>(
    entries: &'a [ImageIndexEntry],
    expected_os: &str,
    expected_arch: &str,
) -> Option<&'a ImageIndexEntry> {
    entries.iter().find(|entry| {
        entry.platform.as_ref().is_some_and(|p| {
            p.os.eq_ignore_ascii_case(expected_os)
                && p.architecture.eq_ignore_ascii_case(expected_arch)
        })
    })
}

/// Render the set of platforms available in an image index for error messages.
///
/// Filters out attestation manifests, which Docker Hub and GHCR emit with
/// `platform: { os: "unknown", architecture: "unknown" }`. Those entries are
/// never selectable by [`select_index_entry`], so showing them in error
/// messages is pure noise.
fn available_platforms(entries: &[ImageIndexEntry]) -> String {
    let mut rendered: Vec<String> = entries
        .iter()
        .filter_map(|entry| entry.platform.as_ref())
        .filter(|p| {
            !(p.os.eq_ignore_ascii_case("unknown")
                && p.architecture.eq_ignore_ascii_case("unknown"))
        })
        .map(|p| format!("{}/{}", p.os, p.architecture))
        .collect();
    rendered.sort();
    rendered.dedup();
    if rendered.is_empty() {
        "<none>".to_string()
    } else {
        rendered.join(", ")
    }
}

/// Compute the SHA256 digest of a file.
///
/// Returns the digest in OCI format: `sha256:<hex>`.
async fn compute_file_digest(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 8192];

    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    Ok(format!("sha256:{:x}", hasher.finalize()))
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ==========================================================================
    // parse_reference tests
    // ==========================================================================

    #[test]
    fn test_parse_reference() {
        let r = parse_reference("ghcr.io/distroless/static:nonroot").unwrap();
        assert_eq!(r.registry(), "ghcr.io");
        assert_eq!(r.repository(), "distroless/static");
        assert_eq!(r.tag(), Some("nonroot"));
    }

    #[test]
    fn test_parse_reference_with_digest() {
        // Digest must be valid SHA256 (64 hex chars)
        let digest = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let r = parse_reference(&format!("nginx@{}", digest)).unwrap();
        assert_eq!(r.repository(), "library/nginx");
    }

    #[test]
    fn test_parse_reference_invalid() {
        let r = parse_reference("not a valid reference!!!");
        assert!(r.is_err());
    }

    #[test]
    fn test_parse_reference_docker_hub_short() {
        let r = parse_reference("nginx:latest").unwrap();
        assert_eq!(r.registry(), "docker.io");
        assert_eq!(r.repository(), "library/nginx");
        assert_eq!(r.tag(), Some("latest"));
    }

    #[test]
    fn test_parse_reference_with_port() {
        let r = parse_reference("localhost:5000/myimage:v1").unwrap();
        assert_eq!(r.registry(), "localhost:5000");
        assert_eq!(r.repository(), "myimage");
        assert_eq!(r.tag(), Some("v1"));
    }

    #[test]
    fn test_parse_reference_no_tag() {
        // Without a tag, it should default to "latest"
        let r = parse_reference("nginx").unwrap();
        assert_eq!(r.repository(), "library/nginx");
    }

    #[test]
    fn test_parse_reference_private_registry() {
        let r = parse_reference("registry.example.com/org/repo:v2.0.0").unwrap();
        assert_eq!(r.registry(), "registry.example.com");
        assert_eq!(r.repository(), "org/repo");
        assert_eq!(r.tag(), Some("v2.0.0"));
    }

    // ==========================================================================
    // compute_file_digest tests
    // ==========================================================================

    #[tokio::test]
    async fn test_compute_file_digest() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.txt");

        // Write known content - empty file has a known SHA256
        std::fs::write(&file_path, b"").unwrap();
        let digest = compute_file_digest(&file_path).await.unwrap();
        // SHA256 of empty string
        assert_eq!(
            digest,
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        // Write "hello" and verify
        std::fs::write(&file_path, b"hello").unwrap();
        let digest = compute_file_digest(&file_path).await.unwrap();
        // SHA256 of "hello"
        assert_eq!(
            digest,
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[tokio::test]
    async fn test_compute_file_digest_larger_content() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("large.bin");

        // Write content larger than the buffer size (8192 bytes)
        let content: Vec<u8> = (0..20000).map(|i| (i % 256) as u8).collect();
        std::fs::write(&file_path, &content).unwrap();

        let digest = compute_file_digest(&file_path).await.unwrap();
        assert!(digest.starts_with("sha256:"));
        assert_eq!(digest.len(), 7 + 64); // "sha256:" + 64 hex chars
    }

    #[tokio::test]
    async fn test_compute_file_digest_nonexistent() {
        let result = compute_file_digest(std::path::Path::new("/nonexistent/path")).await;
        assert!(result.is_err());
    }

    // ==========================================================================
    // Error tests
    // ==========================================================================

    #[test]
    fn test_digest_mismatch_error() {
        let err = Error::digest_mismatch("sha256:expected", "sha256:actual");
        let msg = err.to_string();
        assert!(msg.contains("expected"));
        assert!(msg.contains("actual"));
    }

    #[test]
    fn test_invalid_reference_error() {
        let err = Error::invalid_reference("bad image", "parse error");
        let msg = err.to_string();
        assert!(msg.contains("bad image") || msg.contains("parse error"));
    }

    #[test]
    fn test_blob_pull_failed_error() {
        let err = Error::blob_pull_failed("sha256:abc123", "connection refused");
        let msg = err.to_string();
        assert!(msg.contains("sha256:abc123") || msg.contains("connection refused"));
    }

    // ==========================================================================
    // OciClient tests
    // ==========================================================================

    #[test]
    fn test_oci_client_new() {
        let client = OciClient::new();
        // Just verify it can be created
        let _ = client;
    }

    #[test]
    fn test_oci_client_default() {
        let client = OciClient::default();
        // Verify Default trait works
        let _ = client;
    }

    #[test]
    fn test_oci_client_get_auth_anonymous() {
        let client = OciClient::new();
        let reference = parse_reference("docker.io/library/nginx:latest").unwrap();
        let auth = client.get_auth(&reference);
        assert!(matches!(auth, RegistryAuth::Anonymous));
    }

    #[test]
    fn test_oci_client_get_auth_ghcr_no_token() {
        // Ensure no token env vars are set
        // SAFETY: Test runs in isolation
        unsafe {
            std::env::remove_var("GITHUB_TOKEN");
            std::env::remove_var("GH_TOKEN");
        }

        let client = OciClient::new();
        let reference = parse_reference("ghcr.io/owner/image:latest").unwrap();
        let auth = client.get_auth(&reference);
        assert!(matches!(auth, RegistryAuth::Anonymous));
    }

    // ==========================================================================
    // ResolvedImage tests
    // ==========================================================================

    #[test]
    fn test_resolved_image_debug() {
        let reference = parse_reference("nginx:latest").unwrap();
        let resolved = ResolvedImage {
            reference,
            digest: "sha256:abc123".to_string(),
            layers: vec!["sha256:layer1".to_string()],
            layer_descriptors: vec![],
        };

        let debug_str = format!("{:?}", resolved);
        assert!(debug_str.contains("sha256:abc123"));
    }

    #[test]
    fn test_resolved_image_clone() {
        let reference = parse_reference("nginx:latest").unwrap();
        let resolved = ResolvedImage {
            reference,
            digest: "sha256:abc123".to_string(),
            layers: vec!["sha256:layer1".to_string(), "sha256:layer2".to_string()],
            layer_descriptors: vec![],
        };

        let cloned = resolved.clone();
        assert_eq!(cloned.digest, "sha256:abc123");
        assert_eq!(cloned.layers.len(), 2);
    }

    #[test]
    fn test_resolved_image_empty_layers() {
        let reference = parse_reference("scratch:latest").unwrap();
        let resolved = ResolvedImage {
            reference,
            digest: "sha256:empty".to_string(),
            layers: vec![],
            layer_descriptors: vec![],
        };

        assert!(resolved.layers.is_empty());
        assert!(resolved.layer_descriptors.is_empty());
    }

    // ==========================================================================
    // Image index platform selection tests
    // ==========================================================================

    fn index_entry(os: &str, arch: &str, digest: &str) -> ImageIndexEntry {
        ImageIndexEntry {
            media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
            digest: digest.to_string(),
            size: 0,
            platform: Some(oci_distribution::manifest::Platform {
                architecture: arch.to_string(),
                os: os.to_string(),
                os_version: None,
                os_features: None,
                variant: None,
                features: None,
            }),
            annotations: None,
        }
    }

    #[test]
    fn test_select_index_entry_picks_matching_platform() {
        let entries = vec![
            index_entry("linux", "amd64", "sha256:lin-amd64"),
            index_entry("linux", "arm64", "sha256:lin-arm64"),
            index_entry("darwin", "arm64", "sha256:dar-arm64"),
        ];

        let picked = select_index_entry(&entries, "linux", "arm64").unwrap();
        assert_eq!(picked.digest, "sha256:lin-arm64");

        let picked = select_index_entry(&entries, "darwin", "arm64").unwrap();
        assert_eq!(picked.digest, "sha256:dar-arm64");

        let picked = select_index_entry(&entries, "linux", "amd64").unwrap();
        assert_eq!(picked.digest, "sha256:lin-amd64");
    }

    #[test]
    fn test_select_index_entry_case_insensitive() {
        let entries = vec![index_entry("Linux", "ARM64", "sha256:case")];
        let picked = select_index_entry(&entries, "linux", "arm64").unwrap();
        assert_eq!(picked.digest, "sha256:case");
    }

    #[test]
    fn test_select_index_entry_no_match() {
        let entries = vec![
            index_entry("linux", "amd64", "sha256:a"),
            index_entry("linux", "arm64", "sha256:b"),
        ];
        assert!(select_index_entry(&entries, "windows", "amd64").is_none());
        assert!(select_index_entry(&entries, "darwin", "arm64").is_none());
    }

    #[test]
    fn test_select_index_entry_skips_entries_without_platform() {
        let mut entries = vec![index_entry("linux", "arm64", "sha256:has-plat")];
        entries.insert(
            0,
            ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: "sha256:no-plat".to_string(),
                size: 0,
                platform: None,
                annotations: None,
            },
        );

        let picked = select_index_entry(&entries, "linux", "arm64").unwrap();
        assert_eq!(picked.digest, "sha256:has-plat");
    }

    #[test]
    fn test_available_platforms_renders_sorted_unique() {
        let entries = vec![
            index_entry("linux", "arm64", "sha256:a"),
            index_entry("linux", "amd64", "sha256:b"),
            index_entry("linux", "amd64", "sha256:c"),
        ];
        assert_eq!(available_platforms(&entries), "linux/amd64, linux/arm64");
    }

    #[test]
    fn test_available_platforms_empty() {
        assert_eq!(available_platforms(&[]), "<none>");
    }

    #[test]
    fn test_select_index_entry_ignores_attestation_under_real_query() {
        // Synthetic image index with one real entry and an attestation entry
        // (Docker Hub / GHCR emit `unknown/unknown` for attestation manifests).
        let entries = vec![
            index_entry("linux", "amd64", "sha256:real"),
            index_entry("unknown", "unknown", "sha256:attestation"),
        ];

        // Real platform query selects the real entry.
        let picked = select_index_entry(&entries, "linux", "amd64").unwrap();
        assert_eq!(picked.digest, "sha256:real");

        // No real platform query should ever land on the attestation entry.
        for (os, arch) in [
            ("linux", "arm64"),
            ("darwin", "arm64"),
            ("darwin", "amd64"),
            ("windows", "amd64"),
        ] {
            let picked = select_index_entry(&entries, os, arch);
            assert!(
                picked.is_none_or(|e| e.digest != "sha256:attestation"),
                "attestation entry must not match real platform query {os}/{arch}",
            );
        }
    }

    #[test]
    fn test_available_platforms_filters_attestation_entries() {
        let entries = vec![
            index_entry("linux", "amd64", "sha256:real-amd64"),
            index_entry("linux", "arm64", "sha256:real-arm64"),
            index_entry("unknown", "unknown", "sha256:attestation"),
        ];
        let rendered = available_platforms(&entries);
        assert_eq!(rendered, "linux/amd64, linux/arm64");
        assert!(!rendered.contains("unknown"));
    }

    #[test]
    fn test_available_platforms_entries_without_platform() {
        let entries = vec![ImageIndexEntry {
            media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
            digest: "sha256:no-plat".to_string(),
            size: 0,
            platform: None,
            annotations: None,
        }];
        assert_eq!(available_platforms(&entries), "<none>");
    }
}
