//! CI Pipeline Export Module
//!
//! Handles exporting CI pipelines to various formats (Buildkite, GitLab, GitHub Actions, CircleCI).
//! Outputs to stdout by default for dynamic pipeline upload, or to a file with --output.

use super::args::{CiArgs, ExportFormat};
use crate::providers;

use cuenv_ci::context::CIContext;
use cuenv_ci::emitter::Emitter;
use cuenv_ci::ir::{IntermediateRepresentation, PipelineMetadata, TriggerCondition};
use cuenv_core::Result;
use cuenv_core::ci::PipelineMode;
use cuenv_core::manifest::{Project, RemoteCache};

use std::io::Write;
use std::path::PathBuf;

/// Execute export mode - generate pipeline YAML.
///
/// # Arguments
/// * `args` - CLI arguments
/// * `format` - Export format (buildkite, gitlab, etc.)
///
/// # Errors
///
/// Returns error if:
/// - No projects are found
/// - Pipeline compilation fails
/// - Emitter is not available (feature not enabled)
/// - File write fails (when --output specified)
pub async fn execute_export(args: &CiArgs, format: ExportFormat) -> Result<()> {
    let provider = providers::detect_ci_provider(args.from.clone());
    let context = provider.context();
    let changes = provider.changed_files().await?;

    tracing::info!(
        provider = %context.provider,
        event = %context.event,
        ref_name = %context.ref_name,
        changed_files = changes.files().len(),
        "Export context"
    );

    // Evaluate module and discover projects
    let module = cuenv_ci::discovery::evaluate_module_from_cwd()?;
    let project_count = module.project_count();
    if project_count == 0 {
        return Err(cuenv_core::Error::configuration(
            "No cuenv projects found. Ensure env.cue files declare 'package cuenv'",
        ));
    }

    // Collect projects with their configs
    let mut projects: Vec<(PathBuf, Project)> = Vec::new();
    for instance in module.projects() {
        let config = instance.to_project()?;
        let project_path = module.root.join(&instance.path);
        projects.push((project_path, config));
    }

    // Filter projects by path if specified (and not the default ".")
    let projects: Vec<(PathBuf, Project)> = if args.path == "." {
        projects
    } else {
        let filter_path = module.root.join(&args.path);
        projects
            .into_iter()
            .filter(|(path, _)| path.starts_with(&filter_path))
            .collect()
    };

    tracing::info!(count = projects.len(), "Found projects");

    let pipeline_name = args.pipeline_name();
    let buildkite_ref = BuildkiteRef::detect(context, |name| std::env::var(name).ok());

    // Collect all tasks for export. A project's file references bind among
    // what that project imports, read from its instance in the module.
    let collected = collect_tasks_for_export(
        &projects,
        ExportPipeline {
            name: pipeline_name,
            module_root: &module.root,
            buildkite_ref: buildkite_ref.as_ref(),
        },
    )?;

    if collected.tasks.is_empty() {
        tracing::info!("No tasks to export");
        let empty_yaml = match format {
            ExportFormat::Buildkite => "steps: []\n",
            ExportFormat::Gitlab => "{}\n",
            ExportFormat::GithubActions => "jobs: {}\n",
            ExportFormat::Circleci => "version: 2.1\njobs: {}\n",
        };
        output_yaml(args, empty_yaml)?;
        return Ok(());
    }

    // Build combined IR
    let combined_ir = IntermediateRepresentation {
        version: "1.5".to_string(),
        pipeline: PipelineMetadata {
            name: pipeline_name.to_string(),
            mode: PipelineMode::default(),
            environment: collected.environment,
            requires_onepassword: false,
            project_name: None,
            project_path: None,
            trigger: None,
            concurrency: None,
            pipeline_tasks: vec![],
            pipeline_task_defs: vec![],
        },
        runtimes: collected.runtimes,
        tasks: collected.tasks,
    };

    // Get emitter and generate YAML
    let yaml = emit_pipeline(&combined_ir, format, collected.buildkite_cache)?;
    output_yaml(args, &yaml)?;

    Ok(())
}

/// Collected tasks from project discovery.
struct CollectedTasks<'project> {
    tasks: Vec<cuenv_ci::ir::Task>,
    runtimes: Vec<cuenv_ci::ir::Runtime>,
    environment: Option<String>,
    buildkite_cache: BuildkiteCache<'project>,
}

/// A webhook Buildkite explicitly identifies as a non-PR branch or tag build.
#[derive(Debug, PartialEq, Eq)]
enum BuildkiteRef {
    Branch(String),
    Tag(String),
}

