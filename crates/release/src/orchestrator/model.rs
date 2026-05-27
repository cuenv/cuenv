use crate::artifact::{PackagedArtifact, Target};
use crate::backends::PublishResult;
use cuenv_core::DryRun;
use std::path::PathBuf;

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
    pub dry_run: DryRun,
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
            dry_run: DryRun::No,
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
    pub const fn with_dry_run(mut self, dry_run: DryRun) -> Self {
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
    pub(super) const fn empty(phase: ReleasePhase) -> Self {
        Self {
            phase,
            artifacts: Vec::new(),
            backend_results: Vec::new(),
            success: true,
        }
    }

    /// Creates a report with artifacts.
    #[must_use]
    pub(super) const fn with_artifacts(
        phase: ReleasePhase,
        artifacts: Vec<PackagedArtifact>,
    ) -> Self {
        Self {
            phase,
            artifacts,
            backend_results: Vec::new(),
            success: true,
        }
    }

    /// Adds backend results to the report.
    pub(super) fn add_backend_results(&mut self, results: Vec<PublishResult>) {
        self.success = results.iter().all(|r| r.success);
        self.backend_results = results;
    }
}
