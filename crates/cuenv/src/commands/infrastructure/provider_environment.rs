//! What provider processes inherit from the cuenv process environment.
//!
//! Providers are third-party programs. By default (`inherit`) they keep the
//! ambient environment, so a provider that authenticates through the host's
//! cloud credentials keeps working, but they never receive the keys to
//! cuenv's own secret stores (see [`cuenv_secrets::RESOLVER_ENVIRONMENT_VARIABLES`]), the
//! credentials a project's remote cache is configured with, or the state
//! store's token, unless the project passes a variable of that name itself.
//! The opt-in `isolated` mode starts from an empty environment (see
//! [`ISOLATED_INHERITED_ENVIRONMENT_VARIABLES`]) and adds only what the
//! project passes.
//!
//! Whatever the mode, the project's variables that the running action's
//! policy allows are added last, and variables the policy forbids are never
//! inherited from the host under the same name.
//!
//! This is hygiene, not a sandbox: a provider runs as the user and can read
//! whatever the user can (credential files under `HOME`), and every provider
//! receives every variable the project passes.

use std::collections::BTreeMap;
use std::ffi::OsString;

use cuenv_infrastructure::plugin::{
    ISOLATED_INHERITED_ENVIRONMENT_VARIABLES, isolated_withheld_names,
};
use cuenv_manifest::manifest::ProviderEnvironment;
use cuenv_secrets::resolver_environment_variable_names;

/// What decides which host variables a provider must not inherit.
#[derive(Debug)]
pub(super) struct ProviderEnvironmentInputs<'inputs> {
    /// How much of the host environment providers inherit.
    pub(super) mode: ProviderEnvironment,
    /// The names in the cuenv process environment.
    pub(super) ambient: Vec<OsString>,
    /// Names of the project variables resolved and passed to providers.
    pub(super) provided: &'inputs [String],
    /// Names of project variables the running action's policy forbids.
    pub(super) policy_withheld: &'inputs [String],
    /// Names of variables the project's configuration designates as
    /// credentials (the remote cache's token and header variables).
    pub(super) configured_credentials: &'inputs [String],
    /// The variable holding the state store's authentication token.
    pub(super) token_variable: &'inputs str,
}

/// The variable names to remove from the provider's environment after the
/// host's variables and the project's values are combined.
#[must_use]
#[tracing::instrument(
    level = "debug",
    skip_all,
    fields(mode = ?inputs.mode, ambient = inputs.ambient.len())
)]
pub(super) fn withheld_environment_variables(
    inputs: &ProviderEnvironmentInputs<'_>,
) -> Vec<String> {
    let mut withheld: Vec<String> = inputs
        .policy_withheld
        .iter()
        .cloned()
        .chain(std::iter::once(inputs.token_variable.to_string()))
        .collect();
    match inputs.mode {
        ProviderEnvironment::Inherit => withheld.extend(
            resolver_environment_variable_names(inputs.ambient.iter().cloned())
                .into_iter()
                .chain(inputs.configured_credentials.iter().cloned())
                .filter(|name| !inputs.provided.contains(name)),
        ),
        ProviderEnvironment::Isolated => withheld.extend(isolated_withheld_names(
            inputs.ambient.iter().cloned(),
            inputs.provided,
        )),
    }
    withheld.sort();
    withheld.dedup();
    withheld
}

/// The proxy variables of the `isolated` allowlist whose host values carry
/// credentials, with the credentials removed.
///
/// A proxy URL may hold `user:password@` (`http://user:pass@proxy:3128`).
/// Isolated mode passes the proxy variables so providers can reach the
/// network, but not credentials nobody gave them on purpose, so the URL is
/// passed without its userinfo. Pass `variables` as an overlay (it replaces
/// the inherited values); a name the project passes itself is left alone.
/// Only the names are logged.
///
/// A project that needs an authenticated proxy passes the full URL in its
/// `env` under the same name.
#[must_use]
#[tracing::instrument(level = "debug", skip_all)]
pub(super) fn isolated_proxy_overlay(
    ambient: impl IntoIterator<Item = (OsString, OsString)>,
    provided: &[String],
) -> BTreeMap<String, String> {
    ambient
        .into_iter()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(name, _)| {
            is_proxy_variable(name) && !provided.iter().any(|provided| provided == name)
        })
        .filter_map(|(name, value)| {
            let stripped = without_userinfo(&value)?;
            tracing::warn!(
                variable = %name,
                "the proxy URL carries credentials; providers receive it without them \
                 (pass the full URL in the project's env to give them to providers)"
            );
            Some((name, stripped))
        })
        .collect()
}

