//! Task-level caching glue between the executor, [`cuenv_cas`], and
//! [`cuenv_vcs`].
//!
//! This module is responsible for:
//!
//! 1. Building the [`cuenv_cas::Action`] envelope for a task: a deterministic
//!    summary of everything that affects the task's outputs.
//! 2. Querying the [`cuenv_cas::ActionCache`] for a previous result.
//! 3. Materializing cached outputs back into the workspace on a hit.
//! 4. Persisting outputs and metadata after a successful execution on a miss.

use crate::Result;
use crate::environment::Environment;
use crate::tasks::{Task, TaskCachePolicy};
use cuenv_cas::{
    Action, ActionCache, ActionResult, Cas, Command, Digest, Directory, DirectoryNode,
    ExecutionMetadata, FileNode, OutputFile, Platform, digest_of,
};
use cuenv_vcs::{HashedInput, VcsHasher};
use globset::{Glob, GlobSetBuilder};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use walkdir::WalkDir;

/// Bundle of caching infrastructure used by the task executor.
///
/// All three handles must point at the same logical store; the executor
/// does no cross-store reconciliation.
#[derive(Clone)]
pub struct TaskCacheConfig {
    /// Content-addressed blob store.
    pub cas: Arc<dyn Cas>,
    /// Action -> result lookup table.
    pub action_cache: Arc<dyn ActionCache>,
    /// Strategy for resolving and hashing input files.
    pub vcs_hasher: Arc<dyn VcsHasher>,
    /// Root path the shared [`VcsHasher`] resolves inputs against.
    pub vcs_hasher_root: PathBuf,
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
            .field("vcs_hasher_root", &self.vcs_hasher_root)
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

/// Inputs to [`build_action`].
pub struct BuildActionInput<'a> {
    /// Task definition being hashed.
    pub task: &'a Task,
    /// Human-readable task name for diagnostics.
    pub task_name: &'a str,
    /// Environment resolver used by the executor.
    pub environment: &'a Environment,
    /// Cache infrastructure.
    pub cache: &'a TaskCacheConfig,
    /// Working directory the executor will actually use.
    pub workdir: &'a Path,
    /// Project root used for resolving task inputs.
    pub project_root: &'a Path,
    /// cue module root used for relative workdir normalization when needed.
    pub module_root: &'a Path,
}

/// Build the [`Action`] envelope for a task and compute its digest.
///
/// Returns `Ok(None)` when the task is not eligible for caching.
///
/// # Errors
///
/// Propagates failures from task command resolution and canonical encoding.
///
/// Input hashing failures degrade to `Ok(None)` so cache eligibility never
/// changes whether the task itself is runnable.
pub async fn build_action(input: BuildActionInput<'_>) -> Result<Option<(Action, Digest)>> {
    let BuildActionInput {
        task,
        task_name,
        environment,
        cache,
        workdir,
        project_root,
        module_root,
    } = input;

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

    let mut patterns = Vec::with_capacity(task.inputs.len());
    for input in &task.inputs {
        if let Some(path) = input.as_path() {
            patterns.push(path.clone());
        } else {
            tracing::debug!(
                task = %task_name,
                "skipping cache: task uses non-path input (project/task reference)"
            );
            return Ok(None);
        }
    }

    let Some(hashed) = resolve_hashed_inputs(cache, &patterns, project_root, task_name).await?
    else {
        return Ok(None);
    };
    if hashed.is_empty() {
        tracing::debug!(
            task = %task_name,
            "skipping cache: declared path inputs resolved to no files"
        );
        return Ok(None);
    }
    let input_root_digest = build_input_root_digest(&hashed)?;

    let mut environment_variables = BTreeMap::new();
    let resolved = environment.merge_with_system_hermetic();
    for (key, value) in &resolved {
        environment_variables.insert(key.clone(), value.clone());
    }
    let (task_env, _) = super::env::resolve_task_env(task_name, &task.env).await?;
    for (key, value) in task_env {
        environment_variables.insert(key, value);
    }

    let command_spec = task.command_spec(|command| environment.resolve_command(command))?;
    let mut arguments = Vec::with_capacity(1 + command_spec.args.len());
    arguments.push(command_spec.program);
    arguments.extend(command_spec.args);

    let command = Command {
        arguments,
        environment_variables,
        output_files: task.outputs.clone(),
        output_directories: Vec::new(),
        working_directory: normalize_workdir(workdir, project_root, module_root),
    };
    let command_digest = digest_of(&command)
        .map_err(|e| crate::Error::configuration(format!("command digest: {e}")))?;

    let mut platform_properties = BTreeMap::new();
    platform_properties.insert("os".to_string(), std::env::consts::OS.to_string());
    platform_properties.insert("arch".to_string(), std::env::consts::ARCH.to_string());
    for (key, value) in &cache.runtime_identity_properties {
        platform_properties.insert(key.clone(), value.clone());
    }

    let action = Action {
        command_digest,
        input_root_digest,
        platform: Platform {
            properties: platform_properties,
        },
        cuenv_version: cache.cuenv_version.clone(),
    };
    let action_digest = digest_of(&action)
        .map_err(|e| crate::Error::configuration(format!("action digest: {e}")))?;

    Ok(Some((action, action_digest)))
}

