pub mod env;
pub(crate) mod env_file;
pub mod exec;
pub mod export;
pub mod hooks;
pub mod info;
pub mod owners;
pub mod release;
pub mod secrets;
pub mod sync;
pub mod task;
pub mod task_list;
pub mod task_picker;
pub mod version;

/// Convert cuengine error to `cuenv_core` error
pub(crate) fn convert_engine_error(err: cuengine::CueEngineError) -> cuenv_core::Error {
    match err {
        cuengine::CueEngineError::Configuration { message } => {
            cuenv_core::Error::configuration(message)
        }
        cuengine::CueEngineError::Ffi { function, message } => {
            cuenv_core::Error::ffi(function, message)
        }
        cuengine::CueEngineError::CueParse { path, message } => {
            cuenv_core::Error::cue_parse(&path, message)
        }
        cuengine::CueEngineError::Validation { message } => cuenv_core::Error::validation(message),
        cuengine::CueEngineError::Cache { message } => cuenv_core::Error::configuration(message),
    }
}

pub mod ci_cmd {
    use crate::providers::detect_ci_provider;
    use cuenv_ci::affected::compute_affected_tasks;
    use cuenv_ci::compiler::Compiler;
    use cuenv_ci::discovery::discover_projects;
    use cuenv_ci::emitter::Emitter;
    use cuenv_ci::executor::run_ci;
    use cuenv_ci::ir::{IntermediateRepresentation, PipelineMetadata};
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
        let mut requires_onepassword = false;

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

            // Detect if environment has 1Password secrets
            if let Some(env_name) = &ci_pipeline.environment
                && let Some(env) = &config.env
            {
                requires_onepassword = environment_has_onepassword_refs(env, env_name);
            }

            // Extract GitHub config (merged from CI-level and pipeline-level)
            github_config = ci.github_config_for_pipeline(pipeline_name);

            // Compile project to IR (this builds trigger conditions for the FIRST pipeline)
            let compiler = Compiler::new(config.clone());
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
        let combined_ir = IntermediateRepresentation {
            version: "1.3".to_string(),
            pipeline: PipelineMetadata {
                name: pipeline_name.to_string(),
                environment: pipeline_environment,
                requires_onepassword,
                project_name,
                trigger: trigger_condition,
            },
            runtimes: vec![],
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

    /// Check if environment contains 1Password secret references
    fn environment_has_onepassword_refs(
        env: &cuenv_core::environment::Env,
        environment_name: &str,
    ) -> bool {
        use cuenv_core::environment::{EnvValue, EnvValueSimple};

        let env_vars = env.for_environment(environment_name);
        env_vars.values().any(|value| match value {
            EnvValue::String(s) => s.starts_with("op://"),
            EnvValue::Secret(secret) => secret.resolver == "onepassword",
            EnvValue::WithPolicies(with_policies) => match &with_policies.value {
                EnvValueSimple::Secret(secret) => secret.resolver == "onepassword",
                EnvValueSimple::String(s) => s.starts_with("op://"),
                _ => false,
            },
            _ => false,
        })
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
        environment: Option<String>,
        requires_onepassword: bool,
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
        let mut requires_onepassword = false;

        for project in projects {
            let config = &project.config;

            let Some(ci) = &config.ci else {
                continue;
            };

            let Some(ci_pipeline) = ci.pipelines.iter().find(|p| p.name == pipeline_name) else {
                continue;
            };

            pipeline_environment.clone_from(&ci_pipeline.environment);

            // Detect if environment has 1Password secrets
            if let Some(env_name) = &ci_pipeline.environment
                && let Some(env) = &config.env
            {
                requires_onepassword = environment_has_onepassword_refs(env, env_name);
            }

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

            let compiler = Compiler::new(config.clone());
            let ir = compiler.compile().map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to compile project: {e}"))
            })?;

            let affected_tasks: Vec<_> = ir
                .tasks
                .into_iter()
                .filter(|t| tasks_to_run.contains(&t.id))
                .collect();

            all_ir_tasks.extend(affected_tasks);
        }

