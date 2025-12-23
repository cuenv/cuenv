//! Stage task rendering traits for platform-specific CI step generation.
//!
//! This module provides the `StageRenderer` trait which allows contributors
//! to specify tasks once in the IR, while emitters render them appropriately
//! for each CI platform (GitHub Actions, Buildkite, Local, etc.).
//!
//! ## Architecture
//!
//! 1. Contributors produce `StageTask` with optional `ActionSpec` for GitHub
//! 2. Emitters implement `StageRenderer` to convert tasks to native steps
//! 3. Adding new contributors requires NO changes to emitters
//!
//! ## Example
//!
//! ```rust,ignore
//! // GitHub emitter implementation
//! impl StageRenderer for GitHubStageRenderer {
//!     type Step = Step;
//!     type Error = Infallible;
//!
//!     fn render_task(&self, task: &StageTask) -> Result<Self::Step, Self::Error> {
//!         if let Some(action) = &task.action {
//!             // Render as GitHub Action
//!             Ok(Step::uses(&action.uses).with_name(task.label()))
//!         } else {
//!             // Render as run step
//!             Ok(Step::run(&task.command.join(" ")).with_name(task.label()))
//!         }
//!     }
//! }
//! ```

use crate::ir::{StageConfiguration, StageTask};

/// Trait for rendering stage tasks to platform-specific CI steps.
///
/// Emitters implement this trait to convert the platform-agnostic `StageTask`
/// into native CI step types (e.g., GitHub Actions steps, Buildkite commands).
pub trait StageRenderer {
    /// The native step type for this platform
    type Step;

    /// Error type for rendering failures
    type Error;

    /// Render a single stage task to a platform-native step
    ///
    /// # Errors
    ///
    /// Returns an error if rendering fails (implementation-specific).
    fn render_task(&self, task: &StageTask) -> Result<Self::Step, Self::Error>;

    /// Render all bootstrap tasks (environment setup, runs first)
    ///
    /// # Errors
    ///
    /// Returns an error if any task fails to render.
    fn render_bootstrap(
        &self,
        stages: &StageConfiguration,
    ) -> Result<Vec<Self::Step>, Self::Error> {
        stages
            .bootstrap
            .iter()
            .map(|t| self.render_task(t))
            .collect()
    }

    /// Render all setup tasks (provider configuration, runs after bootstrap)
    ///
    /// # Errors
    ///
    /// Returns an error if any task fails to render.
    fn render_setup(&self, stages: &StageConfiguration) -> Result<Vec<Self::Step>, Self::Error> {
        stages.setup.iter().map(|t| self.render_task(t)).collect()
    }

    /// Render all success tasks (post-success actions)
    ///
    /// # Errors
    ///
    /// Returns an error if any task fails to render.
    fn render_success(&self, stages: &StageConfiguration) -> Result<Vec<Self::Step>, Self::Error> {
        stages.success.iter().map(|t| self.render_task(t)).collect()
    }

    /// Render all failure tasks (post-failure actions)
    ///
    /// # Errors
    ///
    /// Returns an error if any task fails to render.
    fn render_failure(&self, stages: &StageConfiguration) -> Result<Vec<Self::Step>, Self::Error> {
        stages.failure.iter().map(|t| self.render_task(t)).collect()
    }
}

impl StageTask {
    /// Get the display label for this task, falling back to the ID
    #[must_use]
    pub fn label(&self) -> String {
        self.label.clone().unwrap_or_else(|| self.id.clone())
    }

    /// Get the command as a single string (for shell execution)
    #[must_use]
    pub fn command_string(&self) -> String {
        self.command.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;

    /// Simple test renderer that just returns the command string
    struct TestRenderer;

    impl StageRenderer for TestRenderer {
        type Step = String;
        type Error = Infallible;

        fn render_task(&self, task: &StageTask) -> Result<Self::Step, Self::Error> {
            Ok(task.command_string())
        }
    }

    #[test]
    fn test_render_setup_tasks() {
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
            ..Default::default()
        });

        let renderer = TestRenderer;
        let steps = renderer.render_setup(&stages).unwrap();

        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0], "nix build");
        assert_eq!(steps[1], "cuenv secrets setup onepassword");
    }

    #[test]
    fn test_stage_task_label_fallback() {
        let task_with_label = StageTask {
            id: "install-nix".to_string(),
            label: Some("Install Nix".to_string()),
            ..Default::default()
        };
        assert_eq!(task_with_label.label(), "Install Nix");

        let task_without_label = StageTask {
            id: "install-nix".to_string(),
            ..Default::default()
        };
        assert_eq!(task_without_label.label(), "install-nix");
    }
}
