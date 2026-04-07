//! Task-level caching glue between the executor, [`cuenv_cas`], and
//! [`cuenv_vcs`].
//!
//! This module is responsible for:
//!
//! 1. Building the [`cuenv_cas::Action`] envelope for a task — a deterministic
//!    summary of *everything* that affects the task's outputs (declared
//!    inputs, command, args, env vars, platform, cuenv version).
//! 2. Querying the [`cuenv_cas::ActionCache`] for a previous result.
//! 3. Materializing cached outputs back into the workspace on a hit.
//! 4. Persisting outputs + metadata after a successful execution on a miss.
//!
//! The executor invokes these helpers in a thin wrapper around its existing
//! command-spawning path so that caching is layered cleanly on top rather
//! than tangled into the spawn logic.

use crate::Result;
use crate::environment::Environment;
use crate::tasks::{Task, TaskCachePolicy};
use cuenv_cas::{
    Action, ActionCache, ActionResult, Cas, Command, Digest, Directory, ExecutionMetadata,
    FileNode, OutputFile, Platform, digest_of,
};
use cuenv_vcs::VcsHasher;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Bundle of caching infrastructure used by the task executor.
///
/// All three handles must point at the same logical store; the executor
/// does no cross-store reconciliation.
#[derive(Clone)]
pub struct TaskCacheConfig {
    /// Content-addressed blob store.
    pub cas: Arc<dyn Cas>,
    /// Action → result lookup table.
    pub action_cache: Arc<dyn ActionCache>,
    /// Strategy for resolving + hashing input files.
    pub vcs_hasher: Arc<dyn VcsHasher>,
    /// cuenv binary version, baked into every action digest. Bumping this
    /// invalidates all cache entries on upgrade.
    pub cuenv_version: String,
    /// Optional runtime identity properties folded into action identity.
    /// For Nix runtime this includes the locked runtime digest.
    pub runtime_identity_properties: BTreeMap<String, String>,
    /// Optional reason caching is disabled for this run.
    pub cache_disabled_reason: Option<String>,
}

impl std::fmt::Debug for TaskCacheConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskCacheConfig")
            .field("vcs_hasher", &self.vcs_hasher.name())
            .field("cuenv_version", &self.cuenv_version)
            .field(
                "runtime_identity_properties",
                &self.runtime_identity_properties,
            )
            .field("cache_disabled_reason", &self.cache_disabled_reason)
            .finish_non_exhaustive()
    }
}

/// Returns the effective task cache policy.
#[must_use]
pub fn effective_policy(task: &Task) -> TaskCachePolicy {
    task.cache_policy()
}