        Ok(CollectedTasks {
            tasks: all_ir_tasks,
            environment: pipeline_environment,
            requires_onepassword,
        })
    }

    /// Execute Buildkite format output - outputs pipeline YAML to stdout
    #[cfg(feature = "buildkite")]
    #[allow(clippy::print_stdout)]
    async fn execute_buildkite_format(
        pipeline: Option<String>,
        from: Option<String>,
    ) -> Result<()> {
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

        let combined_ir = IntermediateRepresentation {
            version: "1.3".to_string(),
            pipeline: PipelineMetadata {
                name: pipeline_name,
                environment: collected.environment,
                requires_onepassword: collected.requires_onepassword,
                project_name: None,
                trigger: None,
            },
            runtimes: vec![],
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
}

use crate::cli::{StatusFormat, SyncCommands};
use crate::events::{Event, EventSender};
use clap_complete::Shell;
use cuengine::ModuleEvalOptions;
use cuenv_core::{InstanceKind, ModuleEvaluation, Result};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use tokio::time::{Duration, sleep};
use tracing::{Level, event};

/// Compute the relative path from module root to target directory.
///
/// Returns the path suitable for looking up instances in `ModuleEvaluation`.
/// Returns `"."` for the module root itself.
pub fn relative_path_from_root(module_root: &Path, target: &Path) -> PathBuf {
    target.strip_prefix(module_root).map_or_else(
        |_| PathBuf::from("."),
        |p| {
            if p.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        },
    )
}

/// A guard that provides access to the loaded `ModuleEvaluation`.
///
/// This wrapper around `MutexGuard` ensures the inner `Option` is always `Some`
/// by the time it's constructed, providing direct access to the module.
pub struct ModuleGuard<'a> {
    guard: MutexGuard<'a, Option<ModuleEvaluation>>,
}

