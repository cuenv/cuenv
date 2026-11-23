//! Task executor for running tasks with environment support.
//!
//! - Environment variable propagation
//! - Parallel and sequential execution
//! - Host process execution, with directory isolation and result caching

use super::cache::{BuildActionInput, RecordInput, TaskCacheConfig};
pub use super::command::{CommandExecution, execute_command, execute_command_with_redaction};
use super::process::{ProcessOptions, TaskAttempt};
pub use super::result::{TASK_FAILURE_SNIPPET_LINES, TaskResult, summarize_task_failure};
#[cfg(test)]
use super::result::{format_failure_streams, summarize_stream};

use super::{
    ExternalTaskName, Input, Task, TaskDirectory, TaskDirectoryBase, TaskGraph, TaskGroup,
    TaskNode, Tasks,
};
use async_recursion::async_recursion;
use cuenv_core::environment::Environment;
use cuenv_core::{Error, OutputCapture, Result};
#[cfg(test)]
use cuenv_workspaces::PackageManager;
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::process::Command;
use tokio::task::JoinSet;
use tracing::instrument;

#[derive(Clone, Copy)]
struct GroupCompletion<'completion> {
    prefix: &'completion str,
    started: std::time::Instant,
    outcome: &'completion Result<Vec<TaskResult>>,
    total_children: usize,
}

/// Emit a `task.group_completed` event with succeeded/failed/skipped counts
/// derived from the inner result.
///
/// On `Err` the group is reported as failed with all children counted as
/// failed (we lack per-child results once a sequence/parallel aborts).
fn emit_group_completion(completion: GroupCompletion<'_>) {
    let GroupCompletion {
        prefix,
        started,
        outcome,
        total_children,
    } = completion;
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (success, succeeded, failed, skipped) = match outcome {
        Ok(results) => {
            let succeeded = results.iter().filter(|r| r.success).count();
            let failed = results.iter().filter(|r| !r.success).count();
            let skipped = total_children.saturating_sub(succeeded + failed);
            (failed == 0, succeeded, failed, skipped)
        }
        Err(_) => (false, 0_usize, total_children, 0_usize),
    };
    cuenv_events::emit_task_group_completed!(
        prefix,
        success,
        duration_ms,
        succeeded,
        failed,
        skipped
    );
}

/// The pending answer of [`TaskEnvironments::environment_for`].
pub type TaskEnvironmentFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Environment>>> + Send + 'a>>;

/// Supplies the environment of the tasks whose variables differ from
/// [`ExecutorConfig::environment`], such as a task a named context selects
/// variables for.
pub trait TaskEnvironments: std::fmt::Debug + Send + Sync {
    /// The environment for one task, or `None` when it runs with the shared
    /// one.
    fn environment_for<'a>(&'a self, name: &'a str, task: &'a Task) -> TaskEnvironmentFuture<'a>;
}

/// Remote cache authorization policy for a dependency task belonging to another
/// project (`#project:task`).
///
/// External projects default to [`ExternalCachePolicy::LocalOnly`] so running
/// a dependency from another project never silently inherits the invoking
/// project's remote cache write authority.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExternalCachePolicy {
    /// Only local CAS/ActionCache reads and writes are permitted.
    #[default]
    LocalOnly,
    /// Read-only remote fallback is permitted, but remote publication is denied.
    ReadOnlyRemote,
    /// Explicitly authorized to publish to the configured remote cache.
    AllowPublish,
}

/// What a task of another project runs with in place of the run's own
/// project: that project's directory, CUE module and cache identity.
///
/// A task named `#project:task` is a dependency from another project. It
/// reads its inputs, writes its outputs and keys its results as it would in
/// a run of its own project. Its environment comes from
/// [`ExecutorConfig::task_environments`].
#[derive(Debug, Clone)]
pub struct ProjectExecution {
    /// The project's directory, which its inputs and outputs are relative to.
    pub project_root: PathBuf,
    /// Root of the CUE module the project was evaluated in.
    pub cue_module_root: Option<PathBuf>,
    /// Caching infrastructure carrying the project's own runtime identity.
    pub cache: Option<TaskCacheConfig>,
    /// Remote cache authorization for tasks belonging to this external project.
    pub cache_policy: ExternalCachePolicy,
}

/// Task executor configuration
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Whether to capture output (vs streaming to stdout/stderr)
    pub capture_output: OutputCapture,
    /// Maximum parallel tasks (0 = unlimited)
    pub max_parallel: usize,
    /// When `true`, a failing task does not abort the run: its dependents
    /// in later parallel groups are emitted as `task.skipped` (with a
    /// `DependencyFailed` reason) and unrelated sibling chains continue.
    /// A panic / `JoinError` is always fatal — we don't reason about
    /// state after a panic. Mirrors `ci.pipelines[*].continueOnError`.
    pub continue_on_error: bool,
    /// Environment variables to propagate (resolved via policies)
    pub environment: Environment,
    /// Environments for the tasks that do not run with `environment`.
    pub task_environments: Option<Arc<dyn TaskEnvironments>>,
    /// Optional working directory override (reserved for future backends)
    pub working_dir: Option<PathBuf>,
    /// Project root for resolving inputs/outputs (env.cue root)
    pub project_root: PathBuf,
    /// Path to cue.mod root for resolving relative source paths
    pub cue_module_root: Option<PathBuf>,
    /// Optional task-result caching infrastructure (CAS + action cache + VCS hasher).
    /// When `None`, the executor behaves exactly as it did before content-addressed
    /// caching was wired in: tasks always run, nothing is persisted, nothing is read.
    pub cache: Option<TaskCacheConfig>,
    /// VCS workspace used to resolve sandbox inputs when cache storage is unavailable.
    pub sandbox_hasher_root: Option<PathBuf>,
    /// The projects whose tasks run as dependencies of this run, by what
    /// their tasks are qualified with (`#project:task`). A qualified task
    /// whose project is missing here does not run. This supplies where such
    /// a task runs and what it is keyed with; `task_environments` has to
    /// supply its environment, or it runs with `environment`.
    pub external_projects: BTreeMap<String, ProjectExecution>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            capture_output: OutputCapture::Capture,
            max_parallel: 0,
            continue_on_error: false,
            environment: Environment::new(),
            task_environments: None,
            working_dir: None,
            project_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            cue_module_root: None,
            cache: None,
            sandbox_hasher_root: None,
            external_projects: BTreeMap::new(),
        }
    }
}

/// Controls whether remote cache uploads block task completion or run in the
/// background until [`TaskExecutor::flush_remote_uploads`] is awaited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UploadDispatchMode {
    /// Wait for the remote upload to finish before `execute_task` returns.
    Inline,
    /// Spawn remote uploads onto the shared `pending_uploads` queue so
    /// dependent tasks in a DAG can start immediately; `execute_graph` drains
    /// the queue before returning.
    Background,
}

/// Upper bound on how long [`TaskExecutor::flush_remote_uploads`] waits for
/// in-flight background remote cache uploads to complete after a graph run.
const REMOTE_UPLOAD_FLUSH_TIMEOUT: Duration = Duration::from_secs(120);

/// Task executor
pub struct TaskExecutor {
    config: ExecutorConfig,
    /// Scratch store for directory isolation when no result cache is
    /// configured. Created on first use, shared by every clone of this
    /// executor, and removed when the last one is dropped.
    sandbox_store: Arc<OnceLock<SandboxStore>>,
    /// In-flight background remote cache uploads spawned during DAG execution.
    pending_uploads: Arc<tokio::sync::Mutex<tokio::task::JoinSet<()>>>,
    /// Whether `maybe_publish_remote` runs inline or spawns onto `pending_uploads`.
    upload_dispatch: UploadDispatchMode,
}

