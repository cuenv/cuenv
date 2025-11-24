//! Task executor for running tasks with environment support and hermetic, input-addressed execution
//!
//! - Environment variable propagation
//! - Parallel and sequential execution
//! - Hermetic workdir populated from declared inputs (files/dirs/globs)
//! - Persistent task result cache keyed by inputs + command + env + cuenv version + platform

use super::{Task, TaskDefinition, TaskGraph, TaskGroup, Tasks};
use crate::cache::tasks as task_cache;
use crate::environment::Environment;
use crate::manifest::WorkspaceConfig;
use crate::tasks::io::{InputResolver, collect_outputs, populate_hermetic_dir};
use crate::{Error, Result};
use async_recursion::async_recursion;
use chrono::Utc;
use cuenv_workspaces::{
    BunLockfileParser, CargoLockfileParser, CargoTomlDiscovery, LockfileEntry, LockfileParser,
    NpmLockfileParser, PackageJsonDiscovery, PackageManager, PnpmLockfileParser,
    PnpmWorkspaceDiscovery, Workspace, WorkspaceDiscovery, YarnClassicLockfileParser,
    YarnModernLockfileParser, detect_from_command, detect_package_managers,
    materializer::{
        Materializer, cargo_deps::CargoMaterializer, node_modules::NodeModulesMaterializer,
    },
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::task::JoinSet;

/// Task execution result
#[derive(Debug, Clone)]
pub struct TaskResult {
    pub name: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

/// Number of lines from stdout/stderr to include when summarizing failures
pub const TASK_FAILURE_SNIPPET_LINES: usize = 20;

/// Task executor configuration
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Whether to capture output (vs streaming to stdout/stderr)
    pub capture_output: bool,
    /// Maximum parallel tasks (0 = unlimited)
    pub max_parallel: usize,
    /// Environment variables to propagate (resolved via policies)
    pub environment: Environment,
    /// Optional working directory override (for hermetic execution)
    pub working_dir: Option<PathBuf>,
    /// Project root for resolving inputs/outputs (env.cue root)
    pub project_root: PathBuf,
    /// Optional: materialize cached outputs on cache hit
    pub materialize_outputs: Option<PathBuf>,
    /// Optional: cache directory override
    pub cache_dir: Option<PathBuf>,
    /// Optional: print cache path on hits/misses
    pub show_cache_path: bool,
    /// Global workspace configuration
    pub workspaces: Option<HashMap<String, WorkspaceConfig>>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            capture_output: false,
            max_parallel: 0,
            environment: Environment::new(),
            working_dir: None,
            project_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            materialize_outputs: None,
            cache_dir: None,
            show_cache_path: false,
            workspaces: None,
        }
    }
}

/// Task executor
pub struct TaskExecutor {
    config: ExecutorConfig,
}
impl TaskExecutor {
    pub fn new(config: ExecutorConfig) -> Self {
        Self { config }
    }