async fn resolve_hashed_inputs(
    cache: &TaskCacheConfig,
    patterns: &[String],
    project_root: &Path,
    task_name: &str,
) -> Result<Option<Vec<HashedInput>>> {
    let prefixed_patterns =
        match prefix_patterns_for_hasher_root(patterns, project_root, &cache.vcs_hasher_root) {
            Ok(prefixed_patterns) => prefixed_patterns,
            Err(error) => {
                tracing::warn!(
                    task = %task_name,
                    project_root = %project_root.display(),
                    hasher_root = %cache.vcs_hasher_root.display(),
                    error = %error,
                    "skipping cache: cannot map task inputs to cache hasher root"
                );
                return Ok(None);
            }
        };

    let hashed = match cache.vcs_hasher.resolve_and_hash(&prefixed_patterns).await {
        Ok(hashed) => hashed,
        Err(error) => {
            tracing::warn!(
                task = %task_name,
                error = %error,
                "skipping cache: input hashing failed"
            );
            return Ok(None);
        }
    };

    let rebased =
        match rebase_hashed_inputs_for_project_root(hashed, project_root, &cache.vcs_hasher_root) {
            Ok(rebased) => rebased,
            Err(error) => {
                tracing::warn!(
                    task = %task_name,
                    project_root = %project_root.display(),
                    hasher_root = %cache.vcs_hasher_root.display(),
                    error = %error,
                    "skipping cache: hashed inputs escaped task project root"
                );
                return Ok(None);
            }
        };

    Ok(Some(rebased))
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
/// caller can build a `TaskResult` without having executed the task.
///
/// # Errors
///
/// Propagates any error from the [`Cas`] when fetching blobs or restoring
/// output permissions.
pub fn materialize_hit(
    cache: &TaskCacheConfig,
    workdir: &Path,
    result: &ActionResult,
) -> Result<(String, String, i32)> {
    for output_file in &result.output_files {
        let destination = workdir.join(&output_file.path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                crate::Error::configuration(format!(
                    "create output parent {}: {e}",
                    parent.display()
                ))
            })?;
        }
        cache
            .cas
            .get_to_file(&output_file.digest, &destination)
            .map_err(|e| crate::Error::configuration(format!("cas get output: {e}")))?;
        set_executable_if_needed(&destination, output_file.is_executable)?;
    }

    let stdout = if let Some(digest) = &result.stdout_digest {
        let bytes = cache
            .cas
            .get(digest)
            .map_err(|e| crate::Error::configuration(format!("cas get stdout: {e}")))?;
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        String::new()
    };

    let stderr = if let Some(digest) = &result.stderr_digest {
        let bytes = cache
            .cas
            .get(digest)
            .map_err(|e| crate::Error::configuration(format!("cas get stderr: {e}")))?;
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        String::new()
    };

    Ok((stdout, stderr, result.exit_code))
}

/// Persist a successful execution to the cache.
///
/// Failures are best-effort: callers should ignore the result so a cache-write
/// hiccup never fails the user's task.
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

    let resolved_outputs = collect_outputs(workdir, &task.outputs)?;
    let mut output_files = Vec::with_capacity(resolved_outputs.len());
    for relative_path in resolved_outputs {
        let absolute_path = workdir.join(&relative_path);
        let digest = cache
            .cas
            .put_file(&absolute_path)
            .map_err(|e| crate::Error::configuration(format!("cas put output: {e}")))?;
        output_files.push(OutputFile {
            path: path_to_forward_slashes(&relative_path),
            digest,
            is_executable: is_executable(&absolute_path)?,
        });
    }

    let redacted_stdout = cuenv_events::redact(stdout);
    let redacted_stderr = cuenv_events::redact(stderr);
    let stdout_digest = cache
        .cas
        .put_bytes(redacted_stdout.as_bytes())
        .map_err(|e| crate::Error::configuration(format!("cas put stdout: {e}")))?;
    let stderr_digest = cache
        .cas
        .put_bytes(redacted_stderr.as_bytes())
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

