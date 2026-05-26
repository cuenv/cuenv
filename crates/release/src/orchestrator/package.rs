use super::{ReleaseOrchestrator, ReleasePhase, ReleaseReport};
use crate::artifact::{Artifact, ArtifactBuilder, ChecksumsManifest, PackagedArtifact, Target};
use crate::error::{Error, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info};

impl ReleaseOrchestrator {
    /// Builds binaries for all targets.
    ///
    /// This phase compiles the project for each target platform.
    /// Currently a placeholder - actual cross-compilation is handled
    /// by CI matrix builds.
    pub(super) fn build(&self) -> ReleaseReport {
        info!(
            targets = ?self.config.targets,
            "Build phase (cross-compilation handled by CI)"
        );

        if self.config.dry_run.is_dry_run() {
            info!(
                "[dry-run] Would build for {} targets",
                self.config.targets.len()
            );
        }

        ReleaseReport::empty(ReleasePhase::Build)
    }

    /// Packages binaries into tarballs with checksums.
    pub(super) fn package(&self) -> Result<ReleaseReport> {
        info!(
            targets = ?self.config.targets,
            output_dir = %self.config.output_dir.display(),
            "Packaging artifacts"
        );

        self.ensure_output_dir()?;

        let mut packaged_artifacts = Vec::new();
        let mut checksums = ChecksumsManifest::new();

        let builder = ArtifactBuilder::new(
            &self.config.output_dir,
            &self.config.version,
            &self.config.name,
        );

        for target in &self.config.targets {
            let Some(packaged) = self.package_target(&builder, *target)? else {
                continue;
            };

            debug!(
                archive = %packaged.archive_name,
                sha256 = %packaged.sha256,
                "Created artifact"
            );

            checksums.add(&packaged.archive_name, &packaged.sha256);
            packaged_artifacts.push(packaged);
        }

        self.write_checksums_if_needed(&checksums, &packaged_artifacts)?;

        Ok(ReleaseReport::with_artifacts(
            ReleasePhase::Package,
            packaged_artifacts,
        ))
    }

    /// Finds the binary for a given target.
    pub(super) fn find_binary_for_target(&self, target: Target) -> Result<PathBuf> {
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

    pub(super) fn load_existing_artifacts(&self) -> Result<Vec<PackagedArtifact>> {
        let mut artifacts = Vec::new();
        let checksums = self.load_checksums_manifest()?;

        for target in &self.config.targets {
            if let Some(artifact) = self.load_existing_target_artifact(*target, &checksums) {
                artifacts.push(artifact);
            }
        }

        Ok(artifacts)
    }

    fn ensure_output_dir(&self) -> Result<()> {
        if self.config.dry_run.is_dry_run() {
            return Ok(());
        }

        std::fs::create_dir_all(&self.config.output_dir).map_err(|e| {
            Error::artifact(
                format!("Failed to create output directory: {e}"),
                Some(self.config.output_dir.clone()),
            )
        })
    }

    fn package_target(
        &self,
        builder: &ArtifactBuilder,
        target: Target,
    ) -> Result<Option<PackagedArtifact>> {
        let binary_path = self.find_binary_for_target(target)?;

        if self.config.dry_run.is_dry_run() {
            info!(
                target = %target.short_id(),
                binary = %binary_path.display(),
                "[dry-run] Would package artifact"
            );
            return Ok(None);
        }

        let artifact = Artifact {
            target,
            binary_path,
            name: self.config.name.clone(),
        };

        builder.package(&artifact).map(Some)
    }

    fn write_checksums_if_needed(
        &self,
        checksums: &ChecksumsManifest,
        packaged_artifacts: &[PackagedArtifact],
    ) -> Result<()> {
        if self.config.dry_run.is_dry_run() || packaged_artifacts.is_empty() {
            return Ok(());
        }

        let checksums_path = self.config.output_dir.join("CHECKSUMS.txt");
        checksums.write(&checksums_path)?;
        info!(path = %checksums_path.display(), "Wrote checksums file");
        Ok(())
    }

    fn load_checksums_manifest(&self) -> Result<HashMap<String, String>> {
        let checksums_path = self.config.output_dir.join("CHECKSUMS.txt");
        if !checksums_path.exists() {
            return Ok(HashMap::new());
        }

        let content = std::fs::read_to_string(&checksums_path).map_err(|e| {
            Error::artifact(
                format!("Failed to read checksums: {e}"),
                Some(checksums_path.clone()),
            )
        })?;

        Ok(content
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                (parts.len() >= 2).then(|| (parts[1].to_string(), parts[0].to_string()))
            })
            .collect())
    }

    fn load_existing_target_artifact(
        &self,
        target: Target,
        checksums: &HashMap<String, String>,
    ) -> Option<PackagedArtifact> {
        let archive_name = format!(
            "{}-{}-{}-{}.tar.gz",
            self.config.name,
            self.config.version,
            target.os(),
            target.arch()
        );
        let archive_path = self.config.output_dir.join(&archive_name);
        if !archive_path.exists() {
            return None;
        }

        let checksum_path = self
            .config
            .output_dir
            .join(format!("{archive_name}.sha256"));
        let sha256 = checksums
            .get(&archive_name)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());

        Some(PackagedArtifact {
            target,
            archive_path,
            checksum_path,
            archive_name,
            sha256,
        })
    }
}
