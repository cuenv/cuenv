//! HTTP helpers shared across the cuenv workspace.

use std::sync::OnceLock;

static RUSTLS_PROVIDER_INSTALLED: OnceLock<()> = OnceLock::new();

/// Install the process-wide rustls crypto provider required by reqwest.
///
/// The workspace uses reqwest with the `rustls-no-provider` feature, so the
/// application must install a provider before constructing clients. We do that
/// once per process and accept any already-installed provider.
pub fn ensure_rustls_crypto_provider() {
    RUSTLS_PROVIDER_INSTALLED.get_or_init(|| {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
    });
}
