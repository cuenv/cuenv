//! GitHub Actions stage renderer implementation.
//!
//! Converts cuenv IR stage tasks into GitHub Actions workflow steps.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;

/// Transform CI-agnostic secret reference syntax to GitHub Actions syntax.
///
/// Converts `${VAR_NAME}` to `${{ secrets.VAR_NAME }}` for env var values
/// that are entirely a secret reference (not embedded references).
///
/// # Examples
/// - `${FOO}` -> `${{ secrets.FOO }}`
/// - `${FOO_BAR_123}` -> `${{ secrets.FOO_BAR_123 }}`
/// - `prefix-${VAR}` -> unchanged (embedded reference)
/// - `regular_value` -> unchanged
#[must_use]
pub fn transform_secret_ref(value: &str) -> String {
    // Check for pattern: ${UPPERCASE_VAR_NAME}
    let trimmed = value.trim();
    if !trimmed.starts_with("${") || !trimmed.ends_with('}') {
        return value.to_string();
    }

    // Extract the variable name (between ${ and })
    let var_name = &trimmed[2..trimmed.len() - 1];

    // Validate: must be non-empty, start with uppercase letter, contain only A-Z, 0-9, _
    let Some(first_char) = var_name.chars().next() else {
        return value.to_string();
    };

    if !first_char.is_ascii_uppercase() {
        return value.to_string();
    }

    let is_valid_var_name = var_name
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');

    if !is_valid_var_name {
        return value.to_string();
    }

    format!("${{{{ secrets.{var_name} }}}}")
}

use cuenv_ci::StageRenderer;
use cuenv_ci::ir::StageTask;

use super::schema::Step;

/// Specification for a GitHub Action step.
///
/// This is GitHub-specific and extracted from the provider-agnostic `provider_hints`
/// field in `StageTask`. When present under the `github_action` key, GitHub emitters
/// will render this task as a `uses:` step instead of a `run:` step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActionSpec {
    /// GitHub Action reference (e.g., "actions/checkout@v4")
    pub uses: String,

    /// Action inputs
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub inputs: HashMap<String, serde_json::Value>,
}

impl ActionSpec {
    /// Try to extract an ActionSpec from a StageTask's provider_hints.
    ///
    /// Looks for the `github_action` key in the provider_hints JSON.
    #[must_use]
    pub fn from_stage_task(task: &StageTask) -> Option<Self> {
        let hints = task.provider_hints.as_ref()?;
        let action_value = hints.get("github_action")?;
        serde_json::from_value(action_value.clone()).ok()
    }
}

/// Renders stage tasks as GitHub Actions workflow steps.
///
/// Handles both action-based steps (via `ActionSpec` in provider_hints) and run-based steps.
#[derive(Debug, Clone, Default)]
pub struct GitHubStageRenderer;