/// Where exec roots live when there is no result cache to put them beside.
struct SandboxStore {
    /// Removes the store's directory when the executor goes away.
    _root: super::exec_root::ScratchDir,
    config: TaskCacheConfig,
}

impl TaskExecutor {
    /// Create a new executor.
    #[must_use]
    pub fn new(mut config: ExecutorConfig) -> Self {
        if let Some(cache) = &config.cache {
            config
                .sandbox_hasher_root
                .get_or_insert_with(|| cache.vcs_hasher_root.clone());
        }
        Self {
            config,
            sandbox_store: Arc::new(OnceLock::new()),
            pending_uploads: Arc::new(tokio::sync::Mutex::new(tokio::task::JoinSet::new())),
            upload_dispatch: UploadDispatchMode::Inline,
        }
    }

    fn sandbox_only_cache(&self) -> Result<TaskCacheConfig> {
        if let Some(store) = self.sandbox_store.get() {
            return Ok(store.config.clone());
        }

        let parent = std::env::temp_dir().join("cuenv-sandbox");
        std::fs::create_dir_all(&parent)
            .map_err(|error| Error::io_with_path("create sandbox store", parent.clone(), error))?;
        let root = super::exec_root::ScratchDir::create(&parent)?;
        let cache_root = root.path().to_path_buf();
        let hasher_root = self
            .config
            .sandbox_hasher_root
            .clone()
            .or_else(|| self.config.cue_module_root.clone())
            .unwrap_or_else(|| self.config.project_root.clone());
        let config = TaskCacheConfig {
            cas: Arc::new(cuenv_cas::LocalCas::open(&cache_root).map_err(|error| {
                Error::configuration(format!("open sandbox blob store: {error}"))
            })?),
            action_cache: Arc::new(cuenv_cas::LocalActionCache::open(&cache_root).map_err(
                |error| Error::configuration(format!("open sandbox action cache: {error}")),
            )?),
            vcs_hasher: Arc::new(cuenv_vcs::WalkHasher::new(&hasher_root)),
            vcs_hasher_root: hasher_root,
            action_semantics_version: cuenv_cas::ACTION_SEMANTICS_VERSION,
            runtime_identity_properties: std::collections::BTreeMap::new(),
            cache_disabled_reason: Some(
                "result cache unavailable; execution isolation only".to_string(),
            ),
            secret_salt: None,
            mode_override: None,
            cache_root,
            publisher: None,
        };

        // Two tasks racing here each build a store; the loser's is dropped,
        // which removes its directory, and both use the winner's.
        let _ = self.sandbox_store.set(SandboxStore {
            _root: root,
            config,
        });
        self.sandbox_store
            .get()
            .map(|store| store.config.clone())
            .ok_or_else(|| Error::configuration("sandbox store was not initialized"))
    }

