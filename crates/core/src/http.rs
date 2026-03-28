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
            if rustls::crypto::ring::default_provider()
                .install_default()
                .is_err()
            {
                panic!(
                    "Failed to install rustls crypto provider. \
                     cuenv requires a working rustls provider when built with `rustls-no-provider`."
                );
            }
            if rustls::crypto::CryptoProvider::get_default().is_none() {
                panic!(
                    "rustls crypto provider is still missing after installation attempt. \
                     Another incompatible provider may have been installed earlier in the process."
                );
            }
        }
    });
}
