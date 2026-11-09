//! HTTP helpers shared across the cuenv workspace.

use std::sync::OnceLock;

static RUSTLS_PROVIDER_INSTALL: OnceLock<Result<(), String>> = OnceLock::new();

/// Install the process-wide rustls crypto provider required by reqwest.
///
/// The workspace uses reqwest with the `rustls-no-provider` feature, so the
/// application must install a provider before constructing clients. We do that
/// once per process and accept any already-installed provider, including one
/// another thread or library installed between our check and our install.
///
/// # Errors
///
/// Returns a configuration error if no provider could be installed and none
/// is present afterwards.
pub fn ensure_rustls_crypto_provider() -> crate::Result<()> {
    RUSTLS_PROVIDER_INSTALL
        .get_or_init(|| {
            if rustls::crypto::CryptoProvider::get_default().is_some()
                || rustls::crypto::ring::default_provider()
                    .install_default()
                    .is_ok()
                || rustls::crypto::CryptoProvider::get_default().is_some()
            {
                Ok(())
            } else {
                Err("Failed to install the rustls crypto provider required for HTTPS".to_string())
            }
        })
        .clone()
        .map_err(crate::Error::configuration)
}