    /// Execute a single task hermetically with caching
    pub async fn execute_task(&self, name: &str, task: &Task) -> Result<TaskResult> {
        // Resolve workspace dependencies if enabled
        let mut workspace_ctxs: Vec<(Workspace, Vec<LockfileEntry>)> = Vec::new();
        let mut workspace_input_patterns = Vec::new();
        let mut workspace_lockfile_hashes = BTreeMap::new();

        for workspace_name in &task.workspaces {
            if let Some(global_workspaces) = &self.config.workspaces {
                if let Some(ws_config) = global_workspaces.get(workspace_name) {
                    if ws_config.enabled {
                        let (ws, entries, paths, hash) = self
                            .resolve_workspace(name, task, workspace_name, ws_config)
                            .await?;
                        workspace_ctxs.push((ws, entries));
                        workspace_input_patterns.extend(paths);
                        if let Some(h) = hash {
                            workspace_lockfile_hashes.insert(workspace_name.clone(), h);
                        }
                    }
                } else {
                    tracing::warn!(
                        task = %name,
                        workspace = %workspace_name,
                        "Workspace not found in global configuration"
                    );
                }
            }
        }

        // Resolve inputs relative to the workspace root (if any) while keeping
        // project-scoped inputs anchored to the original project path.
        // Use the first workspace as the primary root if multiple are present,
        // or fallback to project root. This might need refinement for multi-workspace tasks.
        let primary_workspace_root = workspace_ctxs.first().map(|(ws, _)| ws.root.clone());

        let project_prefix = primary_workspace_root
            .as_ref()
            .and_then(|root| self.config.project_root.strip_prefix(root).ok())
            .map(|p| p.to_path_buf());
        let input_root = primary_workspace_root
            .clone()
            .unwrap_or_else(|| self.config.project_root.clone());

        let span_inputs = tracing::info_span!("inputs.resolve", task = %name);
        let resolved_inputs = {
            let _g = span_inputs.enter();
            let resolver = InputResolver::new(&input_root);
            let mut all_inputs: Vec<String> = Vec::new();

            if let Some(prefix) = project_prefix.as_ref() {
                for input in &task.inputs {
                    all_inputs.push(prefix.join(input).to_string_lossy().to_string());
                }
            } else {
                all_inputs.extend(task.inputs.iter().cloned());
            }

            all_inputs.extend(workspace_input_patterns.iter().cloned());
            resolver.resolve(&all_inputs)?
        };
        if task_trace_enabled() {
            tracing::info!(
                task = %name,
                input_root = %input_root.display(),
                project_root = %self.config.project_root.display(),
                inputs_count = resolved_inputs.files.len(),
                workspace_inputs = workspace_input_patterns.len(),
                "Resolved task inputs"
            );
        }

        // Build cache key envelope
        let inputs_summary: BTreeMap<String, String> = resolved_inputs.to_summary_map();
        // Ensure deterministic order is already guaranteed by BTreeMap
        let env_summary: BTreeMap<String, String> = self
            .config
            .environment
            .vars
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let cuenv_version = env!("CARGO_PKG_VERSION").to_string();
        let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        let shell_json = serde_json::to_value(&task.shell).ok();
        let workspace_lockfile_hashes_opt = if workspace_lockfile_hashes.is_empty() {
            None
        } else {
            Some(workspace_lockfile_hashes)
        };

        let envelope = task_cache::CacheKeyEnvelope {
            inputs: inputs_summary.clone(),
            command: task.command.clone(),
            args: task.args.clone(),
            shell: shell_json,
            env: env_summary.clone(),
            cuenv_version: cuenv_version.clone(),
            platform: platform.clone(),
            workspace_lockfile_hashes: workspace_lockfile_hashes_opt,
            // Package hashes are implicitly included in inputs_summary because we added member paths to inputs
            workspace_package_hashes: None,
        };
        let (cache_key, envelope_json) = task_cache::compute_cache_key(&envelope)?;

        // Cache lookup
        let span_cache = tracing::info_span!("cache.lookup", task = %name, key = %cache_key);
        let cache_hit = {
            let _g = span_cache.enter();
            task_cache::lookup(&cache_key, self.config.cache_dir.as_deref())
        };

        if let Some(hit) = cache_hit {
            tracing::info!(
                task = %name,
                key = %cache_key,
                path = %hit.path.display(),
                "Task {} cache hit: {}. Skipping execution.",
                name,
                cache_key
            );
            if self.config.show_cache_path {
                tracing::info!(cache_path = %hit.path.display(), "Cache path");
            }
            if let Some(dest) = &self.config.materialize_outputs {
                let count = task_cache::materialize_outputs(
                    &cache_key,
                    dest,
                    self.config.cache_dir.as_deref(),
                )?;
                tracing::info!(materialized = count, dest = %dest.display(), "Materialized cached outputs");
            }
            // On cache hit, surface cached logs so behavior matches a fresh
            // execution from the caller's perspective. We return them in the
            // TaskResult even if `capture_output` is false, allowing callers
            // (like the CLI) to print cached logs explicitly.
            let stdout_path = hit.path.join("logs").join("stdout.log");
            let stderr_path = hit.path.join("logs").join("stderr.log");
            let stdout = std::fs::read_to_string(&stdout_path).unwrap_or_default();
            let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
            // If logs are present, we can return immediately. If logs are
            // missing (older cache format), fall through to execute to
            // backfill logs for future hits.
            if !(stdout.is_empty() && stderr.is_empty()) {
                if !self.config.capture_output {
                    let cmd_str = if task.command.is_empty() {
                        task.args.join(" ")
                    } else {
                        format!("{} {}", task.command, task.args.join(" "))
                    };
                    eprintln!("> [{name}] {cmd_str} (cached)");
                }
                return Ok(TaskResult {
                    name: name.to_string(),
                    exit_code: Some(0),
                    stdout,
                    stderr,
                    success: true,
                });
            } else {
                tracing::info!(
                    task = %name,
                    key = %cache_key,
                    "Cache entry lacks logs; executing to backfill logs"
                );
            }
        }

        tracing::info!(
            task = %name,
            key = %cache_key,
            "Task {} executing hermetically… key {}",
            name,
            cache_key
        );

        let hermetic_root = create_hermetic_dir(name, &cache_key)?;
        if self.config.show_cache_path {
            tracing::info!(hermetic_root = %hermetic_root.display(), "Hermetic working directory");
        }

        // Seed working directory with inputs
        let span_populate =
            tracing::info_span!("inputs.populate", files = resolved_inputs.files.len());
        {
            let _g = span_populate.enter();
            populate_hermetic_dir(&resolved_inputs, &hermetic_root)?;
        }

        // Materialize workspace artifacts (node_modules, target)
        for (ws, entries) in workspace_ctxs {
            self.materialize_workspace(&ws, &entries, &hermetic_root)
                .await?;
        }

        // Initial snapshot to detect undeclared writes
        let initial_hashes: BTreeMap<String, String> = inputs_summary.clone();

        // Build command
        let mut cmd = if let Some(shell) = &task.shell {
            if shell.command.is_some() && shell.flag.is_some() {
                let shell_command = shell.command.as_ref().unwrap();
                let shell_flag = shell.flag.as_ref().unwrap();
                let mut cmd = Command::new(shell_command);
                cmd.arg(shell_flag);
                if task.args.is_empty() {
                    cmd.arg(&task.command);
                } else {
                    let full_command = if task.command.is_empty() {
                        task.args.join(" ")
                    } else {
                        format!("{} {}", task.command, task.args.join(" "))
                    };
                    cmd.arg(full_command);
                }
                cmd
            } else {
                let mut cmd = Command::new(&task.command);
                for arg in &task.args {
                    cmd.arg(arg);
                }
                cmd
            }
        } else {
            let mut cmd = Command::new(&task.command);
            for arg in &task.args {
                cmd.arg(arg);
            }
            cmd
        };

        let workdir = if let Some(dir) = &self.config.working_dir {
            dir.clone()
        } else if let Some(prefix) = project_prefix.as_ref() {
            hermetic_root.join(prefix)
        } else {
            hermetic_root.clone()
        };
        let _ = std::fs::create_dir_all(&workdir);
        cmd.current_dir(&workdir);
        // Set environment variables (resolved + system), set CWD
        let env_vars = self.config.environment.merge_with_system();
        if task_trace_enabled() {
            tracing::info!(
                task = %name,
                hermetic_root = %hermetic_root.display(),
                workdir = %workdir.display(),
                command = %task.command,
                args = ?task.args,
                env_count = env_vars.len(),
                "Launching task command"
            );
        }
        for (k, v) in env_vars {
            cmd.env(k, v);
        }

        // Configure output: always capture to ensure consistent behavior on
        // cache hits and allow callers to decide what to print.
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let stream_logs = !self.config.capture_output;
        if stream_logs {
            let cmd_str = if task.command.is_empty() {
                task.args.join(" ")
            } else {
                format!("{} {}", task.command, task.args.join(" "))
            };
            eprintln!("> [{name}] {cmd_str}");
        }

        let start = std::time::Instant::now();
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::configuration(format!("Failed to spawn task '{}': {}", name, e)))?;

        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        let stdout_task = async move {
            if let Some(stdout) = stdout_handle {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                let mut stdout_lines = Vec::new();
                while let Ok(Some(line)) = lines.next_line().await {
                    if stream_logs {
                        println!("{line}");
                    }
                    stdout_lines.push(line);
                }
                stdout_lines.join("\n")
            } else {
                String::new()
            }
        };