    /// Execute a single task, consulting the action cache when configured.
    ///
    /// Flow:
    /// 1. If a [`TaskCacheConfig`] is set and the task declares `inputs`,
    ///    build the [`cuenv_cas::Action`] envelope and look it up in the
    ///    action cache. On a hit, materialize cached outputs into the
    ///    workdir and return without spawning anything.
    /// 2. On a miss, a task under `sandbox: "dir"` gets a per-action exec
    ///    root holding exactly its declared inputs and runs there.
    /// 3. Otherwise run the task directly on the host.
    /// 4. On a successful miss, project declared outputs back into the
    ///    workspace and persist outputs + result to the cache.
    ///
    /// A task that runs and fails is not an error: its exit code and output
    /// are in the returned [`TaskResult`], and a failed cache write is only
    /// logged.
    ///
    /// # Errors
    ///
    /// Returns an error when the task passes host `HOME` through explicitly,
    /// belongs to a project this run has not loaded, or declares inputs it
    /// cannot read where it runs (an unbound file reference, or a relocating
    /// mapping without directory isolation); when its environment or secrets,
    /// command, working directory, timeout or retry delay cannot be resolved;
    /// when a
    /// sandboxed task's inputs cannot be resolved or staged; when its process
    /// cannot be started or waited on; or when its outputs cannot be
    /// collected or projected back into the workspace., skip(self, task), fields(task_name = %name))]
    pub async fn execute_task(&self, name: &str, task: &Task) -> Result<TaskResult> {
        if task.has_host_home_passthrough() {
            return Err(Error::configuration(
                "host HOME cannot be passed through explicitly; remove the passthrough. Host tasks with hermetic: false inherit it automatically",
            ));
        }

        // The project that owns the task decides where it runs and what it
        // is keyed on. A task of another project never borrows this run's.
        let frame = self.frame_for(name, task)?;
        reject_unbound_file_references(name, task)?;
        reject_unmaterialized_mapping(name, task)?;

        // Load the base task environment before probing strict sandboxing so a
        // locked Nix runtime can derive its verified `/nix/store` closure.
        let base_environment = self.base_task_environment(name, task).await?;
        let nix_context = frame.cache.and_then(|cache| {
            if cache.cache_disabled_reason.is_some() {
                return None;
            }
            let exec_env = base_environment.execution_environment(task.env_passthrough());
            super::sandbox::extract_nix_sandbox_context(
                &cache.runtime_identity_properties,
                exec_env.iter().map(|(k, v)| (k.as_str(), v.as_str())),
            )
        });

        // Resolve the effective sandbox tier up front so explicit `strict`
        // fails closed even when caching is disabled, and `auto` keys the
        // action under the tier that will actually be enforced.
        let sandbox_resolution = super::sandbox::resolve_sandbox_with_nix(
            task.sandbox(),
            super::sandbox::NetworkPolicy::for_task(task),
            nix_context.as_ref(),
        )?;
        if let Some(reason) = &sandbox_resolution.fallback_reason {
            tracing::warn!(
                task = %name,
                reason = %reason,
                "hermetic.sandbox: \"auto\" fell back to \"dir\""
            );
        }
        let effective_sandbox = sandbox_resolution.effective;
        let task_environment =
            Self::finalize_task_environment(base_environment, task, &effective_sandbox)?;

        // Input resolution and directory isolation remain available when
        // result caching is disabled. A sandbox-only config supplies the
        // hasher and execution-root location without permitting cache I/O.
        let cache = match frame.cache.cloned() {
            Some(cache) => Some(cache),
            None if effective_sandbox.uses_exec_root() => Some(self.sandbox_only_cache()?),
            None => None,
        };
        let mut cache_handle: Option<CacheHandle> = if let Some(cache) = cache {
            let workdir = Self::workdir_in(frame, task)?;
            let outcome = super::cache::build_action_with_sandbox(
                BuildActionInput {
                    task,
                    task_name: name,
                    environment: &task_environment,
                    cache: &cache,
                    workdir: &workdir,
                    project_root: frame.project_root,
                    module_root: frame.module_root,
                },
                &effective_sandbox,
            )
            .await?;
            match outcome {
                super::cache::CacheOutcome::Eligible(eligible) => {
                    let root_key = eligible.digest.hash.clone();
                    let inputs = eligible.inputs;
                    let working_directory = eligible.working_directory;

                    // In inherited-output mode the process result has empty
                    // stdout/stderr, while capture mode for the same action
                    // has real bytes. They cannot share an action result.
                    if self.config.capture_output.should_capture() {
                        // Cache lookup. On a hit, short-circuit execution.
                        if let Some(_guard) = acquire_cache_use(&cache, name)
                            && let Some((cached, source)) =
                                super::cache::lookup_with_source(&cache, &eligible.digest, task)
                                    .await?
                        {
                            match self
                                .return_cache_hit(CacheHitInput {
                                    name,
                                    task,
                                    cache: &cache,
                                    action_digest: &eligible.digest,
                                    workdir: &workdir,
                                    cached: &cached,
                                    source,
                                })
                                .await
                            {
                                Ok(result) => {
                                    tracing::debug!(task = %name, "action cache hit");
                                    cuenv_events::emit_task_cache_hit!(
                                        name,
                                        eligible.digest.to_string()
                                    );
                                    return Ok(result);
                                }
                                Err(error) => {
                                    tracing::warn!(
                                        task = %name,
                                        %error,
                                        "cache hit could not be verified and materialized; executing task"
                                    );
                                }
                            }
                        }
                        tracing::debug!(task = %name, "action cache miss");
                        cuenv_events::emit_task_cache_miss!(name);
                        let exec_root = Self::prepare_exec_root(PrepareExecRootInput {
                            name,
                            task,
                            cache: &cache,
                            root_key: &root_key,
                            inputs: &inputs,
                        })?;
                        let run_workdir = exec_root
                            .as_ref()
                            .map(|root| root.workdir(&working_directory))
                            .transpose()?
                            .unwrap_or_else(|| workdir.clone());
                        Some(CacheHandle {
                            cache,
                            action_digest: Some(eligible.digest),
                            action: Some(eligible.action),
                            command: Some(eligible.command),
                            command_digest: Some(eligible.command_digest),
                            contains_secrets: eligible.contains_secrets,
                            publish_authorized: frame.publish_authorized,
                            workdir,
                            exec_root,
                            run_workdir,
                            root_key,
                            inputs,
                            working_directory,
                            effective_sandbox,
                            nix_context,
                        })
                    } else {
                        cuenv_events::emit_task_cache_skipped!(
                            name,
                            cuenv_events::CacheSkipReason::Disabled {
                                reason: Some(
                                    "streamed output cannot be reproduced from the action cache"
                                        .to_string()
                                )
                            }
                        );
                        let exec_root = Self::prepare_exec_root(PrepareExecRootInput {
                            name,
                            task,
                            cache: &cache,
                            root_key: &root_key,
                            inputs: &inputs,
                        })?;
                        let run_workdir = exec_root
                            .as_ref()
                            .map(|root| root.workdir(&working_directory))
                            .transpose()?
                            .unwrap_or_else(|| workdir.clone());
                        Some(CacheHandle {
                            cache,
                            action_digest: None,
                            action: None,
                            command: None,
                            command_digest: None,
                            contains_secrets: eligible.contains_secrets,
                            publish_authorized: false,
                            workdir,
                            exec_root,
                            run_workdir,
                            root_key,
                            inputs,
                            working_directory,
                            effective_sandbox,
                            nix_context,
                        })
                    }
                }
                super::cache::CacheOutcome::Skipped { reason, execution } => {
                    cuenv_events::emit_task_cache_skipped!(name, reason.clone());
                    if effective_sandbox.uses_exec_root() {
                        // Fail closed, as documented: running against the
                        // live checkout would hand back a result that looks
                        // isolated and is not, and any input mapped from
                        // another task or project would simply be missing.
                        let Some(execution) = execution else {
                            return Err(Error::configuration(format!(
                                "task '{name}' requires hermetic.sandbox: \"{}\" but its input \
                                 set cannot be resolved ({reason}); fix the declaration or set \
                                 hermetic.sandbox: \"none\"",
                                effective_sandbox.key_value()
                            )));
                        };
                        let root_key = cuenv_cas::Digest::of_bytes(
                            format!("{name}:{}", workdir.display()).as_bytes(),
                        );
                        let exec_root = Self::prepare_exec_root(PrepareExecRootInput {
                            name,
                            task,
                            cache: &cache,
                            root_key: &root_key.hash,
                            inputs: &execution.inputs,
                        })?;
                        let run_workdir = exec_root
                            .as_ref()
                            .map(|root| root.workdir(&execution.working_directory))
                            .transpose()?
                            .unwrap_or_else(|| workdir.clone());
                        Some(CacheHandle {
                            cache,
                            action_digest: None,
                            action: None,
                            command: None,
                            command_digest: None,
                            contains_secrets: false,
                            publish_authorized: false,
                            workdir,
                            exec_root,
                            run_workdir,
                            root_key: root_key.hash,
                            inputs: execution.inputs,
                            working_directory: execution.working_directory,
                            effective_sandbox,
                            nix_context,
                        })
                    } else {
                        None
                    }
                }
            }
        } else {
            None
        };

        // Real execution. Both backends produce the same `TaskResult`. A
        // sandboxed task runs in its exec root; everything else runs where it
        // always did.
        let start = std::time::Instant::now();
        let context = TaskExecutionContext {
            name,
            task,
            environment: &task_environment,
        };
        let result = self
            .execute_task_with_retries(context, cache_handle.as_mut())
            .await?;
        let duration_ms = start.elapsed().as_millis();

        let resolved_outputs = if result.success {
            cache_handle
                .as_ref()
                .map(|handle| super::cache::collect_outputs(&handle.run_workdir, &task.outputs))
                .transpose()?
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // First-run projection and cache recording use the same resolved set.
        // Projection must succeed before publishing an action-cache entry;
        // otherwise a failed first run could become a successful cache hit.
        if let Some(handle) = &cache_handle
            && handle.exec_root.is_some()
            && result.success
        {
            let existing_outputs = super::cache::collect_outputs(&handle.workdir, &task.outputs)?;
            super::exec_root::project_outputs(super::exec_root::ProjectOutputs {
                exec_root: &handle.run_workdir,
                workdir: &handle.workdir,
                resolved: &resolved_outputs,
                existing: &existing_outputs,
            })?;
        }

        // Persist on successful miss. Cache writes are best-effort: a write
        // failure logs but does not fail the user's task.
        if let Some(handle) = &cache_handle
            && let Some(action_digest) = &handle.action_digest
            && super::cache::effective_policy(&handle.cache, task)
                .mode
                .allows_write()
            && result.exit_code == Some(0)
            && let Some(guard) = acquire_cache_use(&handle.cache, name)
        {
            let recorded = super::cache::record_resolved(
                RecordInput {
                    cache: &handle.cache,
                    action_digest,
                    workdir: &handle.run_workdir,
                    task,
                    stdout: &result.stdout,
                    stderr: &result.stderr,
                    exit_code: 0,
                    duration_ms,
                },
                &resolved_outputs,
            )
            .await;
            match recorded {
                Ok(Some(action_result)) => {
                    self.maybe_publish_remote(PublishRemoteInput {
                        task_name: name,
                        handle,
                        action_result,
                        cache_use_guard: guard,
                    })
                    .await;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(task = %name, error = %e, "cache write failed");
                }
            }
        }

        Ok(result)
    }

    async fn base_task_environment(&self, name: &str, task: &Task) -> Result<Environment> {
        let selected = match &self.config.task_environments {
            Some(environments) => environments.environment_for(name, task).await?,
            None => None,
        };
        let mut environment = selected.unwrap_or_else(|| self.config.environment.clone());
        environment.withhold(self.config.environment.withheld_names().iter().cloned());
        Ok(environment)
    }

    fn finalize_task_environment(
        mut environment: Environment,
        task: &Task,
        sandbox: &super::sandbox::EffectiveSandbox,
    ) -> Result<Environment> {
        if cfg!(target_os = "linux") && sandbox.strict_proof().is_some() {
            environment.set("HOME".to_string(), "/cuenv-home".to_string());
            environment.set("TMPDIR".to_string(), "/tmp".to_string());
            return Ok(environment);
        }
        if !task.is_hermetic() || environment.get("HOME").is_some() || task.env.contains_key("HOME")
        {
            return Ok(environment);
        }

        let home = super::hermetic_home::writable_task_home()?;
        environment.set("HOME".to_string(), home.to_string_lossy().into_owned());
        Ok(environment)
    }

    /// Materialize the exec root a sandboxed task runs in.
    ///
    /// Returns `None` for a task that opted out of isolation.
    fn prepare_exec_root(
        input: PrepareExecRootInput<'_>,
    ) -> Result<Option<super::exec_root::ExecRoot>> {
        let PrepareExecRootInput {
            name,
            task,
            cache,
            root_key,
            inputs,
        } = input;
        if !task.sandbox().uses_exec_root() {
            return Ok(None);
        }
        let exec_root = super::exec_root::prepare(&cache.cache_root, root_key, inputs)?;
        tracing::info!(
            task = %name,
            path = %exec_root.path().display(),
            inputs = inputs.len(),
            "running in a sandboxed exec root"
        );
        Ok(Some(exec_root))
    }

    fn reset_exec_root(name: &str, task: &Task, handle: &mut CacheHandle) -> Result<()> {
        if !task.sandbox().uses_exec_root() {
            return Ok(());
        }

        // Dropping the failed attempt's guard removes every undeclared write
        // it left behind. The next attempt starts from the same verified input
        // snapshot as the action key.
        handle.exec_root.take();
        let exec_root = Self::prepare_exec_root(PrepareExecRootInput {
            name,
            task,
            cache: &handle.cache,
            root_key: &handle.root_key,
            inputs: &handle.inputs,
        })?;
        handle.run_workdir = exec_root
            .as_ref()
            .map(|root| root.workdir(&handle.working_directory))
            .transpose()?
            .unwrap_or_else(|| handle.workdir.clone());
        handle.exec_root = exec_root;
        Ok(())
    }

    async fn execute_task_with_retries(
        &self,
        context: TaskExecutionContext<'_>,
        mut cache_handle: Option<&mut CacheHandle>,
    ) -> Result<TaskResult> {
        let TaskExecutionContext { name, task, .. } = context;
        let retry_attempts = task.retry.as_ref().map_or(0, |retry| retry.attempts);
        let max_attempts = retry_attempts.saturating_add(1);
        let retry_delay = task
            .retry
            .as_ref()
            .and_then(|retry| retry.delay.as_deref())
            .map(|delay| parse_task_duration("retry.delay", delay))
            .transpose()?;

        let mut attempt = 1_u32;
        loop {
            // A timeout is a hard policy violation, not a transient failure:
            // retrying would re-incur the full timeout each attempt, so a
            // timed-out attempt ends the task immediately.
            let result = match self
                .execute_task_non_hermetic(context, cache_handle.as_deref())
                .await?
            {
                TaskAttempt::TimedOut(result) => return Ok(result),
                TaskAttempt::Completed(result) => result,
            };
            if result.success || attempt >= max_attempts {
                return Ok(result);
            }

            attempt = attempt.saturating_add(1);
            cuenv_events::emit_task_retrying!(name, attempt, max_attempts);
            if let Some(delay) = retry_delay {
                tokio::time::sleep(delay).await;
            }
            if let Some(handle) = cache_handle.as_deref_mut() {
                Self::reset_exec_root(name, task, handle)?;
            }
        }
    }

    /// Reproduce a [`TaskResult`] from a cache hit and emit the same
    /// lifecycle events the executor would emit on a normal run, so
    /// downstream renderers (CLI / TUI / JSON) see no behavioral
    /// difference between a cached and an uncached task.
    async fn return_cache_hit(&self, input: CacheHitInput<'_>) -> Result<TaskResult> {
        let CacheHitInput {
            name,
            task,
            cache,
            action_digest,
            workdir,
            cached,
            source,
        } = input;

        let (stdout, stderr, exit_code) = super::cache::materialize_hit(
            cache,
            super::cache::MaterializeOutputs {
                workdir,
                task,
                result: cached,
            },
        )
        .await?;
        if let Err(error) = cache
            .action_cache
            .commit_verified(action_digest, cached)
            .await
        {
            tracing::warn!(
                action = %action_digest,
                %error,
                "could not promote verified action result locally"
            );
        }
        let success = exit_code == 0;

        let cmd_str = if let Some(script) = &task.script {
            format!("[script: {} bytes] (cached)", script.len())
        } else if task.command.is_empty() {
            format!("{} (cached)", task.args.join(" "))
        } else {
            format!("{} {} (cached)", task.command, task.args.join(" "))
        };
        cuenv_events::emit_task_started!(name, cmd_str, false);
        emit_cached_output_events(name, "stdout", &stdout);
        emit_cached_output_events(name, "stderr", &stderr);
        cuenv_events::emit_task_completed!(
            name,
            success,
            Some(exit_code),
            u64::try_from(cached.execution_metadata.duration_ms).unwrap_or(0)
        );

        Ok(TaskResult {
            name: name.to_string(),
            exit_code: Some(exit_code),
            stdout,
            stderr,
            success,
            from_cache: true,
            cache_source: Some(source),
        })
    }

    /// The project a task belongs to: the run's own, or, for a task named
    /// `#project:task`, the project it was loaded from.
    fn frame_for<'frame>(
        &'frame self,
        name: &str,
        task: &'frame Task,
    ) -> Result<TaskFrame<'frame>> {
        let Some(external) = ExternalTaskName::parse(name) else {
            return Ok(self.own_frame(task));
        };
        let project = self
            .config
            .external_projects
            .get(external.project)
            .ok_or_else(|| {
                Error::configuration(format!(
                    "Task '{name}' belongs to project '{}', which this run has not loaded. It \
                     is not run with another project's directory or environment",
                    external.project
                ))
            })?;
        Ok(TaskFrame {
            project_root: &project.project_root,
            module_root: project
                .cue_module_root
                .as_deref()
                .unwrap_or(&project.project_root),
            cache: project.cache.as_ref(),
            publish_authorized: project.cache_policy == ExternalCachePolicy::AllowPublish,
        })
    }

    /// The frame of a task of the run's own project.
    fn own_frame<'frame>(&'frame self, task: &'frame Task) -> TaskFrame<'frame> {
        TaskFrame {
            project_root: task
                .project_root
                .as_deref()
                .unwrap_or(&self.config.project_root),
            module_root: self
                .config
                .cue_module_root
                .as_deref()
                .unwrap_or(&self.config.project_root),
            cache: self.config.cache.as_ref(),
            publish_authorized: true,
        }
    }

    /// Compute the working directory the executor would use for a task of
    /// the run's own project.
    #[cfg(test)]
    fn workdir_for_task(&self, task: &Task) -> Result<PathBuf> {
        Self::workdir_in(self.own_frame(task), task)
    }

    /// Compute the working directory the executor would use for a task.
    ///
    /// Extracted from `execute_task_non_hermetic` so that the cache wrapper
    /// in `execute_task` can resolve outputs against the same path the
    /// task itself ran in.
    ///
    /// A task runs in its `dir`, else in the directory of the file that
    /// defines it, else in its project. That is the same directory whether
    /// the task is run in its own project or as a dependency of another's.
    fn workdir_in(frame: TaskFrame<'_>, task: &Task) -> Result<PathBuf> {
        if let Some(ref dir) = task.directory {
            return resolve_task_directory(frame.module_root, task, dir);
        }
        if let Some(ref source) = task.source {
            return Ok(source.directory().map_or_else(
                || frame.module_root.to_path_buf(),
                |dir| frame.module_root.join(dir),
            ));
        }
        if task.project_root.is_none()
            && !task.is_hermetic()
            && let Some(manager) = cuenv_workspaces::detect_from_command(&task.command)
        {
            return Ok(super::workspace::find_workspace_root(
                manager,
                frame.project_root,
            ));
        }
        Ok(frame.project_root.to_path_buf())
    }

    /// Execute a task non-hermetically (directly in workspace/project root)
    ///
    /// Used for tasks like `bun install` that need to write to the real filesystem.
    async fn execute_task_non_hermetic(
        &self,
        context: TaskExecutionContext<'_>,
        cache_handle: Option<&CacheHandle>,
    ) -> Result<TaskAttempt> {
        let TaskExecutionContext {
            name,
            task,
            environment,
        } = context;

        // A sandboxed task's exec root wins; everything else resolves the
        // working directory the usual way (see `workdir_in`).
        let workdir = match cache_handle {
            Some(handle) => handle.run_workdir.clone(),
            None => Self::workdir_in(self.frame_for(name, task)?, task)?,
        };
        let strict_active =
            cache_handle.is_some_and(|handle| handle.effective_sandbox.strict_proof().is_some());

        tracing::info!(
            task = %name,
            workdir = %workdir.display(),
            sandboxed = cache_handle.is_some(),
            strict = strict_active,
            "Executing task"
        );

        // Emit command being run - always emit task_started for all modes
        // (TUI needs events even when capture_output is true)
        let cmd_str = if let Some(script) = &task.script {
            format!("[script: {} bytes]", script.len())
        } else if task.command.is_empty() {
            task.args.join(" ")
        } else {
            format!("{} {}", task.command, task.args.join(" "))
        };

        cuenv_events::emit_task_started!(name, cmd_str, false);

        // Build command - handle script mode vs command mode.
        // When OS-level strict confinement is active, prepare the sandbox plan
        // before fork so all path allocations happen safely in the parent.
        let sandbox = cache_handle.map_or(&super::sandbox::EffectiveSandbox::None, |handle| {
            &handle.effective_sandbox
        });
        let command_spec = super::command_ext::command_spec_in_sandbox(task, environment, sandbox)?;
        let strict_scratch = if strict_active
            && let Some(handle) = cache_handle
            && let Some(exec_root) = &handle.exec_root
        {
            let tmp_dir = exec_root.path().join(".cuenv-tmp");
            let home_dir = exec_root.path().join(".cuenv-home");
            std::fs::create_dir_all(&tmp_dir).map_err(|error| {
                Error::io_with_path("create sandbox tmp", tmp_dir.clone(), error)
            })?;
            std::fs::create_dir_all(&home_dir).map_err(|error| {
                Error::io_with_path("create sandbox home", home_dir.clone(), error)
            })?;
            let plan =
                super::sandbox::StrictSandboxPlan::prepare(super::sandbox::StrictSandboxOptions {
                    exec_root: exec_root.path(),
                    workdir: &workdir,
                    task_tmp: &tmp_dir,
                    task_home: &home_dir,
                    network: super::sandbox::NetworkPolicy::for_task(task),
                    nix_context: handle.nix_context.as_ref(),
                })?;
            let cmd = plan.build_command(&command_spec.program, &command_spec.args)?;
            Some((cmd, tmp_dir, home_dir, plan))
        } else {
            None
        };

        let (mut cmd, strict_tmp_home) =
            if let Some((cmd, tmp_dir, home_dir, plan)) = strict_scratch {
                (cmd, Some((tmp_dir, home_dir, plan)))
            } else {
                let mut cmd = Command::new(&command_spec.program);
                cmd.args(&command_spec.args);
                (cmd, None)
            };

        // A hermetic command receives exactly the environment represented in
        // its action key. Without `env_clear`, Command inherits every host
        // variable before these overrides are applied.
        cmd.current_dir(&workdir);
        let env_vars = if task.is_hermetic() {
            cmd.env_clear();
            environment
                .execution_environment(task.env_passthrough())
                .into_iter()
                .collect::<std::collections::HashMap<_, _>>()
        } else {
            environment.merge_with_system()
        };
        for (k, v) in &env_vars {
            cmd.env(k, v);
        }
        if let Some((tmp_dir, home_dir, _)) = &strict_tmp_home {
            // Inside the strict mount namespace, private tmp/home are mounted
            // at `/tmp` and `/cuenv-home` on Linux (or the staged dirs on macOS).
            #[cfg(target_os = "linux")]
            {
                let _ = (tmp_dir, home_dir);
                cmd.env("TMPDIR", "/tmp");
                cmd.env("HOME", "/cuenv-home");
            }
            #[cfg(not(target_os = "linux"))]
            {
                cmd.env("TMPDIR", tmp_dir);
                cmd.env("HOME", home_dir);
            }
        }

        // Apply task-level env vars, including secrets resolved at execution time.
        // A policy on one of them names the task as its own project does.
        let own_name = ExternalTaskName::parse(name).map_or(name, |external| external.task);
        let (task_env, secrets) = super::env::resolve_task_env_withheld(
            own_name,
            &task.env,
            environment.withheld_names(),
        )
        .await?;
        cuenv_events::register_secrets(secrets);
        for (key, value) in task_env {
            cmd.env(key, value);
        }
        cmd.env_remove("CUENV_HOOK_SESSION");

        // Force color output even when stdout is piped (for capture mode)
        // These are widely supported: FORCE_COLOR by Node.js/chalk, CLICOLOR_FORCE by BSD/macOS
        if !env_vars.contains_key("FORCE_COLOR") {
            cmd.env("FORCE_COLOR", "1");
        }
        if !env_vars.contains_key("CLICOLOR_FORCE") {
            cmd.env("CLICOLOR_FORCE", "1");
        }

        for name in environment.withheld_names() {
            cmd.env_remove(name);
        }

        let timeout = task
            .timeout
            .as_deref()
            .map(|timeout| parse_task_duration("timeout", timeout))
            .transpose()?;
        #[cfg(target_os = "linux")]
        let (cmd, launcher_stdin, launcher_exchange) = match strict_tmp_home {
            Some((_, _, plan)) => plan.finish_command(cmd)?,
            None => (cmd, None, None),
        };
        #[cfg(not(target_os = "linux"))]
        let (cmd, launcher_stdin, launcher_exchange) = {
            let _ = strict_tmp_home;
            (cmd, None, None)
        };
        super::process::run_task_process(
            name,
            cmd,
            ProcessOptions {
                capture_output: self.config.capture_output,
                timeout,
                isolate_stdio_and_fds: strict_active || task.is_hermetic(),
                launcher_stdin,
                launcher_exchange,
            },
        )
        .await
    }

    /// Execute a task node (single task, group, or list)
    #[async_recursion]
    pub async fn execute_node(
        &self,
        name: &str,
        node: &TaskNode,
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        match node {
            TaskNode::Task(task) => {
                let result = self.execute_task(name, task.as_ref()).await?;
                Ok(vec![result])
            }
            TaskNode::Group(group) => self.execute_parallel(name, group, all_tasks).await,
            TaskNode::Sequence(seq) => self.execute_sequential(name, seq, all_tasks).await,
        }
    }

    async fn execute_sequential(
        &self,
        prefix: &str,
        sequence: &[TaskNode],
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        let emit_lifecycle = !self.config.capture_output.should_capture();
        if emit_lifecycle {
            cuenv_events::emit_task_group_started!(prefix, true, sequence.len());
        }
        let started = std::time::Instant::now();
        let outcome = self
            .execute_sequential_inner(prefix, sequence, all_tasks)
            .await;
        if emit_lifecycle {
            emit_group_completion(GroupCompletion {
                prefix,
                started,
                outcome: &outcome,
                total_children: sequence.len(),
            });
        }
        outcome
    }

    async fn execute_sequential_inner(
        &self,
        prefix: &str,
        sequence: &[TaskNode],
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        let mut results = Vec::new();
        // Track completed step results for output ref resolution within sequences.
        let mut seq_results: std::collections::HashMap<String, TaskResult> =
            std::collections::HashMap::new();

        for (i, step) in sequence.iter().enumerate() {
            let task_name = format!("{}[{}]", prefix, i);

            // For Task steps, resolve any output ref placeholders from prior steps.
            // Only clone when placeholders are actually present to avoid
            // unnecessary allocations in the common (no-refs) case.
            let step = if let TaskNode::Task(task) = step
                && super::output_refs::has_output_refs(&task.args, &task.env)
            {
                let mut resolved_task = (**task).clone();
                let resolver = super::output_refs::OutputRefResolver {
                    task_name: &task_name,
                    results: &seq_results,
                };
                resolver.resolve(&mut resolved_task.args, &mut resolved_task.env)?;
                TaskNode::Task(Box::new(resolved_task))
            } else {
                step.clone()
            };

            let task_results = self.execute_node(&task_name, &step, all_tasks).await?;
            for result in &task_results {
                // Sequences always stop on first error (no configuration option)
                if !result.success {
                    return Err(Error::task_failed(
                        &result.name,
                        result.exit_code.unwrap_or(-1),
                        cuenv_core::tasks::TaskFailureOutput {
                            stdout: &result.stdout,
                            stderr: &result.stderr,
                        },
                    ));
                }
                seq_results.insert(result.name.clone(), result.clone());
            }
            results.extend(task_results);
        }
        Ok(results)
    }

    async fn execute_parallel(
        &self,
        prefix: &str,
        group: &TaskGroup,
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        let emit_lifecycle = !self.config.capture_output.should_capture();
        // Check for "default" task to override parallel execution
        if let Some(default_task) = group.children.get("default") {
            if emit_lifecycle {
                cuenv_events::emit_task_group_started!(prefix, true, 1_usize);
            }
            let started = std::time::Instant::now();
            // Execute only the default task, using the group prefix directly
            // since "default" is implicit when invoking the group name
            let task_name = format!("{}.default", prefix);
            let outcome = self.execute_node(&task_name, default_task, all_tasks).await;
            if emit_lifecycle {
                emit_group_completion(GroupCompletion {
                    prefix,
                    started,
                    outcome: &outcome,
                    total_children: 1,
                });
            }
            return outcome;
        }

        if emit_lifecycle {
            cuenv_events::emit_task_group_started!(
                prefix,
                false,
                group.children.len(),
                None::<&str>,
                group.max_concurrency
            );
        }
        let started = std::time::Instant::now();
        let total_children = group.children.len();
        let outcome = self.execute_parallel_inner(prefix, group, all_tasks).await;
        if emit_lifecycle {
            emit_group_completion(GroupCompletion {
                prefix,
                started,
                outcome: &outcome,
                total_children,
            });
        }
        outcome
    }

    async fn execute_parallel_inner(
        &self,
        prefix: &str,
        group: &TaskGroup,
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        let mut join_set = JoinSet::new();
        let all_tasks = Arc::new(all_tasks.clone());
        let mut all_results = Vec::new();
        let mut merge_results = |results: Vec<TaskResult>| -> Result<()> {
            if let Some(failed) = results.iter().find(|r| !r.success)
                && !self.config.continue_on_error
            {
                return Err(Error::task_failed(
                    &failed.name,
                    failed.exit_code.unwrap_or(-1),
                    cuenv_core::tasks::TaskFailureOutput {
                        stdout: &failed.stdout,
                        stderr: &failed.stderr,
                    },
                ));
            }
            all_results.extend(results);
            Ok(())
        };
        let max_parallel = group_concurrency_limit(self.config.max_parallel, group.max_concurrency);
        for (name, child_node) in &group.children {
            let task_name = format!("{}.{}", prefix, name);
            let child_node = child_node.clone();
            let all_tasks = Arc::clone(&all_tasks);
            let executor = self.clone_with_config();
            join_set.spawn(async move {
                executor
                    .execute_node(&task_name, &child_node, &all_tasks)
                    .await
            });
            if max_parallel > 0
                && join_set.len() >= max_parallel
                && let Some(result) = join_set.join_next().await
            {
                match result {
                    Ok(Ok(results)) => merge_results(results)?,
                    Ok(Err(e)) => return Err(e),
                    Err(e) => {
                        return Err(Error::execution(format!("Task execution panicked: {}", e)));
                    }
                }
            }
        }
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok(results)) => merge_results(results)?,
                Ok(Err(e)) => return Err(e),
                Err(e) => {
                    return Err(Error::execution(format!("Task execution panicked: {}", e)));
                }
            }
        }
        Ok(all_results)
    }

    /// Execute every task in the graph in dependency order.
    ///
    /// # Errors
    /// Returns an error when graph traversal fails or a task errors and
    /// `continue_on_error` is disabled.
    #[instrument(name = "execute_graph", skip(self, graph), fields(task_count = graph.task_count()))]
    pub async fn execute_graph(&self, graph: &TaskGraph) -> Result<Vec<TaskResult>> {
        use super::graph_walk;
        use super::graph_walk::{WalkCallbacks, WalkPolicy};

        let policy = WalkPolicy {
            max_parallel: self.config.max_parallel,
            continue_on_error: self.config.continue_on_error,
        };

        let graph_executor = Arc::new(self.clone_with_upload_mode(UploadDispatchMode::Background));
        let summary_result = graph_walk::walk_parallel_graph(
            graph.inner(),
            policy,
            WalkCallbacks {
                // Resolve output-ref placeholders in args/env against tasks
                // completed in prior parallel groups. Intra-group siblings
                // are independent by definition so they don't appear here.
                prepare_node: |mut node: cuenv_task_graph::GraphNode<Task>,
                               outcomes_so_far: &std::collections::HashMap<
                    String,
                    TaskWalkOutcome,
                >|
                 -> Result<_> {
                    // The walker tracks `TaskWalkOutcome` (result + continue_on_error),
                    // but the resolver only needs the prior `TaskResult`s. Project to
                    // them — and only when this node actually references an output, so
                    // ref-free nodes don't pay for the copy.
                    if super::output_refs::has_output_refs(&node.task.args, &node.task.env) {
                        let results: std::collections::HashMap<String, TaskResult> =
                            outcomes_so_far
                                .iter()
                                .map(|(name, outcome)| (name.clone(), outcome.result.clone()))
                                .collect();
                        let resolver = super::output_refs::OutputRefResolver {
                            task_name: &node.name,
                            results: &results,
                        };
                        resolver.resolve(&mut node.task.args, &mut node.task.env)?;
                    }
                    Ok(node)
                },
                run_node: {
                    let executor = Arc::clone(&graph_executor);
                    move |node: cuenv_task_graph::GraphNode<Task>| {
                        let executor = Arc::clone(&executor);
                        async move {
                            let continue_on_error = node.task.continue_on_error;
                            executor
                                .execute_task(&node.name, &node.task)
                                .await
                                .map(|result| TaskWalkOutcome {
                                    result,
                                    continue_on_error,
                                })
                        }
                    }
                },
                from_join_error: |join_err| {
                    Error::execution(format!("Task execution panicked: {join_err}"))
                },
            },
        )
        .await;

        // Always drain background remote uploads before returning so tasks that
        // already succeeded finish publishing even when a later task fails.
        graph_executor.flush_remote_uploads().await;

        let summary = summary_result?;

        let mut all_results: Vec<TaskResult> = summary
            .outcomes
            .into_iter()
            .map(|(_name, outcome)| outcome.result)
            .collect();

        // Under fail-fast, the walker short-circuits on the first failure
        // and we surface it via Err so callers see the failing task's
        // diagnostics. Under continue_on_error, every outcome is returned
        // and the first failure is surfaced via Err at the end so callers
        // still observe a non-Ok run.
        if !self.config.continue_on_error
            && let Some(failed) = all_results.iter().find(|r| !r.success).cloned()
        {
            return Err(Error::task_failed(
                &failed.name,
                failed.exit_code.unwrap_or(-1),
                cuenv_core::tasks::TaskFailureOutput {
                    stdout: &failed.stdout,
                    stderr: &failed.stderr,
                },
            ));
        }
        if self.config.continue_on_error
            && let Some(failed) = all_results.iter().find(|r| !r.success).cloned()
        {
            // Preserve historical ordering: when failing, only the
            // already-collected outcomes come back via Err; reset
            // all_results to avoid the caller "succeeding" with mixed
            // results.
            let err = Error::task_failed(
                &failed.name,
                failed.exit_code.unwrap_or(-1),
                cuenv_core::tasks::TaskFailureOutput {
                    stdout: &failed.stdout,
                    stderr: &failed.stderr,
                },
            );
            all_results.clear();
            return Err(err);
        }

        Ok(all_results)
    }

    /// Wait for all in-flight background remote cache uploads to complete.
    ///
    /// Bounded by [`REMOTE_UPLOAD_FLUSH_TIMEOUT`] so a stalled remote endpoint
    /// cannot hang process exit indefinitely.
    pub async fn flush_remote_uploads(&self) {
        let mut set = {
            let mut guard = self.pending_uploads.lock().await;
            std::mem::take(&mut *guard)
        };
        if set.is_empty() {
            return;
        }
        let pending = set.len();
        tracing::debug!(pending, "flushing background remote cache uploads");
        let drain = async {
            while let Some(joined) = set.join_next().await {
                if let Err(error) = joined {
                    tracing::warn!(%error, "background remote cache upload task panicked");
                }
            }
        };
        if tokio::time::timeout(REMOTE_UPLOAD_FLUSH_TIMEOUT, drain)
            .await
            .is_err()
        {
            let remaining = set.len();
            set.abort_all();
            tracing::warn!(
                remaining,
                timeout_secs = REMOTE_UPLOAD_FLUSH_TIMEOUT.as_secs(),
                "timed out waiting for background remote cache uploads; aborting remaining"
            );
        }
    }

    async fn maybe_publish_remote(&self, input: PublishRemoteInput<'_>) {
        let PublishRemoteInput {
            task_name,
            handle,
            action_result,
            cache_use_guard,
        } = input;

        let Some(publisher) = &handle.cache.publisher else {
            return;
        };

        let skip_reason: Option<String> = if !handle.publish_authorized {
            Some("external project is not authorized to publish to remote cache".to_string())
        } else if handle.contains_secrets {
            Some("secret-bearing tasks are kept local-only".to_string())
        } else {
            match handle.effective_sandbox.strict_proof() {
                Some(proof) if proof.publish_eligible => None,
                Some(proof) => Some(format!(
                    "sandbox '{}' requires network isolation and a verified runtime for remote publication",
                    proof.platform
                )),
                None => Some(format!(
                    "sandbox tier '{}' does not enforce strict OS confinement",
                    handle.effective_sandbox.key_value()
                )),
            }
        };

        if let Some(reason) = skip_reason {
            tracing::debug!(task = %task_name, %reason, "skipping remote cache publication");
            let _ = cuenv_events::emit(cuenv_events::EventCategory::Cache(
                cuenv_events::CacheEvent::RemoteUploadSkipped {
                    task: task_name.to_string(),
                    reason,
                },
            ));
            return;
        }

        let (Some(action_digest), Some(action), Some(command), Some(command_digest)) = (
            handle.action_digest.as_ref(),
            handle.action.as_ref(),
            handle.command.as_ref(),
            handle.command_digest.as_ref(),
        ) else {
            return;
        };

        let job = OwnedPublishJob {
            task_name: task_name.to_string(),
            publisher: Arc::clone(publisher),
            local_cas: Arc::clone(&handle.cache.cas),
            action: action.clone(),
            action_digest: action_digest.clone(),
            command: command.clone(),
            command_digest: command_digest.clone(),
            action_result,
            _cache_use_guard: cache_use_guard,
        };

        match self.upload_dispatch {
            UploadDispatchMode::Inline => run_remote_publish(job).await,
            UploadDispatchMode::Background => {
                let mut uploads = self.pending_uploads.lock().await;
                uploads.spawn(run_remote_publish(job));
            }
        }
    }

    fn clone_with_config(&self) -> Self {
        self.clone_with_upload_mode(self.upload_dispatch)
    }

    fn clone_with_upload_mode(&self, upload_dispatch: UploadDispatchMode) -> Self {
        // Share the sandbox store and background upload queue so one run
        // creates one store and flushes all spawned uploads together.
        Self {
            config: self.config.clone(),
            sandbox_store: self.sandbox_store.clone(),
            pending_uploads: self.pending_uploads.clone(),
            upload_dispatch,
        }
    }
}

