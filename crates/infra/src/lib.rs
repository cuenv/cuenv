//! Infrastructure as code for cuenv.
//!
//! `cuenv-infra` drives unmodified Terraform provider binaries directly
//! over their gRPC plugin protocol (tfplugin5 and tfplugin6) — no Terraform
//! or OpenTofu CLI involved. Resources are declared in CUE under a
//! project's `infra` block and every managed resource is stored as its own
//! record in a remote Turso (libSQL) database.
//!
//! State is multi-tenant: records are always keyed by the CUE module path
//! (the tenant) and the cuenv project name (the discriminator), so many
//! modules and projects can share one database safely.
//!
//! ```text
//! CUE `infra` ──► InfraEngine ──► ProviderClient ──gRPC──► terraform-provider-*
//!                     │
//!                     └──► StateStore (Turso) ── one row per managed resource
//! ```

pub mod cty;
pub mod engine;
pub mod error;
pub mod plugin;
mod proto;
pub mod registry;
pub mod schema;
pub mod state;
pub mod tenant;

pub use engine::{
    Action, ApplyEvent, EngineOptions, InfraEngine, Plan, PlanMode, PlanSummary, ResourceChange,
    render_plan,
};
pub use error::{InfraError, Result};
pub use state::{
    ManagedResource, MemoryStateStore, ResourceAddress, StateLock, StateStore, TursoConfig,
    TursoStateStore,
};
pub use tenant::{TenantKey, read_module_path};

/// Install the process-wide rustls crypto provider reqwest needs.
///
/// The workspace builds reqwest with `rustls-no-provider`; the CLI installs
/// `ring` at startup, and this makes library and test use safe too.
pub(crate) fn ensure_rustls_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        // Losing an install race to another thread is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}