        let stderr_task = async move {
            if let Some(stderr) = stderr_handle {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                let mut stderr_lines = Vec::new();
                while let Ok(Some(line)) = lines.next_line().await {
                    if stream_logs {
                        eprintln!("{line}");
                    }
                    stderr_lines.push(line);
                }
                stderr_lines.join("\n")
            } else {
                String::new()
            }
        };

        let (stdout, stderr) = tokio::join!(stdout_task, stderr_task);

        let status = child.wait().await.map_err(|e| {
            Error::configuration(format!("Failed to wait for task '{}': {}", name, e))
        })?;
        let duration = start.elapsed();

        let exit_code = status.code().unwrap_or(1);
        let success = status.success();
        if !success {
            tracing::warn!(task = %name, exit = exit_code, "Task failed");
            tracing::error!(task = %name, "Task stdout:\n{}", stdout);
            tracing::error!(task = %name, "Task stderr:\n{}", stderr);
        } else {
            tracing::info!(task = %name, "Task completed successfully");
        }

        // Collect declared outputs and warn on undeclared writes
        let output_patterns: Vec<String> = if let Some(prefix) = project_prefix.as_ref() {
            task.outputs
                .iter()
                .map(|o| prefix.join(o).to_string_lossy().to_string())
                .collect()
        } else {
            task.outputs.clone()
        };
        let outputs = collect_outputs(&hermetic_root, &output_patterns)?;
        let outputs_set: HashSet<PathBuf> = outputs.iter().cloned().collect();
        let mut output_index: Vec<task_cache::OutputIndexEntry> = Vec::new();

        // Stage outputs into a temp dir for cache persistence
        let outputs_stage = std::env::temp_dir().join(format!("cuenv-outputs-{}", cache_key));
        if outputs_stage.exists() {
            let _ = std::fs::remove_dir_all(&outputs_stage);
        }
        std::fs::create_dir_all(&outputs_stage).ok();

        for rel in &outputs {
            let rel_for_project = project_prefix
                .as_ref()
                .and_then(|prefix| rel.strip_prefix(prefix).ok())
                .unwrap_or(rel)
                .to_path_buf();
            let src = hermetic_root.join(rel);
            // Intentionally avoid `let`-chains here to preserve readability
            // and to align with review guidance; allow clippy's collapsible-if.
            #[allow(clippy::collapsible_if)]
            if let Ok(meta) = std::fs::metadata(&src) {
                if meta.is_file() {
                    let dst = outputs_stage.join(&rel_for_project);
                    if let Some(parent) = dst.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::copy(&src, &dst);
                    let (sha, _size) = crate::tasks::io::sha256_file(&src).unwrap_or_default();
                    output_index.push(task_cache::OutputIndexEntry {
                        rel_path: rel_for_project.to_string_lossy().to_string(),
                        size: meta.len(),
                        sha256: sha,
                    });
                }
            }
        }

        // Detect undeclared writes
        let mut warned = false;
        for entry in walkdir::WalkDir::new(&hermetic_root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if p.is_dir() {
                continue;
            }
            let rel = match p.strip_prefix(&hermetic_root) {
                Ok(r) => r.to_path_buf(),
                Err(_) => continue,
            };
            let rel_str = rel.to_string_lossy().to_string();
            let (sha, _size) = crate::tasks::io::sha256_file(p).unwrap_or_default();
            let initial = initial_hashes.get(&rel_str);
            let changed = match initial {
                None => true,
                Some(prev) => prev != &sha,
            };
            if changed && !outputs_set.contains(&rel) {
                if !warned {
                    tracing::warn!(task = %name, "Detected writes to undeclared paths; these are not cached as outputs");
                    warned = true;
                }
                tracing::debug!(path = %rel_str, "Undeclared write");
            }
        }

        // Persist cache entry on success
        if success {
            let meta = task_cache::TaskResultMeta {
                task_name: name.to_string(),
                command: task.command.clone(),
                args: task.args.clone(),
                env_summary,
                inputs_summary: inputs_summary.clone(),
                created_at: Utc::now(),
                cuenv_version,
                platform,
                duration_ms: duration.as_millis(),
                exit_code,
                cache_key_envelope: envelope_json.clone(),
                output_index,
            };
            let logs = task_cache::TaskLogs {
                // Persist logs regardless of capture setting so cache hits can
                // reproduce output faithfully.
                stdout: Some(stdout.clone()),
                stderr: Some(stderr.clone()),
            };
            let cache_span = tracing::info_span!("cache.save", key = %cache_key);
            {
                let _g = cache_span.enter();
                task_cache::save_result(
                    &cache_key,
                    &meta,
                    &outputs_stage,
                    &hermetic_root,
                    logs,
                    self.config.cache_dir.as_deref(),
                )?;
            }

            // Materialize outputs back to project root
            for rel in &outputs {
                let rel_for_project = project_prefix
                    .as_ref()
                    .and_then(|prefix| rel.strip_prefix(prefix).ok())
                    .unwrap_or(rel)
                    .to_path_buf();
                let src = hermetic_root.join(rel);
                let dst = self.config.project_root.join(&rel_for_project);
                if src.exists() {
                    if let Some(parent) = dst.parent() {
                        std::fs::create_dir_all(parent).ok();
                    }
                    // We use copy instead of rename to keep hermetic dir intact for snapshots/logs if needed,
                    // although it's temporary.
                    if let Err(e) = std::fs::copy(&src, &dst) {
                        tracing::warn!(
                            "Failed to copy output {} back to project root: {}",
                            rel.display(),
                            e
                        );
                    }
                }
            }
        } else {
            // Optionally persist logs in a failure/ subdir: not implemented for brevity
        }