impl std::ops::Deref for ModuleGuard<'_> {
    type Target = ModuleEvaluation;

    fn deref(&self) -> &Self::Target {
        // SAFETY: ModuleGuard is only constructed after ensuring the Option is Some
        self.guard
            .as_ref()
            .expect("ModuleGuard invariant violated: module should be loaded")
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Command {
    Version {
        format: String,
    },
    Info {
        /// None = recursive (./...), Some(path) = specific directory only
        path: Option<String>,
        package: String,
        meta: bool,
    },
    EnvPrint {
        path: String,
        package: String,
        format: String,
        environment: Option<String>,
    },
    EnvLoad {
        path: String,
        package: String,
    },
    EnvStatus {
        path: String,
        package: String,
        wait: bool,
        timeout: u64,
        format: StatusFormat,
    },
    EnvInspect {
        path: String,
        package: String,
    },
    EnvCheck {
        path: String,
        package: String,
        shell: crate::cli::ShellType,
    },
    EnvList {
        path: String,
        package: String,
        format: String,
    },
    Task {
        path: String,
        package: String,
        name: Option<String>,
        labels: Vec<String>,
        environment: Option<String>,
        format: String,
        materialize_outputs: Option<String>,
        show_cache_path: bool,
        backend: Option<String>,
        tui: bool,
        interactive: bool,
        help: bool,
        all: bool,
        task_args: Vec<String>,
    },
    Exec {
        path: String,
        package: String,
        command: String,
        args: Vec<String>,
        environment: Option<String>,
    },
    ShellInit {
        shell: crate::cli::ShellType,
    },
    Allow {
        path: String,
        package: String,
        note: Option<String>,
        yes: bool,
    },
    Deny {
        path: String,
        package: String,
        all: bool,
    },
    Export {
        shell: Option<String>,
        package: String,
    },
    Ci {
        dry_run: bool,
        pipeline: Option<String>,
        generate: Option<String>,
        format: Option<String>,
        from: Option<String>,
        force: bool,
        check: bool,
    },
    Tui,
    Web {
        port: u16,
        host: String,
    },
    ChangesetAdd {
        path: String,
        summary: String,
        description: Option<String>,
        packages: Vec<(String, String)>,
    },
    ChangesetStatus {
        path: String,
        json: bool,
    },
    ChangesetFromCommits {
        path: String,
        since: Option<String>,
    },
    ReleaseVersion {
        path: String,
        dry_run: bool,
    },
    ReleasePublish {
        path: String,
        dry_run: bool,
    },
    Completions {
        shell: Shell,
    },
    Sync {
        subcommand: Option<SyncCommands>,
        path: String,
        package: String,
        dry_run: bool,
        check: bool,
        all: bool,
    },
    SecretsSetup {
        provider: crate::cli::SecretsProvider,
        wasm_url: Option<String>,
    },
}

/// Executes CLI commands with centralized module evaluation and event handling.
///
/// The `CommandExecutor` provides lazy-loading of CUE module evaluation, ensuring
/// that the module is only loaded when a command actually needs CUE access.
/// This avoids startup overhead for simple commands like `version` or `completions`.
#[allow(dead_code)]
pub struct CommandExecutor {
    event_sender: EventSender,
    /// Lazy-loaded module evaluation, cached after first access
    module: Mutex<Option<ModuleEvaluation>>,
    /// The CUE package name to evaluate (typically "cuenv")
    package: String,
}

#[allow(dead_code)]
impl CommandExecutor {
    /// Create a new executor with the specified event sender and package name.
    pub fn new(event_sender: EventSender, package: String) -> Self {
        Self {
            event_sender,
            module: Mutex::new(None),
            package,
        }
    }

    /// Get the CUE package name used for evaluation.
    #[allow(dead_code)]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Get or load the module evaluation (cached after first call).
    ///
    /// This method lazily loads the CUE module on first access and caches it
    /// for subsequent calls. Commands that don't need CUE evaluation
    /// (version, completions, etc.) never trigger this load.
    ///
    /// # Arguments
    /// * `path` - Directory to start searching for module root
    ///
    /// # Returns
    /// A `ModuleGuard` that provides direct access to the `ModuleEvaluation`
    pub fn get_module(&self, path: &Path) -> Result<ModuleGuard<'_>> {
        let mut guard = self
            .module
            .lock()
            .map_err(|_| cuenv_core::Error::configuration("Failed to acquire module lock"))?;

        if guard.is_none() {
            let module_root = env_file::find_cue_module_root(path).ok_or_else(|| {
                cuenv_core::Error::configuration(format!(
                    "No CUE module found (looking for cue.mod/) starting from: {}",
                    path.display()
                ))
            })?;

            // Evaluate the entire module recursively
            let options = ModuleEvalOptions {
                recursive: true,
                ..Default::default()
            };
            let raw = cuengine::evaluate_module(&module_root, &self.package, Some(options))
                .map_err(convert_engine_error)?;

            *guard = Some(ModuleEvaluation::from_raw(
                module_root,
                raw.instances,
                raw.projects,
            ));
        }

        Ok(ModuleGuard { guard })
    }

    /// Get the module root path if the module has been loaded.
    ///
    /// Returns `None` if `get_module` hasn't been called yet.
    #[allow(dead_code)]
    pub fn module_root(&self) -> Option<PathBuf> {
        self.module
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|m| m.root.clone()))
    }

    /// Compute the relative path from module root to target directory.
    ///
    /// This is a convenience wrapper around `relative_path_from_root` that
    /// uses the cached module root. Returns an error if the module hasn't
    /// been loaded yet.
    #[allow(dead_code)]
    pub fn relative_path(&self, target: &Path) -> Result<PathBuf> {
        let root = self.module_root().ok_or_else(|| {
            cuenv_core::Error::configuration("Module not loaded; call get_module first")
        })?;
        Ok(relative_path_from_root(&root, target))
    }

    /// Check if a path is a Project (vs Base) using schema unification.
    ///
    /// This uses the CUE schema verification performed during module evaluation
    /// to determine if an instance conforms to `schema.#Project`.
    #[allow(dead_code)]
    pub fn is_project(&self, path: &Path) -> bool {
        self.module
            .lock()
            .ok()
            .and_then(|guard| {
                guard
                    .as_ref()
                    .and_then(|m| m.get(path).map(|i| i.kind == InstanceKind::Project))
            })
            .unwrap_or(false)
    }

    #[allow(clippy::too_many_lines)]
    pub async fn execute(&self, command: Command) -> Result<()> {
        match command {
            Command::Version { format } => self.execute_version(format).await,
            Command::EnvPrint {
                path,
                package,
                format,
                environment,
            } => {
                self.execute_env_print(path, package, format, environment)
                    .await
            }
            Command::Task {
                path,
                package,
                name,
                labels,
                environment,
                format,
                materialize_outputs,
                show_cache_path,
                backend,
                tui,
                interactive,
                help,
                all,
                task_args,
            } => {
                self.execute_task(
                    path,
                    package,
                    name,
                    labels,
                    environment,
                    format,
                    materialize_outputs,
                    show_cache_path,
                    backend,
                    tui,
                    interactive,
                    help,
                    all,
                    task_args,
                )
                .await
            }
            Command::Exec {
                path,
                package,
                command,
                args,
                environment,
            } => {
                self.execute_exec(path, package, command, args, environment)
                    .await
            }
            Command::EnvLoad { path, package } => self.execute_env_load(path, package).await,
            Command::EnvStatus {
                path,
                package,
                wait,
                timeout,
                format,
            } => {
                self.execute_env_status(path, package, wait, timeout, format)
                    .await
            }
            Command::EnvInspect { path, package } => self.execute_env_inspect(path, package).await,
            Command::EnvCheck {
                path,
                package,
                shell,
            } => self.execute_env_check(path, package, shell).await,
            Command::EnvList {
                path,
                package,
                format,
            } => self.execute_env_list(path, package, format).await,
            Command::ShellInit { shell } => {
                self.execute_shell_init(shell);
                Ok(())
            }
            Command::Allow {
                path,
                package,
                note,
                yes,
            } => self.execute_allow(path, package, note, yes).await,
            Command::Deny { path, package, all } => self.execute_deny(path, package, all).await,
            Command::Export { shell, package } => self.execute_export(shell, package).await,
            Command::Ci {
                dry_run,
                pipeline,
                generate,
                format,
                from,
                force,
                check,
            } => {
                self.execute_ci(dry_run, pipeline, generate, format, from, force, check)
                    .await
            }
            Command::Sync {
                subcommand,
                path,
                package,
                dry_run,
                check,
                ..
            } => {
                self.execute_sync(subcommand, path, package, dry_run, check)
                    .await
            }
            // Tui, Web, Completions, Info, Secrets, and release commands are handled directly in main.rs
            Command::Tui
            | Command::Web { .. }
            | Command::Completions { .. }
            | Command::Info { .. }
            | Command::ChangesetAdd { .. }
            | Command::ChangesetStatus { .. }
            | Command::ChangesetFromCommits { .. }
            | Command::ReleaseVersion { .. }
            | Command::ReleasePublish { .. }
            | Command::SecretsSetup { .. } => Ok(()),
        }
    }

    async fn execute_sync(
        &self,
        subcommand: Option<SyncCommands>,
        path: String,
        package: String,
        dry_run: bool,
        check: bool,
    ) -> Result<()> {
        let command_name = "sync";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        // If no subcommand, run all sync operations (ignore + codeowners)
        // If specific subcommand, run only that operation
        // Note: Cubes is not part of the default aggregate sync
        let run_ignore = matches!(subcommand, None | Some(SyncCommands::Ignore { .. }));
        let run_codeowners = matches!(subcommand, None | Some(SyncCommands::Codeowners { .. }));

        // Handle Cubes subcommand separately as it has different parameters
        if let Some(SyncCommands::Cubes {
            path: cube_path,
            package: cube_package,
            dry_run: cube_dry_run,
            check: cube_check,
            diff,
            ..
        }) = &subcommand
        {
            let result = sync::execute_sync_cubes(
                cube_path,
                cube_package,
                *cube_dry_run,
                *cube_check,
                *diff,
                Some(self),
            )
            .await;

            match result {
                Ok(output) => {
                    // Print output to stdout (needed for CLI mode)
                    if !output.is_empty() {
                        println!("{output}");
                    }
                    self.send_event(Event::CommandComplete {
                        command: command_name.to_string(),
                        success: true,
                        output: output.clone(),
                    });
                    return Ok(());
                }
                Err(e) => {
                    self.send_event(Event::CommandComplete {
                        command: command_name.to_string(),
                        success: false,
                        output: format!("Cubes sync error: {e}"),
                    });
                    return Err(e);
                }
            }
        }

        let mut outputs = Vec::new();
        let mut had_error = false;

        if run_ignore {
            match sync::execute_sync_ignore(&path, &package, dry_run, check, Some(self)).await {
                Ok(output) => outputs.push(output),
                Err(e) => {
                    outputs.push(format!("Ignore sync error: {e}"));
                    had_error = true;
                }
            }
        }

        if run_codeowners {
            // CODEOWNERS is a single file at repo root, so it must aggregate all configs
            // from the workspace. Always use workspace sync for codeowners.
            let codeowners_result =
                sync::execute_sync_codeowners_workspace(&package, dry_run, check).await;
            match codeowners_result {
                Ok(output) => outputs.push(output),
                Err(e) => {
                    outputs.push(format!("Codeowners sync error: {e}"));
                    had_error = true;
                }
            }
        }

        let combined_output = outputs.join("\n");

        if had_error {
            self.send_event(Event::CommandComplete {
                command: command_name.to_string(),
                success: false,
                output: combined_output.clone(),
            });
            return Err(cuenv_core::Error::configuration(combined_output));
        }

        // Print output to stdout (needed for CLI mode)
        if !combined_output.is_empty() {
            println!("{combined_output}");
        }
        self.send_event(Event::CommandComplete {
            command: command_name.to_string(),
            success: true,
            output: combined_output,
        });
        Ok(())
    }

    async fn execute_version(&self, _format: String) -> Result<()> {
        let command_name = "version";

        // Send command start event
        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        // Simulate some work with progress updates
        for i in 0..=5 {
            #[allow(clippy::cast_precision_loss)] // Progress calculation for demo purposes
            let progress = i as f32 / 5.0;
            let message = match i {
                0 => "Initializing...".to_string(),
                1 => "Loading version info...".to_string(),
                2 => "Checking build metadata...".to_string(),
                3 => "Gathering system info...".to_string(),
                4 => "Formatting output...".to_string(),
                5 => "Complete".to_string(),
                _ => "Processing...".to_string(),
            };

            self.send_event(Event::CommandProgress {
                command: command_name.to_string(),
                progress,
                message,
            });

            if i < 5 {
                sleep(Duration::from_millis(200)).await;
            }
        }

        // Get version information
        let version_info = version::get_version_info();

        // Send completion event
        self.send_event(Event::CommandComplete {
            command: command_name.to_string(),
            success: true,
            output: version_info,
        });

        Ok(())
    }

    async fn execute_env_print(
        &self,
        path: String,
        package: String,
        format: String,
        environment: Option<String>,
    ) -> Result<()> {
        let command_name = "env print";

        // Send command start event
        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        // Execute the env print command (with cached module from self)
        match env::execute_env_print(&path, &package, &format, environment.as_deref(), Some(self))
            .await
        {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    #[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
    async fn execute_task(
        &self,
        path: String,
        package: String,
        name: Option<String>,
        labels: Vec<String>,
        environment: Option<String>,
        format: String,
        materialize_outputs: Option<String>,
        show_cache_path: bool,
        backend: Option<String>,
        tui: bool,
        interactive: bool,
        help: bool,
        all: bool,
        task_args: Vec<String>,
    ) -> Result<()> {
        let command_name = "task";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        // Execute the task command (with cached module from self)
        match task::execute_task(
            &path,
            &package,
            name.as_deref(),
            &labels,
            environment.as_deref(),
            &format,
            false,
            materialize_outputs.as_deref(),
            show_cache_path,
            backend.as_deref(),
            tui,
            interactive,
            help,
            all,
            &task_args,
            Some(self),
        )
        .await
        {
            Ok(output) => {
                // Print output to stdout (needed for CLI mode)
                if !output.is_empty() {
                    println!("{output}");
                }
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    async fn execute_exec(
        &self,
        path: String,
        package: String,
        command: String,
        args: Vec<String>,
        environment: Option<String>,
    ) -> Result<()> {
        let command_name = "exec";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        // Execute the exec command (with cached module from self)
        match exec::execute_exec(
            &path,
            &package,
            &command,
            &args,
            environment.as_deref(),
            Some(self),
        )
        .await
        {
            Ok(exit_code) => {
                let success = exit_code == 0;
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success,
                    output: format!("Command exited with code {exit_code}"),
                });
                if success {
                    Ok(())
                } else {
                    Err(cuenv_core::Error::configuration(format!(
                        "Command failed with exit code {exit_code}"
                    )))
                }
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    async fn execute_env_load(&self, path: String, package: String) -> Result<()> {
        let command_name = "env load";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match hooks::execute_env_load(&path, &package, Some(self)).await {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    async fn execute_env_status(
        &self,
        path: String,
        package: String,
        wait: bool,
        timeout: u64,
        format: StatusFormat,
    ) -> Result<()> {
        let command_name = "env status";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match hooks::execute_env_status(&path, &package, wait, timeout, format, Some(self)).await {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    async fn execute_env_check(
        &self,
        path: String,
        package: String,
        shell: crate::cli::ShellType,
    ) -> Result<()> {
        let command_name = "env check";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match hooks::execute_env_check(&path, &package, shell, Some(self)).await {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    async fn execute_env_list(&self, path: String, package: String, format: String) -> Result<()> {
        let command_name = "env list";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match env::execute_env_list(&path, &package, &format, Some(self)).await {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    async fn execute_env_inspect(&self, path: String, package: String) -> Result<()> {
        let command_name = "env inspect";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match hooks::execute_env_inspect(&path, &package, Some(self)).await {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    fn execute_shell_init(&self, shell: crate::cli::ShellType) {
        let command_name = "shell init";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        let output = hooks::execute_shell_init(shell);
        self.send_event(Event::CommandComplete {
            command: command_name.to_string(),
            success: true,
            output,
        });
    }

    async fn execute_allow(
        &self,
        path: String,
        package: String,
        note: Option<String>,
        yes: bool,
    ) -> Result<()> {
        let command_name = "allow";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match hooks::execute_allow(&path, &package, note, yes, Some(self)).await {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    async fn execute_deny(&self, path: String, package: String, all: bool) -> Result<()> {
        let command_name = "deny";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match hooks::execute_deny(&path, &package, all).await {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    /// Execute export command safely
    async fn execute_export(&self, shell: Option<String>, package: String) -> Result<()> {
        let command_name = "export";

        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match export::execute_export(shell.as_deref(), &package, Some(self)).await {
            Ok(output) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_ci(
        &self,
        dry_run: bool,
        pipeline: Option<String>,
        generate: Option<String>,
        format: Option<String>,
        from: Option<String>,
        force: bool,
        check: bool,
    ) -> Result<()> {
        let command_name = "ci";
        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        match ci_cmd::execute_ci(dry_run, pipeline, generate, format, from, force, check).await {
            Ok(()) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: true,
                    output: "CI execution completed".to_string(),
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: command_name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }

    fn send_event(&self, event: Event) {
        if let Err(e) = self.event_sender.send(event) {
            event!(Level::ERROR, "Failed to send event: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, EventReceiver};
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tokio::time::timeout;

    fn create_test_executor() -> (CommandExecutor, EventReceiver) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let executor = CommandExecutor::new(sender, "cuenv".to_string());
        (executor, receiver)
    }

    #[allow(dead_code)]
    async fn collect_events(mut receiver: EventReceiver, count: usize) -> Vec<Event> {
        let mut events = Vec::new();
        for _ in 0..count {
            if let Ok(Some(event)) = timeout(Duration::from_millis(500), receiver.recv()).await {
                events.push(event);
            }
        }
        events
    }

    #[tokio::test]
    async fn test_command_executor_new() {
        let (sender, _receiver) = mpsc::unbounded_channel();
        let executor = CommandExecutor::new(sender, "cuenv".to_string());
        assert!(matches!(executor, CommandExecutor { .. }));
        assert_eq!(executor.package(), "cuenv");
    }

    #[tokio::test]
    async fn test_execute_version_command() {
        let (executor, mut receiver) = create_test_executor();

        let handle = tokio::spawn(async move {
            executor
                .execute(Command::Version {
                    format: "simple".to_string(),
                })
                .await
        });

        // Collect events
        let mut events = Vec::new();
        while let Ok(Some(event)) = timeout(Duration::from_millis(1500), receiver.recv()).await {
            let is_complete = matches!(&event, Event::CommandComplete { .. });
            events.push(event);

            // Break when we receive CommandComplete
            if is_complete {
                break;
            }
        }

        let result = handle.await.unwrap();
        assert!(result.is_ok());

        // Verify we got start, progress, and complete events
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::CommandStart { command } if command == "version"))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::CommandProgress { .. }))
        );
        assert!(events.iter().any(|e| matches!(e, Event::CommandComplete { command, success: true, .. } if command == "version")));
    }

    #[tokio::test]
    async fn test_execute_version_progress_events() {
        let (executor, mut receiver) = create_test_executor();

        let handle = tokio::spawn(async move {
            executor
                .execute(Command::Version {
                    format: "simple".to_string(),
                })
                .await
        });

        // Collect progress events
        let mut progress_events = Vec::new();
        while let Ok(Some(event)) = timeout(Duration::from_millis(1500), receiver.recv()).await {
            if let Event::CommandProgress {
                progress, message, ..
            } = event
            {
                progress_events.push((progress, message));
            } else if matches!(event, Event::CommandComplete { .. }) {
                break;
            }
        }

        let result = handle.await.unwrap();
        assert!(result.is_ok());

        // Verify progress sequence
        assert!(!progress_events.is_empty());
        assert!(
            progress_events
                .iter()
                .any(|(_, msg)| msg.contains("Initializing"))
        );
        assert!(
            progress_events
                .iter()
                .any(|(_, msg)| msg.contains("Loading version info"))
        );
        assert!(
            progress_events
                .iter()
                .any(|(_, msg)| msg.contains("Complete"))
        );

        // Verify progress values
        let progress_values: Vec<f32> = progress_events.iter().map(|(p, _)| *p).collect();
        assert!(progress_values.first().unwrap() <= progress_values.last().unwrap());
    }

    #[tokio::test]
    async fn test_execute_env_print_success() {
        let (executor, mut receiver) = create_test_executor();

        // Mock successful env print
        let path = "/tmp/test".to_string();
        let package = "test-package".to_string();
        let format = "json".to_string();

        let handle = tokio::spawn(async move {
            executor
                .execute(Command::EnvPrint {
                    path,
                    package,
                    format,
                    environment: None,
                })
                .await
        });

        // Collect events
        let mut events = Vec::new();
        while let Ok(Some(event)) = timeout(Duration::from_millis(1500), receiver.recv()).await {
            let is_complete = matches!(&event, Event::CommandComplete { .. });
            events.push(event);
            if is_complete {
                break;
            }
        }

        // Note: This might fail due to actual file system operations
        // In a real test, we'd mock the env::execute_env_print function
        let _ = handle.await.unwrap();

        // Verify start event was sent
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::CommandStart { command } if command == "env print"))
        );
        // Verify complete event was sent (success depends on actual execution)
        assert!(events.iter().any(
            |e| matches!(e, Event::CommandComplete { command, .. } if command == "env print")
        ));
    }

    #[tokio::test]
    async fn test_command_enum_variants() {
        // Test Command enum creation
        let version_cmd = Command::Version {
            format: "simple".to_string(),
        };
        assert!(matches!(version_cmd, Command::Version { .. }));

        let env_cmd = Command::EnvPrint {
            path: "/test/path".to_string(),
            package: "test-pkg".to_string(),
            format: "yaml".to_string(),
            environment: Some("production".to_string()),
        };

        if let Command::EnvPrint {
            path,
            package,
            format,
            environment,
        } = env_cmd
        {
            assert_eq!(path, "/test/path");
            assert_eq!(package, "test-pkg");
            assert_eq!(format, "yaml");
            assert_eq!(environment, Some("production".to_string()));
        } else {
            panic!("Expected EnvPrint variant");
        }
    }

    #[tokio::test]
    async fn test_send_event() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let executor = CommandExecutor::new(sender, "cuenv".to_string());

        // Send a test event
        executor.send_event(Event::CommandStart {
            command: "test".to_string(),
        });

        // Verify event was received
        let event = receiver.recv().await.unwrap();
        assert!(matches!(event, Event::CommandStart { command } if command == "test"));
    }

    #[tokio::test]
    async fn test_send_event_with_closed_channel() {
        let (sender, receiver) = mpsc::unbounded_channel();
        let executor = CommandExecutor::new(sender, "cuenv".to_string());

        // Close the receiver
        drop(receiver);

        // Send event should not panic, just log error
        executor.send_event(Event::CommandStart {
            command: "test".to_string(),
        });

        // Test passes if no panic occurs
    }

    #[tokio::test]
    async fn test_execute_version_command_flow() {
        let (executor, mut receiver) = create_test_executor();

        let handle =
            tokio::spawn(async move { executor.execute_version("simple".to_string()).await });

        // Verify the complete flow
        let mut has_start = false;
        let mut has_progress = false;
        let mut has_complete = false;

        while let Ok(Some(event)) = timeout(Duration::from_millis(1500), receiver.recv()).await {
            match event {
                Event::CommandStart { command } if command == "version" => has_start = true,
                Event::CommandProgress { command, .. } if command == "version" => {
                    has_progress = true;
                }
                Event::CommandComplete {
                    command,
                    success: true,
                    ..
                } if command == "version" => {
                    has_complete = true;
                    break;
                }
                _ => {}
            }
        }

        let result = handle.await.unwrap();
        assert!(result.is_ok());
        assert!(has_start);
        assert!(has_progress);
        assert!(has_complete);
    }

    #[tokio::test]
    async fn test_command_debug_trait() {
        let cmd = Command::Version {
            format: "simple".to_string(),
        };
        let debug_str = format!("{cmd:?}");
        assert!(debug_str.contains("Version"));

        let cmd = Command::EnvPrint {
            path: "/path".to_string(),
            package: "pkg".to_string(),
            format: "json".to_string(),
            environment: None,
        };
        let debug_str = format!("{cmd:?}");
        assert!(debug_str.contains("EnvPrint"));
        assert!(debug_str.contains("/path"));
        assert!(debug_str.contains("pkg"));
        assert!(debug_str.contains("json"));
    }

    #[tokio::test]
    async fn test_command_clone_trait() {
        let original = Command::Version {
            format: "simple".to_string(),
        };
        let cloned = original.clone();
        assert!(matches!(cloned, Command::Version { .. }));

        let original = Command::EnvPrint {
            path: "/test".to_string(),
            package: "test".to_string(),
            format: "toml".to_string(),
            environment: Some("dev".to_string()),
        };
        let cloned = original.clone();

        if let Command::EnvPrint {
            path,
            package,
            format,
            environment,
        } = cloned
        {
            assert_eq!(path, "/test");
            assert_eq!(package, "test");
            assert_eq!(format, "toml");
            assert_eq!(environment, Some("dev".to_string()));
        } else {
            panic!("Clone failed");
        }
    }
}
