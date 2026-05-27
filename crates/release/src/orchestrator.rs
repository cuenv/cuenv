//! Release orchestrator.
//!
//! Coordinates the full release pipeline: building, packaging, and publishing
//! to all configured backends.

mod model;
mod package;
mod publish;

use crate::backends::ReleaseBackend;
use crate::error::Result;

pub use model::{OrchestratorConfig, ReleasePhase, ReleaseReport};

/// Release orchestrator.
///
/// Coordinates the full release pipeline across multiple backends.
pub struct ReleaseOrchestrator {
    config: OrchestratorConfig,
    backends: Vec<Box<dyn ReleaseBackend>>,
}

impl ReleaseOrchestrator {
    /// Creates a new release orchestrator.
    #[must_use]
    pub fn new(config: OrchestratorConfig) -> Self {
        Self {
            config,
            backends: Vec::new(),
        }
    }

    /// Adds a backend to the orchestrator.
    #[must_use]
    pub fn with_backend(mut self, backend: Box<dyn ReleaseBackend>) -> Self {
        self.backends.push(backend);
        self
    }

    /// Adds multiple backends to the orchestrator.
    #[must_use]
    pub fn with_backends(mut self, backends: Vec<Box<dyn ReleaseBackend>>) -> Self {
        self.backends.extend(backends);
        self
    }

    /// Returns a reference to the configuration.
    #[must_use]
    pub const fn config(&self) -> &OrchestratorConfig {
        &self.config
    }