        Ok(TaskResult {
            name: name.to_string(),
            exit_code: Some(exit_code),
            stdout,
            stderr,
            success,
        })
    }

    /// Execute a task definition (single task or group)
    #[async_recursion]
    pub async fn execute_definition(
        &self,
        name: &str,
        definition: &TaskDefinition,
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        match definition {
            TaskDefinition::Single(task) => {
                let result = self.execute_task(name, task.as_ref()).await?;
                Ok(vec![result])
            }
            TaskDefinition::Group(group) => self.execute_group(name, group, all_tasks).await,
        }
    }

    async fn execute_group(
        &self,
        prefix: &str,
        group: &TaskGroup,
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        match group {
            TaskGroup::Sequential(tasks) => self.execute_sequential(prefix, tasks, all_tasks).await,
            TaskGroup::Parallel(tasks) => self.execute_parallel(prefix, tasks, all_tasks).await,
        }
    }

    async fn execute_sequential(
        &self,
        prefix: &str,
        tasks: &[TaskDefinition],
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        if !self.config.capture_output {
            eprintln!("> Running sequential group: {prefix}");
        }
        let mut results = Vec::new();
        for (i, task_def) in tasks.iter().enumerate() {
            let task_name = format!("{}[{}]", prefix, i);
            let task_results = self
                .execute_definition(&task_name, task_def, all_tasks)
                .await?;
            for result in &task_results {
                if !result.success {
                    let message = format!(
                        "Sequential task group '{prefix}' halted.\n\n{}",
                        summarize_task_failure(result, TASK_FAILURE_SNIPPET_LINES)
                    );
                    return Err(Error::configuration(message));
                }
            }
            results.extend(task_results);
        }
        Ok(results)
    }

    async fn execute_parallel(
        &self,
        prefix: &str,
        tasks: &HashMap<String, TaskDefinition>,
        all_tasks: &Tasks,
    ) -> Result<Vec<TaskResult>> {
        // Check for "default" task to override parallel execution
        if let Some(default_task) = tasks.get("default") {
            if !self.config.capture_output {
                eprintln!("> Executing default task for group '{prefix}'");
            }
            // Execute only the default task, using the group prefix directly
            // since "default" is implicit when invoking the group name
            let task_name = format!("{}.default", prefix);
            return self
                .execute_definition(&task_name, default_task, all_tasks)
                .await;
        }

        if !self.config.capture_output {
            eprintln!(
                "> Running parallel group: {prefix} (max_parallel={})",
                self.config.max_parallel
            );
        }
        let mut join_set = JoinSet::new();
        let all_tasks = Arc::new(all_tasks.clone());
        let mut all_results = Vec::new();
        let mut merge_results = |results: Vec<TaskResult>| -> Result<()> {
            if let Some(failed) = results.iter().find(|r| !r.success) {
                let message = format!(
                    "Parallel task group '{prefix}' halted.\n\n{}",
                    summarize_task_failure(failed, TASK_FAILURE_SNIPPET_LINES)
                );
                return Err(Error::configuration(message));
            }
            all_results.extend(results);
            Ok(())
        };
        for (name, task_def) in tasks {
            let task_name = format!("{}.{}", prefix, name);
            let task_def = task_def.clone();
            let all_tasks = Arc::clone(&all_tasks);
            let executor = self.clone_with_config();
            join_set.spawn(async move {
                executor
                    .execute_definition(&task_name, &task_def, &all_tasks)
                    .await
            });
            if self.config.max_parallel > 0
                && join_set.len() >= self.config.max_parallel
                && let Some(result) = join_set.join_next().await
            {
                match result {
                    Ok(Ok(results)) => merge_results(results)?,
                    Ok(Err(e)) => return Err(e),
                    Err(e) => {
                        return Err(Error::configuration(format!(
                            "Task execution panicked: {}",
                            e
                        )));
                    }
                }
            }
        }
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok(results)) => merge_results(results)?,
                Ok(Err(e)) => return Err(e),
                Err(e) => {
                    return Err(Error::configuration(format!(
                        "Task execution panicked: {}",
                        e
                    )));
                }
            }
        }
        Ok(all_results)
    }

    pub async fn execute_graph(&self, graph: &TaskGraph) -> Result<Vec<TaskResult>> {
        let parallel_groups = graph.get_parallel_groups()?;
        let mut all_results = Vec::new();
        let mut join_set = JoinSet::new();
        let mut group_iter = parallel_groups.into_iter();
        let mut current_group = group_iter.next();
        while current_group.is_some() || !join_set.is_empty() {
            if let Some(group) = current_group.as_mut() {
                while let Some(node) = group.pop() {
                    let task = node.task.clone();
                    let name = node.name.clone();
                    let executor = self.clone_with_config();
                    join_set.spawn(async move { executor.execute_task(&name, &task).await });
                    if self.config.max_parallel > 0 && join_set.len() >= self.config.max_parallel {
                        break;
                    }
                }
                if group.is_empty() {
                    current_group = group_iter.next();
                }
            }
            if let Some(result) = join_set.join_next().await {
                match result {
                    Ok(Ok(task_result)) => {
                        if !task_result.success {
                            let message = format!(
                                "Task graph execution halted.\n\n{}",
                                summarize_task_failure(&task_result, TASK_FAILURE_SNIPPET_LINES)
                            );
                            return Err(Error::configuration(message));
                        }
                        all_results.push(task_result);
                    }
                    Ok(Err(e)) => return Err(e),
                    Err(e) => {
                        return Err(Error::configuration(format!(
                            "Task execution panicked: {}",
                            e
                        )));
                    }
                }
            }
        }
        Ok(all_results)
    }

    async fn resolve_workspace(
        &self,
        _task_name: &str,
        task: &Task,
        workspace_name: &str,
        config: &WorkspaceConfig,
    ) -> Result<(Workspace, Vec<LockfileEntry>, Vec<String>, Option<String>)> {
        let root = self.config.project_root.clone();
        let task_label = _task_name.to_string();
        let command = task.command.clone();
        let config_pm = config.package_manager.clone();
        let config_root = config.root.clone();
        // WorkspaceConfig doesn't support 'packages' filtering yet, so assume full workspace deps for now,
        // or imply it from context. If we want package filtering, we need to add it to WorkspaceConfig
        // or assume all deps.
        // However, the previous logic used `packages` from WorkspaceInputs.
        // For now, let's assume traversing all dependencies if not specified.
        // But wait, `WorkspaceConfig` in schema doesn't have `packages`.
        // So we probably default to "all relevant" or auto-infer from current directory if we are inside a package.
        let mut packages = Vec::new();

        // If we are in a subdirectory that matches a workspace member, we might want to infer that package.
        // The previous logic did that if `packages` was empty.

        let lockfile_override: Option<String> = None; // WorkspaceConfig doesn't have lockfile override yet.
        // If we need it, we should add it to WorkspaceConfig. For now assume standard discovery.

        let mut traverse_workspace_deps = true;

        // Use spawn_blocking for heavy lifting
        let workspace_name_owned = workspace_name.to_string();
        tokio::task::spawn_blocking(move || {
            let override_for_detection = lockfile_override.as_ref().map(|lock| {
                let candidate = PathBuf::from(lock);
                if candidate.is_absolute() {
                    candidate
                } else {
                    root.join(lock)
                }
            });

            // 1. Detect Package Manager
            // Priority: 1. explicit config, 2. workspace name (if it matches a PM), 3. auto-detect
            let manager = if let Some(pm_str) = config_pm {
                match pm_str.as_str() {
                    "npm" => PackageManager::Npm,
                    "bun" => PackageManager::Bun,
                    "pnpm" => PackageManager::Pnpm,
                    "yarn" => PackageManager::YarnModern,
                    "yarn-classic" => PackageManager::YarnClassic,
                    "cargo" => PackageManager::Cargo,
                    _ => {
                        return Err(Error::configuration(format!(
                            "Unknown package manager: {}",
                            pm_str
                        )));
                    }
                }
            } else {
                // Try to match workspace name to package manager
                match workspace_name_owned.as_str() {
                    "npm" => PackageManager::Npm,
                    "bun" => PackageManager::Bun,
                    "pnpm" => PackageManager::Pnpm,
                    "yarn" => PackageManager::YarnModern,
                    "cargo" => PackageManager::Cargo,
                    _ => {
                         // Auto-detect if name doesn't match
                        // Priority: command hint -> lockfiles
                        let hint = detect_from_command(&command);
                        let detected = match detect_package_managers(&root) {
                            Ok(list) => list,
                            Err(e) => {
                                if override_for_detection.is_some() {
                                    Vec::new()
                                } else {
                                    return Err(Error::configuration(format!(
                                        "Failed to detect package managers: {}",
                                        e
                                    )));
                                }
                            }
                        };

                        if let Some(h) = hint {
                            if detected.contains(&h) {
                                h
                            } else if !detected.is_empty() {
                                // Prefer detected files
                                detected[0]
                            } else if let Some(ref override_path) = override_for_detection {
                                infer_manager_from_lockfile(override_path).ok_or_else(|| {
                                    Error::configuration(
                                        "Unable to infer package manager from lockfile override",
                                    )
                                })?
                            } else {
                                return Err(Error::configuration(
                                    format!("No package manager specified for workspace '{}' and could not detect one", workspace_name_owned),
                                ));
                            }
                        } else if !detected.is_empty() {
                            detected[0]
                        } else if let Some(ref override_path) = override_for_detection {
                            infer_manager_from_lockfile(override_path).ok_or_else(|| {
                                Error::configuration(
                                    "Unable to infer package manager from lockfile override",
                                )
                            })?
                        } else {
                            return Err(Error::configuration(
                                "Could not detect package manager for workspace resolution",
                            ));
                        }
                    }
                }
            };

            // Resolve the actual workspace root by walking up until we find a
            // directory that declares a workspace for this package manager.
            // If config.root is set, use that.
            let workspace_root = if let Some(root_override) = &config_root {
                root.join(root_override)
            } else {
                find_workspace_root(manager, &root)
            };

            if task_trace_enabled() {
                tracing::info!(
                    task = %task_label,
                    manager = %manager,
                    project_root = %root.display(),
                    workspace_root = %workspace_root.display(),
                    "Resolved workspace root for package manager"
                );
            }

            let lockfile_override_path = lockfile_override.as_ref().map(|lock| {
                let candidate = PathBuf::from(lock);
                if candidate.is_absolute() {
                    candidate
                } else {
                    workspace_root.join(lock)
                }
            });

            // 2. Discover Workspace
            let discovery: Box<dyn WorkspaceDiscovery> = match manager {
                PackageManager::Npm
                | PackageManager::Bun
                | PackageManager::YarnClassic
                | PackageManager::YarnModern => Box::new(PackageJsonDiscovery),
                PackageManager::Pnpm => Box::new(PnpmWorkspaceDiscovery),
                PackageManager::Cargo => Box::new(CargoTomlDiscovery),
            };

            let workspace = discovery.discover(&workspace_root).map_err(|e| {
                Error::configuration(format!("Failed to discover workspace: {}", e))
            })?;

            // 3. Parse Lockfile
            let lockfile_path = if let Some(path) = lockfile_override_path {
                if !path.exists() {
                    return Err(Error::configuration(format!(
                        "Workspace lockfile override does not exist: {}",
                        path.display()
                    )));
                }
                path
            } else {
                workspace.lockfile.clone().ok_or_else(|| {
                    Error::configuration("Workspace resolution requires a lockfile")
                })?
            };

            let parser: Box<dyn LockfileParser> = match manager {
                PackageManager::Npm => Box::new(NpmLockfileParser),
                PackageManager::Bun => Box::new(BunLockfileParser),
                PackageManager::Pnpm => Box::new(PnpmLockfileParser),
                PackageManager::YarnClassic => Box::new(YarnClassicLockfileParser),
                PackageManager::YarnModern => Box::new(YarnModernLockfileParser),
                PackageManager::Cargo => Box::new(CargoLockfileParser),
            };

            let entries = parser
                .parse(&lockfile_path)
                .map_err(|e| Error::configuration(format!("Failed to parse lockfile: {}", e)))?;
            if task_trace_enabled() {
                tracing::info!(
                    task = %task_label,
                    lockfile = %lockfile_path.display(),
                    members = entries.len(),
                    "Parsed workspace lockfile"
                );
            }

            // Compute lockfile hash
            let (hash, _) = crate::tasks::io::sha256_file(&lockfile_path)?;

            // Infer packages when none explicitly provided by scoping to the
            // current workspace member. (We intentionally avoid pulling all
            // transitive deps here to keep hashing fast for large monorepos.)
            if packages.is_empty() {
                let current_member = workspace
                    .members
                    .iter()
                    .find(|m| workspace_root.join(&m.path) == root || root.starts_with(workspace_root.join(&m.path)));
                if let Some(member) = current_member {
                    let inferred = vec![member.name.clone()];
                    if task_trace_enabled() {
                        tracing::info!(
                            task = %task_label,
                            inferred_packages = ?inferred,
                            "Inferred workspace packages from current project"
                        );
                    }
                    packages = inferred;
                    traverse_workspace_deps = true;
                }
            }

            // 4. Collect Inputs
            let mut member_paths = Vec::new();

            // Always include workspace configuration files
            member_paths.push(manager.workspace_config_name().to_string());
            if let Ok(rel) = lockfile_path.strip_prefix(&workspace_root) {
                member_paths.push(rel.to_string_lossy().to_string());
            } else {
                member_paths.push(lockfile_path.to_string_lossy().to_string());
            }

            if packages.is_empty() {
                for member in &workspace.members {
                    let manifest_rel = member
                        .path
                        .join(manager.workspace_config_name());
                    member_paths.push(manifest_rel.to_string_lossy().to_string());
                }
            } else {
                let mut to_visit: Vec<String> = packages.clone();
                let mut visited = HashSet::new();

                while let Some(pkg_name) = to_visit.pop() {
                    if visited.contains(&pkg_name) {
                        continue;
                    }
                    visited.insert(pkg_name.clone());

                    if let Some(member) = workspace.find_member(&pkg_name) {
                        let manifest_rel = member
                            .path
                            .join(manager.workspace_config_name());
                        member_paths.push(manifest_rel.to_string_lossy().to_string());

                        // Add dependencies when explicitly requested
                        if traverse_workspace_deps {
                            let mut dependency_candidates: HashSet<String> = HashSet::new();

                            if let Some(entry) = entries.iter().find(|e| e.name == pkg_name) {
                                for dep in &entry.dependencies {
                                    if entries
                                        .iter()
                                        .any(|e| e.name == dep.name && e.is_workspace_member)
                                    {
                                        dependency_candidates.insert(dep.name.clone());
                                    }
                                }
                            }

                            for dep_name in &member.dependencies {
                                if workspace.find_member(dep_name).is_some() {
                                    dependency_candidates.insert(dep_name.clone());
                                }
                            }

                            for dep_name in dependency_candidates {
                                to_visit.push(dep_name);
                            }
                        }
                    }
                }
            }

            if task_trace_enabled() {
                tracing::info!(
                    task = %task_label,
                    members = ?member_paths,
                    "Workspace input member paths selected"
                );
            }

            Ok((workspace, entries, member_paths, Some(hash)))
        })
        .await
        .map_err(|e| Error::configuration(format!("Task execution panicked: {}", e)))?
    }

    async fn materialize_workspace(
        &self,
        workspace: &Workspace,
        entries: &[LockfileEntry],
        target_dir: &Path,
    ) -> Result<()> {
        // Dispatch to appropriate materializer
        let materializer: Box<dyn Materializer> = match workspace.manager {
            PackageManager::Npm
            | PackageManager::Bun
            | PackageManager::Pnpm
            | PackageManager::YarnClassic
            | PackageManager::YarnModern => Box::new(NodeModulesMaterializer),
            PackageManager::Cargo => Box::new(CargoMaterializer),
        };

        materializer
            .materialize(workspace, entries, target_dir)
            .map_err(|e| Error::configuration(format!("Materialization failed: {}", e)))
    }

    fn clone_with_config(&self) -> Self {
        Self {
            config: self.config.clone(),
        }
    }
}

