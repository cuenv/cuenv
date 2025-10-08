//! Task executor for running tasks with environment support and hermetic, input-addressed execution
//!
//! - Environment variable propagation
//! - Parallel and sequential execution
//! - Hermetic workdir populated from declared inputs (files/dirs/globs)
//! - Persistent task result cache keyed by inputs + command + env + cuenv version + platform

use super::{Task, TaskDefinition, TaskGraph, TaskGroup, Tasks};
use crate::cache::tasks as task_cache;
use crate::environment::Environment;
use crate::tasks::io::{InputResolver, collect_outputs, populate_hermetic_dir};
use crate::{Error, Result};
use async_recursion::async_recursion;
use chrono::Utc;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
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

/// Task executor configuration
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Whether to capture output (vs streaming to stdout/stderr)
    pub capture_output: bool,
    /// Maximum parallel tasks (0 = unlimited)
    pub max_parallel: usize,
    /// Environment variables to propagate (resolved via policies)
    pub environment: Environment,
    /// Project root for resolving inputs/outputs (env.cue root)
    pub project_root: PathBuf,
    /// Optional: materialize cached outputs on cache hit
    pub materialize_outputs: Option<PathBuf>,
    /// Optional: print cache path on hits/misses
    pub show_cache_path: bool,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            capture_output: false,
            max_parallel: 0,
            environment: Environment::new(),
            project_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            materialize_outputs: None,
            show_cache_path: false,
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
        // Resolve inputs relative to project root
        let span_inputs = tracing::info_span!("inputs.resolve", task = %name);
        let resolved_inputs = {
            let _g = span_inputs.enter();
            let resolver = InputResolver::new(&self.config.project_root);
            resolver.resolve(&task.inputs)?
        };

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

        let envelope = task_cache::CacheKeyEnvelope {
            inputs: inputs_summary.clone(),
            command: task.command.clone(),
            args: task.args.clone(),
            shell: shell_json,
            env: env_summary.clone(),
            cuenv_version: cuenv_version.clone(),
            platform: platform.clone(),
        };
        let (cache_key, envelope_json) = task_cache::compute_cache_key(&envelope)?;

        // Cache lookup
        let span_cache = tracing::info_span!("cache.lookup", task = %name, key = %cache_key);
        let cache_hit = {
            let _g = span_cache.enter();
            task_cache::lookup(&cache_key)
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
                let count = task_cache::materialize_outputs(&cache_key, dest)?;
                tracing::info!(materialized = count, dest = %dest.display(), "Materialized cached outputs");
            }
            return Ok(TaskResult {
                name: name.to_string(),
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
                success: true,
            });
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

        // Set environment variables (resolved + system), set CWD
        let env_vars = self.config.environment.merge_with_system();
        for (k, v) in env_vars {
            cmd.env(k, v);
        }
        cmd.current_dir(&hermetic_root);

        // Configure output
        if self.config.capture_output {
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());
        } else {
            cmd.stdout(Stdio::inherit());
            cmd.stderr(Stdio::inherit());
        }

        let start = std::time::Instant::now();
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::configuration(format!("Failed to spawn task '{}': {}", name, e)))?;

        let (stdout, stderr) = if self.config.capture_output {
            let stdout_handle = child.stdout.take();
            let stderr_handle = child.stderr.take();

            let stdout_task = async {
                if let Some(stdout) = stdout_handle {
                    let reader = BufReader::new(stdout);
                    let mut lines = reader.lines();
                    let mut stdout_lines = Vec::new();
                    while let Ok(Some(line)) = lines.next_line().await {
                        stdout_lines.push(line);
                    }
                    stdout_lines.join("\n")
                } else {
                    String::new()
                }
            };

            let stderr_task = async {
                if let Some(stderr) = stderr_handle {
                    let reader = BufReader::new(stderr);
                    let mut lines = reader.lines();
                    let mut stderr_lines = Vec::new();
                    while let Ok(Some(line)) = lines.next_line().await {
                        stderr_lines.push(line);
                    }
                    stderr_lines.join("\n")
                } else {
                    String::new()
                }
            };

            tokio::join!(stdout_task, stderr_task)
        } else {
            (String::new(), String::new())
        };

        let status = child.wait().await.map_err(|e| {
            Error::configuration(format!("Failed to wait for task '{}': {}", name, e))
        })?;
        let duration = start.elapsed();

        let exit_code = status.code().unwrap_or(1);
        let success = status.success();
        if !success {
            tracing::warn!(task = %name, exit = exit_code, "Task failed");
        } else {
            tracing::info!(task = %name, "Task completed successfully");
        }

        // Collect declared outputs and warn on undeclared writes
        let outputs = collect_outputs(&hermetic_root, &task.outputs)?;
        let outputs_set: HashSet<PathBuf> = outputs.iter().cloned().collect();
        let mut output_index: Vec<task_cache::OutputIndexEntry> = Vec::new();

        // Stage outputs into a temp dir for cache persistence
        let outputs_stage = std::env::temp_dir().join(format!("cuenv-outputs-{}", cache_key));
        if outputs_stage.exists() {
            let _ = std::fs::remove_dir_all(&outputs_stage);
        }
        std::fs::create_dir_all(&outputs_stage).ok();

        for rel in &outputs {
            let src = hermetic_root.join(rel);
            if let Ok(meta) = std::fs::metadata(&src)
                && meta.is_file()
            {
                let dst = outputs_stage.join(rel);
                if let Some(parent) = dst.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::copy(&src, &dst);
                let (sha, _size) = crate::tasks::io::sha256_file(&src).unwrap_or_default();
                output_index.push(task_cache::OutputIndexEntry {
                    rel_path: rel.to_string_lossy().to_string(),
                    size: meta.len(),
                    sha256: sha,
                });
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
                stdout: if self.config.capture_output {
                    Some(stdout.clone())
                } else {
                    None
                },
                stderr: if self.config.capture_output {
                    Some(stderr.clone())
                } else {
                    None
                },
            };
            let cache_span = tracing::info_span!("cache.save", key = %cache_key);
            {
                let _g = cache_span.enter();
                task_cache::save_result(&cache_key, &meta, &outputs_stage, &hermetic_root, logs)?;
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
                let result = self.execute_task(name, task).await?;
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
        let mut results = Vec::new();
        for (i, task_def) in tasks.iter().enumerate() {
            let task_name = format!("{}[{}]", prefix, i);
            let task_results = self
                .execute_definition(&task_name, task_def, all_tasks)
                .await?;
            for result in &task_results {
                if !result.success {
                    return Err(Error::configuration(format!(
                        "Task '{}' failed in sequential group",
                        result.name
                    )));
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
        let mut join_set = JoinSet::new();
        let all_tasks = Arc::new(all_tasks.clone());
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
                    Ok(Ok(_)) => {}
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
        let mut all_results = Vec::new();
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok(results)) => all_results.extend(results),
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
                            return Err(Error::configuration(format!(
                                "Task '{}' failed",
                                task_result.name
                            )));
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

    fn clone_with_config(&self) -> Self {
        Self {
            config: self.config.clone(),
        }
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
    if base.exists() && let Err(e) = std::fs::remove_dir_all(&base) {
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
            description: Some("Print env task".to_string()),
        };
        let result = executor.execute_task("test", &task).await.unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("test_value"));
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
            description: Some("Second task".to_string()),
        };
        let group = TaskGroup::Sequential(vec![
            TaskDefinition::Single(task1),
            TaskDefinition::Single(task2),
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
            description: Some("Environment variable safety test".to_string()),
        };
        let result = executor.execute_task("env_test", &task).await.unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("; rm -rf /"));
    }
}
