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
pub async fn run_ci(
    dry_run: bool,
    specific_pipeline: Option<String>,
    runner: Arc<dyn TaskRunner>,
) -> Result<()> {
    // 1. Detect Provider
    let provider: Arc<Box<dyn CIProvider>> = if let Some(p) = GitHubProvider::detect() {
        Arc::new(Box::new(p))
    } else {
        // Fallback to local
        if let Some(p) = LocalProvider::detect() {
            Arc::new(Box::new(p))
        } else {
            return Err(cuenv_core::Error::configuration("No CI provider detected"));
        }
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
    // For now, we'll just find the files.
    let projects = discover_projects()?;
    println!("Found {} projects", projects.len());

    // 4. Process each project
    for project in projects {
        // TODO: Load Config properly using cuengine
        // For now, we rely on the placeholder or need a real loader.
        // Since we can't easily invoke cuengine here without more setup (runtime, etc),
        // and we want to avoid huge complexity in this first pass,
        // let's assume we can parse it or skip if we can't.

        // In a real implementation, we would:
        // let config = cuengine::load(project.path)?;

        // Let's skip the actual execution logic for now and just print what we would do
        // based on the placeholder config (which is empty).
        // The goal of this task is to set up the structure.

        let config = &project.config;

        // Determine pipeline to run
        let pipeline_name = specific_pipeline.clone().unwrap_or_else(|| {
            // Match context to pipeline rules in config
            // For now default to "default" or match event name
            "default".to_string()
        });

        // Find pipeline in config
        if let Some(ci) = &config.ci {
            let pipeline = ci.pipelines.iter().find(|p| p.name == pipeline_name);
            if let Some(pipeline) = pipeline {
                let project_root = project.path.parent().unwrap();
                let affected =
                    compute_affected_tasks(&changed_files, &pipeline.tasks, config, project_root);

                if affected.is_empty() {
                    println!("Project {}: No affected tasks", project.path.display());
                    continue;
                }

                println!(
                    "Project {}: Running tasks {:?}",
                    project.path.display(),
                    affected
                );

                if !dry_run {
                    let start_time = Utc::now();
                    let mut tasks_reports = Vec::new();
                    let mut pipeline_status = PipelineStatus::Success;

                    for task_name in affected {
                        println!("  -> Executing {}", task_name);
                        let task_start = std::time::Instant::now();
                        let result = runner.run_task(project_root, &task_name).await;
                        let duration = task_start.elapsed().as_millis() as u64;

                        let status = match result {
                            Ok(_) => {
                                println!("  -> {} passed", task_name);
                                TaskStatus::Success
                            }
                            Err(e) => {
                                println!("  -> {} failed: {}", task_name, e);
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
                        println!("Failed to create report directory: {}", e);
                    } else {
                        // Filename: safe project path + timestamp? or just project name?
                        // Spec said: .cuenv/reports/{sha}/{project}.json
                        // For local we use "current" SHA.
                        let sha_dir = report_dir.join(&context.sha);
                        let _ = std::fs::create_dir_all(&sha_dir);

                        // Sanitize project path for filename
                        let project_filename = project
                            .path
                            .display()
                            .to_string()
                            .replace('/', "-")
                            .replace('\\', "-")
                            + ".json";
                        let report_path = sha_dir.join(project_filename);

                        if let Err(e) = write_report(&report, &report_path) {
                            println!("Failed to write report: {}", e);
                        } else {
                            println!("Report written to: {}", report_path.display());
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