impl GitHubStageRenderer {
    /// Create a new GitHub stage renderer
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl StageRenderer for GitHubStageRenderer {
    type Step = Step;
    type Error = Infallible;

    fn render_task(&self, task: &StageTask) -> Result<Step, Self::Error> {
        // If a GitHub Action is specified in provider_hints, use it
        if let Some(action) = ActionSpec::from_stage_task(task) {
            let mut step = Step::uses(&action.uses).with_name(task.label());

            // Add action inputs (converting from serde_json::Value to serde_yaml::Value)
            for (key, value) in &action.inputs {
                let yaml_value = json_to_yaml_value(value);
                step.with_inputs.insert(key.clone(), yaml_value);
            }

            // Add environment variables (transform secret references)
            for (key, value) in &task.env {
                step.env.insert(key.clone(), transform_secret_ref(value));
            }

            return Ok(step);
        }

        // Otherwise, render as a run step
        let command = task.command_string();
        let mut step = Step::run(&command).with_name(task.label());

        // Add environment variables (transform secret references)
        for (key, value) in &task.env {
            step.env.insert(key.clone(), transform_secret_ref(value));
        }

        Ok(step)
    }
}

/// Convert a `serde_json::Value` to a `serde_yaml::Value`.
fn json_to_yaml_value(json: &serde_json::Value) -> serde_yaml::Value {
    match json {
        serde_json::Value::Null => serde_yaml::Value::Null,
        serde_json::Value::Bool(b) => serde_yaml::Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_yaml::Value::Number(serde_yaml::Number::from(i))
            } else if let Some(f) = n.as_f64() {
                serde_yaml::Value::Number(serde_yaml::Number::from(f))
            } else {
                serde_yaml::Value::Null
            }
        }
        serde_json::Value::String(s) => serde_yaml::Value::String(s.clone()),
        serde_json::Value::Array(arr) => {
            serde_yaml::Value::Sequence(arr.iter().map(json_to_yaml_value).collect())
        }
        serde_json::Value::Object(obj) => {
            let mapping: serde_yaml::Mapping = obj
                .iter()
                .map(|(k, v)| (serde_yaml::Value::String(k.clone()), json_to_yaml_value(v)))
                .collect();
            serde_yaml::Value::Mapping(mapping)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_ci::ir::StageConfiguration;

    /// Helper to create provider_hints with a GitHub action
    fn make_github_action_hints(
        uses: &str,
        inputs: HashMap<String, serde_json::Value>,
    ) -> serde_json::Value {
        let mut action = serde_json::Map::new();
        action.insert(
            "uses".to_string(),
            serde_json::Value::String(uses.to_string()),
        );
        if !inputs.is_empty() {
            action.insert(
                "inputs".to_string(),
                serde_json::Value::Object(inputs.into_iter().collect()),
            );
        }
        let mut hints = serde_json::Map::new();
        hints.insert(
            "github_action".to_string(),
            serde_json::Value::Object(action),
        );
        serde_json::Value::Object(hints)
    }

    #[test]
    fn test_render_run_step() {
        let task = StageTask {
            id: "setup-cuenv".to_string(),
            provider: "cuenv".to_string(),
            label: Some("Setup cuenv".to_string()),
            command: vec![
                "nix".to_string(),
                "build".to_string(),
                ".#cuenv".to_string(),
            ],
            env: {
                let mut env = HashMap::new();
                env.insert(
                    "GITHUB_TOKEN".to_string(),
                    "${{ secrets.GITHUB_TOKEN }}".to_string(),
                );
                env
            },
            ..Default::default()
        };

        let renderer = GitHubStageRenderer::new();
        let step = renderer.render_task(&task).unwrap();

        assert_eq!(step.name, Some("Setup cuenv".to_string()));
        assert_eq!(step.run, Some("nix build .#cuenv".to_string()));
        assert!(step.uses.is_none());
        assert_eq!(
            step.env.get("GITHUB_TOKEN"),
            Some(&"${{ secrets.GITHUB_TOKEN }}".to_string())
        );
    }

    #[test]
    fn test_render_action_step() {
        let mut inputs = HashMap::new();
        inputs.insert(
            "extra-conf".to_string(),
            serde_json::Value::String("accept-flake-config = true".to_string()),
        );

        let task = StageTask {
            id: "install-nix".to_string(),
            provider: "nix".to_string(),
            label: Some("Install Nix".to_string()),
            command: vec![
                "curl".to_string(),
                "-L".to_string(),
                "https://install.nix".to_string(),
            ],
            provider_hints: Some(make_github_action_hints(
                "DeterminateSystems/nix-installer-action@v16",
                inputs,
            )),
            ..Default::default()
        };

        let renderer = GitHubStageRenderer::new();
        let step = renderer.render_task(&task).unwrap();

        assert_eq!(step.name, Some("Install Nix".to_string()));
        assert_eq!(
            step.uses,
            Some("DeterminateSystems/nix-installer-action@v16".to_string())
        );
        assert!(step.run.is_none());
        assert!(step.with_inputs.contains_key("extra-conf"));
    }

    #[test]
    fn test_render_setup_stages() {
        let mut stages = StageConfiguration::default();
        stages.setup.push(StageTask {
            id: "setup-cuenv".to_string(),
            command: vec!["nix".to_string(), "build".to_string()],
            ..Default::default()
        });
        stages.setup.push(StageTask {
            id: "setup-1password".to_string(),
            command: vec![
                "cuenv".to_string(),
                "secrets".to_string(),
                "setup".to_string(),
                "onepassword".to_string(),
            ],
            env: {
                let mut env = HashMap::new();
                // Use CI-agnostic format (what contributors produce)
                env.insert(
                    "OP_SERVICE_ACCOUNT_TOKEN".to_string(),
                    "${OP_SERVICE_ACCOUNT_TOKEN}".to_string(),
                );
                env
            },
            ..Default::default()
        });

        let renderer = GitHubStageRenderer::new();
        let steps = renderer.render_setup(&stages).unwrap();

        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].run, Some("nix build".to_string()));
        assert_eq!(
            steps[1].run,
            Some("cuenv secrets setup onepassword".to_string())
        );
        // Verify transformation: ${VAR} -> ${{ secrets.VAR }}
        assert_eq!(
            steps[1].env.get("OP_SERVICE_ACCOUNT_TOKEN"),
            Some(&"${{ secrets.OP_SERVICE_ACCOUNT_TOKEN }}".to_string())
        );
    }

    #[test]
    fn test_label_fallback() {
        let task_without_label = StageTask {
            id: "setup-1password".to_string(),
            command: vec!["cuenv".to_string()],
            ..Default::default()
        };

        let renderer = GitHubStageRenderer::new();
        let step = renderer.render_task(&task_without_label).unwrap();

        // Falls back to ID when no label
        assert_eq!(step.name, Some("setup-1password".to_string()));
    }

    #[test]
    fn test_transform_secret_ref() {
        // Basic transformation
        assert_eq!(transform_secret_ref("${FOO}"), "${{ secrets.FOO }}");

        // With numbers and underscores
        assert_eq!(
            transform_secret_ref("${FOO_BAR_123}"),
            "${{ secrets.FOO_BAR_123 }}"
        );

        // Real-world example
        assert_eq!(
            transform_secret_ref("${OP_SERVICE_ACCOUNT_TOKEN}"),
            "${{ secrets.OP_SERVICE_ACCOUNT_TOKEN }}"
        );

        // Embedded reference - should NOT transform
        assert_eq!(
            transform_secret_ref("prefix-${VAR}-suffix"),
            "prefix-${VAR}-suffix"
        );

        // Regular value - no change
        assert_eq!(transform_secret_ref("regular_value"), "regular_value");

        // Already correct syntax - no change (idempotent for final output)
        assert_eq!(
            transform_secret_ref("${{ secrets.VAR }}"),
            "${{ secrets.VAR }}"
        );

        // Empty value
        assert_eq!(transform_secret_ref(""), "");

        // Lowercase variable (not a secret ref convention)
        assert_eq!(transform_secret_ref("${foo}"), "${foo}");

        // Empty braces
        assert_eq!(transform_secret_ref("${}"), "${}");
    }

    #[test]
    fn test_action_spec_from_stage_task() {
        let mut inputs = HashMap::new();
        inputs.insert(
            "key".to_string(),
            serde_json::Value::String("value".to_string()),
        );

        let task = StageTask {
            id: "test".to_string(),
            provider_hints: Some(make_github_action_hints("actions/checkout@v4", inputs)),
            ..Default::default()
        };

        let action = ActionSpec::from_stage_task(&task).expect("Should extract action");
        assert_eq!(action.uses, "actions/checkout@v4");
        assert!(action.inputs.contains_key("key"));
    }

    #[test]
    fn test_action_spec_from_stage_task_without_hints() {
        let task = StageTask {
            id: "test".to_string(),
            provider_hints: None,
            ..Default::default()
        };

        assert!(ActionSpec::from_stage_task(&task).is_none());
    }
}
