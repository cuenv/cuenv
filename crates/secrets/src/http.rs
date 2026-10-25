//! Shared HTTP helpers for secret resolvers that talk to REST APIs.
//!
//! The workspace builds reqwest with the `rustls-no-provider` feature, so a
//! rustls crypto provider must be installed before constructing clients. These
//! helpers centralize that bootstrap and proxy configuration so every
//! HTTP-based caller shares one implementation instead of copying it.

use crate::SecretError;
use reqwest::{Client, ClientBuilder, NoProxy, Proxy};
use std::sync::OnceLock;

static RUSTLS_PROVIDER_INSTALL: OnceLock<Result<(), String>> = OnceLock::new();

/// User agent sent by every cuenv HTTP client.
const USER_AGENT: &str = "cuenv";

/// Build a reqwest [`Client`] for a secret resolver.
///
/// Installs the process-wide rustls crypto provider once, then constructs the
/// client from [`client_builder`]. `provider` names the resolver (e.g.
/// `"gcp"`) for error attribution.
///
/// # Errors
///
/// Returns [`SecretError::ResolutionFailed`] if the crypto provider cannot be
/// installed or the HTTP client cannot be constructed.
pub fn build_client(provider: &str) -> Result<Client, SecretError> {
    ensure_rustls_crypto_provider(provider)?;

    client_builder().build().map_err(|err| {
        SecretError::resolution_failed(provider, format!("Failed to create HTTP client: {err}"))
    })
}

/// Create a reqwest [`ClientBuilder`] with cuenv's user agent and proxy policy.
///
/// Platform proxy discovery (macOS `SystemConfiguration`, the Windows
/// registry) is always disabled, because it has panicked in sandboxed and Nix
/// environments and release builds abort on panic. Proxies are instead taken
/// explicitly from `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY`
/// (uppercase first, then lowercase), mirroring reqwest's environment
/// handling: an empty value disables that proxy, and every proxy variable is
/// ignored under CGI (`REQUEST_METHOD` set) to avoid httpoxy.
///
/// The caller must install a rustls crypto provider before calling `build`.
pub fn client_builder() -> ClientBuilder {
    let no_proxy = NoProxy::from_env();
    ProxyEnv::read(|name| std::env::var(name).ok())
        .proxies(no_proxy.as_ref())
        .into_iter()
        .fold(
            Client::builder().user_agent(USER_AGENT).no_proxy(),
            ClientBuilder::proxy,
        )
}

/// Which request schemes a proxy applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyScope {
    Http,
    Https,
    All,
}

impl ProxyScope {
    /// Environment variables for this scope, in lookup order.
    const fn env_names(self) -> [&'static str; 2] {
        match self {
            Self::Http => ["HTTP_PROXY", "http_proxy"],
            Self::Https => ["HTTPS_PROXY", "https_proxy"],
            Self::All => ["ALL_PROXY", "all_proxy"],
        }
    }

    fn proxy(self, url: &str) -> reqwest::Result<Proxy> {
        match self {
            Self::Http => Proxy::http(url),
            Self::Https => Proxy::https(url),
            Self::All => Proxy::all(url),
        }
    }
}

/// Proxy URLs read from the environment, in reqwest's precedence order
/// (scheme-specific proxies before `ALL_PROXY`).
#[derive(Debug, Default, PartialEq, Eq)]
struct ProxyEnv {
    entries: Vec<(ProxyScope, String)>,
}

impl ProxyEnv {
    fn read(lookup: impl Fn(&str) -> Option<String>) -> Self {
        // httpoxy: under CGI, `HTTP_PROXY` can be set by a request header.
        if lookup("REQUEST_METHOD").is_some() {
            return Self::default();
        }

        let entries = [ProxyScope::Http, ProxyScope::Https, ProxyScope::All]
            .into_iter()
            .filter_map(|scope| {
                scope
                    .env_names()
                    .into_iter()
                    .find_map(&lookup)
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .map(|value| (scope, value))
            })
            .collect();
        Self { entries }
    }

    fn proxies(self, no_proxy: Option<&NoProxy>) -> Vec<Proxy> {
        self.entries
            .into_iter()
            .filter_map(|(scope, url)| match scope.proxy(&url) {
                Ok(proxy) => Some(proxy.no_proxy(no_proxy.cloned())),
                Err(err) => {
                    tracing::warn!(?scope, error = %err, "Ignoring invalid proxy URL from environment");
                    None
                }
            })
            .collect()
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
        .map_err(|message| SecretError::resolution_failed(provider, message))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn read(vars: &[(&str, &str)]) -> ProxyEnv {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        ProxyEnv::read(|name| vars.get(name).cloned())
    }

    fn entry(scope: ProxyScope, url: &str) -> (ProxyScope, String) {
        (scope, url.to_string())
    }

    #[test]
    fn no_proxy_variables_yields_no_proxies() {
        assert_eq!(read(&[]), ProxyEnv::default());
    }

    #[test]
    fn reads_all_scopes_in_precedence_order() {
        let env = read(&[
            ("ALL_PROXY", "socks5://all:1080"),
            ("https_proxy", "http://secure:3128"),
            ("HTTP_PROXY", "http://plain:3128"),
        ]);
        assert_eq!(
            env.entries,
            [
                entry(ProxyScope::Http, "http://plain:3128"),
                entry(ProxyScope::Https, "http://secure:3128"),
                entry(ProxyScope::All, "socks5://all:1080"),
            ]
        );
    }

    #[test]
    fn uppercase_wins_and_empty_value_disables() {
        let env = read(&[
            ("HTTP_PROXY", "http://upper:1"),
            ("http_proxy", "http://lower:1"),
            ("HTTPS_PROXY", ""),
            ("https_proxy", "http://ignored:1"),
        ]);
        assert_eq!(env.entries, [entry(ProxyScope::Http, "http://upper:1")]);
    }

    #[test]
    fn cgi_ignores_proxy_variables() {
        let env = read(&[("REQUEST_METHOD", "GET"), ("HTTP_PROXY", "http://evil:1")]);
        assert_eq!(env, ProxyEnv::default());
    }

    #[test]
    fn invalid_proxy_urls_are_skipped() {
        let env = ProxyEnv {
            entries: vec![
                entry(ProxyScope::Http, "http://[::1"),
                entry(ProxyScope::Https, "http://proxy:3128"),
            ],
        };
        assert_eq!(env.proxies(None).len(), 1);
    }

    #[test]
    fn builds_client_with_proxy_environment() {
        temp_env::with_vars(
            [
                ("HTTPS_PROXY", Some("http://proxy.invalid:3128")),
                ("NO_PROXY", Some("localhost,.internal")),
                ("REQUEST_METHOD", None),
            ],
            || {
                assert!(build_client("test").is_ok());
            },
        );
    }
}
