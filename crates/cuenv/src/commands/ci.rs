//! CI command execution module
//!
//! Handles CI pipeline generation and execution for various providers
//! (GitHub Actions, Buildkite, etc.)

use crate::providers::detect_ci_provider;
use cuenv_ci::affected::compute_affected_tasks;
use cuenv_ci::compiler::{Compiler, CompilerOptions};
use cuenv_ci::discovery::discover_projects;
use cuenv_ci::emitter::Emitter;
use cuenv_ci::executor::run_ci;
use cuenv_ci::ir::{IntermediateRepresentation, PipelineMetadata, StageConfiguration};
use cuenv_core::Result;

#[allow(clippy::print_stdout)]
pub async fn execute_ci(
    dry_run: bool,
    pipeline: Option<String>,
    generate: Option<String>,
    format: Option<String>,
    from: Option<String>,
    force: bool,
    check: bool,
) -> Result<()> {
    // Handle --format option for dynamic pipeline output
    if let Some(fmt) = format {
        return execute_format_output(&fmt, pipeline, from, check).await;
    }

    // Handle --generate option for static workflow file generation
    if let Some(provider) = generate {
        return execute_generate(&provider, force);
    }

    // Default: run CI pipelines
    let provider = detect_ci_provider(from);
    run_ci(provider, dry_run, pipeline).await
}

/// Output pipeline in the specified format (e.g., buildkite, github) for dynamic pipelines
#[allow(clippy::print_stdout)]
async fn execute_format_output(
    fmt: &str,
    pipeline: Option<String>,
    from: Option<String>,
    check: bool,
) -> Result<()> {
    match fmt {
        "buildkite" => {
            #[cfg(feature = "buildkite")]
            {
                execute_buildkite_format(pipeline, from).await
            }
            #[cfg(not(feature = "buildkite"))]
            {
                let _ = (pipeline, from);
                Err(cuenv_core::Error::configuration(
                    "Buildkite support is not enabled. Rebuild with --features buildkite",
                ))
            }
        }
        "github" => execute_github_format(pipeline.as_deref(), check),
        _ => Err(cuenv_core::Error::configuration(format!(
            "Unsupported format: {fmt}. Supported formats: buildkite, github"
        ))),
    }
}

/// Execute GitHub Actions format output - generates workflow files
/// When check=true without --pipeline, checks ALL pipelines are in sync.
#[allow(clippy::print_stdout)]
fn execute_github_format(pipeline: Option<&str>, check: bool) -> Result<()> {
    // Discover projects
    let projects = discover_projects()?;
    if projects.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "No cuenv projects found. Ensure env.cue files declare 'package cuenv'",
        ));
    }
    cuenv_events::emit_ci_projects_discovered!(projects.len());

    // Determine which pipelines to process
    // When no --pipeline is specified, process ALL pipelines
    let pipelines_to_process: Vec<String> = if let Some(p) = pipeline {
        vec![p.to_string()]
    } else {
        // No --pipeline specified: process all pipelines
        collect_all_pipeline_names(&projects)
    };

    if pipelines_to_process.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "No pipelines found in any project's CI configuration",
        ));
    }

    // Generate workflows for all pipelines
    let mut all_workflows: Vec<(String, String)> = Vec::new();
    for pipeline_name in &pipelines_to_process {
        let workflows = generate_workflow_for_pipeline(pipeline_name, &projects)?;
        all_workflows.extend(workflows);
    }

    let workflows_dir = std::path::Path::new(".github/workflows");

    // Check mode: compare generated content with existing files
    if check {
        let mut out_of_sync = Vec::new();
        for (filename, content) in &all_workflows {
            let path = workflows_dir.join(filename);
            if path.exists() {
                let existing =
                    std::fs::read_to_string(&path).map_err(|e| cuenv_core::Error::Io {
                        source: e,
                        path: Some(path.clone().into_boxed_path()),
                        operation: "read workflow file".to_string(),
                    })?;
                if existing != *content {
                    out_of_sync.push(filename.clone());
                }
            } else {
                out_of_sync.push(format!("{filename} (missing)"));
            }
        }
        if !out_of_sync.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "Workflows out of sync: {}. Run 'cuenv ci --format github' to update.",
                out_of_sync.join(", ")
            )));
        }
        eprintln!("All {} workflow(s) are in sync.", all_workflows.len());
        return Ok(());
    }

    // Normal mode: write workflow files
    if !workflows_dir.exists() {
        std::fs::create_dir_all(workflows_dir).map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(workflows_dir.to_path_buf().into_boxed_path()),
            operation: "create directory".to_string(),
        })?;
    }

    for (filename, content) in all_workflows {
        let workflow_path = workflows_dir.join(&filename);
        std::fs::write(&workflow_path, &content).map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(workflow_path.clone().into_boxed_path()),
            operation: "write workflow file".to_string(),
        })?;
        println!("Generated: {}", workflow_path.display());
    }

    Ok(())
}

