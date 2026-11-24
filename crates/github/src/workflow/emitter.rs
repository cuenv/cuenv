//! GitHub Actions Workflow Emitter
//!
//! Transforms cuenv IR into GitHub Actions workflow YAML files.

use crate::workflow::schema::{
    CancelInProgress, Concurrency, Environment, Job, JobContainer, JobDefaults, Matrix,
    MatrixInclude, MergeGroupTrigger, PermissionLevel, Permissions, PullRequestTrigger,
    PushTrigger, ReleaseTrigger, RunDefaults, RunsOn, ScheduleTrigger, Step, Strategy, Workflow,
    WorkflowDispatchTrigger, WorkflowInput, WorkflowRunTrigger, WorkflowTriggers,
};
use crate::workflow::stage_renderer::GitHubStageRenderer;
use cuenv_ci::emitter::{Emitter, EmitterError, EmitterResult};
use cuenv_ci::ir::{BuildStage, IntermediateRepresentation, TriggerCondition};
use indexmap::IndexMap;
use std::collections::HashMap;

pub use super::jobs::{
    ArtifactAggregationJobOptions, CuenvBootstrapJobOptions, CuenvSetup, MatrixJobOptions,
    SimpleJobOptions, TaskExecution,
};
pub use super::release::{ReleaseTarget, ReleaseWorkflowBuilder};

/// The main step of a workflow emitted by
/// [`GitHubActionsEmitter::emit_command_workflow`].
#[derive(Debug, Clone, Copy)]
pub struct CommandWorkflow<'workflow> {
    /// Display name of the main step.
    pub step_name: &'workflow str,
    /// Id of the main step, for follow-up steps that read its outcome.
    pub step_id: Option<&'workflow str>,
    /// Shell command the main step runs.
    pub command: &'workflow str,
    /// Concurrency group of the workflow; the default is one group per
    /// workflow and ref.
    pub concurrency_group: Option<&'workflow str>,
    /// Whether a newer run in the same concurrency group cancels one that is
    /// still in progress.
    pub cancel_in_progress: bool,
    /// Condition for the job (`jobs.<id>.if`).
    pub job_if: Option<&'workflow str>,
    /// Steps that run after the main step, before the success and failure
    /// phase steps.
    pub follow_up_steps: &'workflow [Step],
    /// GitHub environment for the job. Unlike the pipeline environment, it
    /// does not select a cuenv environment (`CUENV_ENVIRONMENT`).
    pub job_environment: Option<&'workflow str>,
    /// What the main job's `GITHUB_TOKEN` may do.
    pub token_scope: TokenScope,
    /// A job that runs after the main job, with its own permissions and
    /// without the main job's secrets.
    pub follow_up_job: Option<&'workflow FollowUpJob<'workflow>>,
    /// Steps that run after setup, right before the main step.
    pub leading_steps: &'workflow [Step],
    /// Permissions the main job needs beyond its scope, as
    /// `(scope, "read" | "write")` pairs; configured ceilings still apply.
    pub additional_permissions: &'workflow [(&'workflow str, &'workflow str)],
}

/// What the main job's `GITHUB_TOKEN` may do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TokenScope {
    /// Everything the workflow needs: the tasks' permissions, check runs and,
    /// on pull requests, pull request comments.
    #[default]
    Workflow,
    /// Only the permissions the contributor tasks declare (reading contents
    /// at least): for a job that runs code a pull request controls.
    Tasks,
}

/// Whether `apply_task_github_job` should apply a task's `runner` override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RunnerOverridePolicy {
    /// Apply `task.github_job.runner` when configured.
    Apply,
    /// Preserve the runner already chosen (e.g., an arch-specific runner in a matrix job).
    PreserveExisting,
}

/// Policy for enforcing commit-SHA pins on emitted `uses:` steps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ActionPinPolicy {
    /// Allow tags, branches, or commit SHAs (default).
    #[default]
    Flexible,
    /// Fail workflow emission if any non-local `uses:` step is not pinned to a 40-char hex SHA.
    RequireSha,
}

/// A job that runs after a command workflow's main job.
#[derive(Debug, Clone)]
pub struct FollowUpJob<'a> {
    /// Suffix of the job id, after the main job's id and `-`.
    pub id_suffix: &'a str,
    /// Display name.
    pub name: &'a str,
    /// Condition (`jobs.<id>.if`); `needs.*.result` is the main job's.
    pub if_condition: &'a str,
    /// The job's own permissions.
    pub permissions: Permissions,
    /// Its steps.
    pub steps: &'a [Step],
}

/// GitHub Actions workflow emitter
///
/// Transforms cuenv IR into GitHub Actions workflow YAML that can be
/// committed to `.github/workflows/`.
///
/// # IR to GitHub Actions Mapping
///
/// | IR Field | GitHub Actions |
/// |----------|----------------|
/// | `pipeline.name` | Workflow `name:` |
/// | `pipeline.trigger.branch` | `on.push.branches` / `on.pull_request.branches` |
/// | `task.id` | Job key |
/// | `task.command` | Step with `run: cuenv task {task.id}` or the direct IR command |
/// | `task.depends_on` | Job `needs:` |
/// | `task.manual_approval` | Job with `environment:` |
/// | `task.concurrency_group` | Job-level `concurrency:` |
/// | `task.resources.tags` | `runs-on:` |
/// | `task.outputs` (orchestrator) | `actions/upload-artifact` step |
#[derive(Debug, Clone)]
pub struct GitHubActionsEmitter {
    /// Default runner for jobs
    pub runner: String,
    /// Include Nix installation steps
    pub use_nix: bool,
    /// Include Cachix caching steps
    pub use_cachix: bool,
    /// Cachix cache name
    pub cachix_name: Option<String>,
    /// Cachix auth token secret name
    pub cachix_auth_token_secret: String,
    /// Include cuenv build step (via nix build)
    pub build_cuenv: bool,
    /// Environment name for manual approval tasks
    pub approval_environment: String,
    /// Configured permissions from the manifest
    pub configured_permissions: HashMap<String, String>,
    /// Whether the pipeline explicitly configured `permissions: {}`
    pub workflow_permissions_mode: cuenv_ci::ir::GitHubPermissionsMode,
    /// Dynamic display name for workflow runs (`run-name`)
    pub run_name: Option<String>,
    /// Multi-label runner list when `provider.github.runner` is a list of labels
    pub runner_labels: Option<Vec<String>>,
    /// Default job timeout in minutes (`provider.github.timeoutMinutes`)
    pub timeout_minutes: Option<u32>,
    /// Configurable checkout step (provider/pipeline level; task checkout merges on top)
    pub checkout: Option<cuenv_core::ci::GitHubCheckoutConfig>,
    /// Action pin overrides keyed by `<owner>/<repo>`
    pub action_pins: std::collections::BTreeMap<String, cuenv_core::ci::GitHubActionPin>,
    /// Whether to require every emitted `uses:` step to be pinned to a full 40-char SHA
    pub action_pin_policy: ActionPinPolicy,
    /// Optional project remote cache configuration for trust-boundary workflow emission
    pub remote_cache: Option<cuenv_core::manifest::RemoteCache>,
}

impl Default for GitHubActionsEmitter {
    fn default() -> Self {
        Self {
            runner: "ubuntu-latest".to_string(),
            use_nix: true,
            use_cachix: false,
            cachix_name: None,
            cachix_auth_token_secret: "CACHIX_AUTH_TOKEN".to_string(),
            build_cuenv: true,
            approval_environment: "production".to_string(),
            configured_permissions: HashMap::new(),
            workflow_permissions_mode: cuenv_ci::ir::GitHubPermissionsMode::Inferred,
            run_name: None,
            runner_labels: None,
            timeout_minutes: None,
            checkout: None,
            action_pins: std::collections::BTreeMap::new(),
            action_pin_policy: ActionPinPolicy::Flexible,
            remote_cache: None,
        }
    }
}

impl GitHubActionsEmitter {
    /// Create a new GitHub Actions emitter with default settings
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure remote cache trust-boundary emission for generated workflows.
    #[must_use]
    pub fn with_remote_cache(mut self, remote_cache: cuenv_core::manifest::RemoteCache) -> Self {
        self.remote_cache = Some(remote_cache);
        self
    }

    pub(crate) fn stage_renderer(&self) -> GitHubStageRenderer {
        let mut renderer = GitHubStageRenderer::new()
            .with_cachix_auth_token_secret(self.cachix_auth_token_secret.clone());
        if let Some(name) = &self.cachix_name {
            renderer = renderer.with_cachix(name.clone());
        }
        renderer
    }

    pub(crate) fn add_github_context_env(step: &mut Step) {
        for (key, value) in [
            ("GITHUB_TOKEN", "${{ secrets.GITHUB_TOKEN }}"),
            ("GITHUB_ACTOR", "${{ github.actor }}"),
            ("GITHUB_REF_TYPE", "${{ github.ref_type }}"),
            ("GITHUB_REF_NAME", "${{ github.ref_name }}"),
        ] {
            step.env
                .entry(key.to_string())
                .or_insert_with(|| value.to_string());
        }
    }