impl BuildkiteRef {
    fn detect(context: &CIContext, variable: impl Fn(&str) -> Option<String>) -> Option<Self> {
        if context.provider != "buildkite"
            || context.event != "webhook"
            || context.base_ref.is_some()
            || variable("BUILDKITE_PULL_REQUEST").as_deref() != Some("false")
        {
            return None;
        }
        if let Some(tag) = variable("BUILDKITE_TAG").filter(|tag| !tag.is_empty()) {
            return Some(Self::Tag(tag));
        }
        (!context.ref_name.is_empty()).then(|| Self::Branch(context.ref_name.clone()))
    }

    fn allowed_by(&self, trigger: &TriggerCondition) -> bool {
        let (name, filters) = match self {
            Self::Branch(name) => (name, &trigger.branches),
            Self::Tag(name) => (name, &trigger.tags),
        };
        // Workflow filters may contain provider-specific glob syntax and
        // ordered exclusions. Write authorization requires a literal ref.
        !name.contains(['*', '?', '+', '[', ']', '!', '\\'])
            && !filters.iter().any(|filter| filter.starts_with('!'))
            && filters.contains(name)
    }
}

/// A combined Buildkite step can use one credential and one endpoint.
#[derive(Debug, Clone, Copy, Default)]
enum BuildkiteCache<'project> {
    #[default]
    Unconfigured,
    Disabled,
    Configured {
        remote: &'project RemoteCache,
        ref_allowed: bool,
    },
}

impl<'project> BuildkiteCache<'project> {
    fn for_project(
        project: &'project Project,
        trigger: Option<&TriggerCondition>,
        buildkite_ref: Option<&BuildkiteRef>,
    ) -> Self {
        match project
            .cache
            .as_ref()
            .and_then(|cache| cache.remote.as_ref())
        {
            Some(remote) => Self::Configured {
                remote,
                ref_allowed: buildkite_ref
                    .zip(trigger)
                    .is_some_and(|(reference, trigger)| reference.allowed_by(trigger)),
            },
            None => Self::Unconfigured,
        }
    }

    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unconfigured, Self::Unconfigured) => Self::Unconfigured,
            (
                Self::Configured {
                    remote,
                    ref_allowed,
                },
                Self::Configured {
                    remote: other_remote,
                    ref_allowed: other_allowed,
                },
            ) if remote == other_remote => Self::Configured {
                remote,
                ref_allowed: ref_allowed && other_allowed,
            },
            _ => Self::Disabled,
        }
    }
}

/// The pipeline an export is for.
#[derive(Clone, Copy)]
struct ExportPipeline<'pipeline> {
    name: &'pipeline str,
    /// The root of the CUE module the projects belong to.
    module_root: &'pipeline std::path::Path,
    buildkite_ref: Option<&'pipeline BuildkiteRef>,
}

/// Collect all IR tasks from all projects for a given pipeline.
///
/// For export mode, we collect ALL tasks defined in the pipeline rather than
/// filtering by affected files. The CI orchestrator handles trigger logic.
fn collect_tasks_for_export<'project>(
    projects: &'project [(PathBuf, Project)],
    pipeline: ExportPipeline<'_>,
) -> Result<CollectedTasks<'project>> {
    use cuenv_ci::compiler::{Compiler, CompilerOptions, FileAnchors};

    let pipeline_name = pipeline.name;
    let mut all_ir_tasks = Vec::new();
    let mut pipeline_environment: Option<String> = None;
    let mut compiled_runtimes = Vec::new();
    let mut buildkite_cache = None::<BuildkiteCache<'_>>;

    for (project_path, config) in projects {
        let Some(ci) = &config.ci else {
            continue;
        };

        let Some(ci_pipeline) = ci.pipelines.get(pipeline_name) else {
            continue;
        };

        pipeline_environment.clone_from(&ci_pipeline.environment);

        // Extract task names from pipeline for logging
        let pipeline_task_names: Vec<String> = ci_pipeline
            .tasks
            .iter()
            .map(|t| t.task_name().to_string())
            .collect();

        if pipeline_task_names.is_empty() {
            tracing::debug!(
                project = %project_path.display(),
                "No tasks in pipeline"
            );
            continue;
        }

        tracing::info!(
            project = %project_path.display(),
            tasks = ?pipeline_task_names,
            "Exporting pipeline tasks"
        );

        // Compile with pipeline context
        let options = CompilerOptions {
            pipeline_name: Some(pipeline_name.to_string()),
            pipeline: Some(ci_pipeline.clone()),
            file_anchors: Some(FileAnchors::Instance {
                module_root: pipeline.module_root.to_path_buf(),
                directory: project_path.clone(),
            }),
            ..Default::default()
        };
        let compiler = Compiler::with_options(config.clone(), options);
        let ir = compiler.compile().map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to compile project: {e}"))
        })?;

        let project_cache = BuildkiteCache::for_project(
            config,
            ir.pipeline.trigger.as_ref(),
            pipeline.buildkite_ref,
        );
        buildkite_cache = Some(match buildkite_cache {
            Some(previous) => previous.combine(project_cache),
            None => project_cache,
        });

        // Collect all tasks from the compiled IR (phase tasks + regular tasks)
        // and every project's runtimes, keeping the first runtime per id.
        for runtime in ir.runtimes {
            if !compiled_runtimes
                .iter()
                .any(|existing: &cuenv_ci::ir::Runtime| existing.id == runtime.id)
            {
                compiled_runtimes.push(runtime);
            }
        }
        all_ir_tasks.extend(ir.tasks);
    }

    Ok(CollectedTasks {
        tasks: all_ir_tasks,
        runtimes: compiled_runtimes,
        environment: pipeline_environment,
        buildkite_cache: buildkite_cache.unwrap_or_default(),
    })
}

