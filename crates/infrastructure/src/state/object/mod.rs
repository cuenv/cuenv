//! State in an S3-compatible object store (AWS S3, Cloudflare R2, MinIO).
//!
//! Each tenant is one object, `state.json`, under a key derived from the CUE
//! module path, the package, the project and the environment:
//!
//! ```text
//! [<root>/]<module>/_<package>/<project>/<env | _default>/state.json
//! ```
//!
//! The object holds the format versions, the tenant identity, the last
//! writer, the lock and every record. Every write is a conditional PUT on the
//! ETag of the version it was derived from, made only after checking that the
//! lock in that version is the writer's own, so the lock check and the write
//! are atomic. See ADR-0012 and
//! `docs/design/specs/2026-10-29-infrastructure-object-storage-state.md`.

pub mod client;
pub mod document;
#[cfg(any(test, feature = "fake-s3"))]
pub mod fake;
pub mod key;
pub mod sigv4;
pub mod store;
pub mod xml;

#[cfg(test)]
mod tests;

pub use sigv4::Credentials;
pub use store::{
    AddressingChoice, ManifestEntry, MigrationManifest, ObjectLocation, ObjectStateStore,
    ObjectStoreConfiguration, TenantImport,
};

mod object_state_features;

pub use object_state_features::{
    HistoryEntry, LockDetails, ObjectStateFeatures, ObjectTenant, RestoreRequest, StateSize,
    StoredTenant,
};
