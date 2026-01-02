//! OCI registry client for resolving and pulling images.
//!
//! Uses `oci-distribution` for registry operations.

use oci_distribution::client::{ClientConfig, ClientProtocol};
use oci_distribution::manifest::OciDescriptor;
use oci_distribution::secrets::RegistryAuth;
use oci_distribution::{Client, Reference};
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, info, trace};

use crate::cache::OciCache;
use crate::platform::Platform;
use crate::{Error, Result};

/// OCI registry client for image resolution and blob pulling.
pub struct OciClient {
    client: Client,
}

impl Default for OciClient {
    fn default() -> Self {
        Self::new()
    }
}

impl OciClient {
    /// Create a new OCI client with default configuration.
    #[must_use]
    pub fn new() -> Self {
        let config = ClientConfig {
            protocol: ClientProtocol::Https,
            ..Default::default()
        };
        let client = Client::new(config);
        Self { client }
    }

    /// Resolve an image reference to a digest for a specific platform.
    ///
    /// Returns the manifest digest for the platform-specific image.
    pub async fn resolve_digest(&self, image: &str, platform: &Platform) -> Result<ResolvedImage> {
        let reference = parse_reference(image)?;
        info!(%image, %platform, "Resolving image digest");

        let auth = self.get_auth(&reference);

        // Pull the manifest and config
        let (manifest, digest, _config) = self
            .client
            .pull_manifest_and_config(&reference, &auth)
            .await
            .map_err(|e| Error::Oci(e.to_string()))?;

        trace!(?manifest, "Got manifest");

        // Extract layer digests from manifest
        let layers: Vec<String> = manifest.layers.iter().map(|l| l.digest.clone()).collect();

        // Also store layer descriptors for pulling
        let layer_descriptors: Vec<OciDescriptor> = manifest.layers.clone();

        debug!(
            %image,
            %platform,
            %digest,
            layer_count = layers.len(),
            "Resolved image"
        );

        Ok(ResolvedImage {
            reference,
            digest,
            layers,
            layer_descriptors,
        })
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

        self.client
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
}
