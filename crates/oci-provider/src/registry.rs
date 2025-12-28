//! OCI registry client for resolving and pulling images.
//!
//! Uses `oci-distribution` for registry operations.

use oci_distribution::client::{ClientConfig, ClientProtocol};
use oci_distribution::manifest::OciDescriptor;
use oci_distribution::secrets::RegistryAuth;
use oci_distribution::{Client, Reference};
use std::path::Path;
use tokio::io::AsyncWriteExt;
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
    pub async fn resolve_digest(
        &self,
        image: &str,
        platform: &Platform,
    ) -> Result<ResolvedImage> {
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
    pub async fn pull_blob_by_descriptor(
        &self,
        reference: &Reference,
        descriptor: &OciDescriptor,
        dest: &Path,
    ) -> Result<()> {
        debug!(digest = %descriptor.digest, ?dest, "Pulling blob");

        let _auth = self.get_auth(reference);

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

        debug!(digest = %descriptor.digest, ?dest, "Pulled blob");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_reference() {
        let r = parse_reference("ghcr.io/homebrew/core/jq:1.7.1").unwrap();
        assert_eq!(r.registry(), "ghcr.io");
        assert_eq!(r.repository(), "homebrew/core/jq");
        assert_eq!(r.tag(), Some("1.7.1"));
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
}
