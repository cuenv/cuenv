//! Release workflow construction for GitHub Actions.

use crate::workflow::emitter::GitHubActionsEmitter;
use crate::workflow::schema::{
    Concurrency, Environment, Job, Matrix, PermissionLevel, Permissions, ReleaseTrigger, RunsOn,
    Step, Strategy, Workflow, WorkflowDispatchTrigger, WorkflowInput, WorkflowTriggers,
};
use cuenv_ci::ir::{BuildStage, IntermediateRepresentation};
use indexmap::IndexMap;

/// Target platform configuration for release builds.
#[derive(Debug, Clone)]
pub struct ReleaseTarget {
    /// Target identifier (e.g., "linux-x64")
    pub id: String,
    /// Rust target triple
    pub rust_triple: String,
    /// GitHub Actions runner
    pub runner: String,
}

impl ReleaseTarget {
    /// Default release targets: linux-x64, linux-arm64, darwin-arm64
    ///
    /// Uses the provided runner for Linux builds, falling back to "ubuntu-latest"
    /// if not specified.
    #[must_use]
    pub fn defaults_with_runner(linux_runner: Option<&str>) -> Vec<Self> {
        let linux = linux_runner.unwrap_or("ubuntu-latest").to_string();
        vec![
            Self {
                id: "linux-x64".to_string(),
                rust_triple: "x86_64-unknown-linux-gnu".to_string(),
                runner: linux.clone(),
            },
            Self {
                id: "linux-arm64".to_string(),
                rust_triple: "aarch64-unknown-linux-gnu".to_string(),
                runner: linux,
            },
            Self {
                id: "darwin-arm64".to_string(),
                rust_triple: "aarch64-apple-darwin".to_string(),
                runner: "macos-14".to_string(),
            },
        ]
    }

    /// Default release targets with ubuntu-latest for Linux builds.
    #[must_use]
    pub fn defaults() -> Vec<Self> {
        Self::defaults_with_runner(None)
    }
}

/// Builder for creating workflows with release matrix builds.
pub struct ReleaseWorkflowBuilder {
    emitter: GitHubActionsEmitter,
    targets: Vec<ReleaseTarget>,
}

impl ReleaseWorkflowBuilder {
    /// Create a new release workflow builder with default targets.
    ///
    /// Uses the emitter's configured runner for Linux builds.
    #[must_use]
    pub fn new(emitter: GitHubActionsEmitter) -> Self {
        let targets = ReleaseTarget::defaults_with_runner(Some(&emitter.runner));
        Self { emitter, targets }
    }

    /// Set custom release targets.
    #[must_use]
    pub fn with_targets(mut self, targets: Vec<ReleaseTarget>) -> Self {
        self.targets = targets;
        self
    }

    /// Build a release workflow with matrix build and publish jobs.
    #[must_use]
    pub fn build(&self, ir: &IntermediateRepresentation) -> Workflow {
        let workflow_name = GitHubActionsEmitter::build_workflow_name(ir);

        let triggers = WorkflowTriggers {
            release: Some(ReleaseTrigger {
                types: vec!["published".to_string()],
            }),
            workflow_dispatch: Some(WorkflowDispatchTrigger {
                inputs: {
                    let mut inputs = IndexMap::new();
                    inputs.insert(
                        "tag_name".to_string(),
                        WorkflowInput {
                            description: "Tag to release (e.g., v0.16.0)".to_string(),
                            required: Some(true),
                            default: None,
                            input_type: Some("string".to_string()),
                            options: None,
                        },
                    );
                    inputs
                },
            }),
            ..Default::default()
        };

        let mut jobs = IndexMap::new();
        jobs.insert("build".to_string(), self.build_matrix_job(ir));
        jobs.insert("publish".to_string(), self.build_publish_job(ir));

        Workflow {
            name: workflow_name,
            on: triggers,
            concurrency: Some(Concurrency {
                group: "${{ github.workflow }}-${{ github.head_ref || github.ref }}".to_string(),
                cancel_in_progress: Some(true),
            }),
            permissions: Some(Permissions {
                contents: Some(PermissionLevel::Write),
                id_token: Some(PermissionLevel::Write),
                ..Default::default()
            }),
            env: IndexMap::new(),
            jobs,
        }
    }

