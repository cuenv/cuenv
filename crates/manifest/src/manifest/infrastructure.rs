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
///
/// The top-level `providers`, `resources` and `provider_environment` are the
/// configuration used when no environment is selected. Use
/// [`Infrastructure::select`] to decode a project's raw `infrastructure`
/// value for the environment a command runs against.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Infrastructure {
    /// Where managed resource state is stored. One database serves the
    /// top-level configuration and every named environment.
    pub state: InfrastructureState,

    /// Provider plugins, keyed by local provider name (for example `random`).
    #[serde(default)]
    pub providers: BTreeMap<String, InfrastructureProvider>,

    /// Managed resources, keyed by resource name.
    #[serde(default)]
    pub resources: BTreeMap<String, ManagedResourceDeclaration>,

    /// What provider processes inherit from the cuenv process environment.
    #[serde(default, rename = "providerEnvironment")]
    pub provider_environment: ProviderEnvironment,

    /// Complete provider and resource sets selected with `--env`.
    #[serde(default)]
    pub environments: BTreeMap<String, InfrastructureConfiguration>,
}

/// The complete provider and resource configuration for one named environment.
///
/// Based on `#InfrastructureConfiguration` in schema/infrastructure.cue. A
/// selected environment replaces the top-level configuration entirely:
/// nothing is inherited from the top-level `providers`, `resources` or
/// `providerEnvironment`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InfrastructureConfiguration {
    /// Provider plugins, keyed by local provider name.
    #[serde(default)]
    pub providers: BTreeMap<String, InfrastructureProvider>,

    /// Managed resources, keyed by resource name.
    #[serde(default)]
    pub resources: BTreeMap<String, ManagedResourceDeclaration>,

    /// What this environment's provider processes inherit from the cuenv
    /// process environment.
    #[serde(default, rename = "providerEnvironment")]
    pub provider_environment: ProviderEnvironment,
}

/// What a provider process inherits from the cuenv process environment.
///
/// Based on `providerEnvironment` in schema/infrastructure.cue. In both modes
/// the project's own variables that the running action's policy allows are
/// added last.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ProviderEnvironment {
    /// The ambient environment, minus the credentials of cuenv's own secret
    /// resolvers (unless the project passes them explicitly).
    #[default]
    Inherit,
    /// An empty environment except `PATH`, `HOME`, proxy and TLS variables.
    Isolated,
}

/// An infrastructure command that an `allowInfrastructure` policy can name.
///
/// Based on `#InfrastructureAction` in schema/policy.cue; the serde names are
/// the CUE strings, so a name outside this set fails deserialization.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum InfrastructurePolicyAction {
    /// `cuenv infrastructure plan`.
    Plan,
    /// `cuenv infrastructure apply`.
    Apply,
    /// `cuenv infrastructure destroy`.
    Destroy,
    /// `cuenv infrastructure state list`.
    StateList,
    /// `cuenv infrastructure state remove`.
    StateRemove,
    /// `cuenv infrastructure state recover`.
    StateRecover,
    /// `cuenv infrastructure state adopt`.
    StateAdopt,
    /// `cuenv infrastructure unlock`.
    Unlock,
}

impl InfrastructurePolicyAction {
    /// The name used in CUE and in messages.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Apply => "apply",
            Self::Destroy => "destroy",
            Self::StateList => "state-list",
            Self::StateRemove => "state-remove",
            Self::StateRecover => "state-recover",
            Self::StateAdopt => "state-adopt",
            Self::Unlock => "unlock",
        }
    }
}

impl std::fmt::Display for InfrastructurePolicyAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why a project's raw `infrastructure` value could not be selected.
#[derive(Debug, thiserror::Error)]
pub enum InfrastructureSelectionError {
    /// The value is not an object.
    #[error("`infrastructure` must be an object, found {found}")]
    NotAnObject {
        /// The JSON type that was found.
        found: &'static str,
    },
    /// The requested environment is not declared.
    #[error("{}", unknown_environment_message(.requested, .declared))]
    UnknownEnvironment {
        /// The environment that was requested.
        requested: String,
        /// The environments the project declares, in name order.
        declared: Vec<String>,
    },
    /// The top level sets `providerEnvironment` but the selected environment
    /// does not, so the environment would silently fall back to `inherit`.
    #[error(
        "the top-level `providerEnvironment` does not apply to infrastructure environment \
         '{environment}': an environment replaces the whole top-level configuration, so set \
         `providerEnvironment` on it explicitly (`\"inherit\"` or `\"isolated\"`); falling \
         back to `inherit` silently would give providers the ambient environment you meant \
         to withhold"
    )]
    ProviderEnvironmentNotSet {
        /// The selected environment.
        environment: String,
    },
    /// The selected configuration does not match the infrastructure types
    /// (a missing `state`, a misspelled field, an incomplete value).
    #[error("invalid `infrastructure` configuration: {0}")]
    Invalid(#[from] serde_json::Error),
}

