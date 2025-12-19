//! Content Addressable Storage (CAS) client

use crate::config::RemoteConfig;
use crate::error::{RemoteError, Result};
use crate::merkle::Digest;

/// Client for REAPI ContentAddressableStorage service
///
/// Phase 2: This will wrap the actual gRPC client generated from protos
pub struct CasClient {
    #[allow(dead_code)]
    config: RemoteConfig,
}

impl CasClient {
    /// Create a new CAS client
    pub fn new(config: RemoteConfig) -> Self {
        Self { config }
    }

    /// Find which blobs are missing from the CAS
    ///
    /// Phase 2: Implement using FindMissingBlobs RPC
    pub async fn find_missing_blobs(&self, _digests: &[Digest]) -> Result<Vec<Digest>> {
        // TODO: Implement with actual gRPC call
        Err(RemoteError::config_error(
            "CAS client not yet implemented (Phase 2)",
        ))
    }

    /// Upload multiple blobs to the CAS
    ///
    /// Phase 2: Implement using BatchUpdateBlobs RPC
    pub async fn batch_upload_blobs(&self, _blobs: &[(Digest, Vec<u8>)]) -> Result<()> {
        // TODO: Implement with actual gRPC call
        Err(RemoteError::config_error(
            "CAS client not yet implemented (Phase 2)",
        ))
    }

    /// Download multiple blobs from the CAS
    ///
    /// Phase 2: Implement using BatchReadBlobs RPC
    pub async fn batch_read_blobs(&self, _digests: &[Digest]) -> Result<Vec<Vec<u8>>> {
        // TODO: Implement with actual gRPC call
        Err(RemoteError::config_error(
            "CAS client not yet implemented (Phase 2)",
        ))
    }

    /// Upload a single blob to the CAS
    pub async fn upload_blob(&self, _digest: &Digest, _data: Vec<u8>) -> Result<()> {
        // TODO: Implement with actual gRPC call
        Err(RemoteError::config_error(
            "CAS client not yet implemented (Phase 2)",
        ))
    }

    /// Download a single blob from the CAS
    pub async fn read_blob(&self, _digest: &Digest) -> Result<Vec<u8>> {
        // TODO: Implement with actual gRPC call
        Err(RemoteError::config_error(
            "CAS client not yet implemented (Phase 2)",
        ))
    }
}
