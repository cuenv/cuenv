//! Bazel HTTP/REST remote cache client (`/cas/<sha256>`, `/ac/<sha256>`).
//!
//! Speaks the standard Bazel HTTP remote caching protocol exposed by
//! `bazel-remote` (HTTP mode), nginx, and HTTP proxies fronting object stores
//! such as S3, GCS, or R2.
//!
//! # Tenant isolation caveat
//!
//! When an `instance_name` is configured, it is appended as a path prefix
//! before `/cas/<hash>` and `/ac/<hash>`. Note that `bazel-remote` ignores the
//! HTTP instance prefix for CAS blobs and only namespaces ActionCache keys
//! when explicitly configured to do so, so a URL prefix alone cannot establish
//! tenant isolation.

pub mod action_cache;
pub mod cas;
pub mod client;

pub use action_cache::HttpActionCache;
pub use cas::HttpCas;
pub use client::HttpRemoteClient;