#[derive(Clone)]
struct TaskWalkOutcome {
    result: TaskResult,
    continue_on_error: bool,
}

impl super::graph_walk::WalkOutcome for TaskWalkOutcome {
    fn is_success(&self) -> bool {
        self.result.success
    }

    fn continue_on_error(&self) -> bool {
        self.continue_on_error
    }
}

/// Where a task's project lies and what its results are keyed with.
#[derive(Clone, Copy)]
struct TaskFrame<'frame> {
    /// The project's directory: path inputs are relative to it.
    project_root: &'frame Path,
    /// Root of the project's CUE module: source files, `dir` and mapped
    /// inputs are relative to it.
    module_root: &'frame Path,
    /// The project's caching infrastructure, when it has one.
    cache: Option<&'frame TaskCacheConfig>,
    /// Whether this project is authorized to publish results to a remote cache.
    publish_authorized: bool,
}

struct PublishRemoteInput<'a> {
    task_name: &'a str,
    handle: &'a CacheHandle,
    action_result: cuenv_cas::ActionResult,
    cache_use_guard: cuenv_cas::maintenance::CacheUseGuard,
}

struct OwnedPublishJob {
    task_name: String,
    publisher: Arc<dyn cuenv_cas::ActionPublisher>,
    local_cas: Arc<dyn cuenv_cas::Cas>,
    action: cuenv_cas::Action,
    action_digest: cuenv_cas::Digest,
    command: cuenv_cas::Command,
    command_digest: cuenv_cas::Digest,
    action_result: cuenv_cas::ActionResult,
    _cache_use_guard: cuenv_cas::maintenance::CacheUseGuard,
}

