//! Release orchestrator.
//!
//! Coordinates the full release pipeline: building, packaging, and publishing
//! to all configured backends.

use crate::artifact::{Artifact, ArtifactBuilder, ChecksumsManifest, PackagedArtifact, Target};
use crate::backends::{BackendContext, PublishResult, ReleaseBackend};
use crate::error::{Error, Result};
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// Release phase to execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleasePhase {
    /// Build binaries for all targets.
    Build,
    /// Package binaries into tarballs with checksums.
    Package,
    /// Publish to all backends.
    Publish,
    /// Full pipeline: build, package, publish.
    Full,
}

/// Configuration for the release orchestrator.
#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// Project/binary name.
    pub name: String,
    /// Version being released.
    pub version: String,
    /// Target platforms to build for.
    pub targets: Vec<Target>,
    /// Output directory for artifacts.
    pub output_dir: PathBuf,
    /// Dry run mode (no actual publishing).
    pub dry_run: bool,
    /// Base URL for downloading release assets.
    pub download_base_url: Option<String>,
}

impl OrchestratorConfig {
    /// Creates a new orchestrator configuration.
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            targets: vec![Target::LinuxX64, Target::LinuxArm64, Target::DarwinArm64],
            output_dir: PathBuf::from("target/release-artifacts"),
            dry_run: false,
            download_base_url: None,
        }
    }

    /// Sets the target platforms.
    #[must_use]
    pub fn with_targets(mut self, targets: Vec<Target>) -> Self {
        self.targets = targets;
        self
    }

    /// Sets the output directory.
    #[must_use]
    pub fn with_output_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.output_dir = dir.into();
        self
    }

    /// Sets dry-run mode.
    #[must_use]
    pub const fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Sets the download base URL.
    #[must_use]
    pub fn with_download_url(mut self, url: impl Into<String>) -> Self {
        self.download_base_url = Some(url.into());
        self
    }
}

/// Report from a release operation.
#[derive(Debug, Clone)]
pub struct ReleaseReport {
    /// Phase that was executed.
    pub phase: ReleasePhase,
    /// Packaged artifacts (if package or full phase).
    pub artifacts: Vec<PackagedArtifact>,
    /// Results from each backend (if publish or full phase).
    pub backend_results: Vec<PublishResult>,
    /// Overall success status.
    pub success: bool,
}

impl ReleaseReport {
    /// Creates an empty report.
    #[must_use]
    const fn empty(phase: ReleasePhase) -> Self {
        Self {
            phase,
            artifacts: Vec::new(),
            backend_results: Vec::new(),
            success: true,
        }
    }

    /// Creates a report with artifacts.
    #[must_use]
    const fn with_artifacts(phase: ReleasePhase, artifacts: Vec<PackagedArtifact>) -> Self {
        Self {
            phase,
            artifacts,
            backend_results: Vec::new(),
            success: true,
        }
    }