fn find_workspace_root(manager: PackageManager, start: &Path) -> PathBuf {
    let mut current = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());

    loop {
        let is_root = match manager {
            PackageManager::Npm
            | PackageManager::Bun
            | PackageManager::YarnClassic
            | PackageManager::YarnModern => package_json_has_workspaces(&current),
            PackageManager::Pnpm => current.join("pnpm-workspace.yaml").exists(),
            PackageManager::Cargo => cargo_toml_has_workspace(&current),
        };

        if is_root {
            return current;
        }

        if let Some(parent) = current.parent() {
            current = parent.to_path_buf();
        } else {
            return start.to_path_buf();
        }
    }
}

fn package_json_has_workspaces(dir: &Path) -> bool {
    let path = dir.join("package.json");
    let content = std::fs::read_to_string(&path);
    let Ok(json) = content.and_then(|s| {
        serde_json::from_str::<serde_json::Value>(&s)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }) else {
        return false;
    };

    match json.get("workspaces") {
        Some(serde_json::Value::Array(arr)) => !arr.is_empty(),
        Some(serde_json::Value::Object(map)) => map
            .get("packages")
            .and_then(|packages| packages.as_array())
            .map(|arr| !arr.is_empty())
            .unwrap_or(false),
        _ => false,
    }
}