    /// Executes the specified release phase.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Binary not found for a target
    /// - Failed to create output directory
    /// - Failed to package artifacts
    /// - Backend publish failed
    pub async fn run(&self, phase: ReleasePhase) -> Result<ReleaseReport> {
        match phase {
            ReleasePhase::Build => Ok(self.build()),
            ReleasePhase::Package => self.package(),
            ReleasePhase::Publish => self.publish_only().await,
            ReleasePhase::Full => self.full_pipeline().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{PackagedArtifact, Target};
    use crate::backends::PublishResult;
    use cuenv_core::DryRun;
    use std::path::PathBuf;

    // ==========================================================================
    // ReleasePhase tests
    // ==========================================================================

    #[test]
    fn test_release_phase_debug() {
        assert_eq!(format!("{:?}", ReleasePhase::Build), "Build");
        assert_eq!(format!("{:?}", ReleasePhase::Package), "Package");
        assert_eq!(format!("{:?}", ReleasePhase::Publish), "Publish");
        assert_eq!(format!("{:?}", ReleasePhase::Full), "Full");
    }

    #[test]
    fn test_release_phase_clone() {
        let phase = ReleasePhase::Package;
        let cloned = phase;
        assert_eq!(phase, cloned);
    }

    #[test]
    fn test_release_phase_equality() {
        assert_eq!(ReleasePhase::Build, ReleasePhase::Build);
        assert_ne!(ReleasePhase::Build, ReleasePhase::Package);
        assert_ne!(ReleasePhase::Publish, ReleasePhase::Full);
    }

    // ==========================================================================
    // OrchestratorConfig tests
    // ==========================================================================

    #[test]
    fn test_orchestrator_config_builder() {
        let config = OrchestratorConfig::new("test", "1.0.0")
            .with_targets(vec![Target::LinuxX64])
            .with_output_dir("dist")
            .with_dry_run(DryRun::Yes)
            .with_download_url("https://example.com/releases");

        assert_eq!(config.name, "test");
        assert_eq!(config.version, "1.0.0");
        assert_eq!(config.targets.len(), 1);
        assert_eq!(config.output_dir, PathBuf::from("dist"));
        assert!(config.dry_run.is_dry_run());
        assert_eq!(
            config.download_base_url,
            Some("https://example.com/releases".to_string())
        );
    }

    #[test]
    fn test_orchestrator_config_default_targets() {
        let config = OrchestratorConfig::new("myapp", "2.0.0");

        assert_eq!(config.name, "myapp");
        assert_eq!(config.version, "2.0.0");
        assert_eq!(config.targets.len(), 3);
        assert!(config.targets.contains(&Target::LinuxX64));
        assert!(config.targets.contains(&Target::LinuxArm64));
        assert!(config.targets.contains(&Target::DarwinArm64));
        assert_eq!(config.output_dir, PathBuf::from("target/release-artifacts"));
        assert!(!config.dry_run.is_dry_run());
        assert!(config.download_base_url.is_none());
    }

    #[test]
    fn test_orchestrator_config_clone() {
        let config = OrchestratorConfig::new("app", "1.0.0").with_dry_run(DryRun::Yes);
        let cloned = config.clone();

        assert_eq!(cloned.name, "app");
        assert_eq!(cloned.version, "1.0.0");
        assert!(cloned.dry_run.is_dry_run());
    }

    #[test]
    fn test_orchestrator_config_debug() {
        let config = OrchestratorConfig::new("test", "1.0.0");
        let debug_str = format!("{config:?}");
        assert!(debug_str.contains("test"));
        assert!(debug_str.contains("1.0.0"));
    }

    #[test]
    fn test_orchestrator_config_empty_targets() {
        let config = OrchestratorConfig::new("app", "1.0.0").with_targets(vec![]);
        assert!(config.targets.is_empty());
    }

    #[test]
    fn test_orchestrator_config_into_string_conversion() {
        // Test that Into<String> works for name and version
        let config = OrchestratorConfig::new(String::from("myapp"), String::from("3.0.0"));
        assert_eq!(config.name, "myapp");
        assert_eq!(config.version, "3.0.0");
    }

    // ==========================================================================
    // ReleaseReport tests
    // ==========================================================================

    #[test]
    fn test_release_report_creation() {
        let report = ReleaseReport::empty(ReleasePhase::Build);
        assert_eq!(report.phase, ReleasePhase::Build);
        assert!(report.artifacts.is_empty());
        assert!(report.success);
    }

    #[test]
    fn test_release_report_empty_all_phases() {
        for phase in [
            ReleasePhase::Build,
            ReleasePhase::Package,
            ReleasePhase::Publish,
            ReleasePhase::Full,
        ] {
            let report = ReleaseReport::empty(phase);
            assert_eq!(report.phase, phase);
            assert!(report.artifacts.is_empty());
            assert!(report.backend_results.is_empty());
            assert!(report.success);
        }
    }

    #[test]
    fn test_release_report_with_artifacts() {
        let artifacts = vec![PackagedArtifact {
            target: Target::LinuxX64,
            archive_path: PathBuf::from("/tmp/test.tar.gz"),
            checksum_path: PathBuf::from("/tmp/test.tar.gz.sha256"),
            archive_name: "test-1.0.0-linux-x86_64.tar.gz".to_string(),
            sha256: "abc123".to_string(),
        }];

        let report = ReleaseReport::with_artifacts(ReleasePhase::Package, artifacts.clone());
        assert_eq!(report.phase, ReleasePhase::Package);
        assert_eq!(report.artifacts.len(), 1);
        assert!(report.backend_results.is_empty());
        assert!(report.success);
    }

    #[test]
    fn test_release_report_add_backend_results_all_success() {
        let mut report = ReleaseReport::empty(ReleasePhase::Publish);
        let results = vec![
            PublishResult::success("github", "Published to GitHub"),
            PublishResult::success("homebrew", "Published to Homebrew"),
        ];

        report.add_backend_results(results);
        assert!(report.success);
        assert_eq!(report.backend_results.len(), 2);
    }

    #[test]
    fn test_release_report_add_backend_results_with_failure() {
        let mut report = ReleaseReport::empty(ReleasePhase::Publish);
        let results = vec![
            PublishResult::success("github", "Published"),
            PublishResult::failure("homebrew", "Failed to publish"),
        ];

        report.add_backend_results(results);
        assert!(!report.success);
        assert_eq!(report.backend_results.len(), 2);
    }

    #[test]
    fn test_release_report_clone() {
        let report = ReleaseReport::empty(ReleasePhase::Full);
        let cloned = report.clone();
        assert_eq!(cloned.phase, ReleasePhase::Full);
        assert!(cloned.success);
    }

    #[test]
    fn test_release_report_debug() {
        let report = ReleaseReport::empty(ReleasePhase::Build);
        let debug_str = format!("{report:?}");
        assert!(debug_str.contains("Build"));
        assert!(debug_str.contains("success"));
    }

    // ==========================================================================
    // ReleaseOrchestrator tests
    // ==========================================================================

    #[test]
    fn test_orchestrator_no_backends() {
        let config = OrchestratorConfig::new("test", "1.0.0");
        let orchestrator = ReleaseOrchestrator::new(config);
        assert!(orchestrator.backends.is_empty());
    }

    #[test]
    fn test_orchestrator_config_accessor() {
        let config = OrchestratorConfig::new("myapp", "2.0.0").with_dry_run(DryRun::Yes);
        let orchestrator = ReleaseOrchestrator::new(config);

        let retrieved_config = orchestrator.config();
        assert_eq!(retrieved_config.name, "myapp");
        assert_eq!(retrieved_config.version, "2.0.0");
        assert!(retrieved_config.dry_run.is_dry_run());
    }

    #[test]
    fn test_orchestrator_find_binary_for_target_not_found() {
        let config = OrchestratorConfig::new("nonexistent", "1.0.0");
        let orchestrator = ReleaseOrchestrator::new(config);

        let result = orchestrator.find_binary_for_target(Target::LinuxX64);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Binary not found"));
    }

    #[test]
    fn test_orchestrator_load_existing_artifacts_empty() {
        use tempfile::TempDir;
        let temp = TempDir::new().unwrap();

        let config = OrchestratorConfig::new("test", "1.0.0")
            .with_output_dir(temp.path().to_path_buf())
            .with_targets(vec![Target::LinuxX64]);

        let orchestrator = ReleaseOrchestrator::new(config);
        let artifacts = orchestrator.load_existing_artifacts().unwrap();
        assert!(artifacts.is_empty());
    }

    #[test]
    fn test_orchestrator_load_existing_artifacts_with_checksums() {
        use tempfile::TempDir;
        let temp = TempDir::new().unwrap();

        // Create a test artifact
        let archive_name = "test-1.0.0-linux-x86_64.tar.gz";
        let archive_path = temp.path().join(archive_name);
        std::fs::write(&archive_path, b"test content").unwrap();

        // Create checksums file
        let checksums_content = format!("abc123def456 {archive_name}");
        std::fs::write(temp.path().join("CHECKSUMS.txt"), checksums_content).unwrap();

        let config = OrchestratorConfig::new("test", "1.0.0")
            .with_output_dir(temp.path().to_path_buf())
            .with_targets(vec![Target::LinuxX64]);

        let orchestrator = ReleaseOrchestrator::new(config);
        let artifacts = orchestrator.load_existing_artifacts().unwrap();

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].archive_name, archive_name);
        assert_eq!(artifacts[0].sha256, "abc123def456");
    }

