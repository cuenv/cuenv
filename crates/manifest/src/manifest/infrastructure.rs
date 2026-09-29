use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

// ============================================================================
// Infrastructure as Code Types
// ============================================================================

/// Infrastructure managed through Terraform provider plugins.
///
/// Based on `#Infrastructure` in schema/infrastructure.cue.
///
/// Every infrastructure type rejects unknown fields. The schema closes these
/// definitions too, but a project that does not unify with `#Project` (or a
/// caller that builds the JSON itself) would otherwise have a misspelled
/// field silently ignored: `resource:` for `resources:` would read as "no
/// resources" and plan the deletion of everything. Hidden fields, definitions
/// and `let` bindings are never exported, so they are unaffected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Infrastructure {
    /// Where managed resource state is stored.
    pub state: InfrastructureState,

    /// Provider plugins, keyed by local provider name (for example `random`).
    #[serde(default)]
    pub providers: BTreeMap<String, InfrastructureProvider>,

    /// Managed resources, keyed by resource name.
    #[serde(default)]
    pub resources: BTreeMap<String, ManagedResourceDeclaration>,

    /// Complete provider and resource sets selected with `--env`.
    #[serde(default)]
    pub environments: BTreeMap<String, InfrastructureConfiguration>,
}

/// The complete provider and resource configuration for one named environment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InfrastructureConfiguration {
    #[serde(default)]
    pub providers: BTreeMap<String, InfrastructureProvider>,
    #[serde(default)]
    pub resources: BTreeMap<String, ManagedResourceDeclaration>,
}

impl Infrastructure {
    /// Select a complete named configuration while retaining the common state.
    #[must_use]
    pub fn for_environment(&self, name: &str) -> Option<Self> {
        self.environments.get(name).map(|configuration| Self {
            state: self.state.clone(),
            providers: configuration.providers.clone(),
            resources: configuration.resources.clone(),
            environments: BTreeMap::new(),
        })
    }
}

/// State backend configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InfrastructureState {
    /// Remote Turso (libSQL) database.
    pub turso: TursoState,
}

/// Turso database connection settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TursoState {
    /// Database URL: `libsql://`, `https://` or `wss://`; `http://` and
    /// `ws://` only for a loopback host (a local `sqld`).
    pub url: String,

    /// Environment variable holding the database authentication token.
    #[serde(default = "default_turso_authentication_token_environment_variable")]
    pub authentication_token_environment_variable: String,
}

fn default_turso_authentication_token_environment_variable() -> String {
    "TURSO_AUTH_TOKEN".to_string()
}

/// A Terraform provider plugin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InfrastructureProvider {
    /// Registry source address, for example `hashicorp/random`.
    pub source: String,

    /// Exact provider version to install from the registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Path to a local provider binary; skips registry installation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// Provider configuration block.
    #[serde(default, deserialize_with = "deserialize_configuration")]
    pub configuration: serde_json::Map<String, serde_json::Value>,
}

/// A managed resource declaration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedResourceDeclaration {
    /// Resource type, for example `random_pet`.
    #[serde(rename = "type")]
    pub resource_type: String,

    /// Local provider name; defaults to the resource type prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Resources that must be applied before (and destroyed after) this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Resource configuration arguments.
    #[serde(default, deserialize_with = "deserialize_configuration")]
    pub configuration: serde_json::Map<String, serde_json::Value>,
}

/// Deserializes a `configuration` block.
///
/// A missing field is an empty configuration (through `#[serde(default)]`).
/// An explicit `null` is rejected rather than treated as empty. CUE
/// evaluation of the `infrastructure` block requires concrete values, so an
/// unresolved reference (such as a shadowed import alias) fails there with
/// its own error and never reaches this point as `null`; a `null` here means
/// the value itself is `null`.
fn deserialize_configuration<'de, D>(
    deserializer: D,
) -> Result<serde_json::Map<String, serde_json::Value>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<serde_json::Map<String, serde_json::Value>>::deserialize(deserializer)?.ok_or_else(
        || {
            serde::de::Error::custom(
                "`configuration` must be an object, found null; leave it out for an empty \
                 configuration",
            )
        },
    )
}