/// Proxy variables whose value is a URL (`NO_PROXY` is a host list).
fn is_proxy_variable(name: &str) -> bool {
    ISOLATED_INHERITED_ENVIRONMENT_VARIABLES.contains(&name)
        && name.to_ascii_lowercase().ends_with("_proxy")
        && !name.eq_ignore_ascii_case("no_proxy")
}

/// `url` without the `user:password@` of its authority, or `None` when it
/// has none. A URL without a scheme (`user:pass@host:3128`) is read as an
/// authority, as Go's HTTP client does.
fn without_userinfo(url: &str) -> Option<String> {
    let (scheme, rest) = url
        .split_once("://")
        .map_or(("", url), |(scheme, rest)| (&url[..scheme.len() + 3], rest));
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let (_, host) = authority.rsplit_once('@')?;
    Some(format!("{scheme}{host}{tail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    fn inputs<'inputs>(
        mode: ProviderEnvironment,
        provided: &'inputs [String],
        policy_withheld: &'inputs [String],
    ) -> ProviderEnvironmentInputs<'inputs> {
        ProviderEnvironmentInputs {
            mode,
            ambient: [
                "PATH",
                "HOME",
                "AWS_SECRET_ACCESS_KEY",
                "OP_SERVICE_ACCOUNT_TOKEN",
            ]
            .map(OsString::from)
            .to_vec(),
            provided,
            policy_withheld,
            configured_credentials: &[],
            token_variable: "TURSO_AUTH_TOKEN",
        }
    }

    #[test]
    fn inherit_withholds_resolver_credentials_the_token_and_policy_names() {
        let policy = names(&["DENIED"]);
        let withheld =
            withheld_environment_variables(&inputs(ProviderEnvironment::Inherit, &[], &policy));
        for name in [
            "OP_SERVICE_ACCOUNT_TOKEN",
            "INFISICAL_TOKEN",
            "INFISICAL_CLIENT_ID",
            "INFISICAL_CLIENT_SECRET",
            "VAULT_TOKEN",
            "TURSO_AUTH_TOKEN",
            "DENIED",
        ] {
            assert!(withheld.iter().any(|withheld| withheld == name), "{name}");
        }
        // Cloud credentials stay: the providers of those clouds read them.
        assert!(!withheld.iter().any(|name| name == "AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn inherit_keeps_a_resolver_credential_the_project_passes_explicitly() {
        let provided = names(&["VAULT_TOKEN"]);
        let withheld =
            withheld_environment_variables(&inputs(ProviderEnvironment::Inherit, &provided, &[]));
        assert!(!withheld.iter().any(|name| name == "VAULT_TOKEN"));
        assert!(
            withheld
                .iter()
                .any(|name| name == "OP_SERVICE_ACCOUNT_TOKEN")
        );
    }

    #[test]
    fn a_forbidden_project_variable_stays_withheld_even_if_it_is_a_resolver_credential() {
        let policy = names(&["VAULT_TOKEN"]);
        let withheld =
            withheld_environment_variables(&inputs(ProviderEnvironment::Inherit, &[], &policy));
        assert!(withheld.iter().any(|name| name == "VAULT_TOKEN"));
    }

    #[test]
    fn isolated_withholds_every_ambient_name_outside_the_allowlist() {
        let provided = names(&["AWS_SECRET_ACCESS_KEY"]);
        let withheld =
            withheld_environment_variables(&inputs(ProviderEnvironment::Isolated, &provided, &[]));
        assert!(
            withheld
                .iter()
                .any(|name| name == "OP_SERVICE_ACCOUNT_TOKEN")
        );
        assert!(withheld.iter().any(|name| name == "TURSO_AUTH_TOKEN"));
        assert!(!withheld.iter().any(|name| name == "PATH" || name == "HOME"));
        assert!(
            !withheld.iter().any(|name| name == "AWS_SECRET_ACCESS_KEY"),
            "a variable the project passes is kept"
        );
    }

    #[test]
    fn inherit_withholds_prefix_and_endpoint_names_found_in_the_ambient_environment() {
        // Built at run time: a literal near-miss would be found by the test
        // that scans the sources for credential names.
        let near_miss = format!("OP_{}", "SESSIONS");
        let mut with_sessions = inputs(ProviderEnvironment::Inherit, &[], &[]);
        with_sessions.ambient.extend(
            [
                "OP_SESSION_acme",
                "OP_SESSION_personal",
                "OP_CONNECT_TOKEN",
                "OP_CONNECT_HOST",
                "CUENV_SECRET_SALT",
                "CUENV_SECRET_SALT_PREV",
                near_miss.as_str(),
            ]
            .map(OsString::from),
        );
        let withheld = withheld_environment_variables(&with_sessions);
        for name in [
            "OP_SESSION_acme",
            "OP_SESSION_personal",
            "OP_CONNECT_TOKEN",
            "OP_CONNECT_HOST",
            "CUENV_SECRET_SALT",
            "CUENV_SECRET_SALT_PREV",
        ] {
            assert!(withheld.iter().any(|withheld| withheld == name), "{name}");
        }
        assert!(
            !withheld.contains(&near_miss),
            "only the prefix `OP_SESSION_` is covered"
        );
    }

    #[test]
    fn an_explicitly_passed_session_variable_is_kept() {
        let provided = names(&["OP_SESSION_acme"]);
        let mut with_session = inputs(ProviderEnvironment::Inherit, &provided, &[]);
        with_session.ambient.push(OsString::from("OP_SESSION_acme"));
        let withheld = withheld_environment_variables(&with_session);
        assert!(!withheld.iter().any(|name| name == "OP_SESSION_acme"));
    }

    #[test]
    fn inherit_withholds_the_variables_the_remote_cache_is_configured_with() {
        let configured = names(&["CACHE_TOKEN", "CACHE_HEADER_VALUE"]);
        let mut inputs = inputs(ProviderEnvironment::Inherit, &[], &[]);
        inputs.configured_credentials = &configured;
        let withheld = withheld_environment_variables(&inputs);
        assert!(withheld.iter().any(|name| name == "CACHE_TOKEN"));
        assert!(withheld.iter().any(|name| name == "CACHE_HEADER_VALUE"));
        // Unless the project passes one explicitly.
        let provided = names(&["CACHE_TOKEN"]);
        inputs.provided = &provided;
        let withheld = withheld_environment_variables(&inputs);
        assert!(!withheld.iter().any(|name| name == "CACHE_TOKEN"));
        assert!(withheld.iter().any(|name| name == "CACHE_HEADER_VALUE"));
    }

    fn proxy_overlay(variables: &[(&str, &str)], provided: &[&str]) -> BTreeMap<String, String> {
        isolated_proxy_overlay(
            variables
                .iter()
                .map(|(name, value)| (OsString::from(name), OsString::from(value))),
            &provided
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn proxy_urls_lose_their_userinfo_in_isolated_mode() {
        let overlay = proxy_overlay(
            &[
                ("HTTPS_PROXY", "http://user:p%40ss@proxy.internal:3128"),
                ("http_proxy", "http://token@proxy.internal:3128/path?x=1#f"),
                ("ALL_PROXY", "user:secret@proxy.internal:1080"),
                ("HTTP_PROXY", "http://proxy.internal:3128"),
                ("NO_PROXY", "internal,user@host"),
                ("PATH", "/usr/bin:/home/user@x/bin"),
            ],
            &[],
        );
        assert_eq!(overlay["HTTPS_PROXY"], "http://proxy.internal:3128");
        assert_eq!(
            overlay["http_proxy"],
            "http://proxy.internal:3128/path?x=1#f"
        );
        assert_eq!(overlay["ALL_PROXY"], "proxy.internal:1080");
        assert!(
            !overlay.contains_key("HTTP_PROXY"),
            "a URL without userinfo is passed as it is"
        );
        assert!(
            !overlay.contains_key("NO_PROXY"),
            "a host list is not a URL"
        );
        assert!(!overlay.contains_key("PATH"));
    }

    #[test]
    fn a_proxy_variable_the_project_passes_is_left_alone() {
        let overlay = proxy_overlay(
            &[("HTTPS_PROXY", "http://user:pass@proxy.internal:3128")],
            &["HTTPS_PROXY"],
        );
        assert!(overlay.is_empty());
    }

    #[test]
    fn userinfo_is_found_only_in_the_authority() {
        assert_eq!(
            without_userinfo("http://a:b@h:1").as_deref(),
            Some("http://h:1")
        );
        assert_eq!(
            without_userinfo("http://h:1/a@b").as_deref(),
            None,
            "an @ after the authority is part of the path"
        );
        assert_eq!(without_userinfo("http://h:1?u=a@b"), None);
        assert_eq!(without_userinfo("h:1"), None);
    }
}
