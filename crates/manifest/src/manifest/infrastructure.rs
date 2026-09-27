use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

// ============================================================================
// Infrastructure as Code Types
// ============================================================================

/// Infrastructure managed through Terraform provider plugins.
///
/// Based on `#Infrastructure` in schema/infrastructure.cue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Infrastructure {
    /// Where managed resource state is stored.
    pub state: InfrastructureState,

    /// Provider plugins, keyed by local provider name (for example `random`).
    #[serde(default)]
    pub providers: BTreeMap<String, InfrastructureProvider>,

    /// Managed resources, keyed by resource name.
    #[serde(default)]
    pub resources: BTreeMap<String, ManagedResourceDeclaration>,
}

/// State backend configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InfrastructureState {
    /// Remote Turso (libSQL) database.
    pub turso: TursoState,
}

/// Turso database connection settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TursoState {
    /// Database URL (`libsql://`, `https://`, or `http://` for local sqld).
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
#[serde(rename_all = "camelCase")]
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
/// An explicit `null` is rejected with an actionable message: the CUE export
/// produces `null` for an unresolved reference, and the usual cause is an
/// import alias shadowed by a provider or resource key of the same name
/// (`providers: random: configuration: random.#ProviderConfig`).
fn deserialize_configuration<'de, D>(
    deserializer: D,
) -> Result<serde_json::Map<String, serde_json::Value>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<serde_json::Map<String, serde_json::Value>>::deserialize(deserializer)?.ok_or_else(
        || {
            serde::de::Error::custom(
                "`configuration` must be an object, found null; a null configuration usually \
                 means an import alias is shadowed by a provider or resource key of the same \
                 name (for example `providers: random: configuration: random.#ProviderConfig`), \
                 so rename the import",
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
            "providers": {"random": {"source": "hashicorp/random", "version": "3.7.2"}},
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
    fn null_provider_configuration_explains_shadowed_import() {
        let error = serde_json::from_value::<InfrastructureProvider>(serde_json::json!({
            "source": "hashicorp/random",
            "configuration": null,
        }))
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("`configuration` must be an object"),
            "{error}"
        );
        assert!(error.contains("import alias is shadowed"), "{error}");
    }

    #[test]
    fn null_resource_configuration_explains_shadowed_import() {
        let error = serde_json::from_value::<Infrastructure>(serde_json::json!({
            "state": {"turso": {"url": "libsql://db.turso.io"}},
            "resources": {"pet": {"type": "random_pet", "configuration": null}},
        }))
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("`configuration` must be an object"),
            "{error}"
        );
        assert!(
            error.contains("provider or resource key of the same name"),
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