#[derive(Default)]
struct InputDirectoryBuilder {
    files: BTreeMap<String, FileNode>,
    directories: BTreeMap<String, Self>,
}

impl InputDirectoryBuilder {
    fn insert(&mut self, relative_path: &Path, digest: Digest, is_executable: bool) -> Result<()> {
        let mut components = relative_path.components().peekable();
        let mut current = self;

        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(crate::Error::configuration(format!(
                    "invalid hashed input path '{}'",
                    relative_path.display()
                )));
            };

            let name = name.to_string_lossy().into_owned();
            if components.peek().is_some() {
                current = current.directories.entry(name).or_default();
            } else {
                current.files.insert(
                    name.clone(),
                    FileNode {
                        name,
                        digest: digest.clone(),
                        is_executable,
                    },
                );
            }
        }

        Ok(())
    }

    fn into_directory(self) -> Result<(Directory, Digest)> {
        let mut directories = Vec::with_capacity(self.directories.len());
        for (name, child) in self.directories {
            let (_, child_digest) = child.into_directory()?;
            directories.push(DirectoryNode {
                name,
                digest: child_digest,
            });
        }

        let directory = Directory {
            files: self.files.into_values().collect(),
            directories,
            symlinks: Vec::new(),
        };
        let digest = digest_of(&directory)
            .map_err(|e| crate::Error::configuration(format!("input root digest: {e}")))?;
        Ok((directory, digest))
    }
}

fn build_input_root_digest(hashed: &[HashedInput]) -> Result<Digest> {
    let mut builder = InputDirectoryBuilder::default();
    for input in hashed {
        builder.insert(
            &input.relative_path,
            Digest {
                hash: input.sha256.clone(),
                size_bytes: input.size,
            },
            input.is_executable,
        )?;
    }
    let (_, digest) = builder.into_directory()?;
    Ok(digest)
}

fn prefix_patterns_for_hasher_root(
    patterns: &[String],
    project_root: &Path,
    hasher_root: &Path,
) -> Result<Vec<String>> {
    let prefix = project_root.strip_prefix(hasher_root).map_err(|e| {
        crate::Error::configuration(format!(
            "project root '{}' is not under cache hasher root '{}': {e}",
            project_root.display(),
            hasher_root.display()
        ))
    })?;

    if prefix.as_os_str().is_empty() {
        return Ok(patterns.to_vec());
    }

    Ok(patterns
        .iter()
        .map(|pattern| {
            let trimmed = pattern.trim();
            if trimmed.is_empty() {
                String::new()
            } else {
                path_to_forward_slashes(&prefix.join(trimmed))
            }
        })
        .collect())
}

fn rebase_hashed_inputs_for_project_root(
    hashed: Vec<HashedInput>,
    project_root: &Path,
    hasher_root: &Path,
) -> Result<Vec<HashedInput>> {
    let prefix = project_root.strip_prefix(hasher_root).map_err(|e| {
        crate::Error::configuration(format!(
            "project root '{}' is not under cache hasher root '{}': {e}",
            project_root.display(),
            hasher_root.display()
        ))
    })?;

    if prefix.as_os_str().is_empty() {
        return Ok(hashed);
    }

    hashed
        .into_iter()
        .map(|input| {
            let relative_path = input.relative_path.strip_prefix(prefix).map_err(|e| {
                crate::Error::configuration(format!(
                    "hashed input '{}' is not under task project root '{}': {e}",
                    input.relative_path.display(),
                    project_root.display()
                ))
            })?;

            Ok(HashedInput {
                relative_path: relative_path.to_path_buf(),
                ..input
            })
        })
        .collect()
}

fn normalize_workdir(workdir: &Path, project_root: &Path, module_root: &Path) -> String {
    if let Ok(relative) = workdir.strip_prefix(project_root) {
        return path_to_forward_slashes(relative);
    }
    if let Ok(relative) = workdir.strip_prefix(module_root) {
        return path_to_forward_slashes(relative);
    }
    path_to_forward_slashes(workdir)
}