    /// Adds backend results to the report.
    fn add_backend_results(&mut self, results: Vec<PublishResult>) {
        self.success = results.iter().all(|r| r.success);
        self.backend_results = results;
    }
}

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
            ReleasePhase::Build => self.build().await,
            ReleasePhase::Package => self.package().await,
            ReleasePhase::Publish => self.publish_only().await,
            ReleasePhase::Full => self.full_pipeline().await,
        }
    }

    /// Builds binaries for all targets.
    ///
    /// This phase compiles the project for each target platform.
    /// Currently a placeholder - actual cross-compilation is handled
    /// by CI matrix builds.
    #[allow(clippy::unused_async)]
    async fn build(&self) -> Result<ReleaseReport> {
        info!(
            targets = ?self.config.targets,
            "Build phase (cross-compilation handled by CI)"
        );

        if self.config.dry_run {
            info!(
                "[dry-run] Would build for {} targets",
                self.config.targets.len()
            );
        }

        Ok(ReleaseReport::empty(ReleasePhase::Build))
    }

    /// Packages binaries into tarballs with checksums.
    #[allow(clippy::unused_async, clippy::too_many_lines)]
    async fn package(&self) -> Result<ReleaseReport> {
        info!(
            targets = ?self.config.targets,
            output_dir = %self.config.output_dir.display(),
            "Packaging artifacts"
        );

        // Ensure output directory exists
        if !self.config.dry_run {
            std::fs::create_dir_all(&self.config.output_dir).map_err(|e| {
                Error::artifact(
                    format!("Failed to create output directory: {e}"),
                    Some(self.config.output_dir.clone()),
                )
            })?;
        }

        let mut packaged_artifacts = Vec::new();
        let mut checksums = ChecksumsManifest::new();

        let builder = ArtifactBuilder::new(
            &self.config.output_dir,
            &self.config.version,
            &self.config.name,
        );

        for target in &self.config.targets {
            let binary_path = self.find_binary_for_target(*target)?;

            if self.config.dry_run {
                info!(
                    target = %target.short_id(),
                    binary = %binary_path.display(),
                    "[dry-run] Would package artifact"
                );
                continue;
            }

            let artifact = Artifact {
                target: *target,
                binary_path,
                name: self.config.name.clone(),
            };

            let packaged = builder.package(&artifact)?;

            debug!(
                archive = %packaged.archive_name,
                sha256 = %packaged.sha256,
                "Created artifact"
            );

            checksums.add(&packaged.archive_name, &packaged.sha256);
            packaged_artifacts.push(packaged);
        }

        // Write checksums file
        if !self.config.dry_run && !packaged_artifacts.is_empty() {
            let checksums_path = self.config.output_dir.join("CHECKSUMS.txt");
            checksums.write(&checksums_path)?;
            info!(path = %checksums_path.display(), "Wrote checksums file");
        }

        Ok(ReleaseReport::with_artifacts(
            ReleasePhase::Package,
            packaged_artifacts,
        ))
    }

    /// Publishes existing artifacts to all backends.
    async fn publish_only(&self) -> Result<ReleaseReport> {
        // Load existing artifacts from output directory
        let artifacts = self.load_existing_artifacts()?;

        if artifacts.is_empty() {
            warn!("No artifacts found to publish");
            return Ok(ReleaseReport::empty(ReleasePhase::Publish));
        }

        self.publish_artifacts(&artifacts).await
    }

    /// Runs the full pipeline: build, package, publish.
    async fn full_pipeline(&self) -> Result<ReleaseReport> {
        // Build phase
        self.build().await?;

        // Package phase
        let package_report = self.package().await?;

        // Publish phase
        let mut report = self.publish_artifacts(&package_report.artifacts).await?;
        report.phase = ReleasePhase::Full;
        report.artifacts = package_report.artifacts;

        Ok(report)
    }

    /// Publishes artifacts to all configured backends.
    #[allow(clippy::too_many_lines)] // Multi-backend publishing has multiple iterations
    async fn publish_artifacts(&self, artifacts: &[PackagedArtifact]) -> Result<ReleaseReport> {
        if self.backends.is_empty() {
            warn!("No backends configured");
            return Ok(ReleaseReport::empty(ReleasePhase::Publish));
        }

        let ctx = BackendContext::new(&self.config.name, &self.config.version)
            .with_dry_run(self.config.dry_run);

        let ctx = if let Some(url) = &self.config.download_base_url {
            ctx.with_download_url(url)
        } else {
            ctx
        };

        info!(
            backend_count = self.backends.len(),
            artifact_count = artifacts.len(),
            dry_run = self.config.dry_run,
            "Publishing to backends"
        );

        let mut results = Vec::new();

        for backend in &self.backends {
            info!(backend = backend.name(), "Publishing to backend");

            match backend.publish(&ctx, artifacts).await {
                Ok(result) => {
                    if result.success {
                        info!(
                            backend = backend.name(),
                            message = %result.message,
                            url = ?result.url,
                            "Backend publish succeeded"
                        );
                    } else {
                        warn!(
                            backend = backend.name(),
                            message = %result.message,
                            "Backend publish failed"
                        );
                    }
                    results.push(result);
                }
                Err(e) => {
                    warn!(
                        backend = backend.name(),
                        error = %e,
                        "Backend publish error"
                    );
                    results.push(PublishResult::failure(backend.name(), e.to_string()));
                }
            }
        }

        let mut report = ReleaseReport::empty(ReleasePhase::Publish);
        report.add_backend_results(results);

        Ok(report)
    }

    /// Finds the binary for a given target.
    fn find_binary_for_target(&self, target: Target) -> Result<PathBuf> {
        // Standard Rust target directory structure
        let target_dir = PathBuf::from("target")
            .join(target.rust_triple())
            .join("release")
            .join(&self.config.name);

        if target_dir.exists() {
            return Ok(target_dir);
        }

        // Try without target triple (native build)
        let native_path = PathBuf::from("target")
            .join("release")
            .join(&self.config.name);

        if native_path.exists() {
            return Ok(native_path);
        }

        Err(Error::artifact(
            format!(
                "Binary not found for target {}. Expected at {} or {}",
                target.short_id(),
                target_dir.display(),
                native_path.display()
            ),
            None,
        ))
    }

    /// Loads existing packaged artifacts from the output directory.
    #[allow(clippy::too_many_lines)] // Artifact loading has multiple file operations
    fn load_existing_artifacts(&self) -> Result<Vec<PackagedArtifact>> {
        let mut artifacts = Vec::new();

        // Load checksums first
        let checksums_path = self.config.output_dir.join("CHECKSUMS.txt");
        let checksums: std::collections::HashMap<String, String> = if checksums_path.exists() {
            let content = std::fs::read_to_string(&checksums_path).map_err(|e| {
                Error::artifact(
                    format!("Failed to read checksums: {e}"),
                    Some(checksums_path.clone()),
                )
            })?;

            content
                .lines()
                .filter_map(|line| {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        Some((parts[1].to_string(), parts[0].to_string()))
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            std::collections::HashMap::new()
        };

        // Find all tarballs
        for target in &self.config.targets {
            let archive_name = format!(
                "{}-{}-{}-{}.tar.gz",
                self.config.name,
                self.config.version,
                target.os(),
                target.arch()
            );
            let archive_path = self.config.output_dir.join(&archive_name);
            let checksum_path = self
                .config
                .output_dir
                .join(format!("{archive_name}.sha256"));

            if archive_path.exists() {
                let sha256 = checksums
                    .get(&archive_name)
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());

                artifacts.push(PackagedArtifact {
                    target: *target,
                    archive_path,
                    checksum_path,
                    archive_name,
                    sha256,
                });
            }
        }

        Ok(artifacts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .with_dry_run(true)
            .with_download_url("https://example.com/releases");

        assert_eq!(config.name, "test");
        assert_eq!(config.version, "1.0.0");
        assert_eq!(config.targets.len(), 1);
        assert_eq!(config.output_dir, PathBuf::from("dist"));
        assert!(config.dry_run);
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
        assert_eq!(
            config.output_dir,
            PathBuf::from("target/release-artifacts")
        );
        assert!(!config.dry_run);
        assert!(config.download_base_url.is_none());
    }

    #[test]
    fn test_orchestrator_config_clone() {
        let config = OrchestratorConfig::new("app", "1.0.0").with_dry_run(true);
        let cloned = config.clone();

        assert_eq!(cloned.name, "app");
        assert_eq!(cloned.version, "1.0.0");
        assert!(cloned.dry_run);
    }

    #[test]
    fn test_orchestrator_config_debug() {
        let config = OrchestratorConfig::new("test", "1.0.0");
        let debug_str = format!("{:?}", config);
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
        let debug_str = format!("{:?}", report);
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
        let config = OrchestratorConfig::new("myapp", "2.0.0").with_dry_run(true);
        let orchestrator = ReleaseOrchestrator::new(config);

        let retrieved_config = orchestrator.config();
        assert_eq!(retrieved_config.name, "myapp");
        assert_eq!(retrieved_config.version, "2.0.0");
        assert!(retrieved_config.dry_run);
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
            .with_dry_run(true)
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
            .with_dry_run(true)
            .with_output_dir(temp.path().to_path_buf())
            .with_targets(vec![Target::LinuxX64]);

        let orchestrator = ReleaseOrchestrator::new(config);
        let report = orchestrator.run(ReleasePhase::Package).await;

        // In dry-run mode, package phase skips actual packaging
        // but may fail if binary not found (expected)
        assert!(report.is_err() || report.unwrap().artifacts.is_empty());
    }
}