async fn run_remote_publish(job: OwnedPublishJob) {
    let publish_request = cuenv_cas::PublishActionRequest {
        local_cas: job.local_cas.as_ref(),
        action: &job.action,
        action_digest: &job.action_digest,
        command: &job.command,
        command_digest: &job.command_digest,
        result: &job.action_result,
    };

    match job.publisher.publish(publish_request).await {
        Ok(()) => {
            tracing::debug!(
                task = %job.task_name,
                action = %job.action_digest,
                "published action to remote cache"
            );
            let _ = cuenv_events::emit(cuenv_events::EventCategory::Cache(
                cuenv_events::CacheEvent::RemoteUploaded {
                    task: job.task_name,
                    digest: job.action_digest.to_resource(),
                },
            ));
        }
        Err(error) => {
            tracing::warn!(
                task = %job.task_name,
                action = %job.action_digest,
                %error,
                "remote cache publication failed"
            );
            let _ = cuenv_events::emit(cuenv_events::EventCategory::Cache(
                cuenv_events::CacheEvent::RemoteUploadFailed {
                    task: job.task_name,
                    error: error.to_string(),
                },
            ));
        }
    }
}

fn resolve_task_directory(
    module_root: &Path,
    task: &Task,
    directory: &TaskDirectory,
) -> Result<PathBuf> {
    let source_base = |source: Option<&super::SourceLocation>, base_name: &str| {
        let source = source.ok_or_else(|| {
            Error::configuration(format!(
                "Task dir from '{base_name}' requires source metadata"
            ))
        })?;
        Ok::<_, Error>(
            source
                .directory()
                .map_or_else(|| module_root.to_path_buf(), |dir| module_root.join(dir)),
        )
    };
    let base = match directory.from {
        TaskDirectoryBase::Definition => source_base(task.source.as_ref(), "definition")?,
        TaskDirectoryBase::Caller => source_base(task.caller_source.as_ref(), "caller")?,
        TaskDirectoryBase::Module => module_root.to_path_buf(),
    };

    let resolved = super::workspace::normalize_join(&base, &directory.path);
    if resolved.starts_with(module_root) {
        Ok(resolved)
    } else {
        Err(Error::configuration(format!(
            "Task dir '{}' resolves outside the CUE module root '{}'",
            directory.path,
            module_root.display()
        )))
    }
}