/// Collect all pipeline names from discovered projects
fn collect_all_pipeline_names(
    projects: &[cuenv_ci::discovery::DiscoveredCIProject],
) -> Vec<String> {
    let mut names = std::collections::HashSet::new();
    for project in projects {
        if let Some(ci) = &project.config.ci {
            for pipeline in &ci.pipelines {
                names.insert(pipeline.name.clone());
            }
        }
    }
    let mut sorted: Vec<_> = names.into_iter().collect();
    sorted.sort();
    sorted
}

/// Generate workflow files for a single pipeline
fn generate_workflow_for_pipeline(
    pipeline_name: &str,
    projects: &[cuenv_ci::discovery::DiscoveredCIProject],
) -> Result<Vec<(String, String)>> {
    use cuenv_github::workflow::GitHubActionsEmitter;

    let mut all_ir_tasks = Vec::new();
    let mut found_pipeline = false;
    let mut github_config = cuenv_core::ci::GitHubConfig::default();
    let mut trigger_condition: Option<cuenv_ci::ir::TriggerCondition> = None;
    let mut project_name: Option<String> = None;
    let mut pipeline_environment: Option<String> = None;
    let mut compiled_stages = StageConfiguration::default();
    let mut compiled_runtimes = Vec::new();

    for project in projects {
        let config = &project.config;

        // Find pipeline in config
        let Some(ci) = &config.ci else {
            continue;
        };

        let Some(ci_pipeline) = ci.pipelines.iter().find(|p| p.name == pipeline_name) else {
            continue;
        };

        found_pipeline = true;
        project_name = Some(config.name.clone());
        pipeline_environment.clone_from(&ci_pipeline.environment);

        // Extract GitHub config (merged from CI-level and pipeline-level)
        github_config = ci.github_config_for_pipeline(pipeline_name);

        // Compile project to IR with pipeline context
        // The Compiler will set ir.pipeline.environment from the pipeline,
        // enabling contributors to self-detect their requirements.
        let options = CompilerOptions {
            pipeline: Some(ci_pipeline.clone()),
            ..Default::default()
        };
        let compiler = Compiler::with_options(config.clone(), options);
        let ir = compiler.compile().map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to compile project: {e}"))
        })?;

        // Build trigger condition for the specific pipeline we're generating
        // (compiler.compile() only builds trigger for first pipeline, so we need to rebuild)
        trigger_condition = Some(build_pipeline_trigger_condition(ci_pipeline, ci));

        // Filter IR tasks to pipeline tasks AND their dependencies
        // First, collect all task IDs we need (including transitive deps)
        let mut needed_tasks: std::collections::HashSet<String> =
            ci_pipeline.tasks.iter().cloned().collect();

        // Build a map of task id -> depends_on for the IR tasks
        let task_deps: std::collections::HashMap<String, Vec<String>> = ir
            .tasks
            .iter()
            .map(|t| (t.id.clone(), t.depends_on.clone()))
            .collect();

        // Recursively add dependencies
        let mut to_process: Vec<String> = ci_pipeline.tasks.clone();
        while let Some(task_id) = to_process.pop() {
            if let Some(deps) = task_deps.get(&task_id) {
                for dep in deps {
                    if needed_tasks.insert(dep.clone()) {
                        to_process.push(dep.clone());
                    }
                }
            }
        }

        // Filter IR tasks to needed tasks (pipeline tasks + their dependencies)
        let pipeline_tasks: Vec<_> = ir
            .tasks
            .into_iter()
            .filter(|t| needed_tasks.contains(&t.id))
            .collect();

        // Capture stages and runtimes from compiled IR
        compiled_stages = ir.stages;
        compiled_runtimes = ir.runtimes;

        all_ir_tasks.extend(pipeline_tasks);
    }

    if !found_pipeline {
        return Err(cuenv_core::Error::configuration(format!(
            "No pipeline named '{pipeline_name}' found in any project's CI configuration"
        )));
    }

    if all_ir_tasks.is_empty() {
        // No tasks in this pipeline - return empty workflows list
        return Ok(Vec::new());
    }

    // Build combined IR with trigger conditions from the pipeline
    // Note: requires_onepassword is now derived from stages (1Password contributor)
    let combined_ir = IntermediateRepresentation {
        version: "1.4".to_string(),
        pipeline: PipelineMetadata {
            name: pipeline_name.to_string(),
            environment: pipeline_environment,
            requires_onepassword: false, // Derived from stages, not stored
            project_name,
            trigger: trigger_condition,
        },
        runtimes: compiled_runtimes,
        stages: compiled_stages,
        tasks: all_ir_tasks,
    };

    // Create emitter from config (applies runner, cachix, paths_ignore from manifest)
    let emitter = GitHubActionsEmitter::from_config(&github_config).with_nix();

    // Emit workflow files
    let workflows = emitter.emit_workflows(&combined_ir).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to emit GitHub workflow: {e}"))
    })?;

    // Convert HashMap to Vec of tuples
    Ok(workflows.into_iter().collect())
}

