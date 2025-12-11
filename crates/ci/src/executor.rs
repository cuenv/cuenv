// CI executor outputs to stdout as part of its normal operation
#![allow(clippy::print_stdout)]

use crate::affected::compute_affected_tasks;
use crate::discovery::discover_projects;
use crate::provider::CIProvider;
use crate::provider::github::GitHubProvider;
use crate::provider::local::LocalProvider;
use crate::report::json::write_report;
use crate::report::{ContextReport, PipelineReport, PipelineStatus, TaskReport, TaskStatus};
use async_trait::async_trait;
use chrono::Utc;
use cuenv_core::Result;
use std::sync::Arc;

#[async_trait]
pub trait TaskRunner: Send + Sync {
    async fn run_task(&self, project_root: &std::path::Path, task_name: &str) -> Result<()>;
}

/// Run the CI pipeline logic
///
/// # Errors
/// Returns error if provider detection fails or IO errors occur
///
/// # Panics
/// Panics if project path has no parent directory (should not happen for valid paths)
#[allow(clippy::too_many_lines)]
pub async fn run_ci(
    dry_run: bool,
    specific_pipeline: Option<String>,
    from_ref: Option<String>,
    runner: Arc<dyn TaskRunner>,
) -> Result<()> {
    // 1. Detect Provider
    let provider: Arc<dyn CIProvider> = if let Some(p) = GitHubProvider::detect() {
        Arc::new(p)
    } else if let Some(base_ref) = from_ref {
        Arc::new(LocalProvider::with_base_ref(base_ref))
    } else if let Some(p) = LocalProvider::detect() {
        Arc::new(p)
    } else {
        return Err(cuenv_core::Error::configuration("No CI provider detected"));
    };

    let context = provider.context();
    println!(
        "Context: {} (event: {}, ref: {})",
        context.provider, context.event, context.ref_name
    );

    // 2. Get changed files
    let changed_files = provider.changed_files().await?;
    println!("Changed files: {}", changed_files.len());

    // 3. Discover projects
    // We need a way to load Cuenv configs.
    let projects = discover_projects()?;
    if projects.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "No cuenv projects found. Ensure env.cue files declare 'package cuenv'",
        ));
    }
    println!("Found {} projects", projects.len());

    // Build project map for cross-project dependency resolution
    let mut project_map = std::collections::HashMap::new();
    for project in &projects {
        if let Some(name) = &project.config.name {
            project_map.insert(name.clone(), project.clone());
        }
    }

    // Track if any project failed
    let mut any_failed = false;

    // 4. Process each project
    for project in &projects {
        // Let's skip the actual execution logic for now and just print what we would do
        // based on the placeholder config (which is empty).
        // The goal of this task is to set up the structure.

        let config = &project.config;

        // Determine pipeline to run
        let pipeline_name = specific_pipeline
            .clone()
            .unwrap_or_else(|| "default".to_string());

        // Find pipeline in config
        let Some(ci) = &config.ci else {
            return Err(cuenv_core::Error::configuration(format!(
                "Project {} has no CI configuration",
                project.path.display()
            )));
        };

        let available_pipelines: Vec<&str> = ci.pipelines.iter().map(|p| p.name.as_str()).collect();
        let Some(pipeline) = ci.pipelines.iter().find(|p| p.name == pipeline_name) else {
            return Err(cuenv_core::Error::configuration(format!(
                "Pipeline '{}' not found in project {}. Available pipelines: {}",
                pipeline_name,
                project.path.display(),
                available_pipelines.join(", ")
            )));
        };

        // Get the directory containing the env.cue file
        // If parent is empty (file in current dir), use "." instead
        let project_root = project.path.parent().map_or_else(
            || std::path::Path::new("."),
            |p| {
                if p.as_os_str().is_empty() {
                    std::path::Path::new(".")
                } else {
                    p
                }
            },
        );
        // For release events, run all tasks unconditionally (no affected-file filtering)
        let tasks_to_run = if context.event == "release" {
            pipeline.tasks.clone()
        } else {
            compute_affected_tasks(
                &changed_files,
                &pipeline.tasks,
                project_root,
                config,
                &project_map,
            )
        };

        if tasks_to_run.is_empty() {
            println!("Project {}: No affected tasks", project.path.display());
            continue;
        }

        println!(
            "Project {}: Running tasks {:?}",
            project.path.display(),
            tasks_to_run
        );

        if !dry_run {
            let start_time = Utc::now();
            let mut tasks_reports = Vec::new();
            let mut pipeline_status = PipelineStatus::Success;

            for task_name in tasks_to_run {
                println!("  -> Executing {task_name}");
                let task_start = std::time::Instant::now();
                let result = runner.run_task(project_root, &task_name).await;
                let duration = u64::try_from(task_start.elapsed().as_millis()).unwrap_or(0);

                let status = match result {
                    Ok(()) => {
                        println!("  -> {task_name} passed");
                        TaskStatus::Success
                    }
                    Err(e) => {
                        println!("  -> {task_name} failed: {e}");
                        pipeline_status = PipelineStatus::Failed;
                        TaskStatus::Failed
                    }
                };

                tasks_reports.push(TaskReport {
                    name: task_name,
                    status,
                    duration_ms: duration,
                    exit_code: if status == TaskStatus::Success {
                        Some(0)
                    } else {
                        Some(1)
                    },
                    inputs_matched: vec![], // TODO: track inputs matched
                    cache_key: None,        // TODO: get cache key
                    outputs: vec![],        // TODO: get outputs
                });
            }

            let completed_at = Utc::now();
            #[allow(clippy::cast_sign_loss)]
            let duration_ms = (completed_at - start_time).num_milliseconds() as u64;

            // Generate report
            let report = PipelineReport {
                version: cuenv_core::VERSION.to_string(),
                project: project.path.display().to_string(),
                pipeline: pipeline.name.clone(),
                context: ContextReport {
                    provider: context.provider.clone(),
                    event: context.event.clone(),
                    ref_name: context.ref_name.clone(),
                    base_ref: context.base_ref.clone(),
                    sha: context.sha.clone(),
                    changed_files: changed_files
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect(),
                },
                started_at: start_time,
                completed_at: Some(completed_at),
                duration_ms: Some(duration_ms),
                status: pipeline_status,
                tasks: tasks_reports,
            };

            // Ensure report directory exists
            let report_dir = std::path::Path::new(".cuenv/reports");
            if let Err(e) = std::fs::create_dir_all(report_dir) {
                println!("Failed to create report directory: {e}");
            } else {
                // Filename: safe project path + timestamp? or just project name?
                // Spec said: .cuenv/reports/{sha}/{project}.json
                // For local we use "current" SHA.
                let sha_dir = report_dir.join(&context.sha);
                let _ = std::fs::create_dir_all(&sha_dir);

                // Sanitize project path for filename
                let project_filename =
                    project.path.display().to_string().replace(['/', '\\'], "-") + ".json";
                let report_path = sha_dir.join(project_filename);

                if let Err(e) = write_report(&report, &report_path) {
                    println!("Failed to write report: {e}");
                } else {
                    println!("Report written to: {}", report_path.display());
                }
            }

            // Write GitHub Job Summary (appears in workflow run summary)
            if let Err(e) = crate::report::markdown::write_job_summary(&report) {
                eprintln!("Warning: Failed to write job summary: {e}");
            }

            // Always post results to CI provider before checking for failures
            // This ensures PR comments and check runs are created even when tasks fail
            let check_name = format!("cuenv: {}", pipeline.name);
            match provider.create_check(&check_name).await {
                Ok(handle) => {
                    if let Err(e) = provider.complete_check(&handle, &report).await {
                        eprintln!("Warning: Failed to complete check run: {e}");
                    }
                }
                Err(e) => {
                    eprintln!("Warning: Failed to create check run: {e}");
                }
            }

            // Post PR comment with report summary
            if let Err(e) = provider.upload_report(&report).await {
                eprintln!("Warning: Failed to post PR comment: {e}");
            }

            // Track if this project failed
            if pipeline_status == PipelineStatus::Failed {
                any_failed = true;
            }
        }
    }

    if any_failed {
        return Err(cuenv_core::Error::configuration(
            "One or more CI tasks failed",
        ));
    }

    Ok(())
}
