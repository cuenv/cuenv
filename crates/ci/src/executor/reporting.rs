//! CI report writing, provider notification, and CI-context helpers.

use crate::ir::CachePolicy;
use crate::provider::CIProvider;
use crate::report::PipelineReport;
use crate::report::json::write_report;
use cuenv_core::ci::AnnotationValue;
use std::collections::HashMap;
use std::path::Path;

pub(super) fn cache_policy_override_for(
    context: &crate::context::CIContext,
) -> Option<CachePolicy> {
    if is_fork_pr(context) {
        Some(CachePolicy::Readonly)
    } else {
        None
    }
}

/// Write pipeline report to disk.
pub(super) fn write_pipeline_report(
    report: &PipelineReport,
    context: &crate::context::CIContext,
    project_path: &Path,
) {
    let report_dir = Path::new(".cuenv/reports");
    if let Err(e) = std::fs::create_dir_all(report_dir) {
        tracing::warn!(error = %e, "Failed to create report directory");
        return;
    }

    let sha_dir = report_dir.join(&context.sha);
    let _ = std::fs::create_dir_all(&sha_dir);

    let project_filename = project_path.display().to_string().replace(['/', '\\'], "-") + ".json";
    let report_path = sha_dir.join(project_filename);

    if let Err(e) = write_report(report, &report_path) {
        tracing::warn!(error = %e, "Failed to write report");
    } else {
        cuenv_events::emit_ci_report!(report_path.display());
    }

    if let Err(e) = crate::report::markdown::write_job_summary(report) {
        tracing::warn!(error = %e, "Failed to write job summary");
    }
}

/// Notify CI provider about pipeline results.
pub(super) async fn notify_provider(
    provider: &dyn CIProvider,
    report: &PipelineReport,
    pipeline_name: &str,
) {
    let check_name = format!("cuenv: {pipeline_name}");
    match provider.create_check(&check_name).await {
        Ok(handle) => {
            if let Err(e) = provider.complete_check(&handle, report).await {
                tracing::warn!(error = %e, "Failed to complete check run");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to create check run");
        }
    }

    if let Err(e) = provider.upload_report(report).await {
        tracing::warn!(error = %e, "Failed to post PR comment");
    }
}

/// Resolve pipeline annotation values from capture refs and literals.
pub(super) fn resolve_annotations(
    annotations: &HashMap<String, AnnotationValue>,
    all_captures: &HashMap<String, HashMap<String, String>>,
) -> HashMap<String, String> {
    annotations
        .iter()
        .filter_map(|(label, value)| {
            let resolved = match value {
                AnnotationValue::Literal(s) => Some(s.clone()),
                AnnotationValue::CaptureRef {
                    cuenv_capture_ref,
                    cuenv_task,
                    cuenv_capture,
                } => {
                    if !cuenv_capture_ref {
                        tracing::warn!(label, "Annotation has cuenvCaptureRef=false, skipping");
                        return None;
                    }
                    all_captures
                        .get(cuenv_task.as_str())
                        .and_then(|caps| caps.get(cuenv_capture.as_str()))
                        .cloned()
                }
            };
            resolved.map(|v| (label.clone(), v))
        })
        .collect()
}

/// Register common CI secret environment variables for redaction.
pub(super) fn register_ci_secrets() {
    const SECRET_PATTERNS: &[&str] = &[
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "ACTIONS_RUNTIME_TOKEN",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AZURE_CLIENT_SECRET",
        "GCP_SERVICE_ACCOUNT_KEY",
        "CACHIX_AUTH_TOKEN",
        "CODECOV_TOKEN",
        "CUE_REGISTRY_TOKEN",
        "VSCE_PAT",
        "NPM_TOKEN",
        "CARGO_REGISTRY_TOKEN",
        "PYPI_TOKEN",
        "DOCKER_PASSWORD",
        "CLOUDFLARE_API_TOKEN",
        "OP_SERVICE_ACCOUNT_TOKEN",
        "CUENV_SECRET_SALT",
        "CUENV_SECRET_SALT_PREV",
    ];

    for pattern in SECRET_PATTERNS {
        if let Ok(value) = std::env::var(pattern) {
            cuenv_events::register_secret(value);
        }
    }
}

/// Check if this is a fork PR, which should use a readonly cache.
fn is_fork_pr(context: &crate::context::CIContext) -> bool {
    context.event == "pull_request" && context.ref_name.starts_with("refs/pull/")
}