/// Build trigger condition for a specific pipeline
fn build_pipeline_trigger_condition(
    pipeline: &cuenv_core::ci::Pipeline,
    ci_config: &cuenv_core::ci::CI,
) -> cuenv_ci::ir::TriggerCondition {
    use cuenv_ci::ir::{ManualTriggerConfig, TriggerCondition, WorkflowDispatchInputDef};
    use cuenv_core::ci::ManualTrigger;
    use std::collections::HashMap;

    let when = pipeline.when.as_ref();

    // Extract branch patterns
    let branches = when
        .and_then(|w| w.branch.as_ref())
        .map(cuenv_core::ci::StringOrVec::to_vec)
        .unwrap_or_default();

    // Extract pull_request setting
    let pull_request = when.and_then(|w| w.pull_request);

    // Extract scheduled cron expressions
    let scheduled = when
        .and_then(|w| w.scheduled.as_ref())
        .map(cuenv_core::ci::StringOrVec::to_vec)
        .unwrap_or_default();

    // Extract release types
    let release = when.and_then(|w| w.release.clone()).unwrap_or_default();

    // Build manual trigger config
    let manual = when.and_then(|w| w.manual.as_ref()).map(|m| match m {
        ManualTrigger::Enabled(enabled) => ManualTriggerConfig {
            enabled: *enabled,
            inputs: HashMap::new(),
        },
        ManualTrigger::WithInputs(inputs) => ManualTriggerConfig {
            enabled: true,
            inputs: inputs
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        WorkflowDispatchInputDef {
                            description: v.description.clone(),
                            required: v.required.unwrap_or(false),
                            default: v.default.clone(),
                            input_type: v.input_type.clone(),
                            options: v.options.clone().unwrap_or_default(),
                        },
                    )
                })
                .collect(),
        },
    });

    // Get paths_ignore from provider config
    let paths_ignore = ci_config
        .github_config_for_pipeline(&pipeline.name)
        .paths_ignore
        .unwrap_or_default();

    TriggerCondition {
        branches,
        pull_request,
        scheduled,
        release,
        manual,
        paths: Vec::new(), // Path derivation would require project access
        paths_ignore,
    }
}

