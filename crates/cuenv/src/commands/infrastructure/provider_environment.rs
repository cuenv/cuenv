//! What provider processes inherit from the cuenv process environment.
//!
//! Providers are third-party programs. By default (`inherit`) they keep the
//! ambient environment, so a provider that authenticates through the host's
//! cloud credentials keeps working, but they never receive the keys to
//! cuenv's own secret stores (see [`RESOLVER_CREDENTIAL_ENVIRONMENT_VARIABLES`])
//! or the state store's token, unless the project passes a variable of that
//! name itself. The opt-in `isolated` mode starts from an empty environment
//! (see [`ISOLATED_INHERITED_ENVIRONMENT_VARIABLES`]) and adds only what the
//! project passes.
//!
//! Whatever the mode, the project's variables that the running action's
//! policy allows are added last, and variables the policy forbids are never
//! inherited from the host under the same name.

use std::ffi::OsString;

use cuenv_infrastructure::plugin::isolated_withheld_names;
use cuenv_secrets::RESOLVER_CREDENTIAL_ENVIRONMENT_VARIABLES;

/// What a provider process inherits from the cuenv process environment.
///
/// TODO(m5-integration): this is a local stand-in for
/// `cuenv_manifest::manifest::ProviderEnvironment`, the type of
/// `infrastructure.providerEnvironment` (and of the same field on each
/// environment configuration) that the schema worker adds. Delete this enum,
/// import that one, and read the selected configuration's
/// `provider_environment` where [`super::run`] now passes
/// `ProviderEnvironment::default()`; the variants and their meaning are the
/// same.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum ProviderEnvironment {
    /// The ambient environment, minus the credentials of cuenv's own secret
    /// resolvers (unless the project passes them explicitly).
    #[default]
    Inherit,
    /// An empty environment except `PATH`, `HOME`, `USER`, `LOGNAME`,
    /// `TMPDIR` (cuenv sets its own), proxy and TLS variables, then the
    /// project's values.
    Isolated,
}

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
    /// The variable holding the state store's authentication token.
    pub(super) token_variable: &'inputs str,
}

/// The variable names to remove from the provider's environment after the
/// host's variables and the project's values are combined.
#[must_use]
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
            RESOLVER_CREDENTIAL_ENVIRONMENT_VARIABLES
                .iter()
                .filter(|name| !inputs.provided.iter().any(|provided| provided == *name))
                .map(|name| (*name).to_string()),
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
            ambient: ["PATH", "HOME", "AWS_SECRET_ACCESS_KEY", "OP_SERVICE_ACCOUNT_TOKEN"]
                .map(OsString::from)
                .to_vec(),
            provided,
            policy_withheld,
            token_variable: "TURSO_AUTH_TOKEN",
        }
    }

    #[test]
    fn inherit_withholds_resolver_credentials_the_token_and_policy_names() {
        let policy = names(&["DENIED"]);
        let withheld = withheld_environment_variables(&inputs(
            ProviderEnvironment::Inherit,
            &[],
            &policy,
        ));
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
        assert!(withheld.iter().any(|name| name == "OP_SERVICE_ACCOUNT_TOKEN"));
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
        assert!(withheld.iter().any(|name| name == "OP_SERVICE_ACCOUNT_TOKEN"));
        assert!(withheld.iter().any(|name| name == "TURSO_AUTH_TOKEN"));
        assert!(!withheld.iter().any(|name| name == "PATH" || name == "HOME"));
        assert!(
            !withheld.iter().any(|name| name == "AWS_SECRET_ACCESS_KEY"),
            "a variable the project passes is kept"
        );
    }
}