/// A file reference is bound to the directory it names when a graph is
/// built. One that reaches the executor unbound names files nothing can
/// find, whatever the task's isolation: hashing it would leave inputs out,
/// and a task run in the workspace would read undeclared files.
fn reject_unbound_file_references(name: &str, task: &Task) -> Result<()> {
    let Some(reference) = task.inputs.iter().find_map(Input::as_files) else {
        return Ok(());
    };
    Err(Error::configuration(format!(
        "Task '{name}' reads `{} & {{paths: [...]}}`, which was not bound to the directory it \
         names; run it through `cuenv task`, `cuenv ci` or `cuenv up`, which bind file \
         references when they build the task graph",
        reference.anchor.spelling()
    )))
}

/// Refuse a task that maps an input to a path nothing will put it at.
///
/// A mapping is honoured by staging the file in the task's execution root.
/// A task that runs directly in the workspace (`hermetic: false` or
/// `hermetic.sandbox: "none"`) has none, so it would run without the file
/// while its declaration, and its cache key, say it read it.
fn reject_unmaterialized_mapping(name: &str, task: &Task) -> Result<()> {
    if task.sandbox().uses_exec_root() {
        return Ok(());
    }
    let relocated = task.inputs.iter().find_map(|input| match input {
        Input::Mapped(mapping) if mapping.relocates() => {
            Some((mapping.source.clone(), mapping.destination.clone()))
        }
        _ => None,
    });
    let Some((from, to)) = relocated else {
        return Ok(());
    };
    Err(Error::configuration(format!(
        "Task '{name}' maps the input '{from}' to '{to}', but it runs directly in the workspace \
         (hermetic: false or hermetic.sandbox: \"none\"), where nothing is staged. Read the \
         file where its task produces it and drop the mapping, or run the task under \
         hermetic.sandbox: \"dir\""
    )))
}