/// Result of collecting affected tasks from projects
#[cfg(feature = "buildkite")]
struct CollectedTasks {
    tasks: Vec<cuenv_ci::ir::Task>,
    stages: StageConfiguration,
    runtimes: Vec<cuenv_ci::ir::Runtime>,
    environment: Option<String>,
}

/// Collect affected IR tasks from all projects for a given pipeline
#[cfg(feature = "buildkite")]
fn collect_affected_tasks_for_pipeline(
    projects: &[cuenv_ci::discovery::DiscoveredCIProject],
    project_map: &std::collections::HashMap<String, cuenv_ci::discovery::DiscoveredCIProject>,
    pipeline_name: &str,
    changed_files: &[std::path::PathBuf],
    event: &str,
) -> Result<CollectedTasks> {
    let mut all_ir_tasks = Vec::new();
    let mut pipeline_environment: Option<String> = None;
    let mut compiled_stages = StageConfiguration::default();
    let mut compiled_runtimes = Vec::new();

    for project in projects {
        let config = &project.config;

        let Some(ci) = &config.ci else {
            continue;
        };

        let Some(ci_pipeline) = ci.pipelines.iter().find(|p| p.name == pipeline_name) else {
            continue;
        };

        pipeline_environment.clone_from(&ci_pipeline.environment);

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

        let tasks_to_run = if event == "release" {
            ci_pipeline.tasks.clone()
        } else {
            compute_affected_tasks(
                changed_files,
                &ci_pipeline.tasks,
                project_root,
                config,
                project_map,
            )
        };

        if tasks_to_run.is_empty() {
            eprintln!("Project {}: No affected tasks", project.path.display());
            continue;
        }

        eprintln!(
            "Project {}: Affected tasks {:?}",
            project.path.display(),
            tasks_to_run
        );

        // Compile with pipeline context for environment-aware contributors
        let options = CompilerOptions {
            pipeline: Some(ci_pipeline.clone()),
            ..Default::default()
        };
        let compiler = Compiler::with_options(config.clone(), options);
        let ir = compiler.compile().map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to compile project: {e}"))
        })?;

        let affected_tasks: Vec<_> = ir
            .tasks
            .into_iter()
            .filter(|t| tasks_to_run.contains(&t.id))
            .collect();

        // Capture stages and runtimes from compiled IR
        compiled_stages = ir.stages;
        compiled_runtimes = ir.runtimes;

        all_ir_tasks.extend(affected_tasks);
    }

    Ok(CollectedTasks {
        tasks: all_ir_tasks,
        stages: compiled_stages,
        runtimes: compiled_runtimes,
        environment: pipeline_environment,
    })
}

/// Execute Buildkite format output - outputs pipeline YAML to stdout
#[cfg(feature = "buildkite")]
#[allow(clippy::print_stdout)]
async fn execute_buildkite_format(pipeline: Option<String>, from: Option<String>) -> Result<()> {
    use cuenv_buildkite::BuildkiteEmitter;
    use std::collections::HashMap;

    let provider = detect_ci_provider(from);
    let context = provider.context();
    let changed_files = provider.changed_files().await?;

    eprintln!(
        "Context: {} (event: {}, ref: {})",
        context.provider, context.event, context.ref_name
    );
    eprintln!("Changed files: {}", changed_files.len());

    let projects = discover_projects()?;
    if projects.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "No cuenv projects found. Ensure env.cue files declare 'package cuenv'",
        ));
    }
    eprintln!("Found {} projects", projects.len());

    let mut project_map = HashMap::new();
    for project in &projects {
        let name = project.config.name.trim();
        if !name.is_empty() {
            project_map.insert(name.to_string(), project.clone());
        }
    }

    let pipeline_name = pipeline.unwrap_or_else(|| "default".to_string());
    let collected = collect_affected_tasks_for_pipeline(
        &projects,
        &project_map,
        &pipeline_name,
        &changed_files,
        &context.event,
    )?;

    if collected.tasks.is_empty() {
        eprintln!("No affected tasks to run");
        println!("steps: []");
        return Ok(());
    }

    // Note: requires_onepassword is now derived from stages (1Password contributor)
    let combined_ir = IntermediateRepresentation {
        version: "1.4".to_string(),
        pipeline: PipelineMetadata {
            name: pipeline_name,
            environment: collected.environment,
            requires_onepassword: false, // Derived from stages, not stored
            project_name: None,
            trigger: None,
        },
        runtimes: collected.runtimes,
        stages: collected.stages,
        tasks: collected.tasks,
    };

    let emitter = BuildkiteEmitter::new().with_emojis();
    let yaml = emitter.emit(&combined_ir).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to emit Buildkite pipeline: {e}"))
    })?;

    println!("{yaml}");
    Ok(())
}

