//! Bazel Remote Execution API v2 client for cuenv's content-addressed cache.
//!
//! cuenv already stores REAPI protobuf messages and digests them the way
//! REAPI defines (see `cuenv_cas::reapi`), so this crate is transport only:
//! it moves those same bytes over gRPC.
//!
//! Because the protocol is the standard one, any REAPI cache works —
//! `bazel-remote`, buildbarn, BuildBuddy, NativeLink, EngFlow and Namespace
//! all expose the `grpcs://` endpoint that Bazel's `--remote_cache` takes,
//! and cuenv dials it the same way.
//!
//! ```
//! use cuenv_cas::{Cas, LocalCas};
//! use cuenv_cas_remote::{Credentials, LayeredCas, RemoteConfig};
//! use std::sync::Arc;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let config = RemoteConfig::new("grpcs://cache.example.com:443")
//!     .with_credentials(Credentials::Bearer("example-token".into()));
//! # let _ = config;
//! let local_dir = tempfile::tempdir()?;
//! let remote_dir = tempfile::tempdir()?;
//! let local = Arc::new(LocalCas::open(local_dir.path())?) as Arc<dyn Cas>;
//! // A RemoteCas can be used here after connecting it with `config`.
//! let remote = Arc::new(LocalCas::open(remote_dir.path())?) as Arc<dyn Cas>;
//!
//! // Reads fall through to the remote and are kept locally; writes stay
//! // local until `with_push()` says otherwise.
//! let cas = LayeredCas::new(local, remote);
//! # let runtime = tokio::runtime::Runtime::new()?;
//! # let digest = runtime.block_on(cas.put_bytes(b"example"))?;
//! # assert_eq!(runtime.block_on(cas.get(&digest))?, b"example");
//! # Ok(())
//! # }
//! ```
//!
//! # Soundness
//!
//! A shared cache multiplies the consequence of an unsound entry: a wrong
//! result stops being one developer's confusing afternoon and becomes every
//! machine's. cuenv's default `"dir"` sandbox runs a task among only its
//! declared inputs, which catches undeclared *relative* reads, but it is not
//! an OS boundary: a task can still open an absolute host path or reach the
//! network, and so record an entry that is wrong elsewhere. That is why
//! [`RemoteConfig`] is read-only until [`RemoteConfig::writable`] is called,
//! and why callers should keep it that way until strict filesystem and
//! network confinement lands.

pub mod action_cache;
pub mod cas;
pub mod client;
pub mod config;
pub mod error;
pub mod layered;

pub use action_cache::RemoteActionCache;
pub use cas::RemoteCas;
pub use client::RemoteClient;
pub use config::{Credentials, RemoteConfig};
pub use error::{Error, Result};
pub use layered::{LayeredActionCache, LayeredCas};
