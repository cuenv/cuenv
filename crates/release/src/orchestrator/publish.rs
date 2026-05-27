use super::{ReleaseOrchestrator, ReleasePhase, ReleaseReport};
use crate::artifact::PackagedArtifact;
use crate::backends::{BackendContext, PublishResult, ReleaseBackend};
use crate::error::Result;
use tracing::{info, warn};

struct BackendPublishRequest<'a> {
    backend: &'a dyn ReleaseBackend,
    context: &'a BackendContext,
    artifacts: &'a [PackagedArtifact],
}

impl ReleaseOrchestrator {
    /// Publishes existing artifacts to all backends.
    pub(super) async fn publish_only(&self) -> Result<ReleaseReport> {
        let artifacts = self.load_existing_artifacts()?;

        if artifacts.is_empty() {
            warn!("No artifacts found to publish");
            return Ok(ReleaseReport::empty(ReleasePhase::Publish));
        }

        self.publish_artifacts(&artifacts).await
    }

    /// Runs the full pipeline: build, package, publish.
    pub(super) async fn full_pipeline(&self) -> Result<ReleaseReport> {
        self.build();

        let package_report = self.package()?;

        let mut report = self.publish_artifacts(&package_report.artifacts).await?;
        report.phase = ReleasePhase::Full;
        report.artifacts = package_report.artifacts;

        Ok(report)
    }

    /// Publishes artifacts to all configured backends.
    async fn publish_artifacts(&self, artifacts: &[PackagedArtifact]) -> Result<ReleaseReport> {
        if self.backends.is_empty() {
            warn!("No backends configured");
            return Ok(ReleaseReport::empty(ReleasePhase::Publish));
        }

        let context = self.backend_context();

        info!(
            backend_count = self.backends.len(),
            artifact_count = artifacts.len(),
            dry_run = self.config.dry_run.is_dry_run(),
            "Publishing to backends"
        );

        let mut results = Vec::new();

        for backend in &self.backends {
            results.push(
                Self::publish_backend(BackendPublishRequest {
                    backend: backend.as_ref(),
                    context: &context,
                    artifacts,
                })
                .await,
            );
        }

        let mut report = ReleaseReport::empty(ReleasePhase::Publish);
        report.add_backend_results(results);

        Ok(report)
    }

    fn backend_context(&self) -> BackendContext {
        let context = BackendContext::new(&self.config.name, &self.config.version)
            .with_dry_run(self.config.dry_run);

        if let Some(url) = &self.config.download_base_url {
            context.with_download_url(url)
        } else {
            context
        }
    }

    async fn publish_backend(request: BackendPublishRequest<'_>) -> PublishResult {
        let BackendPublishRequest {
            backend,
            context,
            artifacts,
        } = request;

        info!(backend = backend.name(), "Publishing to backend");

        match backend.publish(context, artifacts).await {
            Ok(result) => {
                Self::log_publish_result(backend.name(), &result);
                result
            }
            Err(e) => {
                warn!(
                    backend = backend.name(),
                    error = %e,
                    "Backend publish error"
                );
                PublishResult::failure(backend.name(), e.to_string())
            }
        }
    }

    fn log_publish_result(backend_name: &str, result: &PublishResult) {
        if result.success {
            info!(
                backend = backend_name,
                message = %result.message,
                url = ?result.url,
                "Backend publish succeeded"
            );
        } else {
            warn!(
                backend = backend_name,
                message = %result.message,
                "Backend publish failed"
            );
        }
    }
}