    /// Apply cache settings to a step that runs the task executor. Returns the
    /// GitHub Environment required to release a write-capable credential.
    pub(super) fn apply_remote_cache_step_env(
        &self,
        step: &mut Step,
        ir: &IntermediateRepresentation,
        job_environment: Option<&str>,
    ) -> Option<String> {
        let remote = self.remote_cache.as_ref()?;
        let ci = remote.ci.as_ref();
        let auth_env = remote.auth.as_ref().and_then(|auth| {
            auth.bearer_token_env
                .as_deref()
                .or_else(|| auth.header.as_ref().map(|header| header.value_env.as_str()))
                .filter(|name| valid_cache_credential_env(name))
        });

        let protected_trigger = ir.pipeline.trigger.as_ref().is_some_and(|trigger| {
            let has_manual = trigger
                .manual
                .as_ref()
                .is_some_and(|manual| manual.enabled || !manual.inputs.is_empty());
            trigger.pull_request != Some(true)
                && trigger.scheduled.is_empty()
                && !has_manual
                && (!trigger.branches.is_empty()
                    || !trigger.tags.is_empty()
                    || !trigger.release.is_empty())
        });

        if ci.is_some_and(|ci| ci.auth_mode == cuenv_core::manifest::RemoteCacheCiAuthMode::Ambient)
        {
            let isolated = ci.is_none_or(|ci| ci.isolated_cache_dir);
            match ambient_upload_policy(remote, ir.pipeline.trigger.as_ref()) {
                AmbientUploadPolicy::Disabled => {
                    step.env
                        .insert("CUENV_REMOTE_CACHE_UPLOAD".into(), "false".into());
                    if isolated {
                        step.env.insert(
                            "CUENV_CACHE_DIR".into(),
                            "${{ runner.temp }}/cuenv-cache/untrusted".into(),
                        );
                    }
                }
                AmbientUploadPolicy::Always => {
                    step.env
                        .insert("CUENV_REMOTE_CACHE_UPLOAD".into(), "true".into());
                    if isolated {
                        step.env.insert(
                            "CUENV_CACHE_DIR".into(),
                            "${{ runner.temp }}/cuenv-cache/trusted".into(),
                        );
                    }
                }
                AmbientUploadPolicy::Dynamic(expr) => {
                    step.env.insert(
                        "CUENV_REMOTE_CACHE_UPLOAD".into(),
                        format!("${{{{ {expr} }}}}"),
                    );
                    if isolated {
                        step.env.insert(
                            "CUENV_CACHE_DIR".into(),
                            format!(
                                "${{{{ runner.temp }}}}/cuenv-cache/${{{{ ({expr}) && 'trusted' || 'untrusted' }}}}"
                            ),
                        );
                    }
                }
            }
            return None;
        }

        if let Some(name) = auth_env {
            step.env.shift_remove(name);
        }

        let write_secret = ci.and_then(|ci| ci.write_secret.as_deref());
        let write = if remote.upload && protected_trigger {
            write_secret
                .filter(|secret| valid_identifier(secret))
                .zip(ci.and_then(|ci| ci.write_environment.as_deref()))
                .filter(|(_, environment)| {
                    !environment.is_empty()
                        && job_environment.is_none_or(|existing| existing == *environment)
                })
        } else {
            None
        };

        let secret = write.map(|(secret, _)| secret).or_else(|| {
            ci.and_then(|ci| ci.read_secret.as_deref()).filter(|read| {
                valid_identifier(read)
                    && !write_secret.is_some_and(|write| read.eq_ignore_ascii_case(write))
            })
        });
        let Some((secret, variable)) = secret.zip(auth_env) else {
            self.disable_remote_cache_step_env(step);
            return None;
        };

        step.env
            .insert("CUENV_REMOTE_CACHE".into(), remote.endpoint.clone());
        step.env.insert(
            "CUENV_REMOTE_CACHE_PROTOCOL".into(),
            match remote.protocol {
                cuenv_core::manifest::CacheProtocol::Grpc => "grpc",
                cuenv_core::manifest::CacheProtocol::Http => "http",
            }
            .into(),
        );
        step.env.insert(
            "CUENV_REMOTE_CACHE_INSTANCE".into(),
            remote.instance.clone(),
        );
        step.env.insert(
            "CUENV_REMOTE_CACHE_UPLOAD".into(),
            write.is_some().to_string(),
        );
        step.env
            .insert(variable.to_string(), format!("${{{{ secrets.{secret} }}}}"));

        if ci.is_none_or(|ci| ci.isolated_cache_dir) {
            let cache_dir = if write.is_some() {
                "trusted"
            } else {
                "untrusted"
            };
            step.env.insert(
                "CUENV_CACHE_DIR".into(),
                format!("${{{{ runner.temp }}}}/cuenv-cache/{cache_dir}"),
            );
        }
        write.map(|(_, environment)| environment.to_string())
    }

    /// Disable inherited cache settings for commands outside the task executor.
    pub(super) fn disable_remote_cache_step_env(&self, step: &mut Step) {
        let Some(remote) = &self.remote_cache else {
            return;
        };
        step.env.insert("CUENV_REMOTE_CACHE".into(), String::new());
        step.env
            .insert("CUENV_REMOTE_CACHE_UPLOAD".into(), "false".into());
        if remote.ci.as_ref().is_none_or(|ci| ci.isolated_cache_dir) {
            step.env.insert(
                "CUENV_CACHE_DIR".into(),
                "${{ runner.temp }}/cuenv-cache/untrusted".into(),
            );
        }
    }

    /// Create an emitter from a `GitHubConfig` manifest configuration.
    ///
    /// This applies all configuration from the CUE manifest to the emitter.
    #[must_use]
    pub fn from_config(config: &crate::config::GitHubConfig) -> Self {
        let mut emitter = Self::default();

        // Apply runner configuration
        if let Some(runner) = &config.runner {
            match runner {
                cuenv_core::ci::StringOrVec::String(label) => {
                    emitter.runner.clone_from(label);
                }
                cuenv_core::ci::StringOrVec::Vec(labels) => {
                    if let Some(first) = labels.first() {
                        emitter.runner.clone_from(first);
                    }
                    emitter.runner_labels = Some(labels.clone());
                }
            }
        }

        // Apply default job timeout
        emitter.timeout_minutes = config.timeout_minutes;

        // Apply checkout configuration
        emitter.checkout.clone_from(&config.checkout);

        // Apply action pin overrides and strict pinning policy
        if let Some(pins) = &config.pins {
            emitter.action_pins.clone_from(pins);
        }
        if config.require_pinned_actions == Some(true) {
            emitter.action_pin_policy = ActionPinPolicy::RequireSha;
        }

        // Apply Cachix configuration
        if let Some(cachix) = &config.cachix {
            emitter.use_cachix = true;
            emitter.cachix_name = Some(cachix.name.clone());
            if let Some(auth_token) = &cachix.auth_token {
                emitter.cachix_auth_token_secret.clone_from(auth_token);
            }
        }

        // Apply run-name configuration
        emitter.run_name.clone_from(&config.run_name);

        // Apply configured permissions
        if let Some(permissions) = &config.permissions {
            emitter.configured_permissions.clone_from(permissions);
            emitter.workflow_permissions_mode = if permissions.is_empty() {
                cuenv_ci::ir::GitHubPermissionsMode::ExplicitEmpty
            } else {
                cuenv_ci::ir::GitHubPermissionsMode::Inferred
            };
        }

        emitter
    }

    /// Get the configured runner as a `RunsOn` value
    #[must_use]
    pub fn runner_as_runs_on(&self) -> RunsOn {
        self.runner_labels
            .as_ref()
            .filter(|labels| !labels.is_empty())
            .map_or_else(
                || RunsOn::Label(self.runner.clone()),
                |labels| RunsOn::Labels(labels.clone()),
            )
    }

    /// Apply job-level GitHub overrides from `task.github_job` and emitter defaults.
    pub(super) fn apply_task_github_job(
        &self,
        task: &cuenv_ci::ir::Task,
        job: &mut Job,
        runner_policy: RunnerOverridePolicy,
    ) {
        // Pipeline-level timeout default (task override applied below).
        if job.timeout_minutes.is_none() {
            job.timeout_minutes = self.timeout_minutes;
        }

        let Some(cfg) = task.github_job.as_ref() else {
            return;
        };

        if runner_policy == RunnerOverridePolicy::Apply
            && let Some(runner) = &cfg.runner
        {
            job.runs_on = match runner {
                cuenv_core::ci::StringOrVec::String(label) => RunsOn::Label(label.clone()),
                cuenv_core::ci::StringOrVec::Vec(labels) => RunsOn::Labels(labels.clone()),
            };
        }

        if let Some(concurrency) = &cfg.concurrency {
            let group = concurrency.group.clone().unwrap_or_else(|| {
                // Job concurrency groups are repository-wide, so namespace by workflow.
                format!(
                    "${{{{ github.workflow }}}}-{}-${{{{ github.ref }}}}",
                    task.id
                )
            });
            let cancel_in_progress = concurrency
                .cancel_in_progress
                .as_ref()
                .map(Self::resolve_cancel_in_progress);
            job.concurrency = Some(Concurrency {
                group,
                cancel_in_progress,
            });
        }

        if let Some(timeout) = cfg.timeout_minutes {
            job.timeout_minutes = Some(timeout);
        }

        if let Some(env_cfg) = &cfg.environment {
            job.environment = Some(match env_cfg {
                cuenv_core::ci::TaskGitHubEnvironment::Name(name) => {
                    Environment::Name(name.clone())
                }
                cuenv_core::ci::TaskGitHubEnvironment::Detailed { name, url } => match url {
                    Some(url) => Environment::WithUrl {
                        name: name.clone(),
                        url: url.clone(),
                    },
                    None => Environment::Name(name.clone()),
                },
            });
        }

        if let Some(container_cfg) = &cfg.container {
            job.container = Some(match container_cfg {
                cuenv_core::ci::TaskGitHubContainer::Image(image) => {
                    JobContainer::Image(image.clone())
                }
                cuenv_core::ci::TaskGitHubContainer::Detailed(c) => {
                    if c.options.is_none() && c.env.is_empty() && c.volumes.is_empty() {
                        JobContainer::Image(c.image.clone())
                    } else {
                        JobContainer::Config {
                            image: c.image.clone(),
                            options: c.options.clone(),
                            env: c.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                            volumes: c.volumes.clone(),
                        }
                    }
                }
            });
        }

        if let Some(defaults_cfg) = &cfg.defaults
            && let Some(run_cfg) = &defaults_cfg.run
            && (run_cfg.working_directory.is_some() || run_cfg.shell.is_some())
        {
            job.defaults = Some(JobDefaults {
                run: Some(RunDefaults {
                    working_directory: run_cfg.working_directory.clone(),
                    shell: run_cfg.shell.clone(),
                }),
            });
        }

        if let Some(if_expr) = &cfg.if_condition {
            job.if_condition = Some(if_expr.clone());
        }

        if !cfg.outputs.is_empty() {
            let step_id = Self::task_step_id(task).unwrap_or_else(|| sanitize_job_id(&task.id));
            for output_name in &cfg.outputs {
                job.outputs.insert(
                    output_name.clone(),
                    format!("${{{{ steps.{step_id}.outputs.{output_name} }}}}"),
                );
            }
        }

        if let Some(matrix_cfg) = &cfg.matrix {
            let include = match &matrix_cfg.include {
                Some(cuenv_core::ci::TaskGitHubMatrixInclude::Static(entries)) => {
                    MatrixInclude::Static(
                        entries
                            .iter()
                            .map(|entry| {
                                entry
                                    .iter()
                                    .map(|(k, v)| {
                                        (
                                            k.clone(),
                                            serde_yaml::to_value(v)
                                                .unwrap_or(serde_yaml::Value::Null),
                                        )
                                    })
                                    .collect()
                            })
                            .collect(),
                    )
                }
                Some(cuenv_core::ci::TaskGitHubMatrixInclude::Expression(expr)) => {
                    MatrixInclude::Expression(Self::format_dynamic_matrix_include(expr))
                }
                None => MatrixInclude::Static(Vec::new()),
            };
            job.strategy = Some(Strategy {
                matrix: Matrix { include },
                fail_fast: matrix_cfg.fail_fast,
                max_parallel: matrix_cfg.max_parallel,
            });
            if runner_policy == RunnerOverridePolicy::Apply
                && cfg.runner.is_none()
                && let Some(runs_on) = &matrix_cfg.runs_on
            {
                job.runs_on = match runs_on {
                    cuenv_core::ci::StringOrVec::String(label) => RunsOn::Label(label.clone()),
                    cuenv_core::ci::StringOrVec::Vec(labels) => RunsOn::Labels(labels.clone()),
                };
            }
        }
    }