fn collect_outputs(workdir: &Path, patterns: &[String]) -> Result<Vec<PathBuf>> {
    if patterns.is_empty() {
        return Ok(Vec::new());
    }

    let mut builder = GlobSetBuilder::new();
    let mut has_patterns = false;
    for pattern in patterns {
        let trimmed = pattern.trim();
        if trimmed.is_empty() {
            continue;
        }

        let looks_like_glob = trimmed.contains('*')
            || trimmed.contains('{')
            || trimmed.contains('?')
            || trimmed.contains('[');
        let mut glob_pattern = trimmed.to_string();
        let absolute = workdir.join(trimmed);
        if absolute.is_dir() && !looks_like_glob {
            glob_pattern = format!("{}/**/*", trimmed.trim_end_matches('/'));
        }

        let glob = Glob::new(&glob_pattern).map_err(|e| {
            crate::Error::configuration(format!("invalid output glob '{glob_pattern}': {e}"))
        })?;
        builder.add(glob);
        has_patterns = true;
    }

    if !has_patterns {
        return Ok(Vec::new());
    }

    let globset = builder
        .build()
        .map_err(|e| crate::Error::configuration(format!("failed to build output globset: {e}")))?;

    let mut resolved = Vec::new();
    for entry in WalkDir::new(workdir) {
        let entry = entry.map_err(|e| {
            crate::Error::configuration(format!("walk output tree {}: {e}", workdir.display()))
        })?;
        if entry.file_type().is_dir() {
            continue;
        }

        let relative = entry.path().strip_prefix(workdir).map_err(|e| {
            crate::Error::configuration(format!(
                "output path '{}' not under workdir '{}': {e}",
                entry.path().display(),
                workdir.display()
            ))
        })?;
        if globset.is_match(relative) {
            resolved.push(relative.to_path_buf());
        }
    }

    resolved.sort();
    Ok(resolved)
}