    #[test]
    fn test_orchestrator_load_existing_artifacts_no_checksums() {
        use tempfile::TempDir;
        let temp = TempDir::new().unwrap();

        // Create a test artifact without checksums file
        let archive_name = "test-1.0.0-linux-x86_64.tar.gz";
        let archive_path = temp.path().join(archive_name);
        std::fs::write(&archive_path, b"test content").unwrap();

        let config = OrchestratorConfig::new("test", "1.0.0")
            .with_output_dir(temp.path().to_path_buf())
            .with_targets(vec![Target::LinuxX64]);

        let orchestrator = ReleaseOrchestrator::new(config);
        let artifacts = orchestrator.load_existing_artifacts().unwrap();

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].sha256, "unknown");
    }

    #[tokio::test]
    async fn test_orchestrator_build_dry_run() {
        let config = OrchestratorConfig::new("test", "1.0.0")
            .with_dry_run(DryRun::Yes)
            .with_targets(vec![Target::LinuxX64]);

        let orchestrator = ReleaseOrchestrator::new(config);
        let report = orchestrator.run(ReleasePhase::Build).await.unwrap();

        assert_eq!(report.phase, ReleasePhase::Build);
        assert!(report.success);
        assert!(report.artifacts.is_empty());
    }

    #[tokio::test]
    async fn test_orchestrator_publish_no_artifacts() {
        use tempfile::TempDir;
        let temp = TempDir::new().unwrap();

        let config = OrchestratorConfig::new("test", "1.0.0")
            .with_output_dir(temp.path().to_path_buf())
            .with_targets(vec![]);

        let orchestrator = ReleaseOrchestrator::new(config);
        let report = orchestrator.run(ReleasePhase::Publish).await.unwrap();

        assert_eq!(report.phase, ReleasePhase::Publish);
        assert!(report.success);
        assert!(report.artifacts.is_empty());
    }

    #[tokio::test]
    async fn test_orchestrator_publish_no_backends() {
        use tempfile::TempDir;
        let temp = TempDir::new().unwrap();

        // Create a test artifact
        let archive_name = "test-1.0.0-linux-x86_64.tar.gz";
        std::fs::write(temp.path().join(archive_name), b"test content").unwrap();

        let config = OrchestratorConfig::new("test", "1.0.0")
            .with_output_dir(temp.path().to_path_buf())
            .with_targets(vec![Target::LinuxX64]);

        let orchestrator = ReleaseOrchestrator::new(config);
        let report = orchestrator.run(ReleasePhase::Publish).await.unwrap();

        // No backends configured, so no backend results
        assert!(report.backend_results.is_empty());
    }

    #[tokio::test]
    async fn test_orchestrator_package_dry_run() {
        use tempfile::TempDir;
        let temp = TempDir::new().unwrap();

        let config = OrchestratorConfig::new("test", "1.0.0")
            .with_dry_run(DryRun::Yes)
            .with_output_dir(temp.path().to_path_buf())
            .with_targets(vec![Target::LinuxX64]);

        let orchestrator = ReleaseOrchestrator::new(config);
        let report = orchestrator.run(ReleasePhase::Package).await;

        // In dry-run mode, package phase skips actual packaging
        // but may fail if binary not found (expected)
        assert!(report.is_err() || report.unwrap().artifacts.is_empty());
    }
}