fn unknown_environment_message(requested: &str, declared: &[String]) -> String {
    if declared.is_empty() {
        format!(
            "infrastructure environment '{requested}' is not declared; the project declares none"
        )
    } else {
        format!(
            "infrastructure environment '{requested}' is not declared; declared environments: {}",
            declared.join(", ")
        )
    }
}

impl Infrastructure {
    /// Select a complete named configuration while retaining the common state.
    #[must_use]
    pub fn for_environment(&self, name: &str) -> Option<Self> {
        self.environments.get(name).map(|configuration| Self {
            state: self.state.clone(),
            providers: configuration.providers.clone(),
            resources: configuration.resources.clone(),
            provider_environment: configuration.provider_environment,
            environments: BTreeMap::new(),
        })
    }

    /// Strictly decode a project's raw `infrastructure` value for the
    /// configuration a command runs against.
    ///
    /// `environment` is the name given with `--env`, or `None` for the
    /// top-level configuration. Only the selected configuration is decoded:
    /// the other environments (and, when one is selected, the top-level
    /// `providers`, `resources` and `providerEnvironment`) are discarded
    /// first, because ordinary evaluation may leave them as incomplete CUE
    /// values that would fail strict decoding. The result has an empty
    /// `environments` map and holds the selected configuration in
    /// `providers`, `resources` and `provider_environment`.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureSelectionError::NotAnObject`] when the value
    /// is not an object, [`InfrastructureSelectionError::UnknownEnvironment`]
    /// (naming the declared environments) when `environment` is not declared,
    /// [`InfrastructureSelectionError::ProviderEnvironmentNotSet`] when the
    /// top level sets `providerEnvironment` and the selected environment does
    /// not (the key must be present in the raw value; refusing is safer than
    /// carrying the top-level mode over, because an environment replaces the
    /// top-level configuration entirely), and
    /// [`InfrastructureSelectionError::Invalid`] when the selected
    /// configuration does not decode strictly.
    pub fn select(
        mut value: serde_json::Value,
        environment: Option<&str>,
    ) -> Result<Self, InfrastructureSelectionError> {
        let found = json_type_name(&value);
        let object = value
            .as_object_mut()
            .ok_or(InfrastructureSelectionError::NotAnObject { found })?;
        if let Some(name) = environment {
            let declared: Vec<String> = object
                .get("environments")
                .and_then(serde_json::Value::as_object)
                .map(|environments| environments.keys().cloned().collect())
                .unwrap_or_default();
            if !declared.iter().any(|declared_name| declared_name == name) {
                return Err(InfrastructureSelectionError::UnknownEnvironment {
                    requested: name.to_owned(),
                    declared,
                });
            }
            // Presence in the raw value, not the decoded value: an absent key
            // and an explicit `inherit` both decode to `Inherit`.
            let selected_sets_mode = object
                .get("environments")
                .and_then(|environments| environments.get(name))
                .and_then(serde_json::Value::as_object)
                .is_none_or(|configuration| configuration.contains_key("providerEnvironment"));
            if object.contains_key("providerEnvironment") && !selected_sets_mode {
                return Err(InfrastructureSelectionError::ProviderEnvironmentNotSet {
                    environment: name.to_owned(),
                });
            }
            object.remove("providers");
            object.remove("resources");
            object.remove("providerEnvironment");
        }
        if let Some(environments) = object
            .get_mut("environments")
            .and_then(serde_json::Value::as_object_mut)
        {
            environments.retain(|declared_name, _| environment == Some(declared_name.as_str()));
        }
        let infrastructure: Self = serde_json::from_value(value)?;
        match environment {
            None => Ok(infrastructure),
            Some(name) => infrastructure.for_environment(name).ok_or_else(|| {
                InfrastructureSelectionError::UnknownEnvironment {
                    requested: name.to_owned(),
                    declared: Vec::new(),
                }
            }),
        }
    }
}

const fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "a list",
        serde_json::Value::Object(_) => "an object",
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
    fn declared_with_environments() -> serde_json::Value {
        serde_json::json!({
            "state": {"turso": {"url": "libsql://db.turso.io"}},
            "providers": {"random": {"source": "hashicorp/random", "version": "3.9.1"}},
            "resources": {"top": {"type": "random_pet"}},
            "providerEnvironment": "isolated",
            "environments": {
                "dev": {
                    "providers": {"random": {"source": "hashicorp/random", "path": "/bin/p"}},
                    "resources": {"dev": {"type": "random_pet"}},
                    "providerEnvironment": "inherit",
                },
                "prod": {
                    "providers": {"random": null},
                    "providerEnvironment": "isolated",
                },
            },
        })
    }

    #[test]
    fn provider_environment_defaults_to_inherit_and_parses_isolated() {
        let value = serde_json::json!({"state": {"turso": {"url": "libsql://db.turso.io"}}});
        let infrastructure: Infrastructure = serde_json::from_value(value).unwrap();
        assert_eq!(
            infrastructure.provider_environment,
            ProviderEnvironment::Inherit
        );
        let isolated: InfrastructureConfiguration =
            serde_json::from_value(serde_json::json!({"providerEnvironment": "isolated"})).unwrap();
        assert_eq!(isolated.provider_environment, ProviderEnvironment::Isolated);
        let error = serde_json::from_value::<InfrastructureConfiguration>(
            serde_json::json!({"providerEnvironment": "open"}),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unknown variant `open`"), "{error}");
    }

    #[test]
    fn select_without_environment_uses_the_top_level_and_ignores_overlays() {
        let selected = Infrastructure::select(declared_with_environments(), None).unwrap();
        assert!(selected.resources.contains_key("top"));
        assert_eq!(selected.provider_environment, ProviderEnvironment::Isolated);
        assert!(selected.environments.is_empty());
    }

    #[test]
    fn select_replaces_the_top_level_with_the_named_environment() {
        let selected = Infrastructure::select(declared_with_environments(), Some("dev")).unwrap();
        assert_eq!(
            selected.resources.keys().collect::<Vec<_>>(),
            vec!["dev"],
            "top-level resources must not be inherited"
        );
        assert_eq!(selected.providers["random"].path.as_deref(), Some("/bin/p"));
        assert_eq!(selected.provider_environment, ProviderEnvironment::Inherit);
        assert_eq!(selected.state.turso.url, "libsql://db.turso.io");
    }

    #[test]
    fn select_decodes_only_the_selected_environment() {
        // `prod` holds an incomplete provider (null), which must not matter
        // while `dev` is selected, and must fail when `prod` is selected.
        Infrastructure::select(declared_with_environments(), Some("dev")).unwrap();
        let error = Infrastructure::select(declared_with_environments(), Some("prod"))
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("invalid `infrastructure` configuration"),
            "{error}"
        );
    }

    #[test]
    fn select_names_the_declared_environments() {
        let error = Infrastructure::select(declared_with_environments(), Some("qa"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("'qa'"), "{error}");
        assert!(
            error.contains("declared environments: dev, prod"),
            "{error}"
        );

        let none = Infrastructure::select(
            serde_json::json!({"state": {"turso": {"url": "libsql://db.turso.io"}}}),
            Some("qa"),
        )
        .unwrap_err()
        .to_string();
        assert!(none.contains("declares none"), "{none}");
    }

    #[test]
    fn select_rejects_a_value_that_is_not_an_object() {
        for (value, found) in [
            (serde_json::json!(42), "a number"),
            (serde_json::json!(null), "null"),
            (serde_json::json!("x"), "a string"),
            (serde_json::json!([]), "a list"),
        ] {
            let error = Infrastructure::select(value, None).unwrap_err().to_string();
            assert_eq!(
                error,
                format!("`infrastructure` must be an object, found {found}")
            );
        }
    }

    #[test]
    fn select_rejects_misspelled_fields_in_the_selected_environment() {
        let mut value = declared_with_environments();
        value["environments"]["dev"]["resource"] = serde_json::json!({});
        let error = Infrastructure::select(value, Some("dev"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown field `resource`"), "{error}");
    }

    #[test]
    fn a_selected_environment_must_set_the_provider_environment_the_top_level_sets() {
        let mut value = declared_with_environments();
        value["environments"]["dev"]
            .as_object_mut()
            .unwrap()
            .remove("providerEnvironment");
        let error = Infrastructure::select(value, Some("dev")).unwrap_err();
        assert!(
            matches!(
                &error,
                InfrastructureSelectionError::ProviderEnvironmentNotSet { environment }
                    if environment == "dev"
            ),
            "{error:?}"
        );
        let text = error.to_string();
        assert!(text.contains("'dev'"), "{text}");
        assert!(text.contains("`providerEnvironment`"), "{text}");
    }

    #[test]
    fn an_explicit_inherit_satisfies_the_requirement_and_is_not_confused_with_absence() {
        // `inherit` is also what an absent key decodes to: only the raw key
        // tells them apart.
        let selected = Infrastructure::select(declared_with_environments(), Some("dev")).unwrap();
        assert_eq!(selected.provider_environment, ProviderEnvironment::Inherit);
    }

    #[test]
    fn an_environment_without_the_mode_is_fine_when_the_top_level_does_not_set_one() {
        let mut value = declared_with_environments();
        value.as_object_mut().unwrap().remove("providerEnvironment");
        value["environments"]["dev"]
            .as_object_mut()
            .unwrap()
            .remove("providerEnvironment");
        let selected = Infrastructure::select(value.clone(), Some("dev")).unwrap();
        assert_eq!(selected.provider_environment, ProviderEnvironment::Inherit);
        // And the top level selects as before.
        let top = Infrastructure::select(value, None).unwrap();
        assert!(top.resources.contains_key("top"));
    }

    #[test]
    fn the_top_level_mode_alone_does_not_stop_a_run_without_an_environment() {
        let selected = Infrastructure::select(declared_with_environments(), None).unwrap();
        assert_eq!(selected.provider_environment, ProviderEnvironment::Isolated);
    }
}
