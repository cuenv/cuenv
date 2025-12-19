use crate::merkle::digest::Digest;
use crate::RemoteError;
use async_trait::async_trait;
use crate::reapi::build::bazel::remote::execution::v2 as reapi;
use tonic::transport::Channel;
use tonic::Request;

#[async_trait]
pub trait Cas {
    async fn find_missing_blobs(&self, digests: Vec<Digest>) -> Result<Vec<Digest>, RemoteError>;
    async fn batch_upload_blobs(&self, blobs: Vec<(Digest, Vec<u8>)>) -> Result<(), RemoteError>;
    async fn batch_read_blobs(&self, digests: Vec<Digest>) -> Result<Vec<Vec<u8>>, RemoteError>;
}

#[derive(Clone)]
pub struct CasClient {
    inner: reapi::content_addressable_storage_client::ContentAddressableStorageClient<Channel>,
    instance_name: String,
}

impl CasClient {
    pub fn new(channel: Channel, instance_name: String) -> Self {
        Self {
            inner: reapi::content_addressable_storage_client::ContentAddressableStorageClient::new(channel),
            instance_name,
        }
    }
}

#[async_trait]
impl Cas for CasClient {
    async fn find_missing_blobs(&self, digests: Vec<Digest>) -> Result<Vec<Digest>, RemoteError> {
        let request = Request::new(reapi::FindMissingBlobsRequest {
            instance_name: self.instance_name.clone(),
            blob_digests: digests.iter().map(|d| reapi::Digest {
                hash: d.hash.clone(),
                size_bytes: d.size_bytes,
            }).collect(),
            digest_function: reapi::digest_function::Value::Sha256 as i32,
        });

        let mut client = self.inner.clone();
        let response = client.find_missing_blobs(request).await?
            .into_inner();

        Ok(response.missing_blob_digests.into_iter().map(|d| Digest {
            hash: d.hash,
            size_bytes: d.size_bytes,
        }).collect())
    }

    async fn batch_upload_blobs(&self, blobs: Vec<(Digest, Vec<u8>)>) -> Result<(), RemoteError> {
        let mut client = self.inner.clone();
        
        let requests: Vec<reapi::batch_update_blobs_request::Request> = blobs.into_iter().map(|(digest, data)| {
            reapi::batch_update_blobs_request::Request {
                digest: Some(reapi::Digest {
                    hash: digest.hash,
                    size_bytes: digest.size_bytes,
                }),
                data: data.into(),
                compressor: reapi::compressor::Value::Identity as i32,
            }
        }).collect();

        let request = Request::new(reapi::BatchUpdateBlobsRequest {
            instance_name: self.instance_name.clone(),
            requests,
            digest_function: reapi::digest_function::Value::Sha256 as i32,
        });

        let response = client.batch_update_blobs(request).await?.into_inner();

        for resp in response.responses {
            if let Some(status) = resp.status {
                if status.code != 0 {
                    return Err(RemoteError::Grpc(tonic::Status::new(
                        tonic::Code::from_i32(status.code),
                        status.message,
                    )));
                }
            }
        }

        Ok(())
    }

    async fn batch_read_blobs(&self, digests: Vec<Digest>) -> Result<Vec<Vec<u8>>, RemoteError> {
        let mut client = self.inner.clone();
        let request = Request::new(reapi::BatchReadBlobsRequest {
            instance_name: self.instance_name.clone(),
            digests: digests.iter().map(|d| reapi::Digest {
                hash: d.hash.clone(),
                size_bytes: d.size_bytes,
            }).collect(),
            acceptable_compressors: vec![reapi::compressor::Value::Identity as i32],
            digest_function: reapi::digest_function::Value::Sha256 as i32,
        });

        let response = client.batch_read_blobs(request).await?.into_inner();

        let mut result = Vec::new();
        for resp in response.responses {
            if let Some(status) = resp.status {
                if status.code != 0 {
                    return Err(RemoteError::Grpc(tonic::Status::new(
                        tonic::Code::from_i32(status.code),
                        status.message,
                    )));
                }
            }
            result.push(resp.data.to_vec());
        }
        Ok(result)
    }
}