/// Emit pipeline using the appropriate emitter.
fn emit_pipeline(
    ir: &IntermediateRepresentation,
    format: ExportFormat,
    buildkite_cache: BuildkiteCache<'_>,
) -> Result<String> {
    match format {
        ExportFormat::Buildkite => emit_buildkite(ir, buildkite_cache),
        ExportFormat::Gitlab => emit_gitlab(ir),
        ExportFormat::GithubActions => emit_github_actions(ir),
        ExportFormat::Circleci => emit_circleci(ir),
    }
}

#[cfg(feature = "buildkite")]
fn emit_buildkite(ir: &IntermediateRepresentation, cache: BuildkiteCache<'_>) -> Result<String> {
    use cuenv_buildkite::BuildkiteEmitter;
    use cuenv_buildkite::emitter::BuildkiteCacheTrust;
    let emitter = BuildkiteEmitter::new().with_emojis();
    let emitter = match cache {
        BuildkiteCache::Unconfigured => emitter,
        BuildkiteCache::Disabled => emitter.without_remote_cache(),
        BuildkiteCache::Configured {
            remote,
            ref_allowed,
        } => emitter.with_remote_cache(
            remote.clone(),
            if ref_allowed {
                BuildkiteCacheTrust::Protected
            } else {
                BuildkiteCacheTrust::Untrusted
            },
        ),
    };
    emitter
        .emit(ir)
        .map_err(|e| cuenv_core::Error::configuration(format!("Buildkite emitter failed: {e}")))
}

#[cfg(not(feature = "buildkite"))]
fn emit_buildkite(_ir: &IntermediateRepresentation, _cache: BuildkiteCache<'_>) -> Result<String> {
    Err(cuenv_core::Error::configuration(
        "Buildkite support is not enabled. Rebuild with --features buildkite",
    ))
}

#[cfg(feature = "github")]
fn emit_github_actions(ir: &IntermediateRepresentation) -> Result<String> {
    use cuenv_github::workflow::GitHubActionsEmitter;
    let emitter = GitHubActionsEmitter::default();
    emitter.emit(ir).map_err(|e| {
        cuenv_core::Error::configuration(format!("GitHub Actions emitter failed: {e}"))
    })
}

#[cfg(not(feature = "github"))]
fn emit_github_actions(_ir: &IntermediateRepresentation) -> Result<String> {
    Err(cuenv_core::Error::configuration(
        "GitHub Actions support is not enabled. Rebuild with --features github",
    ))
}

fn emit_gitlab(_ir: &IntermediateRepresentation) -> Result<String> {
    // TODO: Implement GitLab emitter
    Err(cuenv_core::Error::configuration(
        "GitLab CI export is not yet implemented",
    ))
}

fn emit_circleci(_ir: &IntermediateRepresentation) -> Result<String> {
    // TODO: Implement CircleCI emitter
    Err(cuenv_core::Error::configuration(
        "CircleCI export is not yet implemented",
    ))
}

/// Output YAML to stdout or file based on args.
fn output_yaml(args: &CiArgs, yaml: &str) -> Result<()> {
    if let Some(path) = &args.output {
        let mut file = std::fs::File::create(path)
            .map_err(|e| cuenv_core::Error::io_with_path("create", path.clone(), e))?;
        file.write_all(yaml.as_bytes())
            .map_err(|e| cuenv_core::Error::io_with_path("write", path.clone(), e))?;
        tracing::info!(path = %path.display(), "Wrote pipeline YAML");
        Ok(())
    } else {
        cuenv_events::println_redacted(yaml);
        Ok(())
    }
}

#[cfg(all(test, feature = "buildkite"))]
mod tests {
    use super::*;

    fn webhook() -> CIContext {
        CIContext {
            provider: "buildkite".into(),
            event: "webhook".into(),
            ref_name: "main".into(),
            ..Default::default()
        }
    }