impl ManagedResourceDeclaration {
    /// Local provider name: explicit `provider`, otherwise the resource
    /// type up to the first underscore (Terraform's convention).
    #[must_use]
    pub fn provider_name(&self) -> &str {
        self.provider.as_deref().unwrap_or_else(|| {
            self.resource_type
                .split_once('_')
                .map_or(self.resource_type.as_str(), |(prefix, _)| prefix)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_infrastructure_block() {
        let infrastructure: Infrastructure = serde_json::from_value(serde_json::json!({
            "state": {"turso": {"url": "libsql://db.turso.io"}},
            "providers": {"random": {"source": "hashicorp/random", "version": "3.9.1"}},
            "resources": {
                "pet": {"type": "random_pet", "configuration": {"length": 2}},
                "identifier": {"type": "random_id", "provider": "random", "dependsOn": ["pet"], "configuration": {}},
            },
        }))
        .unwrap();
        assert_eq!(
            infrastructure
                .state
                .turso
                .authentication_token_environment_variable,
            "TURSO_AUTH_TOKEN"
        );
        assert_eq!(infrastructure.resources["pet"].provider_name(), "random");
        assert_eq!(
            infrastructure.resources["identifier"].depends_on,
            vec!["pet".to_string()]
        );
    }

    #[test]
    fn missing_configuration_is_empty() {
        let provider: InfrastructureProvider =
            serde_json::from_value(serde_json::json!({"source": "hashicorp/random"})).unwrap();
        assert!(provider.configuration.is_empty());
        let declaration: ManagedResourceDeclaration =
            serde_json::from_value(serde_json::json!({"type": "random_pet"})).unwrap();
        assert!(declaration.configuration.is_empty());
    }

    #[test]
    fn object_configuration_is_preserved() {
        let declaration: ManagedResourceDeclaration = serde_json::from_value(serde_json::json!({
            "type": "random_password",
            "configuration": {"length": 16, "special": false},
        }))
        .unwrap();
        assert_eq!(declaration.configuration["length"], 16);
        assert_eq!(declaration.configuration["special"], false);
    }

    #[test]
    fn null_provider_configuration_is_rejected() {
        let error = serde_json::from_value::<InfrastructureProvider>(serde_json::json!({
            "source": "hashicorp/random",
            "configuration": null,
        }))
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("`configuration` must be an object, found null"),
            "{error}"
        );
        assert!(error.contains("leave it out"), "{error}");
    }

    #[test]
    fn null_resource_configuration_is_rejected() {
        let error = serde_json::from_value::<Infrastructure>(serde_json::json!({
            "state": {"turso": {"url": "libsql://db.turso.io"}},
            "resources": {"pet": {"type": "random_pet", "configuration": null}},
        }))
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("`configuration` must be an object, found null"),
            "{error}"
        );
    }

    #[test]
    fn non_object_configuration_is_rejected() {
        let error = serde_json::from_value::<ManagedResourceDeclaration>(serde_json::json!({
            "type": "random_pet",
            "configuration": [1, 2],
        }))
        .unwrap_err()
        .to_string();
        assert!(error.contains("expected a map"), "{error}");
    }

    #[test]
    fn unknown_fields_are_rejected_at_every_level() {
        let valid = serde_json::json!({
            "state": {"turso": {"url": "libsql://db.turso.io"}},
            "providers": {"random": {"source": "hashicorp/random", "version": "3.9.1"}},
            "resources": {"pet": {"type": "random_pet"}},
        });
        serde_json::from_value::<Infrastructure>(valid.clone()).unwrap();

        let typos: [(&[&str], &str); 6] = [
            (&[], "resource"),
            (&["state"], "tursoo"),
            (&["state", "turso"], "authTokenEnv"),
            (&["providers", "random"], "sourcee"),
            (&["providers", "random"], "versions"),
            (&["resources", "pet"], "configurations"),
        ];
        for (path, field) in typos {
            let mut document = valid.clone();
            let target = path
                .iter()
                .fold(&mut document, |value, key| &mut value[*key]);
            target[field] = serde_json::json!({});
            let error = serde_json::from_value::<Infrastructure>(document)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(&format!("unknown field `{field}`")),
                "{field}: {error}"
            );
        }
    }

    #[test]
    fn provider_name_defaults_to_type_prefix() {
        let declaration = ManagedResourceDeclaration {
            resource_type: "cloudflare_dns_record".into(),
            provider: None,
            depends_on: Vec::new(),
            configuration: serde_json::Map::new(),
        };
        assert_eq!(declaration.provider_name(), "cloudflare");
    }
}