/// Build the [`Action`] envelope for a task and compute its digest.
///
/// Returns `Ok(None)` when the task is not eligible for caching (no declared
/// inputs, or uses input variants that aren't yet supported in Phase 1c —
/// project references, task output references). The cache is **opt-in per
/// task**: the user enables it by declaring `inputs` on the task.
///
/// # Errors
///
/// Propagates any failure from the [`VcsHasher`] or canonical encoding.
pub async fn build_action(
    task: &Task,
    task_name: &str,
    environment: &Environment,
    cache: &TaskCacheConfig,
) -> Result<Option<(Action, Digest)>> {
    if let Some(reason) = &cache.cache_disabled_reason {
        tracing::debug!(task = %task_name, reason, "skipping cache");
        return Ok(None);
    }

    let policy = effective_policy(task);
    if !policy.mode.allows_read() && !policy.mode.allows_write() {
        tracing::debug!(task = %task_name, "skipping cache: task cache mode is never");
        return Ok(None);
    }

    if task.inputs.is_empty() {
        return Ok(None);
    }

    let mut patterns: Vec<String> = Vec::with_capacity(task.inputs.len());
    for input in &task.inputs {
        if let Some(p) = input.as_path() {
            patterns.push(p.clone());
        } else {
            tracing::debug!(
                task = %task_name,
                "skipping cache: task uses non-path input (project/task reference)"
            );
            return Ok(None);
        }
    }

    let hashed = cache
        .vcs_hasher
        .resolve_and_hash(&patterns)
        .await
        .map_err(|e| crate::Error::configuration(format!("input hashing failed: {e}")))?;

    // Build a flat input-root Directory message. We sort by relative path
    // (BTreeMap) so the canonical encoding is order-invariant. We do *not*
    // mirror the on-disk subdirectory layout — for cache key purposes,
    // (path, content_hash) pairs are equivalent and cheaper to construct.
    let mut file_entries: BTreeMap<String, Digest> = BTreeMap::new();
    for h in &hashed {
        let digest = Digest {
            hash: h.sha256.clone(),
            size_bytes: h.size,
        };
        file_entries.insert(h.relative_path.to_string_lossy().into_owned(), digest);
    }

    let directory = Directory {
        files: file_entries
            .into_iter()
            .map(|(name, digest)| FileNode {
                name,
                digest,
                is_executable: false,
            })
            .collect(),
        directories: Vec::new(),
        symlinks: Vec::new(),
    };
    let input_root_digest = digest_of(&directory)
        .map_err(|e| crate::Error::configuration(format!("input root digest: {e}")))?;

    // Build the Command envelope. Resolved env vars: project-level
    // environment merged with task-level env, with passthrough refs read
    // from the host. Output refs (`cuenv:ref:*`) and secret refs are
    // skipped — they're resolved separately by OutputRefResolver before
    // execution and don't contribute to identity.
    let mut env_vars: BTreeMap<String, String> = BTreeMap::new();
    let resolved = environment.merge_with_system_hermetic();
    for (k, v) in &resolved {
        env_vars.insert(k.clone(), v.clone());
    }
    for (key, value) in &task.env {
        if let Some(s) = value.as_str() {
            if let Some(host) = super::output_refs::parse_passthrough(s) {
                if let Ok(host_val) = std::env::var(host) {
                    env_vars.insert(key.clone(), host_val);
                }
            } else if !s.starts_with("cuenv:ref:") {
                env_vars.insert(key.clone(), s.to_string());
            }
        } else if let Some(n) = value.as_i64() {
            env_vars.insert(key.clone(), n.to_string());
        } else if let Some(b) = value.as_bool() {
            env_vars.insert(key.clone(), b.to_string());
        }
    }

    let mut arguments = Vec::with_capacity(1 + task.args.len());
    arguments.push(task.command.clone());
    arguments.extend(task.args.iter().cloned());

    let command = Command {
        arguments,
        environment_variables: env_vars,
        output_files: task.outputs.clone(),
        output_directories: Vec::new(),
        working_directory: String::new(),
    };
    let command_digest = digest_of(&command)
        .map_err(|e| crate::Error::configuration(format!("command digest: {e}")))?;

    let mut platform_props = BTreeMap::new();
    platform_props.insert("os".to_string(), std::env::consts::OS.to_string());
    platform_props.insert("arch".to_string(), std::env::consts::ARCH.to_string());
    for (key, value) in &cache.runtime_identity_properties {
        platform_props.insert(key.clone(), value.clone());
    }

    let action = Action {
        command_digest,
        input_root_digest,
        platform: Platform {
            properties: platform_props,
        },
        cuenv_version: cache.cuenv_version.clone(),
    };
    let action_digest = digest_of(&action)
        .map_err(|e| crate::Error::configuration(format!("action digest: {e}")))?;

    Ok(Some((action, action_digest)))
}

/// Query the action cache for a previous result.
///
/// # Errors
///
/// Propagates any error from the underlying [`ActionCache`] implementation.
pub fn lookup(
    cache: &TaskCacheConfig,
    action_digest: &Digest,
    task: &Task,
) -> Result<Option<ActionResult>> {
    let policy = effective_policy(task);
    if !policy.mode.allows_read() {
        return Ok(None);
    }

    let Some(result) = cache
        .action_cache
        .lookup(action_digest)
        .map_err(|e| crate::Error::configuration(format!("action cache lookup: {e}")))?
    else {
        return Ok(None);
    };

    if result.exit_code != 0 {
        tracing::warn!(
            action = %action_digest,
            exit_code = result.exit_code,
            "ignoring invalid cached result with non-zero exit code"
        );
        return Ok(None);
    }

    if is_expired(&result, policy.max_age.as_deref())? {
        tracing::debug!(
            action = %action_digest,
            max_age = ?policy.max_age,
            "cache entry expired"
        );
        return Ok(None);
    }

    Ok(Some(result))
}

/// Materialize a cache hit's outputs into `workdir`.
///
/// Returns `(stdout, stderr, exit_code)` reconstructed from the CAS so the
/// caller can build a `TaskResult` and emit completion events without
/// having executed the task.
///
/// # Errors
///
/// Propagates any error from the [`Cas`] when fetching blobs.
pub fn materialize_hit(
    cache: &TaskCacheConfig,
    workdir: &Path,
    result: &ActionResult,
) -> Result<(String, String, i32)> {
    for of in &result.output_files {
        let dst = workdir.join(&of.path);
        cache
            .cas
            .get_to_file(&of.digest, &dst)
            .map_err(|e| crate::Error::configuration(format!("cas get output: {e}")))?;
    }
    let stdout = if let Some(d) = &result.stdout_digest {
        let bytes = cache
            .cas
            .get(d)
            .map_err(|e| crate::Error::configuration(format!("cas get stdout: {e}")))?;
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        String::new()
    };
    let stderr = if let Some(d) = &result.stderr_digest {
        let bytes = cache
            .cas
            .get(d)
            .map_err(|e| crate::Error::configuration(format!("cas get stderr: {e}")))?;
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        String::new()
    };
    Ok((stdout, stderr, result.exit_code))
}