/// Generate static workflow file (e.g., GitHub Actions)
#[allow(clippy::print_stdout)]
fn execute_generate(provider: &str, force: bool) -> Result<()> {
    match provider {
        "github" => generate_github_workflow(force),
        "buildkite" => generate_buildkite_bootstrap(force),
        _ => Err(cuenv_core::Error::configuration(format!(
            "Unsupported CI provider for --generate: {provider}. Supported: github, buildkite"
        ))),
    }
}

#[allow(clippy::print_stdout)]
fn generate_github_workflow(force: bool) -> Result<()> {
    let workflow_content = r#"name: CI

on:
  push:
    branches: ["main"]
  pull_request:
    branches: ["main"]

jobs:
  ci:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - uses: cachix/install-nix-action@v22
        with:
          nix_path: nixpkgs=channel:nixos-unstable

      - name: Install cuenv
        run: curl -fsSL https://cuenv.sh/install | sh

      - name: Run CI
        run: cuenv ci
"#;

    let workflows_dir = std::path::Path::new(".github/workflows");
    if !workflows_dir.exists() {
        std::fs::create_dir_all(workflows_dir).map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(workflows_dir.to_path_buf().into_boxed_path()),
            operation: "create directory".to_string(),
        })?;
    }

    let workflow_path = workflows_dir.join("ci.yml");

    if workflow_path.exists() && !force {
        return Err(cuenv_core::Error::configuration(format!(
            "Workflow file already exists at: {}. Use --force to overwrite.",
            workflow_path.display()
        )));
    }

    std::fs::write(&workflow_path, workflow_content).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(workflow_path.clone().into_boxed_path()),
        operation: "write workflow file".to_string(),
    })?;

    println!(
        "Generated GitHub Actions workflow at: {}",
        workflow_path.display()
    );
    Ok(())
}

#[allow(clippy::print_stdout)]
fn generate_buildkite_bootstrap(force: bool) -> Result<()> {
    let pipeline_content = r#"# Buildkite bootstrap pipeline for cuenv
# This generates a dynamic pipeline based on affected tasks
steps:
  - label: ":pipeline: Generate Pipeline"
    command: cuenv ci --format buildkite | buildkite-agent pipeline upload
"#;

    let buildkite_dir = std::path::Path::new(".buildkite");
    if !buildkite_dir.exists() {
        std::fs::create_dir_all(buildkite_dir).map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(buildkite_dir.to_path_buf().into_boxed_path()),
            operation: "create directory".to_string(),
        })?;
    }

    let pipeline_path = buildkite_dir.join("pipeline.yml");

    if pipeline_path.exists() && !force {
        return Err(cuenv_core::Error::configuration(format!(
            "Pipeline file already exists at: {}. Use --force to overwrite.",
            pipeline_path.display()
        )));
    }

    std::fs::write(&pipeline_path, pipeline_content).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(pipeline_path.clone().into_boxed_path()),
        operation: "write pipeline file".to_string(),
    })?;

    println!(
        "Generated Buildkite bootstrap pipeline at: {}",
        pipeline_path.display()
    );
    Ok(())
}