    /// Resolve the step `id` for a task step when outputs or explicit `stepId` are configured.
    #[must_use]
    pub(super) fn task_step_id(task: &cuenv_ci::ir::Task) -> Option<String> {
        let cfg = task.github_job.as_ref()?;
        if let Some(step_id) = &cfg.step_id {
            Some(step_id.clone())
        } else if !cfg.outputs.is_empty() {
            Some(sanitize_job_id(&task.id))
        } else {
            None
        }
    }

    /// Format a dynamic `matrix.include` expression, wrapping job output references in `fromJSON(...)`.
    #[must_use]
    fn format_dynamic_matrix_include(expr: &str) -> String {
        let trimmed = expr.trim();
        let inner = trimmed
            .strip_prefix("${{")
            .and_then(|rest| rest.strip_suffix("}}"))
            .map(str::trim);
        if let Some(inner_expr) = inner {
            if inner_expr.to_ascii_lowercase().starts_with("fromjson(") {
                trimmed.to_string()
            } else {
                format!("${{{{ fromJSON({inner_expr}) }}}}")
            }
        } else if trimmed.to_ascii_lowercase().starts_with("fromjson(") {
            format!("${{{{ {trimmed} }}}}")
        } else {
            format!("${{{{ fromJSON({trimmed}) }}}}")
        }
    }

    /// Default GitHub workflow concurrency group expression.
    pub const DEFAULT_CONCURRENCY_GROUP: &'static str =
        "${{ github.workflow }}-${{ github.head_ref || github.ref }}";

    /// Default GitHub workflow concurrency group when `merge_group` is enabled,
    /// isolating each merge-queue entry by its `head_sha` so queued runs do not
    /// cancel one another.
    pub const MERGE_GROUP_CONCURRENCY_GROUP: &'static str = "${{ github.workflow }}-${{ github.event_name == 'merge_group' && github.event.merge_group.head_sha || github.head_ref || github.ref }}";

    /// Build workflow-level concurrency from IR pipeline metadata.
    #[must_use]
    pub fn build_workflow_concurrency(ir: &IntermediateRepresentation) -> Concurrency {
        let default_group = if ir
            .pipeline
            .trigger
            .as_ref()
            .is_some_and(|t| t.merge_group.is_some())
        {
            Self::MERGE_GROUP_CONCURRENCY_GROUP
        } else {
            Self::DEFAULT_CONCURRENCY_GROUP
        };

        let Some(cfg) = ir.pipeline.concurrency.as_ref() else {
            return Concurrency {
                group: default_group.to_string(),
                cancel_in_progress: Some(CancelInProgress::Bool(true)),
            };
        };

        let group = cfg
            .group
            .clone()
            .unwrap_or_else(|| default_group.to_string());
        let cancel_in_progress = cfg
            .cancel_in_progress
            .as_ref()
            .map(Self::resolve_cancel_in_progress);

        Concurrency {
            group,
            cancel_in_progress,
        }
    }

    fn resolve_cancel_in_progress(
        config: &cuenv_core::ci::CancelInProgressConfig,
    ) -> CancelInProgress {
        use cuenv_core::ci::CancelInProgressConfig;
        match config {
            CancelInProgressConfig::Bool(value) => CancelInProgress::Bool(*value),
            CancelInProgressConfig::Expression(expr) => CancelInProgress::Expression(expr.clone()),
            CancelInProgressConfig::ByEvent(by_event) => {
                let events = [
                    ("pull_request", by_event.pull_request),
                    ("push", by_event.push),
                    ("merge_group", by_event.merge_group),
                    ("workflow_run", by_event.workflow_run),
                    ("workflow_dispatch", by_event.manual),
                    ("schedule", by_event.scheduled),
                    ("release", by_event.release),
                ];
                let clauses: Vec<String> = events
                    .into_iter()
                    .filter(|&(_, enabled)| enabled == Some(true))
                    .map(|(event, _)| format!("github.event_name == '{event}'"))
                    .collect();
                if clauses.is_empty() {
                    CancelInProgress::Bool(false)
                } else {
                    CancelInProgress::Expression(format!("${{{{ {} }}}}", clauses.join(" || ")))
                }
            }
        }
    }

    /// Workflow-level permissions block (`Some({})` when explicitly empty, otherwise `None`).
    #[must_use]
    pub fn workflow_permissions(&self) -> Option<Permissions> {
        self.workflow_permissions_mode
            .is_explicit_empty()
            .then(Permissions::default)
    }

    /// Validate that explicit empty `permissions: {}` blocks do not conflict with
    /// scopes required by tasks or emitted contributor steps.
    ///
    /// # Errors
    ///
    /// Returns [`EmitterError::InvalidIR`] if a task or phase step requires a
    /// non-`none` permission scope withheld by an explicit empty `permissions: {}`.
    pub fn validate_workflow_permissions(
        &self,
        ir: &IntermediateRepresentation,
    ) -> EmitterResult<()> {
        if self.workflow_permissions_mode.is_explicit_empty() {
            Self::ensure_no_required_scopes(
                &format!("pipeline '{}'", ir.pipeline.name),
                ir.tasks.iter(),
            )?;
        }

        for task in ir.regular_tasks() {
            if !task.github_permissions_mode.is_explicit_empty() {
                continue;
            }
            let matching_phases = ir.tasks.iter().filter(|step| {
                step.phase.is_some()
                    && (step.github_task_filter.is_empty()
                        || step.github_task_filter.contains(&task.id))
            });
            Self::ensure_no_required_scopes(
                &format!("task '{}'", task.id),
                std::iter::once(task).chain(matching_phases),
            )?;
        }

        Ok(())
    }