#[derive(Clone, Copy)]
struct TaskExecutionContext<'execution> {
    name: &'execution str,
    task: &'execution Task,
    environment: &'execution Environment,
}

/// What `execute_task` carries from the cache lookup to the record step.
///
/// The exec root is held here so it lives exactly as long as the run: the
/// guard's `Drop` removes the directory, so a task that fails or times out
/// cleans up on the way out without a separate path saying so.
struct CacheHandle {
    cache: TaskCacheConfig,
    action_digest: Option<cuenv_cas::Digest>,
    action: Option<cuenv_cas::Action>,
    command: Option<cuenv_cas::Command>,
    command_digest: Option<cuenv_cas::Digest>,
    contains_secrets: bool,
    publish_authorized: bool,
    /// Where the user expects outputs to land, which is not where a
    /// sandboxed task runs.
    workdir: PathBuf,
    /// Directory the command actually executes in.
    run_workdir: PathBuf,
    /// Present only for a task under `sandbox: "dir"`.
    exec_root: Option<super::exec_root::ExecRoot>,
    /// Stable label used to create a fresh unique root for every retry.
    root_key: String,
    /// Verified input snapshot reused to rebuild a retry root.
    inputs: Vec<cuenv_vcs::HashedInput>,
    /// Working directory relative to each execution root.
    working_directory: PathBuf,
    /// The OS/directory sandbox tier actually enforced for this run.
    effective_sandbox: super::sandbox::EffectiveSandbox,
    /// Optional pinned Nix runtime context for Linux strict sandboxing.
    nix_context: Option<super::sandbox::NixSandboxContext>,
}

