//! Shared HTTP helpers for secret resolvers that talk to REST APIs.
//!
//! The workspace builds reqwest with the `rustls-no-provider` feature, so a
//! rustls crypto provider must be installed before constructing clients. These
//! helpers centralize that bootstrap (plus the system-proxy panic workaround)
//! so every HTTP-based resolver shares one implementation instead of copying it.

use crate::SecretError;
use reqwest::Client;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;

static RUSTLS_PROVIDER_INSTALL: OnceLock<Result<(), String>> = OnceLock::new();

/// Build a reqwest [`Client`] for a secret resolver.
///
/// Installs the process-wide rustls crypto provider once, then constructs the
/// client. On platforms where system proxy discovery panics, it retries with
/// proxies disabled. `provider` names the resolver (e.g. `"gcp"`) for error
/// attribution.
///
/// # Errors
///
/// Returns [`SecretError::ResolutionFailed`] if the crypto provider cannot be
/// installed or the HTTP client cannot be constructed.
pub fn build_client(provider: &str) -> Result<Client, SecretError> {
    ensure_rustls_crypto_provider(provider)?;

    let primary = catch_unwind(AssertUnwindSafe(|| {
        Client::builder().user_agent("cuenv").build()
    }));

    match primary {
        Ok(Ok(client)) => Ok(client),
        Ok(Err(primary_err)) => Client::builder()
            .user_agent("cuenv")
            .no_proxy()
            .build()
            .map_err(|fallback_err| {
                resolution_error(
                    provider,
                    format!(
                        "Failed to create HTTP client: primary={primary_err}; fallback={fallback_err}"
                    ),
                )
            }),
        Err(_) => Client::builder()
            .user_agent("cuenv")
            .no_proxy()
            .build()
            .map_err(|fallback_err| {
                resolution_error(
                    provider,
                    format!(
                        "Failed to create HTTP client after system proxy discovery panicked: fallback={fallback_err}"
                    ),
                )
            }),
    }
}

fn ensure_rustls_crypto_provider(provider: &str) -> Result<(), SecretError> {
    let result = RUSTLS_PROVIDER_INSTALL.get_or_init(|| {
        if rustls::crypto::CryptoProvider::get_default().is_some() {
            return Ok(());
        }
        if rustls::crypto::ring::default_provider()
            .install_default()
            .is_ok()
        {
            return Ok(());
        }
        if rustls::crypto::CryptoProvider::get_default().is_some() {
            return Ok(());
        }
        Err("Failed to install rustls crypto provider for HTTP client".to_string())
    });

    result
        .clone()
        .map_err(|message| resolution_error(provider, message))
}

/// Normalize a configured API base URL by trimming surrounding whitespace and
/// any trailing slash, so callers can safely append path segments.
#[must_use]
pub fn normalize_api_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

/// Read an environment variable, trimming whitespace and treating an empty
/// value as absent.
#[must_use]
pub fn env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn resolution_error(name: impl Into<String>, message: impl Into<String>) -> SecretError {
    SecretError::ResolutionFailed {
        name: name.into(),
        message: message.into(),
    }
}