fn cargo_toml_has_workspace(dir: &Path) -> bool {
    let path = dir.join("Cargo.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };

    content.contains("[workspace]")
}

fn task_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        matches!(
            std::env::var("CUENV_TRACE_TASKS")
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Build a compact, user-friendly summary for a failed task, including the
/// exit code and the tail of stdout/stderr to help with diagnostics.
pub fn summarize_task_failure(result: &TaskResult, max_output_lines: usize) -> String {
    let exit_code = result
        .exit_code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let mut sections = Vec::new();
    sections.push(format!(
        "Task '{}' failed with exit code {}.",
        result.name, exit_code
    ));

    let output = format_failure_streams(result, max_output_lines);
    if output.is_empty() {
        sections.push(
            "No stdout/stderr were captured; rerun with RUST_LOG=debug to stream task logs."
                .to_string(),
        );
    } else {
        sections.push(output);
    }

    sections.join("\n\n")
}

fn format_failure_streams(result: &TaskResult, max_output_lines: usize) -> String {
    let mut streams = Vec::new();

    if let Some(stdout) = summarize_stream("stdout", &result.stdout, max_output_lines) {
        streams.push(stdout);
    }

    if let Some(stderr) = summarize_stream("stderr", &result.stderr, max_output_lines) {
        streams.push(stderr);
    }

    streams.join("\n\n")
}

fn summarize_stream(label: &str, content: &str, max_output_lines: usize) -> Option<String> {
    let normalized = content.trim_end();
    if normalized.is_empty() {
        return None;
    }

    let lines: Vec<&str> = normalized.lines().collect();
    let total = lines.len();
    let start = total.saturating_sub(max_output_lines);
    let snippet = lines[start..].join("\n");

    let header = if total > max_output_lines {
        format!("{label} (last {max_output_lines} of {total} lines):")
    } else {
        format!("{label}:")
    };

    Some(format!("{header}\n{snippet}"))
}

fn infer_manager_from_lockfile(path: &Path) -> Option<PackageManager> {
    match path.file_name().and_then(|n| n.to_str())? {
        "package-lock.json" => Some(PackageManager::Npm),
        "bun.lock" => Some(PackageManager::Bun),
        "pnpm-lock.yaml" => Some(PackageManager::Pnpm),
        "yarn.lock" => Some(PackageManager::YarnModern),
        "Cargo.lock" => Some(PackageManager::Cargo),
        _ => None,
    }
}

fn create_hermetic_dir(task_name: &str, key: &str) -> Result<PathBuf> {
    // Use OS temp dir; name scoped by task and cache key prefix.
    // IMPORTANT: Ensure the workdir is clean on every run to preserve hermeticity.
    let sanitized_task = task_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();

    let base = std::env::temp_dir().join(format!(
        "cuenv-work-{}-{}",
        sanitized_task,
        &key[..12.min(key.len())]
    ));

    // If a directory from a previous run exists, remove it before reuse.
    // This avoids contamination from artifacts left by failed runs where no cache was saved.
    if base.exists()
        && let Err(e) = std::fs::remove_dir_all(&base)
    {
        // If we cannot remove the previous directory (e.g. in-use on Windows),
        // fall back to a unique, fresh directory to maintain hermetic execution.
        let ts = Utc::now().format("%Y%m%d%H%M%S%3f");
        let fallback = std::env::temp_dir().join(format!(
            "cuenv-work-{}-{}-{}",
            sanitized_task,
            &key[..12.min(key.len())],
            ts
        ));
        tracing::warn!(
            previous = %base.display(),
            fallback = %fallback.display(),
            error = %e,
            "Failed to clean previous hermetic workdir; using fresh fallback directory"
        );
        std::fs::create_dir_all(&fallback).map_err(|e| Error::Io {
            source: e,
            path: Some(fallback.clone().into()),
            operation: "create_dir_all".into(),
        })?;
        return Ok(fallback);
    }

    std::fs::create_dir_all(&base).map_err(|e| Error::Io {
        source: e,
        path: Some(base.clone().into()),
        operation: "create_dir_all".into(),
    })?;
    Ok(base)
}

/// Execute an arbitrary command with the cuenv environment
pub async fn execute_command(
    command: &str,
    args: &[String],
    environment: &Environment,
) -> Result<i32> {
    tracing::info!("Executing command: {} {:?}", command, args);
    let mut cmd = Command::new(command);
    cmd.args(args);
    let env_vars = environment.merge_with_system();
    for (key, value) in env_vars {
        cmd.env(key, value);
    }
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    cmd.stdin(Stdio::inherit());
    let status = cmd.status().await.map_err(|e| {
        Error::configuration(format!("Failed to execute command '{}': {}", command, e))
    })?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_executor_config_default() {
        let config = ExecutorConfig::default();
        assert!(!config.capture_output);
        assert_eq!(config.max_parallel, 0);
        assert!(config.environment.is_empty());
    }

    #[tokio::test]
    async fn test_task_result() {
        let result = TaskResult {
            name: "test".to_string(),
            exit_code: Some(0),
            stdout: "output".to_string(),
            stderr: String::new(),
            success: true,
        };
        assert_eq!(result.name, "test");
        assert_eq!(result.exit_code, Some(0));
        assert!(result.success);
        assert_eq!(result.stdout, "output");
    }

    #[tokio::test]
    async fn test_execute_simple_task() {
        let config = ExecutorConfig {
            capture_output: true,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let task = Task {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            shell: None,
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspaces: vec![],
            description: Some("Hello task".to_string()),
        };
        let result = executor.execute_task("test", &task).await.unwrap();
        assert!(result.success);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn test_execute_with_environment() {
        let mut config = ExecutorConfig {
            capture_output: true,
            ..Default::default()
        };
        config
            .environment
            .set("TEST_VAR".to_string(), "test_value".to_string());
        let executor = TaskExecutor::new(config);
        let task = Task {
            command: "printenv".to_string(),
            args: vec!["TEST_VAR".to_string()],
            shell: None,
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspaces: vec![],
            description: Some("Print env task".to_string()),
        };
        let result = executor.execute_task("test", &task).await.unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("test_value"));
    }

    #[tokio::test]
    async fn test_workspace_inputs_include_workspace_root_when_project_is_nested() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Workspace root with workspaces + lockfile
        fs::write(
            root.join("package.json"),
            r#"{
  "name": "root-app",
  "version": "0.0.0",
  "workspaces": ["packages/*", "apps/*"],
  "dependencies": {
    "@rawkodeacademy/content-technologies": "workspace:*"
  }
}"#,
        )
        .unwrap();
        // Deliberately omit the workspace member name for apps/site to mimic lockfiles
        // that only record member paths, ensuring we can still discover dependencies.
        fs::write(
            root.join("bun.lock"),
            r#"{
  "lockfileVersion": 1,
  "workspaces": {
    "": {
      "name": "root-app",
      "dependencies": {
        "@rawkodeacademy/content-technologies": "workspace:*"
      }
    },
    "packages/content-technologies": {
      "name": "@rawkodeacademy/content-technologies",
      "version": "0.0.1"
    },
    "apps/site": {
      "version": "0.0.0",
      "dependencies": {
        "@rawkodeacademy/content-technologies": "workspace:*"
      }
    }
  },
  "packages": {}
}"#,
        )
        .unwrap();

        // Workspace member packages
        fs::create_dir_all(root.join("packages/content-technologies")).unwrap();
        fs::write(
            root.join("packages/content-technologies/package.json"),
            r#"{
  "name": "@rawkodeacademy/content-technologies",
  "version": "0.0.1"
}"#,
        )
        .unwrap();

        fs::create_dir_all(root.join("apps/site")).unwrap();
        fs::write(
            root.join("apps/site/package.json"),
            r#"{
  "name": "site",
  "version": "0.0.0",
  "dependencies": {
    "@rawkodeacademy/content-technologies": "workspace:*"
  }
}"#,
        )
        .unwrap();

        let mut workspaces = HashMap::new();
        workspaces.insert(
            "bun".to_string(),
            WorkspaceConfig {
                enabled: true,
                package_manager: Some("bun".to_string()),
                root: None,
            },
        );

        let config = ExecutorConfig {
            capture_output: true,
            project_root: root.join("apps/site"),
            workspaces: Some(workspaces),
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);

        let task = Task {
            command: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                "find ../.. -maxdepth 4 -type d | sort".to_string(),
            ],
            shell: None,
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec!["package.json".to_string()],
            outputs: vec![],
            external_inputs: None,
            workspaces: vec!["bun".to_string()],
            description: None,
        };

        let result = executor.execute_task("install", &task).await.unwrap();
        assert!(
            result.success,
            "command failed stdout='{}' stderr='{}'",
            result.stdout, result.stderr
        );
        assert!(
            result
                .stdout
                .split_whitespace()
                .any(|line| line.ends_with("packages/content-technologies")),
            "should include workspace member from workspace root; stdout='{}' stderr='{}'",
            result.stdout,
            result.stderr
        );
    }

    #[tokio::test]
    async fn test_execute_failing_task() {
        let config = ExecutorConfig {
            capture_output: true,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let task = Task {
            command: "false".to_string(),
            args: vec![],
            shell: None,
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspaces: vec![],
            description: Some("Failing task".to_string()),
        };
        let result = executor.execute_task("test", &task).await.unwrap();
        assert!(!result.success);
        assert_eq!(result.exit_code, Some(1));
    }

    #[tokio::test]
    async fn test_execute_sequential_group() {
        let config = ExecutorConfig {
            capture_output: true,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let task1 = Task {
            command: "echo".to_string(),
            args: vec!["first".to_string()],
            shell: None,
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspaces: vec![],
            description: Some("First task".to_string()),
        };
        let task2 = Task {
            command: "echo".to_string(),
            args: vec!["second".to_string()],
            shell: None,
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspaces: vec![],
            description: Some("Second task".to_string()),
        };
        let group = TaskGroup::Sequential(vec![
            TaskDefinition::Single(Box::new(task1)),
            TaskDefinition::Single(Box::new(task2)),
        ]);
        let all_tasks = Tasks::new();
        let results = executor
            .execute_group("seq", &group, &all_tasks)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].stdout.contains("first"));
        assert!(results[1].stdout.contains("second"));
    }

    #[tokio::test]
    async fn test_command_injection_prevention() {
        let config = ExecutorConfig {
            capture_output: true,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let malicious_task = Task {
            command: "echo".to_string(),
            args: vec!["hello".to_string(), "; rm -rf /".to_string()],
            shell: None,
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspaces: vec![],
            description: Some("Malicious task test".to_string()),
        };
        let result = executor
            .execute_task("malicious", &malicious_task)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("hello ; rm -rf /"));
    }

    #[tokio::test]
    async fn test_special_characters_in_args() {
        let config = ExecutorConfig {
            capture_output: true,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let special_chars = vec![
            "$USER",
            "$(whoami)",
            "`whoami`",
            "&& echo hacked",
            "|| echo failed",
            "> /tmp/hack",
            "| cat",
        ];
        for special_arg in special_chars {
            let task = Task {
                command: "echo".to_string(),
                args: vec!["safe".to_string(), special_arg.to_string()],
                shell: None,
                env: HashMap::new(),
                depends_on: vec![],
                inputs: vec![],
                outputs: vec![],
                external_inputs: None,
                workspaces: vec![],
                description: Some("Special character test".to_string()),
            };
            let result = executor.execute_task("special", &task).await.unwrap();
            assert!(result.success);
            assert!(result.stdout.contains("safe"));
            assert!(result.stdout.contains(special_arg));
        }
    }

    #[tokio::test]
    async fn test_environment_variable_safety() {
        let mut config = ExecutorConfig {
            capture_output: true,
            ..Default::default()
        };
        config
            .environment
            .set("DANGEROUS_VAR".to_string(), "; rm -rf /".to_string());
        let executor = TaskExecutor::new(config);
        let task = Task {
            command: "printenv".to_string(),
            args: vec!["DANGEROUS_VAR".to_string()],
            shell: None,
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspaces: vec![],
            description: Some("Environment variable safety test".to_string()),
        };
        let result = executor.execute_task("env_test", &task).await.unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("; rm -rf /"));
    }

    #[tokio::test]
    async fn test_execute_graph_parallel_groups() {
        // two independent tasks -> can run in same parallel group
        let config = ExecutorConfig {
            capture_output: true,
            max_parallel: 2,
            ..Default::default()
        };
        let executor = TaskExecutor::new(config);
        let mut graph = TaskGraph::new();

        let t1 = Task {
            command: "echo".into(),
            args: vec!["A".into()],
            shell: None,
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspaces: vec![],
            description: None,
        };
        let t2 = Task {
            command: "echo".into(),
            args: vec!["B".into()],
            shell: None,
            env: HashMap::new(),
            depends_on: vec![],
            inputs: vec![],
            outputs: vec![],
            external_inputs: None,
            workspaces: vec![],
            description: None,
        };

        graph.add_task("t1", t1).unwrap();
        graph.add_task("t2", t2).unwrap();
        let results = executor.execute_graph(&graph).await.unwrap();
        assert_eq!(results.len(), 2);
        let joined = results.iter().map(|r| r.stdout.clone()).collect::<String>();
        assert!(joined.contains("A") && joined.contains("B"));
    }
}