#[derive(Clone, Copy)]
struct PrepareExecRootInput<'input> {
    name: &'input str,
    task: &'input Task,
    cache: &'input TaskCacheConfig,
    root_key: &'input str,
    inputs: &'input [cuenv_vcs::HashedInput],
}

#[derive(Clone, Copy)]
struct CacheHitInput<'cache> {
    name: &'cache str,
    task: &'cache Task,
    cache: &'cache TaskCacheConfig,
    action_digest: &'cache cuenv_cas::Digest,
    workdir: &'cache Path,
    cached: &'cache cuenv_cas::ActionResult,
    source: cuenv_cas::CacheSource,
}

fn acquire_cache_use(
    cache: &TaskCacheConfig,
    task: &str,
) -> Option<cuenv_cas::maintenance::CacheUseGuard> {
    match cuenv_cas::maintenance::CacheUseGuard::acquire(&cache.cache_root) {
        Ok(guard) => Some(guard),
        Err(error) => {
            tracing::warn!(task, %error, "cache transaction unavailable");
            None
        }
    }
}

fn emit_cached_output_events(name: &str, stream: &'static str, content: &str) {
    for line in content.lines() {
        cuenv_events::emit_task_output!(name, stream, line);
    }
}

fn group_concurrency_limit(global: usize, group: Option<u32>) -> usize {
    let group = group.and_then(|value| usize::try_from(value).ok());
    match (global, group) {
        (0, Some(group)) => group,
        (global, Some(group)) if group > 0 => global.min(group),
        (global, _) => global,
    }
}

fn parse_task_duration(field: &str, spec: &str) -> Result<Duration> {
    let raw = spec.trim();
    if raw.is_empty() {
        return Err(Error::configuration(format!("{field} must not be empty")));
    }

    let digits_len = raw.bytes().take_while(|byte| byte.is_ascii_digit()).count();
    if digits_len == 0 || digits_len == raw.len() {
        return Err(Error::configuration(format!(
            "invalid task {field} '{raw}': expected <int><unit> (e.g. 30m, 1h)"
        )));
    }

    let quantity: u64 = raw[..digits_len]
        .parse()
        .map_err(|e| Error::configuration(format!("invalid task {field} '{raw}': {e}")))?;
    let unit = raw[digits_len..].trim().to_ascii_lowercase();

    // Convert `quantity` units of `factor` seconds each into a Duration,
    // rejecting overflow with the field/spec context already in scope.
    let secs = |factor: u64| -> Result<Duration> {
        quantity
            .checked_mul(factor)
            .map(Duration::from_secs)
            .ok_or_else(|| Error::configuration(format!("task {field} '{raw}' is too large")))
    };

    match unit.as_str() {
        "ms" => Ok(Duration::from_millis(quantity)),
        "s" => secs(1),
        "m" => secs(60),
        "h" => secs(60 * 60),
        "d" => secs(24 * 60 * 60),
        _ => Err(Error::configuration(format!(
            "invalid task {field} unit in '{raw}': use ms|s|m|h|d"
        ))),
    }
}

#[cfg(test)]
#[path = "executor_tests.rs"]
mod tests;
