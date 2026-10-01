//! The environment variables cuenv's own secret machinery reads.
//!
//! These hold the keys to the user's secret stores, or point at them. cuenv
//! reads them to resolve secrets, redacts the values that are secret from its
//! output, and does not hand any of them to processes it starts for other
//! purposes (infrastructure providers) unless a project passes them
//! explicitly.
//!
//! A variable is listed by its exact name or, for families whose names are
//! computed (1Password writes `OP_SESSION_<account>`), by a prefix. One table
//! ([`RESOLVER_ENVIRONMENT_VARIABLES`]) is the source of truth for both
//! withholding and redaction, so the two cannot drift apart.

use std::ffi::OsString;

/// How a variable's name is matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamePattern {
    /// The whole name.
    Exact(&'static str),
    /// Any name that starts with this text.
    Prefix(&'static str),
}

impl NamePattern {
    /// Whether `name` is matched by this pattern.
    #[must_use]
    pub fn matches(self, name: &str) -> bool {
        match self {
            Self::Exact(exact) => name == exact,
            Self::Prefix(prefix) => name.starts_with(prefix),
        }
    }
}

/// What a variable's value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// A secret: withheld from providers and redacted from output.
    Credential,
    /// Not secret, but it locates a secret store: withheld from providers,
    /// not redacted (redacting a host name would garble ordinary output).
    Endpoint,
}

/// One entry of [`RESOLVER_ENVIRONMENT_VARIABLES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolverVariable {
    /// Which names the entry covers.
    pub pattern: NamePattern,
    /// What the values are.
    pub kind: ValueKind,
}

const fn credential(pattern: NamePattern) -> ResolverVariable {
    ResolverVariable {
        pattern,
        kind: ValueKind::Credential,
    }
}

const fn endpoint(pattern: NamePattern) -> ResolverVariable {
    ResolverVariable {
        pattern,
        kind: ValueKind::Endpoint,
    }
}

/// Variables that exist only to authenticate or locate cuenv's own secret
/// machinery (1Password, Infisical, Vault, and the salt that fingerprints
/// secrets in task cache keys).
///
/// Cloud platform credentials that the platform's own tools and Terraform
/// providers read as well (`AWS_*`, `GOOGLE_APPLICATION_CREDENTIALS`,
/// `GOOGLE_OAUTH_ACCESS_TOKEN`) are not listed: withholding them would break
/// the providers that need them.
pub const RESOLVER_ENVIRONMENT_VARIABLES: &[ResolverVariable] = &[
    credential(NamePattern::Exact("OP_SERVICE_ACCOUNT_TOKEN")),
    credential(NamePattern::Exact("OP_CONNECT_TOKEN")),
    // The 1Password CLI's session tokens: `OP_SESSION_<account shorthand>`.
    credential(NamePattern::Prefix("OP_SESSION_")),
    endpoint(NamePattern::Exact("OP_CONNECT_HOST")),
    credential(NamePattern::Exact("INFISICAL_TOKEN")),
    credential(NamePattern::Exact("INFISICAL_CLIENT_ID")),
    credential(NamePattern::Exact("INFISICAL_CLIENT_SECRET")),
    credential(NamePattern::Exact("VAULT_TOKEN")),
    credential(NamePattern::Exact("CUENV_SECRET_SALT")),
    credential(NamePattern::Exact("CUENV_SECRET_SALT_PREV")),
];

/// The table entry that covers `name`, if any.
fn entry_for(name: &str) -> Option<&'static ResolverVariable> {
    RESOLVER_ENVIRONMENT_VARIABLES
        .iter()
        .find(|entry| entry.pattern.matches(name))
}

/// Whether `name` is one of the variables cuenv keeps from providers: a
/// resolver credential, or the endpoint of a secret store.
#[must_use]
pub fn is_resolver_environment_variable(name: &str) -> bool {
    entry_for(name).is_some()
}

/// Whether the value of `name` is a secret that must be redacted from output.
#[must_use]
pub fn is_resolver_credential(name: &str) -> bool {
    entry_for(name).is_some_and(|entry| entry.kind == ValueKind::Credential)
}