    /// Build the matrix build job.
    fn build_matrix_job(&self, ir: &IntermediateRepresentation) -> Job {
        let matrix_include: Vec<IndexMap<String, serde_yaml::Value>> = self
            .targets
            .iter()
            .map(|t| {
                let mut entry = IndexMap::new();
                entry.insert(
                    "target".to_string(),
                    serde_yaml::Value::String(t.id.clone()),
                );
                entry.insert(
                    "rust-triple".to_string(),
                    serde_yaml::Value::String(t.rust_triple.clone()),
                );
                entry.insert(
                    "runs-on".to_string(),
                    serde_yaml::Value::String(t.runner.clone()),
                );
                entry
            })
            .collect();

        let mut steps = Vec::new();

        steps.push(
            Step::uses("actions/checkout@v4")
                .with_name("Checkout")
                .with_input("fetch-depth", serde_yaml::Value::Number(0.into())),
        );

        steps.extend(
            self.emitter
                .stage_renderer()
                .render_tasks(&ir.sorted_phase_tasks(BuildStage::Bootstrap)),
        );

        if let Some(cuenv_task) = ir
            .sorted_phase_tasks(BuildStage::Setup)
            .iter()
            .find(|t| t.id == "setup-cuenv")
        {
            let command = cuenv_task.command.first().cloned().unwrap_or_default();
            steps.push(Step::run(&command).with_name("Setup cuenv"));
        }

        let environment = ir.pipeline.environment.as_deref();
        let build_cmd = environment.map_or_else(
            || "cuenv release binaries --build-only --target ${{ matrix.target }}".to_string(),
            |env| {
                "cuenv release binaries --build-only --target ${{ matrix.target }} -e $ENV"
                    .replace("$ENV", env)
            },
        );
        steps.push(Step::run(&build_cmd).with_name("Build for ${{ matrix.target }}"));

        let mut upload_step = Step::uses("actions/upload-artifact@v4")
            .with_name("Upload binary")
            .with_input(
                "name",
                serde_yaml::Value::String("binary-${{ matrix.target }}".to_string()),
            )
            .with_input(
                "path",
                serde_yaml::Value::String("target/${{ matrix.rust-triple }}/release/*".to_string()),
            );
        upload_step.with_inputs.insert(
            "if-no-files-found".to_string(),
            serde_yaml::Value::String("error".to_string()),
        );
        upload_step.with_inputs.insert(
            "include-hidden-files".to_string(),
            serde_yaml::Value::Bool(true),
        );
        steps.push(upload_step);

        Job {
            name: Some("Build ${{ matrix.target }}".to_string()),
            runs_on: RunsOn::Label("${{ matrix.runs-on }}".to_string()),
            needs: Vec::new(),
            if_condition: None,
            strategy: Some(Strategy {
                matrix: Matrix {
                    include: matrix_include,
                },
                fail_fast: Some(false),
                max_parallel: None,
            }),
            environment: None,
            env: IndexMap::new(),
            concurrency: None,
            continue_on_error: None,
            timeout_minutes: Some(60),
            steps,
        }
    }

