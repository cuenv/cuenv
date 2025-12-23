//! GitHub Actions stage renderer implementation.
//!
//! Converts cuenv IR stage tasks into GitHub Actions workflow steps.

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

/// Renders stage tasks as GitHub Actions workflow steps.
///
/// Handles both action-based steps (via `ActionSpec`) and run-based steps.
#[derive(Debug, Clone, Default)]
pub struct GitHubStageRenderer;

impl GitHubStageRenderer {
    /// Create a new GitHub stage renderer
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl StageRenderer for GitHubStageRenderer {
    type Step = Step;
    type Error = Infallible;

    fn render_task(&self, task: &StageTask) -> Result<Step, Self::Error> {
        // If a GitHub Action is specified, use it
        if let Some(action) = &task.action {
            let mut step = Step::uses(&action.uses).with_name(task.label());

            // Add action inputs
            for (key, value) in &action.inputs {
                step.with_inputs.insert(key.clone(), value.clone());
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

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_ci::ir::{ActionSpec, StageConfiguration};
    use std::collections::HashMap;

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
        let task = StageTask {
            id: "install-nix".to_string(),
            provider: "nix".to_string(),
            label: Some("Install Nix".to_string()),
            command: vec![
                "curl".to_string(),
                "-L".to_string(),
                "https://install.nix".to_string(),
            ],
            action: Some(ActionSpec {
                uses: "DeterminateSystems/nix-installer-action@v16".to_string(),
                inputs: {
                    let mut inputs = HashMap::new();
                    inputs.insert(
                        "extra-conf".to_string(),
                        serde_yaml::Value::String("accept-flake-config = true".to_string()),
                    );
                    inputs
                },
            }),
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
}