fn path_to_forward_slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(unix)]
fn is_executable(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::metadata(path)
        .map_err(|e| crate::Error::configuration(format!("metadata {}: {e}", path.display())))?;
    Ok(metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> Result<bool> {
    Ok(false)
}

#[cfg(unix)]
fn set_executable_if_needed(path: &Path, is_executable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if !is_executable {
        return Ok(());
    }

    let mut permissions = std::fs::metadata(path)
        .map_err(|e| crate::Error::configuration(format!("metadata {}: {e}", path.display())))?
        .permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    std::fs::set_permissions(path, permissions).map_err(|e| {
        crate::Error::configuration(format!("set permissions {}: {e}", path.display()))
    })?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable_if_needed(_path: &Path, _is_executable: bool) -> Result<()> {
    Ok(())
}

/// Inputs to [`record`] grouped to keep call sites self-documenting.
pub struct RecordInput<'a> {
    /// Cache configuration.
    pub cache: &'a TaskCacheConfig,
    /// Action digest the result is keyed under.
    pub action_digest: &'a Digest,
    /// Working directory the task ran in.
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
            vcs_hasher_root: root.to_path_buf(),
            cuenv_version: "test-version".to_string(),
            runtime_identity_properties: BTreeMap::new(),
            cache_disabled_reason: None,
        }
    }

    fn make_task(command: &str, args: &[&str], inputs: &[&str], outputs: &[&str]) -> Task {
        Task {
            command: command.to_string(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            inputs: inputs
                .iter()
                .map(|path| Input::Path((*path).to_string()))
                .collect(),
            outputs: outputs.iter().map(|output| (*output).to_string()).collect(),
            cache: Some(TaskCachePolicy {
                mode: TaskCacheMode::ReadWrite,
                max_age: None,
            }),
            ..Task::default()
        }
    }

    async fn build_action_for_test(input: BuildActionInput<'_>) -> Option<(Action, Digest)> {
        build_action(input).await.unwrap()
    }

    #[tokio::test]
    async fn build_action_returns_none_when_no_inputs() {
        let tmp = TempDir::new().unwrap();
        let cache = make_cache(tmp.path());
        let task = make_task("echo", &["hi"], &[], &[]);
        let env = Environment::new();

        let result = build_action_for_test(BuildActionInput {
            task: &task,
            task_name: "no-inputs",
            environment: &env,
            cache: &cache,
            workdir: tmp.path(),
            project_root: tmp.path(),
            module_root: tmp.path(),
        })
        .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn build_action_is_deterministic() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("input.txt"), "payload").unwrap();
        let cache = make_cache(tmp.path());
        let task = make_task("echo", &["hi"], &["input.txt"], &[]);
        let env = Environment::new();

        let (_, first) = build_action_for_test(BuildActionInput {
            task: &task,
            task_name: "t",
            environment: &env,
            cache: &cache,
            workdir: tmp.path(),
            project_root: tmp.path(),
            module_root: tmp.path(),
        })
        .await
        .unwrap();
        let (_, second) = build_action_for_test(BuildActionInput {
            task: &task,
            task_name: "t",
            environment: &env,
            cache: &cache,
            workdir: tmp.path(),
            project_root: tmp.path(),
            module_root: tmp.path(),
        })
        .await
        .unwrap();
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn build_action_changes_when_input_changes() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("input.txt"), "first").unwrap();
        let cache = make_cache(tmp.path());
        let task = make_task("echo", &["hi"], &["input.txt"], &[]);
        let env = Environment::new();

        let (_, first) = build_action_for_test(BuildActionInput {
            task: &task,
            task_name: "t",
            environment: &env,
            cache: &cache,
            workdir: tmp.path(),
            project_root: tmp.path(),
            module_root: tmp.path(),
        })
        .await
        .unwrap();

        fs::write(tmp.path().join("input.txt"), "second").unwrap();
        let (_, second) = build_action_for_test(BuildActionInput {
            task: &task,
            task_name: "t",
            environment: &env,
            cache: &cache,
            workdir: tmp.path(),
            project_root: tmp.path(),
            module_root: tmp.path(),
        })
        .await
        .unwrap();

        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn build_action_hashes_inputs_relative_to_task_project_root() {
        let tmp = TempDir::new().unwrap();
        let workspace_root = tmp.path();
        let nested_project_root = workspace_root.join("packages/app");
        fs::create_dir_all(nested_project_root.join("src")).unwrap();
        fs::create_dir_all(workspace_root.join("src")).unwrap();
        fs::write(workspace_root.join("src/input.txt"), "workspace-root").unwrap();
        fs::write(nested_project_root.join("src/input.txt"), "nested-project").unwrap();

        let cache = make_cache(workspace_root);
        let task = make_task("echo", &["hi"], &["src/input.txt"], &[]);
        let env = Environment::new();

        let (_, first) = build_action_for_test(BuildActionInput {
            task: &task,
            task_name: "nested",
            environment: &env,
            cache: &cache,
            workdir: &nested_project_root,
            project_root: &nested_project_root,
            module_root: workspace_root,
        })
        .await
        .unwrap();

        fs::write(
            workspace_root.join("src/input.txt"),
            "workspace-root-updated",
        )
        .unwrap();
        let (_, second) = build_action_for_test(BuildActionInput {
            task: &task,
            task_name: "nested",
            environment: &env,
            cache: &cache,
            workdir: &nested_project_root,
            project_root: &nested_project_root,
            module_root: workspace_root,
        })
        .await
        .unwrap();

        assert_eq!(first, second);

        fs::write(
            nested_project_root.join("src/input.txt"),
            "nested-project-updated",
        )
        .unwrap();
        let (_, third) = build_action_for_test(BuildActionInput {
            task: &task,
            task_name: "nested",
            environment: &env,
            cache: &cache,
            workdir: &nested_project_root,
            project_root: &nested_project_root,
            module_root: workspace_root,
        })
        .await
        .unwrap();

        assert_ne!(first, third);
    }

    #[tokio::test]
    async fn build_action_changes_when_command_changes() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("input.txt"), "payload").unwrap();
        let cache = make_cache(tmp.path());
        let env = Environment::new();

        let task1 = make_task("cargo", &["build"], &["input.txt"], &[]);
        let task2 = make_task("cargo", &["test"], &["input.txt"], &[]);

        let (_, first) = build_action_for_test(BuildActionInput {
            task: &task1,
            task_name: "t",
            environment: &env,
            cache: &cache,
            workdir: tmp.path(),
            project_root: tmp.path(),
            module_root: tmp.path(),
        })
        .await
        .unwrap();
        let (_, second) = build_action_for_test(BuildActionInput {
            task: &task2,
            task_name: "t",
            environment: &env,
            cache: &cache,
            workdir: tmp.path(),
            project_root: tmp.path(),
            module_root: tmp.path(),
        })
        .await
        .unwrap();
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn build_action_changes_when_script_changes() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("input.txt"), "payload").unwrap();
        let cache = make_cache(tmp.path());
        let env = Environment::new();

        let task1 = Task {
            script: Some("echo one".to_string()),
            inputs: vec![Input::Path("input.txt".to_string())],
            cache: Some(TaskCachePolicy {
                mode: TaskCacheMode::ReadWrite,
                max_age: None,
            }),
            ..Task::default()
        };
        let task2 = Task {
            script: Some("echo two".to_string()),
            inputs: vec![Input::Path("input.txt".to_string())],
            cache: Some(TaskCachePolicy {
                mode: TaskCacheMode::ReadWrite,
                max_age: None,
            }),
            ..Task::default()
        };

        let (_, first) = build_action_for_test(BuildActionInput {
            task: &task1,
            task_name: "script",
            environment: &env,
            cache: &cache,
            workdir: tmp.path(),
            project_root: tmp.path(),
            module_root: tmp.path(),
        })
        .await
        .unwrap();
        let (_, second) = build_action_for_test(BuildActionInput {
            task: &task2,
            task_name: "script",
            environment: &env,
            cache: &cache,
            workdir: tmp.path(),
            project_root: tmp.path(),
            module_root: tmp.path(),
        })
        .await
        .unwrap();

        assert_ne!(first, second);
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

        let (_, action_digest) = build_action_for_test(BuildActionInput {
            task: &task,
            task_name: "t",
            environment: &env,
            cache: &cache,
            workdir: &workdir,
            project_root: tmp.path(),
            module_root: tmp.path(),
        })
        .await
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

        let fresh = tmp.path().join("fresh");
        fs::create_dir_all(&fresh).unwrap();
        let (stdout, stderr, exit_code) = materialize_hit(&cache, &fresh, &recorded).unwrap();
        assert_eq!(stdout, "stdout-text");
        assert_eq!(stderr, "stderr-text");
        assert_eq!(exit_code, 0);
        assert_eq!(fs::read(fresh.join("out.txt")).unwrap(), b"produced");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn record_and_materialize_preserve_executable_outputs() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let workdir = tmp.path().join("work");
        fs::create_dir_all(&workdir).unwrap();
        fs::write(tmp.path().join("input.txt"), "in").unwrap();
        let script = workdir.join("bin/run.sh");
        fs::create_dir_all(script.parent().unwrap()).unwrap();
        fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        let cache = make_cache(tmp.path());
        let task = make_task("echo", &["hi"], &["input.txt"], &["bin"]);
        let env = Environment::new();

        let (_, action_digest) = build_action_for_test(BuildActionInput {
            task: &task,
            task_name: "exec",
            environment: &env,
            cache: &cache,
            workdir: &workdir,
            project_root: tmp.path(),
            module_root: tmp.path(),
        })
        .await
        .unwrap();

        record(RecordInput {
            cache: &cache,
            action_digest: &action_digest,
            workdir: &workdir,
            task: &task,
            stdout: "",
            stderr: "",
            exit_code: 0,
            duration_ms: 1,
        })
        .unwrap();

        let recorded = lookup(&cache, &action_digest, &task).unwrap().unwrap();
        let fresh = tmp.path().join("fresh");
        fs::create_dir_all(&fresh).unwrap();
        materialize_hit(&cache, &fresh, &recorded).unwrap();

        let mode = fs::metadata(fresh.join("bin/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0);
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

        let result = build_action_for_test(BuildActionInput {
            task: &task,
            task_name: "never",
            environment: &env,
            cache: &cache,
            workdir: tmp.path(),
            project_root: tmp.path(),
            module_root: tmp.path(),
        })
        .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn build_action_returns_none_when_explicit_input_is_missing() {
        let tmp = TempDir::new().unwrap();
        let cache = make_cache(tmp.path());
        let task = make_task("echo", &["hi"], &["missing.txt"], &[]);
        let env = Environment::new();

        let result = build_action_for_test(BuildActionInput {
            task: &task,
            task_name: "missing",
            environment: &env,
            cache: &cache,
            workdir: tmp.path(),
            project_root: tmp.path(),
            module_root: tmp.path(),
        })
        .await;

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

        let (_, action_digest) = build_action_for_test(BuildActionInput {
            task: &task,
            task_name: "ttl",
            environment: &env,
            cache: &cache,
            workdir: &workdir,
            project_root: tmp.path(),
            module_root: tmp.path(),
        })
        .await
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

        let (_, action_digest) = build_action_for_test(BuildActionInput {
            task: &task,
            task_name: "non-zero",
            environment: &env,
            cache: &cache,
            workdir: &workdir,
            project_root: tmp.path(),
            module_root: tmp.path(),
        })
        .await
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