    #[test]
    fn buildkite_requires_explicit_non_pr_webhook_metadata() {
        let mut context = webhook();
        let non_pr = |name: &str| (name == "BUILDKITE_PULL_REQUEST").then(|| "false".into());
        assert_eq!(
            BuildkiteRef::detect(&context, non_pr),
            Some(BuildkiteRef::Branch("main".into()))
        );
        assert_eq!(BuildkiteRef::detect(&context, |_| None), None);
        assert_eq!(
            BuildkiteRef::detect(&context, |name| {
                (name == "BUILDKITE_PULL_REQUEST").then(|| "12".into())
            }),
            None
        );

        context.base_ref = Some("main".into());
        assert_eq!(BuildkiteRef::detect(&context, non_pr), None);
        context.base_ref = None;
        context.event = "api".into();
        assert_eq!(BuildkiteRef::detect(&context, non_pr), None);
    }

    #[test]
    fn buildkite_matches_a_tag_against_tags_only() {
        let context = webhook();
        let reference = BuildkiteRef::detect(&context, |name| match name {
            "BUILDKITE_PULL_REQUEST" => Some("false".into()),
            "BUILDKITE_TAG" => Some("v1.0.0".into()),
            _ => None,
        });
        assert_eq!(reference, Some(BuildkiteRef::Tag("v1.0.0".into())));
        let mut trigger = TriggerCondition {
            branches: vec!["v1.0.0".into()],
            tags: vec!["v*".into()],
            ..Default::default()
        };
        assert!(!reference.as_ref().is_some_and(|r| r.allowed_by(&trigger)));
        trigger.tags.push("v1.0.0".into());
        assert!(reference.as_ref().is_some_and(|r| r.allowed_by(&trigger)));
        trigger.tags.push("!v1.*".into());
        assert!(!reference.as_ref().is_some_and(|r| r.allowed_by(&trigger)));
    }

    #[test]
    fn buildkite_export_routes_credentials_by_build_and_project_configuration()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let repo = tempfile::tempdir()?;
        let project: Project = serde_json::from_value(serde_json::json!({
            "name": "app",
            "tasks": { "build": { "command": "true" } },
            "ci": {
                "pipelines": {
                    "default": {
                        "tasks": [{ "_name": "build" }],
                        "when": { "branch": "main", "pullRequest": true },
                        "derivePaths": false
                    }
                }
            },
            "cache": {
                "remote": {
                    "endpoint": "https://cache.example.com",
                    "protocol": "http",
                    "upload": true,
                    "auth": { "bearerTokenEnv": "CACHE_TOKEN" },
                    "ci": {
                        "readSecret": "CACHE_READ",
                        "writeSecret": "CACHE_WRITE",
                        "writeQueue": "protected"
                    }
                }
            }
        }))?;
        let mut projects = vec![(repo.path().to_path_buf(), project.clone())];
        let branch = BuildkiteRef::Branch("main".into());
        let pipeline = ExportPipeline {
            name: "default",
            module_root: repo.path(),
            buildkite_ref: Some(&branch),
        };

        let trusted = collect_tasks_for_export(&projects, pipeline)?;
        let mut ir = IntermediateRepresentation::new("default");
        ir.tasks = trusted.tasks;
        let yaml = emit_pipeline(&ir, ExportFormat::Buildkite, trusted.buildkite_cache)?;
        assert!(yaml.contains("cuenv ci --pipeline default"), "{yaml}");
        assert!(yaml.contains("CACHE_TOKEN: CACHE_WRITE"), "{yaml}");
        assert!(yaml.contains("queue: protected"), "{yaml}");

        let pull_request = collect_tasks_for_export(
            &projects,
            ExportPipeline {
                buildkite_ref: None,
                ..pipeline
            },
        )?;
        let yaml = emit_pipeline(&ir, ExportFormat::Buildkite, pull_request.buildkite_cache)?;
        assert!(yaml.contains("CACHE_TOKEN: CACHE_READ"), "{yaml}");
        assert!(!yaml.contains("CACHE_WRITE"), "{yaml}");
        assert!(!yaml.contains("queue: protected"), "{yaml}");

        let conflicting_project = Project {
            name: "other".into(),
            cache: None,
            ..project
        };
        projects.push((repo.path().join("other"), conflicting_project));
        let mixed = collect_tasks_for_export(&projects, pipeline)?;
        let yaml = emit_pipeline(&ir, ExportFormat::Buildkite, mixed.buildkite_cache)?;
        assert!(yaml.contains("CUENV_REMOTE_CACHE: ''"), "{yaml}");
        assert!(
            yaml.contains("CUENV_REMOTE_CACHE_UPLOAD: 'false'"),
            "{yaml}"
        );
        assert!(!yaml.contains("secrets:"), "{yaml}");
        Ok(())
    }
}
