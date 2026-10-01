//! The environment variables cuenv's own secret resolvers authenticate with.
//!
//! These hold the keys to the user's secret stores. cuenv reads them to
//! resolve secrets, redacts their values from its output, and does not hand
//! them to processes it starts for other purposes (infrastructure providers)
//! unless a project passes them explicitly.

/// Environment variables that exist only to authenticate cuenv's secret
/// resolvers (1Password, Infisical, Vault).
///
/// Cloud platform credentials that the platform's own tools and Terraform
/// providers read as well (`AWS_*`, `GOOGLE_APPLICATION_CREDENTIALS`,
/// `GOOGLE_OAUTH_ACCESS_TOKEN`) are not listed: withholding them would break
/// the providers that need them.
pub const RESOLVER_CREDENTIAL_ENVIRONMENT_VARIABLES: &[&str] = &[
    "OP_SERVICE_ACCOUNT_TOKEN",
    "INFISICAL_TOKEN",
    "INFISICAL_CLIENT_ID",
    "INFISICAL_CLIENT_SECRET",
    "VAULT_TOKEN",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_known_resolver_credentials_are_listed() {
        for name in [
            "OP_SERVICE_ACCOUNT_TOKEN",
            "INFISICAL_TOKEN",
            "INFISICAL_CLIENT_SECRET",
            "VAULT_TOKEN",
        ] {
            assert!(
                RESOLVER_CREDENTIAL_ENVIRONMENT_VARIABLES.contains(&name),
                "{name}"
            );
        }
    }
}
