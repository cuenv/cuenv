use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ============================================================================
// Infrastructure as Code Types
// ============================================================================

/// Infrastructure managed through Terraform provider plugins.
///
/// Based on `#Infra` in schema/infra.cue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Infra {
    /// Where managed resource state is stored.
    pub state: InfraState,

    /// Provider plugins, keyed by local provider name (e.g. `random`).
    #[serde(default)]
    pub providers: BTreeMap<String, InfraProvider>,

    /// Managed resources, keyed by resource name.
    #[serde(default)]
    pub resources: BTreeMap<String, ManagedResourceSpec>,
}

/// State backend configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InfraState {
    /// Remote Turso (libSQL) database.
    pub turso: TursoState,
}

/// Turso database connection settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TursoState {
    /// Database URL (`libsql://`, `https://`, or `http://` for local sqld).
    pub url: String,

    /// Environment variable holding the database auth token.
    #[serde(default = "default_turso_token_env")]
    pub auth_token_env: String,
}

fn default_turso_token_env() -> String {
    "TURSO_AUTH_TOKEN".to_string()
}

/// A Terraform provider plugin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InfraProvider {
    /// Registry source address, e.g. `hashicorp/random`.
    pub source: String,

    /// Exact provider version to install from the registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Path to a local provider binary; skips registry installation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// Provider configuration block.
    #[serde(default)]
    pub config: serde_json::Map<String, serde_json::Value>,
}

/// A managed resource declaration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedResourceSpec {
    /// Resource type, e.g. `random_pet`.
    #[serde(rename = "type")]
    pub resource_type: String,

    /// Local provider name; defaults to the resource type prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Names of resources that must be created before this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Resource configuration arguments.
    #[serde(default)]
    pub config: serde_json::Map<String, serde_json::Value>,
}

impl ManagedResourceSpec {
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
    fn deserializes_infra_block() {
        let infra: Infra = serde_json::from_value(serde_json::json!({
            "state": {"turso": {"url": "libsql://db.turso.io"}},
            "providers": {"random": {"source": "hashicorp/random", "version": "3.7.2"}},
            "resources": {
                "pet": {"type": "random_pet", "config": {"length": 2}},
                "id": {"type": "random_id", "provider": "random", "dependsOn": ["pet"], "config": {}},
            },
        }))
        .unwrap();
        assert_eq!(infra.state.turso.auth_token_env, "TURSO_AUTH_TOKEN");
        assert_eq!(infra.resources["pet"].provider_name(), "random");
        assert_eq!(infra.resources["id"].depends_on, vec!["pet".to_string()]);
    }

    #[test]
    fn provider_name_defaults_to_type_prefix() {
        let spec = ManagedResourceSpec {
            resource_type: "cloudflare_dns_record".into(),
            provider: None,
            depends_on: Vec::new(),
            config: serde_json::Map::new(),
        };
        assert_eq!(spec.provider_name(), "cloudflare");
    }
}