/// Persist a successful execution to the cache.
///
/// Failures are best-effort: callers should `.ok()` the result so a
/// cache-write hiccup never fails the user's task.
///
/// # Errors
///
/// Returns an error if the [`Cas`] or [`ActionCache`] persistence fails.
pub fn record(input: RecordInput<'_>) -> Result<()> {
    let RecordInput {
        cache,
        action_digest,
        workdir,
        task,
        stdout,
        stderr,
        exit_code,
        duration_ms,
    } = input;

    if exit_code != 0 {
        tracing::debug!(exit_code, "skipping cache write for non-zero exit code");
        return Ok(());
    }

    let mut output_files = Vec::with_capacity(task.outputs.len());
    for pattern in &task.outputs {
        let abs = workdir.join(pattern);
        if abs.is_file() {
            let digest = cache
                .cas
                .put_file(&abs)
                .map_err(|e| crate::Error::configuration(format!("cas put output: {e}")))?;
            output_files.push(OutputFile {
                path: pattern.clone(),
                digest,
                is_executable: false,
            });
        } else {
            tracing::debug!(
                output = %pattern,
                "declared output not found after task execution; skipping"
            );
        }
    }

    let stdout_digest = cache
        .cas
        .put_bytes(stdout.as_bytes())
        .map_err(|e| crate::Error::configuration(format!("cas put stdout: {e}")))?;
    let stderr_digest = cache
        .cas
        .put_bytes(stderr.as_bytes())
        .map_err(|e| crate::Error::configuration(format!("cas put stderr: {e}")))?;

    let result = ActionResult {
        output_files,
        output_directories: Vec::new(),
        exit_code,
        stdout_digest: Some(stdout_digest),
        stderr_digest: Some(stderr_digest),
        execution_metadata: ExecutionMetadata {
            worker: "local".to_string(),
            duration_ms,
            created_at: chrono::Utc::now(),
        },
    };
    cache
        .action_cache
        .update(action_digest, &result)
        .map_err(|e| crate::Error::configuration(format!("action cache update: {e}")))?;
    Ok(())
}

fn is_expired(result: &ActionResult, max_age: Option<&str>) -> Result<bool> {
    let Some(spec) = max_age else {
        return Ok(false);
    };
    let max_age_duration = parse_max_age(spec)?;
    let Some(max_age_duration) = max_age_duration else {
        return Ok(false);
    };

    let now = chrono::Utc::now();
    let age = now.signed_duration_since(result.execution_metadata.created_at);
    if age < chrono::Duration::zero() {
        // Clock skew: if created_at is in the future, treat as stale for safety.
        return Ok(true);
    }
    let age = age
        .to_std()
        .map_err(|e| crate::Error::configuration(format!("invalid cache age: {e}")))?;
    Ok(age > max_age_duration)
}

fn parse_max_age(spec: &str) -> Result<Option<Duration>> {
    let raw = spec.trim();
    if raw.is_empty() {
        return Err(crate::Error::configuration(
            "cache.maxAge must not be empty".to_string(),
        ));
    }
    if raw.eq_ignore_ascii_case("infinite")
        || raw.eq_ignore_ascii_case("inf")
        || raw.eq_ignore_ascii_case("never")
    {
        return Ok(None);
    }

    let digits_len = raw.bytes().take_while(|byte| byte.is_ascii_digit()).count();
    if digits_len == 0 || digits_len == raw.len() {
        return Err(crate::Error::configuration(format!(
            "invalid cache.maxAge '{raw}': expected <int><unit> (e.g. 30m, 1h)"
        )));
    }

    let quantity: u64 = raw[..digits_len]
        .parse()
        .map_err(|e| crate::Error::configuration(format!("invalid cache.maxAge '{raw}': {e}")))?;
    let unit = raw[digits_len..].trim().to_ascii_lowercase();

    let duration = match unit.as_str() {
        "ms" => Duration::from_millis(quantity),
        "s" => Duration::from_secs(quantity),
        "m" => Duration::from_secs(multiply_checked(quantity, 60, raw)?),
        "h" => Duration::from_secs(multiply_checked(quantity, 60 * 60, raw)?),
        "d" => Duration::from_secs(multiply_checked(quantity, 24 * 60 * 60, raw)?),
        _ => {
            return Err(crate::Error::configuration(format!(
                "invalid cache.maxAge unit in '{raw}': use ms|s|m|h|d|infinite"
            )));
        }
    };

    Ok(Some(duration))
}