/// The variable names to withhold from a process that must not hold cuenv's
/// secret machinery.
///
/// That is every exact name in [`RESOLVER_ENVIRONMENT_VARIABLES`], and the
/// names in `ambient` that a prefix entry covers (`OP_SESSION_<account>`
/// exists only once the user signed in). Each name once, in order. Names
/// that are not valid unicode cannot match and are skipped.
#[must_use]
pub fn resolver_environment_variable_names(
    ambient: impl IntoIterator<Item = OsString>,
) -> Vec<String> {
    let mut names: Vec<String> = RESOLVER_ENVIRONMENT_VARIABLES
        .iter()
        .filter_map(|entry| match entry.pattern {
            NamePattern::Exact(name) => Some(name.to_string()),
            NamePattern::Prefix(_) => None,
        })
        .chain(
            ambient
                .into_iter()
                .filter_map(|name| name.into_string().ok())
                .filter(|name| is_resolver_environment_variable(name)),
        )
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The non-empty values of the resolver credentials in `environment`, for
/// registration with the redaction registry.
#[must_use]
pub fn resolver_credential_values(
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<String> {
    environment
        .into_iter()
        .filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            let value = value.into_string().ok()?;
            (is_resolver_credential(&name) && !value.is_empty()).then_some(value)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(text: &str) -> OsString {
        OsString::from(text)
    }

    /// The names the resolver crates and the commands that use them name in
    /// their source are the ground truth for what cuenv's secret machinery
    /// reads. A resolver that learns a new credential variable fails this
    /// test until the table, which drives withholding and redaction, lists it
    /// too; the table cannot drift from the code it protects.
    #[test]
    fn every_credential_variable_the_resolver_crates_name_is_in_the_table() {
        // Names these crates use that are configuration, not credentials.
        const NOT_CREDENTIALS: &[&str] = &["INFISICAL_API_URL", "INFISICAL_ORGANIZATION_SLUG"];
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut found = Vec::new();
        for directory in ["1password", "infisical", "aws", "gcp"] {
            found.extend(variable_names_in(&crates.join(directory).join("src")));
        }
        found.extend(variable_names_in(
            &crates.join("cuenv").join("src").join("commands"),
        ));
        assert!(
            found.iter().any(|name| name == "OP_SERVICE_ACCOUNT_TOKEN"),
            "the scan must find the resolvers' sources, found {found:?}"
        );
        let missing: Vec<&String> = found
            .iter()
            .filter(|name| looks_like_a_resolver_credential(name))
            .filter(|name| !NOT_CREDENTIALS.contains(&name.as_str()))
            .filter(|name| !is_resolver_environment_variable(name))
            .collect();
        assert!(
            missing.is_empty(),
            "credential variables missing from RESOLVER_ENVIRONMENT_VARIABLES: {missing:?}"
        );
    }

    #[test]
    fn the_scan_would_notice_a_credential_missing_from_the_table() {
        let names = quoted_names("let t = std::env::var(\"OP_NEW_API_TOKEN\"); \"plain text\"");
        assert_eq!(names, ["OP_NEW_API_TOKEN"]);
        assert!(looks_like_a_resolver_credential("OP_NEW_API_TOKEN"));
        assert!(!is_resolver_environment_variable("OP_NEW_API_TOKEN"));
        assert!(!looks_like_a_resolver_credential("OP_TEST_LOG"));
    }

    /// Upper-case string literals of the Rust files under `directory`.
    fn variable_names_in(directory: &std::path::Path) -> Vec<String> {
        let mut names = Vec::new();
        let Ok(entries) = std::fs::read_dir(directory) else {
            return names;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                names.extend(variable_names_in(&path));
            } else if path.extension().is_some_and(|extension| extension == "rs")
                && let Ok(source) = std::fs::read_to_string(&path)
            {
                names.extend(quoted_names(&source));
            }
        }
        names
    }

    /// The `"NAME"` literals of `source` where NAME is upper case letters,
    /// numerals and underscores.
    fn quoted_names(source: &str) -> Vec<String> {
        source
            .split('"')
            .skip(1)
            .step_by(2)
            .filter(|literal| {
                literal.len() > 4
                    && literal.starts_with(|first: char| first.is_ascii_uppercase())
                    && literal
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            })
            .map(str::to_string)
            .collect()
    }

    /// A name in a family the resolvers' tools define that reads as a
    /// credential.
    fn looks_like_a_resolver_credential(name: &str) -> bool {
        const FAMILIES: &[&str] = &["OP_", "INFISICAL_", "VAULT_"];
        const CREDENTIAL_WORDS: &[&str] = &["TOKEN", "SECRET", "SESSION", "PASSWORD", "CLIENT_ID"];
        FAMILIES.iter().any(|family| name.starts_with(family))
            && CREDENTIAL_WORDS.iter().any(|word| name.contains(word))
    }

    #[test]
    fn the_known_resolver_credentials_are_credentials() {
        for name in [
            "OP_SERVICE_ACCOUNT_TOKEN",
            "OP_CONNECT_TOKEN",
            "INFISICAL_TOKEN",
            "INFISICAL_CLIENT_SECRET",
            "VAULT_TOKEN",
            "CUENV_SECRET_SALT",
            "CUENV_SECRET_SALT_PREV",
        ] {
            assert!(is_resolver_credential(name), "{name}");
            assert!(is_resolver_environment_variable(name), "{name}");
        }
    }

    #[test]
    fn a_prefix_covers_computed_names_and_only_those() {
        assert!(is_resolver_credential("OP_SESSION_my_account"));
        assert!(is_resolver_credential("OP_SESSION_"));
        assert!(!is_resolver_environment_variable("OP_SESSIONS"));
        assert!(!is_resolver_environment_variable("XOP_SESSION_a"));
        assert!(!is_resolver_environment_variable("AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn the_connect_host_is_withheld_but_not_a_secret() {
        assert!(is_resolver_environment_variable("OP_CONNECT_HOST"));
        assert!(!is_resolver_credential("OP_CONNECT_HOST"));
    }

    #[test]
    fn credential_values_come_from_exact_and_prefix_names_only() {
        let values = resolver_credential_values([
            (os("OP_SESSION_acme"), os("session-value")),
            (os("OP_CONNECT_TOKEN"), os("connect-value")),
            (os("OP_CONNECT_HOST"), os("https://connect.internal")),
            (os("CUENV_SECRET_SALT"), os("salt-value")),
            (os("VAULT_TOKEN"), os("")),
            (os("PATH"), os("/usr/bin")),
        ]);
        assert_eq!(values, ["session-value", "connect-value", "salt-value"]);
    }

    #[test]
    fn withheld_names_are_every_exact_name_and_the_ambient_prefix_matches() {
        let names = resolver_environment_variable_names([
            os("PATH"),
            os("OP_SESSION_b"),
            os("OP_SESSION_a"),
            os("OP_SESSION_b"),
        ]);
        for expected in [
            "OP_SESSION_a",
            "OP_SESSION_b",
            "VAULT_TOKEN",
            "OP_CONNECT_HOST",
        ] {
            assert!(
                names.iter().any(|name| name == expected),
                "{expected}: {names:?}"
            );
        }
        assert!(!names.iter().any(|name| name == "PATH"), "{names:?}");
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(names, sorted, "each name once, in order");
    }
}