    fn ensure_no_required_scopes<'a>(
        owner: &str,
        tasks: impl Iterator<Item = &'a cuenv_ci::ir::Task>,
    ) -> EmitterResult<()> {
        for task in tasks {
            if task.deployment {
                return Err(EmitterError::InvalidIR(format!(
                    "{owner} declares explicit empty `permissions: {{}}`, but task '{}' requires 'contents: write' (deployment)",
                    task.id
                )));
            }
            for (scope, level) in &task.github_permissions {
                if level != "none" {
                    return Err(EmitterError::InvalidIR(format!(
                        "{owner} declares explicit empty `permissions: {{}}`, but task '{}' requires '{scope}: {level}'",
                        task.id
                    )));
                }
            }
        }
        Ok(())
    }

    /// Clamp only explicitly configured scopes; a ceiling never adds a grant.
    #[must_use]
    pub fn apply_configured_permissions(&self, mut permissions: Permissions) -> Permissions {
        for (key, value) in &self.configured_permissions {
            if let (Some(Some(required)), Some(ceiling)) = (
                Self::permission_slot(&mut permissions, key),
                Self::permission_level(value),
            ) {
                *required = (*required).min(ceiling);
            }
        }
        permissions
    }

    fn permission_level(value: &str) -> Option<PermissionLevel> {
        match value {
            "none" => Some(PermissionLevel::None),
            "read" => Some(PermissionLevel::Read),
            "write" => Some(PermissionLevel::Write),
            _ => None,
        }
    }

    fn permission_slot<'a>(
        permissions: &'a mut Permissions,
        key: &str,
    ) -> Option<&'a mut Option<PermissionLevel>> {
        match key {
            "actions" => Some(&mut permissions.actions),
            "artifact-metadata" => Some(&mut permissions.artifact_metadata),
            "attestations" => Some(&mut permissions.attestations),
            "checks" => Some(&mut permissions.checks),
            "contents" => Some(&mut permissions.contents),
            "deployments" => Some(&mut permissions.deployments),
            "discussions" => Some(&mut permissions.discussions),
            "id-token" => Some(&mut permissions.id_token),
            "issues" => Some(&mut permissions.issues),
            "models" => Some(&mut permissions.models),
            "packages" => Some(&mut permissions.packages),
            "pages" => Some(&mut permissions.pages),
            "pull-requests" => Some(&mut permissions.pull_requests),
            "repository-projects" => Some(&mut permissions.repository_projects),
            "security-events" => Some(&mut permissions.security_events),
            "statuses" => Some(&mut permissions.statuses),
            _ => None,
        }
    }

    /// Set the default runner for jobs
    #[must_use]
    pub fn with_runner(mut self, runner: impl Into<String>) -> Self {
        self.runner = runner.into();
        self
    }

    /// Enable Nix installation steps
    #[must_use]
    pub const fn with_nix(mut self) -> Self {
        self.use_nix = true;
        self
    }

    /// Disable Nix installation steps
    #[must_use]
    pub const fn without_nix(mut self) -> Self {
        self.use_nix = false;
        self
    }

    /// Enable Cachix caching with the given cache name
    #[must_use]
    pub fn with_cachix(mut self, name: impl Into<String>) -> Self {
        self.use_cachix = true;
        self.cachix_name = Some(name.into());
        self
    }

    /// Set the Cachix auth token secret name
    #[must_use]
    pub fn with_cachix_auth_token_secret(mut self, secret: impl Into<String>) -> Self {
        self.cachix_auth_token_secret = secret.into();
        self
    }

    /// Disable automatic cuenv build step
    #[must_use]
    pub const fn without_cuenv_build(mut self) -> Self {
        self.build_cuenv = false;
        self
    }

    /// Set the environment name for manual approval tasks
    #[must_use]
    pub fn with_approval_environment(mut self, env: impl Into<String>) -> Self {
        self.approval_environment = env.into();
        self
    }

    /// Emit a thin mode workflow, returning the filename and YAML content.
    ///
    /// This is the primary entry point for thin workflow generation. It builds
    /// a single-job workflow that delegates task execution to `cuenv ci`.
    ///
    /// # Errors
    ///
    /// Returns `EmitterError::Serialization` if YAML serialization fails.
    pub fn emit_thin_workflow(
        &self,
        ir: &IntermediateRepresentation,
    ) -> EmitterResult<(String, String)> {
        let workflow_name = Self::build_workflow_name(ir);
        let filename = format!("{}.yml", sanitize_filename(&workflow_name));
        let yaml = self.emit_thin(ir)?;
        Ok((filename, yaml))
    }

    /// Emit a thin workflow whose main step runs `command` instead of
    /// `cuenv ci --pipeline <name>`, returning the filename and YAML content.
    ///
    /// The bootstrap, setup, success and failure phase steps are the same as
    /// in [`Self::emit_thin_workflow`].
    ///
    /// # Errors
    ///
    /// Returns `EmitterError::Serialization` if YAML serialization fails.
    pub fn emit_command_workflow(
        &self,
        ir: &IntermediateRepresentation,
        command: &CommandWorkflow<'_>,
    ) -> EmitterResult<(String, String)> {
        let workflow_name = Self::build_workflow_name(ir);
        let filename = format!("{}.yml", sanitize_filename(&workflow_name));
        let yaml = self.emit_thin_with(ir, command, TaskExecution::Direct)?;
        Ok((filename, yaml))
    }

    fn emit_thin_with(
        &self,
        ir: &IntermediateRepresentation,
        command: &CommandWorkflow<'_>,
        cache_execution: TaskExecution,
    ) -> EmitterResult<String> {
        use crate::workflow::stage_renderer::GitHubStageRenderer;

        if ir.pipeline.changes.is_some() {
            return Err(EmitterError::InvalidIR(format!(
                "pipeline '{}' sets `changes`, which needs separate jobs; thin mode runs one job and `cuenv ci` already skips unaffected tasks",
                ir.pipeline.name
            )));
        }

        let mut scoped_ir = ir.clone();
        scoped_ir.tasks.retain(|step| {
            step.phase.is_none()
                || step.github_task_filter.is_empty()
                || ir
                    .tasks
                    .iter()
                    .any(|task| task.phase.is_none() && step.github_task_filter.contains(&task.id))
        });
        let ir = &scoped_ir;
        self.validate_workflow_permissions(ir)?;
        if self.workflow_permissions_mode.is_explicit_empty()
            && let Some((scope, level)) = command
                .additional_permissions
                .iter()
                .find(|(_, level)| *level != "none")
        {
            return Err(EmitterError::InvalidIR(format!(
                "pipeline '{}' declares explicit empty `permissions: {{}}`, but workflow requires '{scope}: {level}'",
                ir.pipeline.name
            )));
        }
        let workflow_name = Self::build_workflow_name(ir);
        let workflow_filename = format!("{}.yml", sanitize_filename(&workflow_name));
        let triggers = self.build_triggers(ir, &workflow_filename);
        let job_permissions = if self.workflow_permissions_mode.is_explicit_empty() {
            None
        } else {
            let mut permissions = match command.token_scope {
                TokenScope::Workflow => self.build_permissions(ir),
                TokenScope::Tasks => self.permissions_for_tasks(ir.tasks.iter()),
            };
            for (key, value) in command.additional_permissions {
                if let (Some(slot), Some(level)) = (
                    Self::permission_slot(&mut permissions, key),
                    Self::permission_level(value),
                ) {
                    *slot = Some(slot.unwrap_or(PermissionLevel::None).max(level));
                }
            }
            Some(self.apply_configured_permissions(permissions))
        };

        let mut renderer = GitHubStageRenderer::new()
            .with_cachix_auth_token_secret(self.cachix_auth_token_secret.clone());
        if let Some(name) = &self.cachix_name {
            renderer = renderer.with_cachix(name.clone());
        }
        let mut steps = Vec::new();

        // Checkout step
        steps.push(self.build_checkout_step(None, Some(2)));

        // Bootstrap and setup phase steps
        let (phase_steps, secret_env) =
            self.render_phase_steps(ir, TaskExecution::Orchestrated, CuenvSetup::BuildInJob);
        steps.extend(phase_steps);
        steps.extend(command.leading_steps.iter().cloned());

        let mut main_step = Step::run(command.command).with_name(command.step_name);
        if let Some(id) = command.step_id {
            main_step = main_step.with_id(id);
        }
        Self::add_github_context_env(&mut main_step);

        if let Some(env) = &ir.pipeline.environment {
            main_step = main_step.with_env("CUENV_ENVIRONMENT", env.clone());
        }

        // Pass secret env vars from setup tasks to main step (e.g., OP_SERVICE_ACCOUNT_TOKEN)
        // Transform ${VAR} to ${{ secrets.VAR }} format for GitHub Actions
        for (key, value) in secret_env {
            main_step = main_step.with_env(
                key,
                crate::workflow::stage_renderer::transform_secret_ref(&value),
            );
        }

        // Apply remote cache trust boundaries (separate RO/RW credentials,
        // GitHub Environment gating for writes, isolated runner cache dirs).
        let job_environment = command
            .job_environment
            .or(ir.pipeline.environment.as_deref());
        let cache_write_environment = match cache_execution {
            TaskExecution::Orchestrated => {
                self.apply_remote_cache_step_env(&mut main_step, ir, job_environment)
            }
            TaskExecution::Direct => {
                self.disable_remote_cache_step_env(&mut main_step);
                None
            }
        };

        steps.push(main_step);
        steps.extend(command.follow_up_steps.iter().cloned());

        // Success phase steps
        for task in ir.sorted_phase_tasks(BuildStage::Success) {
            let mut step = renderer.render_task(task);
            step.if_condition = Some("success()".to_string());
            steps.push(step);
        }

        // Failure phase steps
        for task in ir.sorted_phase_tasks(BuildStage::Failure) {
            let mut step = renderer.render_task(task);
            step.if_condition = Some("failure()".to_string());
            steps.push(step);
        }

        let environment = job_environment
            .map(str::to_string)
            .or(cache_write_environment);

        // Build single job
        let job = Job {
            permissions: job_permissions,
            name: Some(workflow_name.clone()),
            runs_on: self.runner_as_runs_on(),
            needs: Vec::new(),
            if_condition: command.job_if.map(str::to_string),
            outputs: IndexMap::new(),
            strategy: None,
            environment: environment.map(Environment::Name),
            container: None,
            defaults: None,
            env: IndexMap::new(),
            concurrency: None,
            continue_on_error: None,
            timeout_minutes: self.timeout_minutes,
            steps,
        };

        let mut jobs = IndexMap::new();
        let main_job = sanitize_job_id(&workflow_name);
        jobs.insert(main_job.clone(), job);
        if let Some(follow_up) = &command.follow_up_job {
            jobs.insert(
                format!("{main_job}-{}", follow_up.id_suffix),
                Job {
                    permissions: Some(follow_up.permissions.clone()),
                    name: Some(follow_up.name.to_string()),
                    runs_on: self.runner_as_runs_on(),
                    needs: vec![main_job.clone()],
                    if_condition: Some(follow_up.if_condition.to_string()),
                    outputs: IndexMap::new(),
                    strategy: None,
                    environment: None,
                    container: None,
                    defaults: None,
                    env: IndexMap::new(),
                    concurrency: None,
                    continue_on_error: None,
                    timeout_minutes: None,
                    steps: follow_up.steps.to_vec(),
                },
            );
        }

        let concurrency = if command.concurrency_group.is_some() || !command.cancel_in_progress {
            Concurrency {
                group: command.concurrency_group.map_or_else(
                    || Self::DEFAULT_CONCURRENCY_GROUP.to_string(),
                    str::to_string,
                ),
                cancel_in_progress: Some(CancelInProgress::Bool(command.cancel_in_progress)),
            }
        } else {
            Self::build_workflow_concurrency(ir)
        };

        let mut workflow = Workflow {
            name: workflow_name,
            run_name: self.run_name.clone(),
            on: triggers,
            concurrency: Some(concurrency),
            permissions: self.workflow_permissions(),
            env: IndexMap::new(),
            jobs,
        };
        self.apply_pipeline_gate(ir, &mut workflow)?;

        self.serialize_workflow_with_pins(&workflow)
    }

    /// Emit multiple workflow files for projects with multiple pipelines.
    ///
    /// Returns a map of filename to YAML content.
    /// Each pipeline in the IR generates a separate workflow file.
    ///
    /// # Errors
    ///
    /// Returns `EmitterError::Serialization` if YAML serialization fails.
    pub fn emit_workflows(
        &self,
        ir: &IntermediateRepresentation,
    ) -> EmitterResult<HashMap<String, String>> {
        let mut workflows = HashMap::new();

        // Build workflow name with optional project prefix for monorepo support
        let workflow_name = Self::build_workflow_name(ir);

        // Generate a single workflow with all tasks as jobs
        let workflow = self.build_workflow(ir, &workflow_name)?;
        let filename = format!("{}.yml", sanitize_filename(&workflow_name));
        let yaml = self.serialize_workflow_with_pins(&workflow)?;
        workflows.insert(filename, yaml);

        Ok(workflows)
    }

    /// Build the workflow name, prefixing with project name if available (for monorepo support)
    pub(crate) fn build_workflow_name(ir: &IntermediateRepresentation) -> String {
        ir.pipeline.project_name.as_ref().map_or_else(
            || ir.pipeline.name.clone(),
            |project| format!("{}-{}", project, ir.pipeline.name),
        )
    }

    /// Build a workflow from the IR
    fn build_workflow(
        &self,
        ir: &IntermediateRepresentation,
        workflow_name: &str,
    ) -> EmitterResult<Workflow> {
        self.validate_workflow_permissions(ir)?;
        Self::validate_custom_steps(ir)?;
        let workflow_filename = format!("{}.yml", sanitize_filename(workflow_name));
        let triggers = self.build_triggers(ir, &workflow_filename);
        let jobs = self.build_jobs(ir);

        let mut workflow = Workflow {
            name: workflow_name.to_string(),
            run_name: self.run_name.clone(),
            on: triggers,
            concurrency: Some(Self::build_workflow_concurrency(ir)),
            permissions: self.workflow_permissions(),
            env: IndexMap::new(),
            jobs,
        };
        self.apply_pipeline_changes(ir, &mut workflow)?;
        self.apply_pipeline_gate(ir, &mut workflow)?;

        Ok(workflow)
    }

    /// Inject a per-lane change detection job (`#Pipeline.changes`) and wire
    /// downstream lane jobs to `needs: [<changes_job>]` and `if: needs.<changes_job>.outputs.<lane> == 'true'`
    /// unless the task overrides `if`.
    ///
    /// # Errors
    ///
    /// Returns [`EmitterError::InvalidIR`] if a lane name is not a valid output
    /// name, a lane lists no task or a task no job of the workflow has, or the
    /// detection job's ID is already a job of the workflow.
    pub fn apply_pipeline_changes(
        &self,
        ir: &IntermediateRepresentation,
        workflow: &mut Workflow,
    ) -> EmitterResult<()> {
        let Some(changes) = &ir.pipeline.changes else {
            return Ok(());
        };
        if changes.lanes.is_empty() {
            return Ok(());
        }

        let job_name = changes.job.as_deref().unwrap_or("changes");
        let changes_job_id = sanitize_job_id(job_name);
        if workflow.jobs.contains_key(&changes_job_id) {
            return Err(EmitterError::InvalidIR(format!(
                "pipeline '{}' change detection job '{changes_job_id}' has the ID of another job; set `changes.job` to a different name",
                ir.pipeline.name
            )));
        }
        for (lane_name, lane) in &changes.lanes {
            if lane_name.is_empty()
                || !lane_name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(EmitterError::InvalidIR(format!(
                    "pipeline '{}' change lane '{lane_name}' must be non-empty and use only letters, digits, '-' and '_'",
                    ir.pipeline.name
                )));
            }
            if lane.tasks.is_empty() {
                return Err(EmitterError::InvalidIR(format!(
                    "pipeline '{}' change lane '{lane_name}' lists no tasks",
                    ir.pipeline.name
                )));
            }
        }

        let mut outputs = IndexMap::new();
        for lane_name in changes.lanes.keys() {
            outputs.insert(
                lane_name.clone(),
                format!("${{{{ steps.detect.outputs.{lane_name} }}}}"),
            );
        }

        let mut setup_steps = Vec::new();
        let mut detect_step = if let Some(script) = &changes.script {
            Step::run(script).with_name("Detect changed lanes")
        } else {
            // The default detection runs `cuenv`, so the job sets it up as a
            // pipeline job does.
            setup_steps = self
                .render_phase_steps(ir, TaskExecution::Orchestrated, CuenvSetup::BuildInJob)
                .0;
            let mut step = Step::run(Self::default_change_detection_script(ir, changes))
                .with_name("Detect changed lanes");
            step.shell = Some("sh".to_string());
            step
        };
        detect_step.id = Some("detect".to_string());

        let runs_on = match &changes.runner {
            Some(cuenv_core::ci::StringOrVec::String(label)) => RunsOn::Label(label.clone()),
            Some(cuenv_core::ci::StringOrVec::Vec(labels)) => RunsOn::Labels(labels.clone()),
            None => self.runner_as_runs_on(),
        };

        let permissions = Some(self.apply_configured_permissions(Permissions {
            contents: Some(PermissionLevel::Read),
            ..Default::default()
        }));

        let changes_job = Job {
            permissions,
            name: Some(job_name.to_string()),
            runs_on,
            needs: Vec::new(),
            if_condition: None,
            outputs,
            strategy: None,
            environment: None,
            container: None,
            defaults: None,
            env: IndexMap::new(),
            concurrency: None,
            continue_on_error: None,
            timeout_minutes: Some(10),
            steps: std::iter::once(self.build_checkout_step(None, Some(0)))
                .chain(setup_steps)
                .chain(std::iter::once(detect_step))
                .collect(),
        };

        // A job named `<task>-<suffix>` is a matrix variant of `<task>` only when
        // no other task owns that job ID (`check` must not capture `check-web`).
        let task_job_ids: std::collections::HashSet<String> = ir
            .regular_tasks()
            .map(|task| sanitize_job_id(&task.id))
            .collect();

        for (lane_name, lane) in &changes.lanes {
            for task_id in &lane.tasks {
                let sanitized = sanitize_job_id(task_id);
                let variant_prefix = format!("{sanitized}-");
                let mut matched = false;
                for (job_id, job) in &mut workflow.jobs {
                    if job_id == &sanitized
                        || (job_id.starts_with(&variant_prefix) && !task_job_ids.contains(job_id))
                    {
                        matched = true;
                        if !job.needs.contains(&changes_job_id) {
                            job.needs.push(changes_job_id.clone());
                        }
                        if job.if_condition.is_none() {
                            job.if_condition = Some(format!(
                                "needs.{changes_job_id}.outputs.{lane_name} == 'true'"
                            ));
                        }
                    }
                }
                if !matched {
                    return Err(EmitterError::InvalidIR(format!(
                        "pipeline '{}' change lane '{lane_name}' lists task '{task_id}', which has no job in this workflow",
                        ir.pipeline.name
                    )));
                }
            }
        }

        let existing = std::mem::take(&mut workflow.jobs);
        workflow.jobs.insert(changes_job_id, changes_job);
        for (id, job) in existing {
            workflow.jobs.insert(id, job);
        }
        Ok(())
    }

    /// The default detection step: ask `cuenv ci` which tasks a dry run of the
    /// pipeline selects, and set each lane's output when one of its tasks is
    /// among them. The provider supplies the comparison base for the event.
    ///
    /// Lane tasks match whole task names. When `cuenv` cannot plan the
    /// pipeline the step warns and runs every lane, since nothing is known
    /// to be unaffected.
    fn default_change_detection_script(
        ir: &IntermediateRepresentation,
        changes: &cuenv_core::ci::PipelineChanges,
    ) -> String {
        use std::fmt::Write as _;

        let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
        let mut command = format!("cuenv ci --pipeline {}", quote(&ir.pipeline.name));
        if let Some(path) = &ir.pipeline.project_path {
            let _ = write!(command, " --path {}", quote(path));
        }
        command.push_str(" --dry-run --plan-format lines --output \"$plan\"");

        let mut script = format!(
            "set -eu\n\
             plan=\"${{RUNNER_TEMP:-/tmp}}/cuenv-plan.txt\"\n\
             lane_hit() {{\n\
               for task in \"$@\"; do\n\
                 if grep -Fxq -- \"$task\" \"$plan\"; then\n\
                   echo true\n\
                   return 0\n\
                 fi\n\
               done\n\
               echo false\n\
             }}\n\
             if {command}; then\n"
        );
        for (lane_name, lane) in &changes.lanes {
            let tasks = lane
                .tasks
                .iter()
                .map(|task| quote(task))
                .collect::<Vec<_>>()
                .join(" ");
            let _ = writeln!(
                script,
                "  echo \"{lane_name}=$(lane_hit {tasks})\" >> \"$GITHUB_OUTPUT\""
            );
        }
        script.push_str(
            "else\n  echo \"::warning::cuenv could not plan the pipeline; running every lane\"\n",
        );
        for lane_name in changes.lanes.keys() {
            let _ = writeln!(script, "  echo \"{lane_name}=true\" >> \"$GITHUB_OUTPUT\"");
        }
        script.push_str("fi");
        script
    }

    /// Inject a configurable gate job (`#Pipeline.gate`) that aggregates every
    /// other job in the workflow into a single stable required check.
    ///
    /// # Errors
    ///
    /// Returns [`EmitterError::InvalidIR`] if the gate's ID is already a job of
    /// the workflow or `required` names a job the workflow does not have.
    pub fn apply_pipeline_gate(
        &self,
        ir: &IntermediateRepresentation,
        workflow: &mut Workflow,
    ) -> EmitterResult<()> {
        use std::fmt::Write as _;

        let Some(gate) = &ir.pipeline.gate else {
            return Ok(());
        };

        let gate_job_id = sanitize_job_id(&gate.name);
        if gate_job_id.is_empty() || workflow.jobs.contains_key(&gate_job_id) {
            return Err(EmitterError::InvalidIR(format!(
                "pipeline '{}' gate '{}' must have a name that is not the ID of another job",
                ir.pipeline.name, gate.name
            )));
        }
        if let Some(unknown) = gate
            .required
            .iter()
            .find(|id| !workflow.jobs.contains_key(&sanitize_job_id(id)))
        {
            return Err(EmitterError::InvalidIR(format!(
                "pipeline '{}' gate '{}' requires '{unknown}', which is not a job of this workflow (jobs: {})",
                ir.pipeline.name,
                gate.name,
                workflow.jobs.keys().cloned().collect::<Vec<_>>().join(", ")
            )));
        }
        let needs: Vec<String> = workflow
            .jobs
            .keys()
            .filter(|id| *id != &gate_job_id)
            .cloned()
            .collect();
        let required: std::collections::HashSet<String> =
            gate.required.iter().map(|id| sanitize_job_id(id)).collect();

        let mut script = String::from(
            "status=0\n\
             check_required() {\n\
               case \"$2\" in\n\
                 success) ;;\n\
                 *) echo \"FAIL: required job '$1' finished with '$2'\" >&2; status=1 ;;\n\
               esac\n\
             }\n\
             check_optional() {\n\
               case \"$2\" in\n\
                 success|skipped) ;;\n\
                 *) echo \"FAIL: job '$1' finished with '$2'\" >&2; status=1 ;;\n\
               esac\n\
             }\n",
        );

        let mut env = IndexMap::new();
        for (idx, need_id) in needs.iter().enumerate() {
            let var_name = format!("NEED_RESULT_{idx}");
            env.insert(
                var_name.clone(),
                format!("${{{{ needs.{need_id}.result }}}}"),
            );
            let helper = if required.contains(need_id) {
                "check_required"
            } else {
                "check_optional"
            };
            let escaped = need_id.replace('\'', "'\\''");
            let _ = writeln!(script, "{helper} '{escaped}' \"${var_name}\"");
        }
        script.push_str("exit \"$status\"");

        let mut gate_step = Step::run(&script).with_name(format!("Check {}", gate.name));
        gate_step.shell = Some("sh".to_string());
        gate_step.env = env;

        let runs_on = match &gate.runner {
            Some(cuenv_core::ci::StringOrVec::String(label)) => RunsOn::Label(label.clone()),
            Some(cuenv_core::ci::StringOrVec::Vec(labels)) => RunsOn::Labels(labels.clone()),
            None => self.runner_as_runs_on(),
        };

        let gate_job = Job {
            permissions: Some(Permissions::default()),
            name: Some(gate.name.clone()),
            runs_on,
            needs,
            if_condition: Some("always()".to_string()),
            outputs: IndexMap::new(),
            strategy: None,
            environment: None,
            container: None,
            defaults: None,
            env: IndexMap::new(),
            concurrency: None,
            continue_on_error: None,
            timeout_minutes: Some(gate.timeout_minutes.unwrap_or(5)),
            steps: vec![gate_step],
        };

        workflow.jobs.insert(gate_job_id, gate_job);
        Ok(())
    }

    /// Build workflow triggers from IR
    #[must_use]
    pub fn build_triggers(
        &self,
        ir: &IntermediateRepresentation,
        workflow_filename: &str,
    ) -> WorkflowTriggers {
        let trigger = ir.pipeline.trigger.as_ref();

        WorkflowTriggers {
            push: Self::build_push_trigger(trigger, workflow_filename),
            pull_request: Self::build_pr_trigger(trigger, workflow_filename),
            merge_group: Self::build_merge_group_trigger(trigger),
            workflow_run: Self::build_workflow_run_trigger(trigger),
            release: Self::build_release_trigger(trigger),
            workflow_dispatch: Self::build_manual_trigger(trigger),
            schedule: Self::build_schedule_trigger(trigger),
        }
    }

    /// Build push trigger from IR trigger condition
    fn build_push_trigger(
        trigger: Option<&TriggerCondition>,
        workflow_filename: &str,
    ) -> Option<PushTrigger> {
        let trigger = trigger?;

        // Only emit push trigger if we have branch or tag conditions
        if trigger.branches.is_empty() && trigger.tags.is_empty() {
            return None;
        }

        // GitHub never evaluates path filters for tag pushes.
        let paths = Self::build_trigger_paths(&trigger.paths, workflow_filename);

        Some(PushTrigger {
            branches: trigger.branches.clone(),
            tags: trigger.tags.clone(),
            paths,
        })
    }

    /// Build pull request trigger from IR trigger condition
    fn build_pr_trigger(
        trigger: Option<&TriggerCondition>,
        workflow_filename: &str,
    ) -> Option<PullRequestTrigger> {
        let trigger = trigger?;

        // Only emit PR trigger if explicitly enabled - never default to running on PRs
        if trigger.pull_request != Some(true) {
            return None;
        }

        if let Some(cfg) = trigger.pull_request_config.as_ref() {
            let branches = cfg.branches.clone().unwrap_or_default();
            let (paths, paths_ignore) = if let Some(explicit_paths) = cfg.paths.as_ref() {
                (explicit_paths.clone(), cfg.paths_ignore.clone())
            } else if !cfg.paths_ignore.is_empty() {
                // GitHub forbids combining `paths` and `paths-ignore` on the same trigger.
                (Vec::new(), cfg.paths_ignore.clone())
            } else {
                (
                    Self::build_trigger_paths(&trigger.paths, workflow_filename),
                    Vec::new(),
                )
            };

            Some(PullRequestTrigger {
                branches,
                types: cfg.types.clone(),
                paths,
                paths_ignore,
            })
        } else {
            let paths = Self::build_trigger_paths(&trigger.paths, workflow_filename);

            Some(PullRequestTrigger {
                branches: trigger.branches.clone(),
                paths,
                ..Default::default()
            })
        }
    }

    /// Build merge queue (`merge_group`) trigger from IR trigger condition.
    ///
    /// Derived `paths` filters are never applied because GitHub does not
    /// support path filters on `merge_group`.
    fn build_merge_group_trigger(trigger: Option<&TriggerCondition>) -> Option<MergeGroupTrigger> {
        let mg = trigger?.merge_group.as_ref()?;
        Some(MergeGroupTrigger {
            types: mg.types.clone(),
        })
    }

    /// Build upstream workflow (`workflow_run`) trigger from IR trigger condition.
    ///
    /// Derived `paths` filters are never applied because GitHub does not
    /// support path filters on `workflow_run`.
    fn build_workflow_run_trigger(
        trigger: Option<&TriggerCondition>,
    ) -> Option<WorkflowRunTrigger> {
        let wr = trigger?.workflow_run.as_ref()?;
        if wr.workflows.is_empty() {
            return None;
        }
        Some(WorkflowRunTrigger {
            workflows: wr.workflows.clone(),
            types: wr.types.clone(),
            branches: wr.branches.clone(),
        })
    }

    /// Build release trigger from IR trigger condition
    fn build_release_trigger(trigger: Option<&TriggerCondition>) -> Option<ReleaseTrigger> {
        let trigger = trigger?;

        if trigger.release.is_empty() {
            return None;
        }

        Some(ReleaseTrigger {
            types: trigger.release.clone(),
        })
    }

    /// Build schedule trigger from IR trigger condition
    fn build_schedule_trigger(trigger: Option<&TriggerCondition>) -> Option<Vec<ScheduleTrigger>> {
        let trigger = trigger?;

        if trigger.scheduled.is_empty() {
            return None;
        }

        Some(
            trigger
                .scheduled
                .iter()
                .map(|cron| ScheduleTrigger { cron: cron.clone() })
                .collect(),
        )
    }

    /// Build trigger paths, adding the workflow file itself when path filtering is active.
    ///
    /// When a workflow has path-based triggers (e.g., only trigger on changes to `src/**`),
    /// this ensures the workflow also triggers when its own definition file changes.
    fn build_trigger_paths(paths: &[String], workflow_filename: &str) -> Vec<String> {
        if paths.is_empty() {
            return Vec::new();
        }

        let workflow_path = format!(".github/workflows/{workflow_filename}");

        if paths.contains(&workflow_path) {
            return paths.to_vec();
        }

        let mut result = paths.to_vec();
        result.push(workflow_path);
        result.sort();
        result
    }

    /// Build manual (`workflow_dispatch`) trigger from IR trigger condition
    fn build_manual_trigger(trigger: Option<&TriggerCondition>) -> Option<WorkflowDispatchTrigger> {
        let trigger = trigger?;
        let manual = trigger.manual.as_ref()?;

        if !manual.enabled && manual.inputs.is_empty() {
            return None;
        }

        Some(WorkflowDispatchTrigger {
            inputs: manual
                .inputs
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        WorkflowInput {
                            description: v.description.clone(),
                            required: Some(v.required),
                            default: v.default.clone(),
                            input_type: v.input_type.clone(),
                            options: if v.options.is_empty() {
                                None
                            } else {
                                Some(v.options.clone())
                            },
                        },
                    )
                })
                .collect(),
        })
    }

    /// Thin-job requirements, including the cuenv CI reporter
    #[must_use]
    pub fn build_permissions(&self, ir: &IntermediateRepresentation) -> Permissions {
        let mut permissions = Self::required_permissions(ir.tasks.iter());
        // `cuenv ci` reports a check run; `cuenv task` in expanded jobs does not.
        permissions.checks = Some(PermissionLevel::Write);
        if ir
            .pipeline
            .trigger
            .as_ref()
            .is_some_and(|trigger| trigger.pull_request == Some(true))
        {
            permissions.pull_requests = Some(PermissionLevel::Write);
        }
        self.apply_configured_permissions(permissions)
    }

    /// Calculate permissions for the task and the phase steps actually emitted.
    #[must_use]
    pub fn build_job_permissions(
        &self,
        task: &cuenv_ci::ir::Task,
        phases: &[&cuenv_ci::ir::Task],
    ) -> Permissions {
        if task.github_permissions_mode.is_explicit_empty() {
            return Permissions::default();
        }
        self.permissions_for_tasks(std::iter::once(task).chain(phases.iter().copied()))
    }

    /// Calculate the `jobs.<id>.permissions` field, honoring explicit empty blocks.
    #[must_use]
    pub(super) fn job_permissions_option(
        &self,
        task: &cuenv_ci::ir::Task,
        phases: &[&cuenv_ci::ir::Task],
    ) -> Option<Permissions> {
        if task.github_permissions_mode.is_explicit_empty() {
            Some(Permissions::default())
        } else if self.workflow_permissions_mode.is_explicit_empty() {
            None
        } else {
            Some(self.build_job_permissions(task, phases))
        }
    }

    pub(super) fn permissions_for_tasks<'a>(
        &self,
        tasks: impl Iterator<Item = &'a cuenv_ci::ir::Task>,
    ) -> Permissions {
        self.apply_configured_permissions(Self::required_permissions(tasks))
    }

    pub(super) fn required_permissions<'a>(
        tasks: impl Iterator<Item = &'a cuenv_ci::ir::Task>,
    ) -> Permissions {
        let mut permissions = Permissions {
            contents: Some(PermissionLevel::Read),
            ..Default::default()
        };
        for task in tasks {
            if task.deployment {
                permissions.contents = Some(PermissionLevel::Write);
            }
            for (key, value) in &task.github_permissions {
                if let (Some(slot), Some(level)) = (
                    Self::permission_slot(&mut permissions, key),
                    Self::permission_level(value),
                ) {
                    *slot = Some(slot.unwrap_or(PermissionLevel::None).max(level));
                }
            }
        }
        permissions
    }

    /// Build jobs from IR tasks
    ///
    /// Uses phase tasks for bootstrap and setup steps, respecting `depends_on`
    /// relationships for correct step ordering.
    fn build_jobs(&self, ir: &IntermediateRepresentation) -> IndexMap<String, Job> {
        let mut jobs = IndexMap::new();

        for task in ir.regular_tasks() {
            // Build base job using phase tasks for bootstrap/setup
            let mut job = self.build_simple_job(
                task,
                ir,
                &if task.github_execution == Some(cuenv_core::ci::TaskGitHubExecution::Direct) {
                    SimpleJobOptions::direct(None)
                } else {
                    SimpleJobOptions::orchestrated(ir.pipeline.environment.as_deref(), None)
                },
            );

            // Apply additional job configuration from task

            // Determine runner from task resources or use default
            if let Some(resources) = &task.resources
                && let Some(tag) = resources.tags.first()
            {
                job.runs_on = RunsOn::Label(tag.clone());
            }

            // Map task dependencies to sanitized job IDs
            job.needs = task.depends_on.iter().map(|d| sanitize_job_id(d)).collect();

            // Handle concurrency group (fallback when not already set via task.github_job)
            if job.concurrency.is_none()
                && let Some(group) = &task.concurrency_group
            {
                job.concurrency = Some(Concurrency {
                    group: group.clone(),
                    cancel_in_progress: Some(CancelInProgress::Bool(false)),
                });
            }

            jobs.insert(sanitize_job_id(&task.id), job);
        }

        jobs
    }

    /// Build the checkout step for a job, merging provider/pipeline checkout config
    /// with any per-task checkout override.
    #[must_use]
    pub(super) fn build_checkout_step(
        &self,
        task: Option<&cuenv_ci::ir::Task>,
        default_fetch_depth: Option<u32>,
    ) -> Step {
        let task_checkout = task
            .and_then(|t| t.github_job.as_ref())
            .and_then(|j| j.checkout.as_ref());
        let merged = match (&self.checkout, task_checkout) {
            (Some(base), Some(over)) => Some(base.merge(over)),
            (None, Some(over)) => Some(over.clone()),
            (Some(base), None) => Some(base.clone()),
            (None, None) => None,
        };

        let Some(cfg) = merged else {
            let mut step = Step::uses("actions/checkout@v7").with_name("Checkout");
            if let Some(depth) = default_fetch_depth {
                step = step.with_input("fetch-depth", serde_yaml::Value::Number(depth.into()));
            }
            return step;
        };

        let uses = match cfg.uses.as_deref() {
            Some(action) if action.contains('@') => action.to_string(),
            Some("actions/checkout") | None => "actions/checkout@v7".to_string(),
            Some(action) => action.to_string(),
        };

        let mut step = Step::uses(uses).with_name("Checkout");
        if let Some(persist) = cfg.persist_credentials {
            step = step.with_input("persist-credentials", serde_yaml::Value::Bool(persist));
        }
        if let Some(depth) = cfg.fetch_depth.or(default_fetch_depth) {
            step = step.with_input("fetch-depth", serde_yaml::Value::Number(depth.into()));
        }
        if let Some(git_ref) = &cfg.git_ref {
            step = step.with_input("ref", serde_yaml::Value::String(git_ref.clone()));
        }
        if let Some(submodules) = &cfg.submodules {
            let val = match submodules {
                cuenv_core::ci::CheckoutSubmodules::Bool(b) => serde_yaml::Value::Bool(*b),
                cuenv_core::ci::CheckoutSubmodules::Mode(s) => serde_yaml::Value::String(s.clone()),
            };
            step = step.with_input("submodules", val);
        }
        step
    }

    /// Convert a custom `TaskGitHubStep` into a workflow `Step`.
    pub(super) fn render_custom_step(custom: &cuenv_core::ci::TaskGitHubStep) -> Step {
        let mut step = Step {
            name: custom.name.clone(),
            id: custom.id.clone(),
            if_condition: custom.if_condition.clone(),
            uses: custom.uses.clone(),
            run: custom.run.clone(),
            working_directory: custom.working_directory.clone(),
            shell: custom.shell.clone(),
            ..Default::default()
        };
        for (k, v) in &custom.with_inputs {
            if let Ok(yaml_val) = serde_yaml::to_value(v) {
                step.with_inputs.insert(k.clone(), yaml_val);
            }
        }
        for (k, v) in &custom.env {
            step.env.insert(k.clone(), v.clone());
        }
        step
    }

    /// Validate that every custom step in `ir` sets exactly one of `uses` or `run`.
    ///
    /// # Errors
    ///
    /// Returns [`EmitterError::InvalidIR`] if a custom step sets neither or both.
    pub fn validate_custom_steps(ir: &IntermediateRepresentation) -> EmitterResult<()> {
        for task in &ir.tasks {
            let Some(steps) = task.github_job.as_ref().and_then(|j| j.steps.as_ref()) else {
                continue;
            };
            for (phase, list) in [("before", &steps.before), ("after", &steps.after)] {
                for (idx, step) in list.iter().enumerate() {
                    match (&step.uses, &step.run) {
                        (Some(_), Some(_)) => {
                            return Err(EmitterError::InvalidIR(format!(
                                "task '{}' custom step {phase}[{idx}] cannot set both `uses` and `run`",
                                task.id
                            )));
                        }
                        (None, None) => {
                            return Err(EmitterError::InvalidIR(format!(
                                "task '{}' custom step {phase}[{idx}] must set either `uses` or `run`",
                                task.id
                            )));
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    /// Validate that every `needs.<job>.outputs.<name>` reference in `workflow`
    /// points to a job in the workflow that declares `<name>` in its `outputs`.
    ///
    /// # Errors
    ///
    /// Returns [`EmitterError::InvalidIR`] if any reference names an unknown job
    /// or an undeclared output.
    pub fn validate_job_output_references(workflow: &Workflow) -> EmitterResult<()> {
        for (consumer_id, job) in &workflow.jobs {
            for text in Self::collect_job_expression_strings(job) {
                for (target_job, output_name) in Self::extract_needs_output_refs(text) {
                    let Some(upstream) = workflow.jobs.get(target_job) else {
                        return Err(EmitterError::InvalidIR(format!(
                            "job '{consumer_id}' references output '{output_name}' from unknown job '{target_job}'"
                        )));
                    };
                    if !upstream.outputs.contains_key(output_name) {
                        return Err(EmitterError::InvalidIR(format!(
                            "job '{consumer_id}' references undeclared output '{output_name}' from job '{target_job}'"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn collect_job_expression_strings(job: &Job) -> Vec<&str> {
        let mut texts = Vec::new();
        if let Some(cond) = job.if_condition.as_deref() {
            texts.push(cond);
        }
        match &job.runs_on {
            RunsOn::Label(label) => texts.push(label.as_str()),
            RunsOn::Labels(labels) => texts.extend(labels.iter().map(String::as_str)),
        }
        if let Some(strategy) = &job.strategy
            && let MatrixInclude::Expression(expr) = &strategy.matrix.include
        {
            texts.push(expr.as_str());
        }
        if let Some(env_cfg) = &job.environment {
            match env_cfg {
                Environment::Name(name) => texts.push(name.as_str()),
                Environment::WithUrl { name, url } => {
                    texts.push(name.as_str());
                    texts.push(url.as_str());
                }
            }
        }
        if let Some(concurrency) = &job.concurrency {
            texts.push(concurrency.group.as_str());
            if let Some(CancelInProgress::Expression(expr)) = &concurrency.cancel_in_progress {
                texts.push(expr.as_str());
            }
        }
        texts.extend(job.env.values().map(String::as_str));
        for step in &job.steps {
            if let Some(cond) = step.if_condition.as_deref() {
                texts.push(cond);
            }
            if let Some(run) = step.run.as_deref() {
                texts.push(run);
            }
            texts.extend(step.env.values().map(String::as_str));
            for val in step.with_inputs.values() {
                if let Some(s) = val.as_str() {
                    texts.push(s);
                }
            }
        }
        texts
    }

    fn extract_needs_output_refs(text: &str) -> Vec<(&str, &str)> {
        let mut refs = Vec::new();
        let mut rest = text;
        while let Some(idx) = rest.find("needs.") {
            let after_needs = &rest[idx + "needs.".len()..];
            let job_len = after_needs
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
                .count();
            if job_len > 0 {
                let job_id = &after_needs[..job_len];
                if let Some(after_outputs) = after_needs[job_len..].strip_prefix(".outputs.") {
                    let out_len = after_outputs
                        .bytes()
                        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
                        .count();
                    if out_len > 0 {
                        refs.push((job_id, &after_outputs[..out_len]));
                    }
                }
            }
            rest = after_needs;
        }
        refs
    }

    /// Serialize a workflow to YAML, applying configured action pins, enforcing
    /// `requirePinnedActions`, and preserving `# <tag>` trailing comments.
    ///
    /// # Errors
    ///
    /// Returns [`EmitterError::InvalidIR`] if `requirePinnedActions` is enabled
    /// and any non-local `uses:` step is not pinned to a 40-character hex SHA,
    /// if a job references an undeclared upstream job output,
    /// or [`EmitterError::Serialization`] if YAML serialization fails.
    pub fn serialize_workflow_with_pins(&self, workflow: &Workflow) -> EmitterResult<String> {
        Self::validate_job_output_references(workflow)?;
        let mut pinned = workflow.clone();
        let mut tag_comments: Vec<(String, String)> = Vec::new();

        for job in pinned.jobs.values_mut() {
            for step in &mut job.steps {
                let Some(uses) = step.uses.as_mut() else {
                    continue;
                };
                if uses.starts_with("./") || uses.starts_with("docker://") {
                    continue;
                }
                let action_name = uses.split('@').next().unwrap_or(uses.as_str()).to_string();
                if let Some(pin) = self.action_pins.get(&action_name) {
                    let rewritten = format!("{action_name}@{}", pin.git_ref);
                    if let Some(tag) = &pin.tag {
                        tag_comments.push((rewritten.clone(), tag.clone()));
                    }
                    *uses = rewritten;
                }
                if self.action_pin_policy == ActionPinPolicy::RequireSha {
                    let is_sha = uses.split_once('@').is_some_and(|(_, git_ref)| {
                        git_ref.len() == 40 && git_ref.bytes().all(|b| b.is_ascii_hexdigit())
                    });
                    if !is_sha {
                        return Err(EmitterError::InvalidIR(format!(
                            "action '{uses}' is not pinned to a full 40-character commit SHA (requirePinnedActions is enabled)"
                        )));
                    }
                }
            }
        }

        let mut yaml = serde_yaml::to_string(&pinned)
            .map_err(|e| EmitterError::Serialization(e.to_string()))?;

        if !tag_comments.is_empty() {
            let mut out = String::with_capacity(yaml.len() + tag_comments.len() * 8);
            for line in yaml.lines() {
                out.push_str(line);
                if let Some((_, rhs)) = line.split_once("uses: ") {
                    let trimmed = rhs.trim().trim_matches('\'').trim_matches('"');
                    if let Some((_, tag)) = tag_comments.iter().find(|(u, _)| u == trimmed) {
                        out.push_str(" # ");
                        out.push_str(tag);
                    }
                }
                out.push('\n');
            }
            yaml = out;
        }

        // Add generation header
        let header =
            "# Generated by cuenv - do not edit manually\n# Regenerate with: cuenv sync ci\n\n";

        Ok(format!("{header}{yaml}"))
    }
}

impl Emitter for GitHubActionsEmitter {
    /// Emit a thin mode GitHub Actions workflow.
    ///
    /// Thin mode generates a single-job workflow that:
    /// 1. Runs bootstrap phase steps (e.g., install Nix)
    /// 2. Runs setup phase steps (e.g., build cuenv)
    /// 3. Executes `cuenv ci --pipeline <name>` for orchestration
    /// 4. Runs success/failure phase steps with conditions
    fn emit_thin(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
        let pipeline_name = &ir.pipeline.name;
        let command = ir.pipeline.project_path.as_ref().map_or_else(
            || format!("cuenv ci --pipeline {pipeline_name}"),
            |path| format!("cuenv ci --pipeline {pipeline_name} --path {path}"),
        );
        let step_name = format!("Run pipeline: {pipeline_name}");
        self.emit_thin_with(
            ir,
            &CommandWorkflow {
                step_name: &step_name,
                step_id: None,
                command: &command,
                concurrency_group: None,
                cancel_in_progress: true,
                job_if: None,
                follow_up_steps: &[],
                job_environment: None,
                token_scope: TokenScope::Workflow,
                follow_up_job: None,
                leading_steps: &[],
                additional_permissions: &[],
            },
            TaskExecution::Orchestrated,
        )
    }

    /// Emit an expanded mode GitHub Actions workflow.
    ///
    /// Expanded mode generates a multi-job workflow where each task becomes
    /// a separate job with dependencies managed by GitHub Actions (`needs:`).
    fn emit_expanded(&self, ir: &IntermediateRepresentation) -> EmitterResult<String> {
        let workflow_name = Self::build_workflow_name(ir);
        let workflow = self.build_workflow(ir, &workflow_name)?;
        self.serialize_workflow_with_pins(&workflow)
    }

    fn format_name(&self) -> &'static str {
        "github"
    }

    fn file_extension(&self) -> &'static str {
        "yml"
    }

    fn description(&self) -> &'static str {
        "GitHub Actions workflow YAML emitter"
    }

    fn validate(&self, ir: &IntermediateRepresentation) -> EmitterResult<()> {
        // Validate task IDs are valid job identifiers
        for task in &ir.tasks {
            if task.id.contains(' ') {
                return Err(EmitterError::InvalidIR(format!(
                    "Task ID '{}' contains spaces, which are not allowed in GitHub Actions job IDs",
                    task.id
                )));
            }
        }

        // Validate dependencies exist
        let task_ids: std::collections::HashSet<_> = ir.tasks.iter().map(|t| &t.id).collect();
        for task in &ir.tasks {
            for dep in &task.depends_on {
                if !task_ids.contains(dep) {
                    return Err(EmitterError::InvalidIR(format!(
                        "Task '{}' depends on non-existent task '{}'",
                        task.id, dep
                    )));
                }
            }
        }

        Ok(())
    }
}

enum AmbientUploadPolicy {
    Disabled,
    Always,
    Dynamic(String),
}

fn ambient_upload_policy(
    remote: &cuenv_core::manifest::RemoteCache,
    trigger: Option<&TriggerCondition>,
) -> AmbientUploadPolicy {
    if !remote.upload {
        return AmbientUploadPolicy::Disabled;
    }
    let Some(trigger) = trigger else {
        return AmbientUploadPolicy::Disabled;
    };
    if trigger.pull_request == Some(true) {
        return AmbientUploadPolicy::Disabled;
    }
    let has_protected =
        !trigger.branches.is_empty() || !trigger.tags.is_empty() || !trigger.release.is_empty();
    if !has_protected {
        return AmbientUploadPolicy::Disabled;
    }

    let has_manual = trigger
        .manual
        .as_ref()
        .is_some_and(|manual| manual.enabled || !manual.inputs.is_empty());
    if trigger.scheduled.is_empty() && !has_manual {
        return AmbientUploadPolicy::Always;
    }

    protected_trigger_expression(trigger)
        .map_or(AmbientUploadPolicy::Disabled, AmbientUploadPolicy::Dynamic)
}

fn protected_trigger_expression(trigger: &TriggerCondition) -> Option<String> {
    let mut push_refs = Vec::new();
    for (patterns, prefix, ref_type) in [
        (&trigger.branches, "refs/heads/", "branch"),
        (&trigger.tags, "refs/tags/", "tag"),
    ] {
        match push_ref_clause(patterns, prefix, ref_type) {
            PushRefClause::Empty => {}
            PushRefClause::Invalid => return None,
            PushRefClause::Match(clause) => push_refs.push(clause),
        }
    }

    let mut clauses = Vec::new();
    if !push_refs.is_empty() {
        let joined = push_refs.join(" || ");
        let needs_parens = push_refs.len() > 1 || joined.contains(" || ");
        let push_expr = if needs_parens {
            format!("github.event_name == 'push' && ({joined})")
        } else {
            format!("github.event_name == 'push' && {joined}")
        };
        clauses.push(push_expr);
    }
    if !trigger.release.is_empty() {
        clauses.push("github.event_name == 'release'".to_string());
    }

    match clauses.as_slice() {
        [] => None,
        [single] => Some(single.clone()),
        many => Some(
            many.iter()
                .map(|clause| {
                    if clause.contains(" && ") {
                        format!("({clause})")
                    } else {
                        clause.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(" || "),
        ),
    }
}

enum PushRefClause {
    Empty,
    Invalid,
    Match(String),
}

fn push_ref_clause(patterns: &[String], ref_prefix: &str, ref_type: &str) -> PushRefClause {
    if patterns.is_empty() {
        return PushRefClause::Empty;
    }
    if patterns
        .iter()
        .any(|p| p.is_empty() || p.contains('\'') || p.chars().any(char::is_control))
    {
        return PushRefClause::Invalid;
    }
    let has_glob = patterns
        .iter()
        .any(|p| p.contains(['*', '?', '[', ']', '!', '+']));
    if has_glob {
        return PushRefClause::Match(format!("github.ref_type == '{ref_type}'"));
    }
    let checks: Vec<String> = patterns
        .iter()
        .map(|p| format!("github.ref == '{ref_prefix}{p}'"))
        .collect();
    PushRefClause::Match(checks.join(" || "))
}

/// Sanitize a string for use as a workflow filename
fn valid_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

fn valid_cache_credential_env(name: &str) -> bool {
    valid_identifier(name)
        && !matches!(
            name,
            "CUENV_REMOTE_CACHE"
                | "CUENV_REMOTE_CACHE_PROTOCOL"
                | "CUENV_REMOTE_CACHE_INSTANCE"
                | "CUENV_REMOTE_CACHE_UPLOAD"
                | "CUENV_CACHE_DIR"
        )
}

fn sanitize_filename(name: &str) -> String {
    name.to_lowercase()
        .replace(' ', "-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// Sanitize a string for use as a job ID
fn sanitize_job_id(id: &str) -> String {
    id.replace(['.', ' '], "-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

#[cfg(test)]
#[path = "emitter_tests.rs"]
mod tests;
