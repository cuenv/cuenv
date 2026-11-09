//! Object-store state implementation and migration capabilities.

#[path = "store/legacy_inventory.rs"]
pub mod legacy_inventory;
#[path = "store/legacy_migration.rs"]
pub mod legacy_migration;
#[path = "store/legacy_source.rs"]
mod legacy_source;
#[path = "store/migration_manifest.rs"]
pub mod migration_manifest;
#[path = "store/object_state_store.rs"]
mod object_state_store;

pub use object_state_store::{
    AddressingChoice, MAXIMUM_STATE_BYTES, ManifestEntry, MigrationManifest, ObjectLocation,
    ObjectStateStore, ObjectStoreConfiguration, TenantImport,
};
