//! Release workflow construction for GitHub Actions.

use crate::workflow::emitter::GitHubActionsEmitter;
use crate::workflow::schema::{
    Concurrency, Environment, Job, Matrix, PermissionLevel, ReleaseTrigger, RunsOn, Step, Strategy,
    Workflow, WorkflowDispatchTrigger, WorkflowInput, WorkflowTriggers,
};
use cuenv_ci::ir::{BuildStage, IntermediateRepresentation};
use indexmap::IndexMap;

const CUENV_SETUP_TASK: &str = "cuenv:contributor:cuenv.setup";
const ONEPASSWORD_SETUP_TASK: &str = "cuenv:contributor:1password.setup";

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
            permissions: None,
            env: IndexMap::new(),
            jobs,
        }
    }

    /// Build the matrix build job.
    fn build_matrix_job(&self, ir: &IntermediateRepresentation) -> Job {
        let scoped_ir = GitHubActionsEmitter::ir_for_job(ir, "build");
        let ir = &scoped_ir;
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
            Step::uses("actions/checkout@v7")
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
            .find(|t| t.id == CUENV_SETUP_TASK)
        {
            steps.push(self.emitter.stage_renderer().render_task(cuenv_task));
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
            permissions: Some(
                self.emitter
                    .permissions_for_tasks(ir.tasks.iter().filter(|task| {
                        task.phase == Some(BuildStage::Bootstrap) || task.id == CUENV_SETUP_TASK
                    })),
            ),
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
        let scoped_ir = GitHubActionsEmitter::ir_for_job(ir, "publish");
        let ir = &scoped_ir;
        let mut steps = Vec::new();

        steps.push(
            Step::uses("actions/checkout@v7")
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
            .find(|t| t.id == CUENV_SETUP_TASK)
        {
            steps.push(self.emitter.stage_renderer().render_task(cuenv_task));
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

        let onepassword_task = ir
            .sorted_phase_tasks(BuildStage::Setup)
            .into_iter()
            .find(|task| task.id == ONEPASSWORD_SETUP_TASK);
        if let Some(task) = onepassword_task {
            steps.push(self.emitter.stage_renderer().render_task(task));
        }

        let environment = ir.pipeline.environment.as_deref();
        let publish_cmd = environment.map_or_else(
            || "cuenv release binaries --publish-only".to_string(),
            |env| format!("cuenv release binaries --publish-only -e {env}"),
        );
        let mut publish_step = Step::run(&publish_cmd).with_name("Publish release");
        GitHubActionsEmitter::add_github_context_env(&mut publish_step);

        if onepassword_task.is_some() {
            publish_step.env.insert(
                "OP_SERVICE_ACCOUNT_TOKEN".to_string(),
                "${{ secrets.OP_SERVICE_ACCOUNT_TOKEN }}".to_string(),
            );
        }
        steps.push(publish_step);

        Job {
            permissions: Some({
                let mut required =
                    GitHubActionsEmitter::required_permissions(ir.tasks.iter().filter(|task| {
                        task.phase == Some(BuildStage::Bootstrap)
                            || task.id == CUENV_SETUP_TASK
                            || task.id == ONEPASSWORD_SETUP_TASK
                    }));
                // Uploading GitHub release assets uses GITHUB_TOKEN.
                required.contents = Some(PermissionLevel::Write);
                self.emitter.apply_configured_permissions(required)
            }),
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

    #[test]
    fn release_permissions_are_job_scoped() {
        let emitter = GitHubActionsEmitter::new();
        let builder = ReleaseWorkflowBuilder::new(emitter);
        let workflow = builder.build(&release_ir());
        let document = serde_yaml::to_value(workflow).unwrap();
        assert!(document.get("permissions").is_none());
        assert_eq!(
            document["jobs"]["build"]["permissions"]["contents"].as_str(),
            Some("read")
        );
        assert_eq!(
            document["jobs"]["publish"]["permissions"]["contents"].as_str(),
            Some("write")
        );
        assert_eq!(
            document["jobs"]["publish"]["permissions"]["id-token"].as_str(),
            None
        );
    }

    #[test]
    fn release_jobs_render_compiled_contributor_steps_and_permissions()
    -> Result<(), Box<dyn std::error::Error>> {
        use cuenv_ci::compiler::{Compiler, CompilerOptions};
        use cuenv_core::ci::{CI, Pipeline};
        use cuenv_core::manifest::Project;

        let mut project = Project::new("release");
        project.ci = Some(serde_json::from_value::<CI>(serde_json::json!({
            "contributors": [
                {
                    "id": "cuenv",
                    "tasks": [{
                        "id": "cuenv.setup",
                        "label": "Setup cuenv",
                        "priority": 10,
                        "script": "install-cuenv --version stable",
                        "env": {"GITHUB_TOKEN": "${{ secrets.GITHUB_TOKEN }}"},
                        "ci": {"github": {"permissions": {"actions": "write"}}}
                    }]
                },
                {
                    "id": "1password",
                    "tasks": [{
                        "id": "1password.setup",
                        "label": "Setup 1Password",
                        "priority": 20,
                        "command": "cuenv",
                        "args": ["secrets", "setup", "onepassword"],
                        "dependsOn": ["cuenv.setup"],
                        "env": {"OP_SERVICE_ACCOUNT_TOKEN": "${OP_SERVICE_ACCOUNT_TOKEN}"},
                        "ci": {"github": {"permissions": {"id-token": "write"}}}
                    }]
                }
            ]
        }))?);
        let ir = Compiler::with_options(
            project,
            CompilerOptions {
                pipeline: Some(Pipeline {
                    environment: Some("production".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .compile()?;
        assert!(ir.tasks.iter().any(|task| task.id == CUENV_SETUP_TASK));
        assert!(
            ir.tasks
                .iter()
                .any(|task| task.id == ONEPASSWORD_SETUP_TASK)
        );

        let workflow = ReleaseWorkflowBuilder::new(GitHubActionsEmitter::new()).build(&ir);
        let build = &workflow.jobs["build"];
        let publish = &workflow.jobs["publish"];
        for job in [build, publish] {
            let setup = job
                .steps
                .iter()
                .find(|step| step.name.as_deref() == Some("Setup cuenv"))
                .ok_or("cuenv setup is missing")?;
            assert_eq!(setup.run.as_deref(), Some("install-cuenv --version stable"));
            assert_eq!(
                setup.env.get("GITHUB_TOKEN").map(String::as_str),
                Some("${{ secrets.GITHUB_TOKEN }}")
            );
            assert_eq!(
                job.permissions
                    .as_ref()
                    .and_then(|permissions| permissions.actions),
                Some(PermissionLevel::Write)
            );
        }
        let setup = publish
            .steps
            .iter()
            .find(|step| step.name.as_deref() == Some("Setup 1Password"))
            .ok_or("1Password setup is missing")?;
        assert_eq!(
            setup.run.as_deref(),
            Some("cuenv secrets setup onepassword")
        );
        assert_eq!(
            setup
                .env
                .get("OP_SERVICE_ACCOUNT_TOKEN")
                .map(String::as_str),
            Some("${{ secrets.OP_SERVICE_ACCOUNT_TOKEN }}")
        );
        assert_eq!(
            publish
                .permissions
                .as_ref()
                .and_then(|permissions| permissions.id_token),
            Some(PermissionLevel::Write)
        );
        assert_eq!(
            build
                .permissions
                .as_ref()
                .and_then(|permissions| permissions.id_token),
            None
        );
        Ok(())
    }

    struct ActionTaskFixture<'fixture> {
        id: &'fixture str,
        label: &'fixture str,
        priority: i32,
        uses: &'fixture str,
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
            github_task_filter: Vec::new(),
            github_permissions: std::collections::BTreeMap::new(),
            github_execution: None,
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
                    label: "Install Nix",
                    priority: 2,
                    uses: "cachix/install-nix-action@v31",
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
            .position(|step| step.uses.as_deref() == Some("cachix/install-nix-action@v31"))
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
