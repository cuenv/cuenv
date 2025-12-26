//! Trusted Publishing Stage Contributor
//!
//! Contributes crates.io OIDC authentication task for trusted publishing.
//!
//! When enabled, adds a step using `rust-lang/crates-io-auth-action@v1` to obtain
//! a short-lived token for publishing to crates.io without storing long-lived secrets.

use crate::config::GitHubConfig;
use cuenv_ci::StageContributor;
use cuenv_ci::ir::{BuildStage, IntermediateRepresentation, StageTask};
use cuenv_core::manifest::Project;
use std::collections::HashMap;

/// Trusted Publishing stage contributor for crates.io
///
/// When active (trusted publishing is enabled for crates.io), contributes:
/// - Setup: Authenticate with crates.io using OIDC
#[derive(Debug, Clone, Copy, Default)]
pub struct TrustedPublishingContributor;

impl TrustedPublishingContributor {
    /// Check if trusted publishing for crates.io is enabled
    fn is_crates_io_enabled(project: &Project) -> bool {
        project
            .ci
            .as_ref()
            .and_then(|ci| ci.provider.as_ref())
            .and_then(|p| p.get("github"))
            .and_then(|v| serde_json::from_value::<GitHubConfig>(v.clone()).ok())
            .and_then(|cfg| cfg.trusted_publishing)
            .and_then(|tp| tp.crates_io)
            .unwrap_or(false)
    }
}

impl StageContributor for TrustedPublishingContributor {
    fn id(&self) -> &'static str {
        "trusted-publishing"
    }

    fn is_active(&self, _ir: &IntermediateRepresentation, project: &Project) -> bool {
        Self::is_crates_io_enabled(project)
    }

    fn contribute(
        &self,
        ir: &IntermediateRepresentation,
        project: &Project,
    ) -> (Vec<(BuildStage, StageTask)>, bool) {
        // Idempotency: check if already contributed
        if ir.stages.setup.iter().any(|t| t.id == "auth-crates-io") {
            return (vec![], false);
        }

        if !Self::is_crates_io_enabled(project) {
            return (vec![], false);
        }

        // Build provider hints for the GitHub Action
        let mut github_action = serde_json::Map::new();
        github_action.insert(
            "uses".to_string(),
            serde_json::Value::String("rust-lang/crates-io-auth-action@v1".to_string()),
        );

        let mut provider_hints = serde_json::Map::new();
        provider_hints.insert(
            "github_action".to_string(),
            serde_json::Value::Object(github_action),
        );

        (
            vec![(
                BuildStage::Setup,
                StageTask {
                    id: "auth-crates-io".to_string(),
                    provider: "trusted-publishing".to_string(),
                    label: Some("Authenticate with crates.io".to_string()),
                    command: vec![],
                    shell: false,
                    env: HashMap::new(),
                    depends_on: vec![],
                    priority: 50, // Run late in setup, before task execution
                    secrets: HashMap::new(),
                    provider_hints: Some(serde_json::Value::Object(provider_hints)),
                },
            )],
            true,
        )
    }
}
