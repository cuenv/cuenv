//! Infrastructure as code for cuenv.
//!
//! `cuenv-infrastructure` drives unmodified Terraform provider binaries directly
//! over their gRPC plugin protocol (tfplugin5 and tfplugin6) — no Terraform
//! or OpenTofu CLI involved. Resources are declared in CUE under a
//! project's `infrastructure` block and every managed resource is stored as its own
//! record in a remote Turso (libSQL) database.
//!
//! State is multi-tenant: records are always keyed by the CUE module path
//! (the tenant) and the cuenv project name (the discriminator), so many
//! modules and projects can share one database safely.
//!
//! ```text
//! CUE `infrastructure` ──► InfrastructureEngine ──► ProviderClient ──gRPC──► terraform-provider-*
//!                     │
//!                     └──► StateStore (Turso) ── one row per managed resource
//! ```

pub mod cancellation;
pub mod engine;
pub mod error;
pub mod object_change;
pub mod plugin;
mod protocol;
pub mod registry;
pub mod schema;
pub mod state;
pub mod tenant;
pub mod type_system;
pub mod unrecorded;

pub use cancellation::Cancellation;
pub use engine::{
    Action, ApplyContext, ApplyEvent, EngineOptions, EngineSetup, InfrastructureEngine, Plan,
    PlanDigest, PlanMode, PlanSummary, ResourceChange, render_plan,
};
pub use error::{InfrastructureError, Result};
pub use state::{
    LockInformation, ManagedResource, MemoryStateStore, ResourceAddress, StateLock, StateStore,
    TursoConfiguration, TursoStateStore,
};
pub use tenant::{TenantKey, read_module_path};
pub use unrecorded::{UnrecordedRecord, UnrecordedStore};

/// Install the process-wide rustls cryptography provider reqwest needs.
///
/// The workspace builds reqwest with `rustls-no-provider`; the CLI installs
/// `ring` at startup, and this makes library and test use safe too.
pub(crate) fn ensure_rustls_cryptography_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        // Losing an install race to another thread is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}