    /// Build the publish job that runs after all builds complete.
    fn build_publish_job(&self, ir: &IntermediateRepresentation) -> Job {
        let mut steps = Vec::new();

        steps.push(
            Step::uses("actions/checkout@v4")
                .with_name("Checkout")
                .with_input("fetch-depth", serde_yaml::Value::Number(0.into())),
        );

        steps.extend(
            self.emitter
                .stage_renderer()
                .render_tasks(&ir.sorted_phase_tasks(BuildStage::Bootstrap)),
        );

        if let Some(cuenv_task) = ir
            .sorted_phase_tasks(BuildStage::Setup)
            .iter()
            .find(|t| t.id == "setup-cuenv")
        {
            let command = cuenv_task.command.first().cloned().unwrap_or_default();
            steps.push(Step::run(&command).with_name("Setup cuenv"));
        }

        for target in &self.targets {
            let mut download_step = Step::uses("actions/download-artifact@v4")
                .with_name(format!("Download {}", target.id))
                .with_input(
                    "name",
                    serde_yaml::Value::String(format!("binary-{}", target.id)),
                )
                .with_input(
                    "path",
                    serde_yaml::Value::String(format!("target/{}/release", target.rust_triple)),
                );
            download_step.continue_on_error = Some(false);
            steps.push(download_step);
        }

        let has_1password = ir
            .sorted_phase_tasks(BuildStage::Setup)
            .iter()
            .any(|t| t.id == "setup-1password");
        if has_1password {
            steps.push(Step::run("cuenv secrets setup onepassword").with_name("Setup 1Password"));
        }

        let environment = ir.pipeline.environment.as_deref();
        let publish_cmd = environment.map_or_else(
            || "cuenv release binaries --publish-only".to_string(),
            |env| format!("cuenv release binaries --publish-only -e {env}"),
        );
        let mut publish_step = Step::run(&publish_cmd).with_name("Publish release");
        GitHubActionsEmitter::add_github_context_env(&mut publish_step);

        if has_1password {
            publish_step.env.insert(
                "OP_SERVICE_ACCOUNT_TOKEN".to_string(),
                "${{ secrets.OP_SERVICE_ACCOUNT_TOKEN }}".to_string(),
            );
        }
        steps.push(publish_step);

        Job {
            name: Some("Publish Release".to_string()),
            runs_on: RunsOn::Label(self.emitter.runner.clone()),
            needs: vec!["build".to_string()],
            if_condition: None,
            strategy: None,
            environment: Some(Environment::Name(
                ir.pipeline
                    .environment
                    .clone()
                    .unwrap_or_else(|| "production".to_string()),
            )),
            env: IndexMap::new(),
            concurrency: None,
            continue_on_error: None,
            timeout_minutes: Some(30),
            steps,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::hestia::HESTIA_ACTION;
    use cuenv_ci::ir::{CachePolicy, PipelineMetadata, Task};
    use cuenv_core::ci::PipelineMode;
    use std::collections::BTreeMap;

    struct ActionTaskFixture<'a> {
        id: &'a str,
        label: &'a str,
        priority: i32,
        uses: &'a str,
    }

    fn action_task(fixture: &ActionTaskFixture<'_>) -> Task {
        Task {
            id: fixture.id.to_string(),
            runtime: None,
            command: Vec::new(),
            shell: false,
            env: BTreeMap::new(),
            secrets: BTreeMap::new(),
            resources: None,
            concurrency_group: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            depends_on: Vec::new(),
            cache_policy: CachePolicy::Disabled,
            deployment: false,
            manual_approval: false,
            matrix: None,
            artifact_downloads: Vec::new(),
            params: BTreeMap::new(),
            phase: Some(BuildStage::Bootstrap),
            label: Some(fixture.label.to_string()),
            priority: Some(fixture.priority),
            contributor: Some("test".to_string()),
            condition: None,
            provider_hints: Some(serde_json::json!({
                "github_action": {
                    "uses": fixture.uses,
                    "inputs": {}
                }
            })),
        }
    }

    fn release_ir() -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.5".to_string(),
            pipeline: PipelineMetadata {
                name: "release".to_string(),
                mode: PipelineMode::Expanded,
                environment: Some("production".to_string()),
                requires_onepassword: false,
                project_name: Some("example".to_string()),
                project_path: None,
                trigger: None,
                pipeline_tasks: Vec::new(),
                pipeline_task_defs: Vec::new(),
            },
            runtimes: Vec::new(),
            tasks: vec![
                action_task(&ActionTaskFixture {
                    id: "install-nix",
                    label: "Install Determinate Nix",
                    priority: 2,
                    uses: "DeterminateSystems/determinate-nix-action@v3",
                }),
                action_task(&ActionTaskFixture {
                    id: "hestia.setup",
                    label: "Setup Hestia Nix Cache",
                    priority: 4,
                    uses: HESTIA_ACTION,
                }),
            ],
        }
    }

    fn assert_hestia_after_nix(job: &Job) -> Result<(), String> {
        let nix_index = job
            .steps
            .iter()
            .position(|step| {
                step.uses.as_deref() == Some("DeterminateSystems/determinate-nix-action@v3")
            })
            .ok_or_else(|| {
                format!(
                    "{} should install Nix",
                    job.name.as_deref().unwrap_or("job")
                )
            })?;
        let hestia_index = job
            .steps
            .iter()
            .position(|step| step.uses.as_deref() == Some(HESTIA_ACTION))
            .ok_or_else(|| {
                format!(
                    "{} should install Hestia",
                    job.name.as_deref().unwrap_or("job")
                )
            })?;

        assert!(
            hestia_index > nix_index,
            "Hestia must be configured after Nix is installed"
        );
        Ok(())
    }

    #[test]
    fn release_jobs_render_bootstrap_cache_contributors() -> Result<(), String> {
        let workflow =
            ReleaseWorkflowBuilder::new(GitHubActionsEmitter::new()).build(&release_ir());
        let build = workflow
            .jobs
            .get("build")
            .ok_or_else(|| "release workflow should contain build job".to_string())?;
        let publish = workflow
            .jobs
            .get("publish")
            .ok_or_else(|| "release workflow should contain publish job".to_string())?;

        assert_hestia_after_nix(build)?;
        assert_hestia_after_nix(publish)
    }
}