fn multiply_checked(quantity: u64, factor: u64, raw: &str) -> Result<u64> {
    quantity.checked_mul(factor).ok_or_else(|| {
        crate::Error::configuration(format!("cache.maxAge '{raw}' is too large to represent"))
    })
}

/// Inputs to [`record`] (grouped to keep the function under the `clippy::too_many_arguments`
/// threshold and to make call sites self-documenting).
pub struct RecordInput<'a> {
    /// Cache configuration.
    pub cache: &'a TaskCacheConfig,
    /// Action digest the result is keyed under.
    pub action_digest: &'a Digest,
    /// Working directory the task ran in (used to resolve declared outputs).
    pub workdir: &'a Path,
    /// The task definition.
    pub task: &'a Task,
    /// Captured stdout.
    pub stdout: &'a str,
    /// Captured stderr.
    pub stderr: &'a str,
    /// Process exit code.
    pub exit_code: i32,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u128,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Environment;
    use crate::tasks::{Input, Task, TaskCacheMode, TaskCachePolicy};
    use cuenv_cas::{LocalActionCache, LocalCas};
    use cuenv_vcs::WalkHasher;
    use std::fs;
    use tempfile::TempDir;

    fn make_cache(root: &Path) -> TaskCacheConfig {
        TaskCacheConfig {
            cas: Arc::new(LocalCas::open(root).unwrap()),
            action_cache: Arc::new(LocalActionCache::open(root).unwrap()),
            vcs_hasher: Arc::new(WalkHasher::new(root)),
            cuenv_version: "test-version".to_string(),
            runtime_identity_properties: BTreeMap::new(),
            cache_disabled_reason: None,
        }
    }

    fn make_task(command: &str, args: &[&str], inputs: &[&str], outputs: &[&str]) -> Task {
        Task {
            command: command.to_string(),
            args: args.iter().map(|s| (*s).to_string()).collect(),
            inputs: inputs
                .iter()
                .map(|p| Input::Path((*p).to_string()))
                .collect(),
            outputs: outputs.iter().map(|s| (*s).to_string()).collect(),
            cache: Some(TaskCachePolicy {
                mode: TaskCacheMode::ReadWrite,
                max_age: None,
            }),
            ..Task::default()
        }
    }

    #[tokio::test]
    async fn build_action_returns_none_when_no_inputs() {
        let tmp = TempDir::new().unwrap();
        let cache = make_cache(tmp.path());
        let task = make_task("echo", &["hi"], &[], &[]);
        let env = Environment::new();

        let result = build_action(&task, "no-inputs", &env, &cache)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn build_action_is_deterministic() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("input.txt"), "payload").unwrap();
        let cache = make_cache(tmp.path());
        let task = make_task("echo", &["hi"], &["input.txt"], &[]);
        let env = Environment::new();

        let (_, d1) = build_action(&task, "t", &env, &cache)
            .await
            .unwrap()
            .unwrap();
        let (_, d2) = build_action(&task, "t", &env, &cache)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(d1, d2);
    }

    #[tokio::test]
    async fn build_action_changes_when_input_changes() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("input.txt"), "first").unwrap();
        let cache = make_cache(tmp.path());
        let task = make_task("echo", &["hi"], &["input.txt"], &[]);
        let env = Environment::new();

        let (_, d1) = build_action(&task, "t", &env, &cache)
            .await
            .unwrap()
            .unwrap();

        fs::write(tmp.path().join("input.txt"), "second").unwrap();
        let (_, d2) = build_action(&task, "t", &env, &cache)
            .await
            .unwrap()
            .unwrap();

        assert_ne!(d1, d2);
    }

    #[tokio::test]
    async fn build_action_changes_when_command_changes() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("input.txt"), "payload").unwrap();
        let cache = make_cache(tmp.path());
        let env = Environment::new();

        let task1 = make_task("cargo", &["build"], &["input.txt"], &[]);
        let task2 = make_task("cargo", &["test"], &["input.txt"], &[]);

        let (_, d1) = build_action(&task1, "t", &env, &cache)
            .await
            .unwrap()
            .unwrap();
        let (_, d2) = build_action(&task2, "t", &env, &cache)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(d1, d2);
    }

    #[tokio::test]
    async fn record_then_lookup_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let workdir = tmp.path().join("work");
        fs::create_dir_all(&workdir).unwrap();
        fs::write(tmp.path().join("input.txt"), "in").unwrap();
        fs::write(workdir.join("out.txt"), "produced").unwrap();

        let cache = make_cache(tmp.path());
        let task = make_task("echo", &["hi"], &["input.txt"], &["out.txt"]);
        let env = Environment::new();

        let (_, action_digest) = build_action(&task, "t", &env, &cache)
            .await
            .unwrap()
            .unwrap();

        record(RecordInput {
            cache: &cache,
            action_digest: &action_digest,
            workdir: &workdir,
            task: &task,
            stdout: "stdout-text",
            stderr: "stderr-text",
            exit_code: 0,
            duration_ms: 42,
        })
        .unwrap();

        let recorded = lookup(&cache, &action_digest, &task).unwrap().unwrap();
        assert_eq!(recorded.exit_code, 0);
        assert_eq!(recorded.output_files.len(), 1);
        assert_eq!(recorded.output_files[0].path, "out.txt");

        // Materializing into a fresh workdir should reproduce the file.
        let fresh = tmp.path().join("fresh");
        fs::create_dir_all(&fresh).unwrap();
        let (stdout, stderr, exit) = materialize_hit(&cache, &fresh, &recorded).unwrap();
        assert_eq!(stdout, "stdout-text");
        assert_eq!(stderr, "stderr-text");
        assert_eq!(exit, 0);
        assert_eq!(fs::read(fresh.join("out.txt")).unwrap(), b"produced");
    }

    #[tokio::test]
    async fn build_action_returns_none_when_cache_mode_never() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("input.txt"), "payload").unwrap();
        let cache = make_cache(tmp.path());
        let task = Task {
            command: "echo".to_string(),
            args: vec!["hi".to_string()],
            inputs: vec![Input::Path("input.txt".to_string())],
            cache: Some(TaskCachePolicy {
                mode: TaskCacheMode::Never,
                max_age: None,
            }),
            ..Task::default()
        };
        let env = Environment::new();

        let result = build_action(&task, "never", &env, &cache).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn lookup_respects_max_age() {
        let tmp = TempDir::new().unwrap();
        let workdir = tmp.path().join("work");
        fs::create_dir_all(&workdir).unwrap();
        fs::write(tmp.path().join("input.txt"), "in").unwrap();
        fs::write(workdir.join("out.txt"), "produced").unwrap();

        let cache = make_cache(tmp.path());
        let task = Task {
            command: "echo".to_string(),
            args: vec!["hi".to_string()],
            inputs: vec![Input::Path("input.txt".to_string())],
            outputs: vec!["out.txt".to_string()],
            cache: Some(TaskCachePolicy {
                mode: TaskCacheMode::ReadWrite,
                max_age: Some("1ms".to_string()),
            }),
            ..Task::default()
        };
        let env = Environment::new();

        let (_, action_digest) = build_action(&task, "ttl", &env, &cache)
            .await
            .unwrap()
            .unwrap();
        record(RecordInput {
            cache: &cache,
            action_digest: &action_digest,
            workdir: &workdir,
            task: &task,
            stdout: "stdout-text",
            stderr: "stderr-text",
            exit_code: 0,
            duration_ms: 42,
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(5));
        let lookup_result = lookup(&cache, &action_digest, &task).unwrap();
        assert!(lookup_result.is_none());
    }

    #[tokio::test]
    async fn record_skips_non_zero_exit_codes() {
        let tmp = TempDir::new().unwrap();
        let workdir = tmp.path().join("work");
        fs::create_dir_all(&workdir).unwrap();
        fs::write(tmp.path().join("input.txt"), "in").unwrap();
        fs::write(workdir.join("out.txt"), "produced").unwrap();

        let cache = make_cache(tmp.path());
        let task = make_task("echo", &["hi"], &["input.txt"], &["out.txt"]);
        let env = Environment::new();

        let (_, action_digest) = build_action(&task, "non-zero", &env, &cache)
            .await
            .unwrap()
            .unwrap();

        record(RecordInput {
            cache: &cache,
            action_digest: &action_digest,
            workdir: &workdir,
            task: &task,
            stdout: "stdout-text",
            stderr: "stderr-text",
            exit_code: 1,
            duration_ms: 42,
        })
        .unwrap();

        let lookup_result = lookup(&cache, &action_digest, &task).unwrap();
        assert!(lookup_result.is_none());
    }
}
