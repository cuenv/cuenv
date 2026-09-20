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

use super::TaskCommandExt;
use crate::{Task, TaskCacheMode, TaskCachePolicy};
use cuenv_cas::{
    Action, ActionCache, ActionResult, Cas, Command, Digest, Directory, DirectoryNode,
    CanonicalMessage, ExecutionMetadata, FileNode, OutputFile, Platform, canonical_bytes, digest_of,
    missing_blobs,
};
use cuenv_core::Result;
use cuenv_core::environment::{ActionEnvironment, Environment};
use cuenv_events::CacheSkipReason;
use cuenv_vcs::{HashedInput, VcsHasher};
use globset::{Glob, GlobSetBuilder};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use walkdir::WalkDir;

/// Outcome of evaluating a task's cache eligibility.
#[derive(Debug)]
pub enum CacheOutcome {
    /// Task is eligible for caching with the computed action and digest.
    Eligible(Box<Action>, Digest),
    /// Task is not eligible; the [`CacheSkipReason`] explains why so renderers
    /// can surface the reason to the user.
    Skipped(CacheSkipReason),
}

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
    /// Execution-semantics salt baked into every action digest. See
    /// [`cuenv_cas::ACTION_SEMANTICS_VERSION`]; bumping it invalidates every
    /// entry, so it tracks changes in what execution *means*, not releases.
    pub action_semantics_version: u32,
    /// Optional runtime identity properties folded into action identity.
    /// For Nix runtime this includes the locked runtime digest.
    pub runtime_identity_properties: BTreeMap<String, String>,
    /// Optional reason caching is disabled for this run.
    pub cache_disabled_reason: Option<String>,
    /// Salt for fingerprinting secret-derived environment values into the
    /// action key, from `CUENV_SECRET_SALT`. Without it, a task whose
    /// environment holds a secret is not cacheable.
    pub secret_salt: Option<String>,
    /// Run-wide override of every task's declared cache mode, from
    /// `CUENV_CACHE`. `None` honours what each task declares.
    pub mode_override: Option<TaskCacheMode>,
}

impl std::fmt::Debug for TaskCacheConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskCacheConfig")
            .field("vcs_hasher", &self.vcs_hasher.name())
            .field("vcs_hasher_root", &self.vcs_hasher_root)
            .field("action_semantics_version", &self.action_semantics_version)
            .field(
                "runtime_identity_properties",
                &self.runtime_identity_properties,
            )
            .field("cache_disabled_reason", &self.cache_disabled_reason)
            .finish_non_exhaustive()
    }
}

/// Returns the effective task cache policy, after any run-wide override.
///
/// An override can only ever narrow what a task does — it cannot cache a task
/// whose own policy is `never` — so `CUENV_CACHE=read-write` does not turn
/// caching on for tasks that never opted in.
#[must_use]
pub fn effective_policy(cache: &TaskCacheConfig, task: &Task) -> TaskCachePolicy {
    let mut policy = task.cache_policy();
    if let Some(override_mode) = cache.mode_override
        && policy.mode != TaskCacheMode::Never
    {
        policy.mode = override_mode;
    }
    policy
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
/// Returns a [`CacheOutcome`] explaining either the resulting action +
/// digest (eligible) or the structured reason caching was skipped. The
/// caller is expected to surface the skip reason as a `CacheSkipped` event.
///
/// # Errors
///
/// Propagates failures from task command resolution and canonical encoding.
///
/// Input hashing failures degrade to a [`CacheOutcome::Skipped`] so cache
/// eligibility never changes whether the task itself is runnable.
pub async fn build_action(input: BuildActionInput<'_>) -> Result<CacheOutcome> {
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
        return Ok(CacheOutcome::Skipped(CacheSkipReason::Disabled {
            reason: Some(reason.clone()),
        }));
    }

    let policy = effective_policy(cache, task);
    if !policy.mode.allows_read() && !policy.mode.allows_write() {
        tracing::debug!(task = %task_name, "skipping cache: task cache mode is never");
        return Ok(CacheOutcome::Skipped(CacheSkipReason::NeverMode));
    }

    // A non-hermetic task reads and writes the live workspace and inherits
    // ambient host environment variables. The action key records neither, so
    // an entry written here would be keyed on a fraction of what produced it.
    if !task.is_hermetic() {
        tracing::debug!(
            task = %task_name,
            "skipping cache: task opted out of hermetic execution"
        );
        return Ok(CacheOutcome::Skipped(CacheSkipReason::NonHermetic));
    }

    if task.inputs.is_empty() {
        return Ok(CacheOutcome::Skipped(CacheSkipReason::EmptyInputs));
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
            return Ok(CacheOutcome::Skipped(CacheSkipReason::NonPathRef));
        }
    }

    if !task.env.is_empty() {
        tracing::debug!(
            task = %task_name,
            "skipping cache: task defines task-level environment entries resolved at execution time"
        );
        return Ok(CacheOutcome::Skipped(CacheSkipReason::RuntimeEnv));
    }

    let hashed = match resolve_hashed_inputs(cache, &patterns, project_root, task_name).await? {
        ResolveOutcome::Resolved(h) => h,
        ResolveOutcome::Skipped(reason) => return Ok(CacheOutcome::Skipped(reason)),
    };
    if hashed.is_empty() {
        tracing::debug!(
            task = %task_name,
            "skipping cache: declared path inputs resolved to no files"
        );
        return Ok(CacheOutcome::Skipped(CacheSkipReason::NoResolvedInputs));
    }
    let input_root_digest = build_input_root_digest(&hashed)?;

    // A workdir that resolves under neither root would put a host-specific
    // absolute path in the key, which no other machine can reproduce.
    let Some(working_directory) = normalize_workdir(workdir, project_root, module_root) else {
        tracing::warn!(
            task = %task_name,
            workdir = %workdir.display(),
            project_root = %project_root.display(),
            module_root = %module_root.display(),
            "skipping cache: working directory is outside the project and module roots"
        );
        return Ok(CacheOutcome::Skipped(CacheSkipReason::UnportableWorkdir));
    };

    let environment_variables = match environment
        .action_environment(task.env_passthrough(), cache.secret_salt.as_deref())
    {
        ActionEnvironment::Ready(environment_variables) => environment_variables,
        ActionEnvironment::SecretsWithoutSalt { names } => {
            tracing::debug!(
                task = %task_name,
                secrets = ?names,
                "skipping cache: secret-derived environment values and no CUENV_SECRET_SALT"
            );
            return Ok(CacheOutcome::Skipped(
                CacheSkipReason::SecretsWithoutCacheSalt,
            ));
        }
    };

    let command_spec = task.command_spec(|command| environment.resolve_command(command))?;
    let mut arguments = Vec::with_capacity(1 + command_spec.args.len());
    arguments.push(command_spec.program);
    arguments.extend(command_spec.args);

    let command = Command {
        arguments,
        environment_variables,
        output_files: task.outputs.clone(),
        output_directories: Vec::new(),
        working_directory,
    };
    let Some(command_digest) = store_message(cache, &command, "command", task_name).await else {
        return Ok(store_unwritable());
    };

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
        action_semantics_version: cache.action_semantics_version,
    };
    let Some(action_digest) = store_message(cache, &action, "action", task_name).await else {
        return Ok(store_unwritable());
    };

    Ok(CacheOutcome::Eligible(Box::new(action), action_digest))
}

/// Store a message in the CAS and return its digest, or `None` if the store
/// would not take it.
///
/// The digest is what the cache is keyed on; storing the message it was
/// computed from is what makes a key explicable afterwards. Without the blob,
/// a miss can only be reported ("these digests differ"), never explained
/// ("this environment variable changed").
///
/// A store that cannot be written — read-only, full, wrong permissions —
/// disables caching for the task rather than failing it. Cache eligibility
/// must never decide whether a user's command runs.
async fn store_message(
    cache: &TaskCacheConfig,
    message: &impl CanonicalMessage,
    kind: &str,
    task_name: &str,
) -> Option<Digest> {
    let bytes = match canonical_bytes(message) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(task = %task_name, kind, %error, "skipping cache: canonical encode failed");
            return None;
        }
    };
    match cache.cas.put_bytes(&bytes).await {
        Ok(digest) => Some(digest),
        Err(error) => {
            tracing::warn!(task = %task_name, kind, %error, "skipping cache: cannot write to the blob store");
            None
        }
    }
}

/// The skip reported when the blob store will not accept a write.
fn store_unwritable() -> CacheOutcome {
    CacheOutcome::Skipped(CacheSkipReason::Disabled {
        reason: Some("cache store is not writable".to_string()),
    })
}

/// Internal outcome from input resolution, distinguishing skip reasons.
enum ResolveOutcome {
    Resolved(Vec<HashedInput>),
    Skipped(CacheSkipReason),
}

async fn resolve_hashed_inputs(
    cache: &TaskCacheConfig,
    patterns: &[String],
    project_root: &Path,
    task_name: &str,
) -> Result<ResolveOutcome> {
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
                return Ok(ResolveOutcome::Skipped(CacheSkipReason::HasherRootMismatch));
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
            return Ok(ResolveOutcome::Skipped(CacheSkipReason::HashFailed));
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
                return Ok(ResolveOutcome::Skipped(CacheSkipReason::HasherRootMismatch));
            }
        };

    Ok(ResolveOutcome::Resolved(rebased))
}

/// Query the action cache for a previous result.
///
/// # Errors
///
/// Propagates any error from the underlying [`ActionCache`] implementation.
pub async fn lookup(
    cache: &TaskCacheConfig,
    action_digest: &Digest,
    task: &Task,
) -> Result<Option<ActionResult>> {
    let policy = effective_policy(cache, task);
    if !policy.mode.allows_read() {
        return Ok(None);
    }

    let Some(result) = cache
        .action_cache
        .lookup(action_digest)
        .await
        .map_err(|e| cuenv_core::Error::configuration(format!("action cache lookup: {e}")))?
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

    // An entry only promises its blobs; eviction or an interrupted write can
    // break that promise. Checking now turns a dangling entry into an
    // ordinary miss instead of a materialization that fails halfway and
    // leaves a half-restored output tree behind.
    let missing = missing_blobs(cache.cas.as_ref(), &result)
        .await
        .map_err(|e| cuenv_core::Error::configuration(format!("cache integrity check: {e}")))?;
    if !missing.is_empty() {
        tracing::warn!(
            action = %action_digest,
            missing = missing.len(),
            first_missing = %missing[0],
            "ignoring cache entry referencing blobs the store no longer holds"
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
pub async fn materialize_hit(
    cache: &TaskCacheConfig,
    workdir: &Path,
    result: &ActionResult,
) -> Result<(String, String, i32)> {
    materialize_outputs(cache, workdir, result).await?;

    let stdout = if let Some(digest) = &result.stdout_digest {
        let bytes = cache
            .cas
            .get(digest)
            .await
            .map_err(|e| cuenv_core::Error::configuration(format!("cas get stdout: {e}")))?;
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        String::new()
    };

    let stderr = if let Some(digest) = &result.stderr_digest {
        let bytes = cache
            .cas
            .get(digest)
            .await
            .map_err(|e| cuenv_core::Error::configuration(format!("cas get stderr: {e}")))?;
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        String::new()
    };

    Ok((stdout, stderr, result.exit_code))
}

/// Restore a cache hit's output files, staging them first.
///
/// Every blob is fetched and digest-verified into a staging directory inside
/// `workdir` before anything replaces a real file. A missing or corrupt blob
/// therefore aborts before the workspace is touched, instead of leaving half
/// the outputs from the cached run and half from whatever was there before.
///
/// Full atomicity would need a directory swap, which is not available when
/// outputs land in a tree that holds unrelated files. What this does
/// guarantee is that the fallible work — fetching, verifying, setting modes —
/// happens entirely in staging, and the commit phase is renames within one
/// filesystem.
async fn materialize_outputs(
    cache: &TaskCacheConfig,
    workdir: &Path,
    result: &ActionResult,
) -> Result<()> {
    if result.output_files.is_empty() {
        return Ok(());
    }

    let staging = StagingDir::create(workdir)?;
    let mut staged = Vec::with_capacity(result.output_files.len());

    for output_file in &result.output_files {
        let relative = safe_output_path(&output_file.path)?;
        let staged_path = staging.path().join(&relative);
        create_parent_dir(&staged_path)?;
        cache
            .cas
            .get_to_file(&output_file.digest, &staged_path)
            .await
            .map_err(|e| cuenv_core::Error::configuration(format!("cas get output: {e}")))?;
        set_executable_if_needed(&staged_path, output_file.is_executable)?;
        staged.push((staged_path, workdir.join(&relative)));
    }

    for (from, to) in staged {
        create_parent_dir(&to)?;
        std::fs::rename(&from, &to).map_err(|e| {
            cuenv_core::Error::configuration(format!(
                "install cached output {}: {e}",
                to.display()
            ))
        })?;
    }

    Ok(())
}

/// Reject a cached output path that would escape the working directory.
///
/// Output paths come out of the action cache, which in a shared-cache
/// deployment means they come from whoever last wrote the entry. A path like
/// `../../.ssh/authorized_keys` must never be joined onto the workspace root.
fn safe_output_path(path: &str) -> Result<PathBuf> {
    let reject = || {
        cuenv_core::Error::configuration(format!(
            "refusing to materialize cached output '{path}': \
             output paths must be relative and stay inside the working directory"
        ))
    };

    let mut normalized = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            // `.` carries no meaning once the path is rebuilt component-wise.
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            // `..`, `/` and Windows prefixes all escape the working directory.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(reject());
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        return Err(reject());
    }
    Ok(normalized)
}

fn create_parent_dir(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent).map_err(|e| {
        cuenv_core::Error::configuration(format!("create output parent {}: {e}", parent.display()))
    })
}

/// Prefix of the scratch directories [`StagingDir`] creates.
const STAGING_PREFIX: &str = ".cuenv-stage-";

fn is_staging_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(STAGING_PREFIX))
}

/// A scratch directory inside the workspace, removed when dropped.
///
/// It lives inside `workdir` rather than the cache root so that the commit
/// phase is a same-filesystem rename; a staging area under `$XDG_CACHE_HOME`
/// would degrade to a copy whenever the cache and the workspace sit on
/// different devices.
struct StagingDir {
    path: PathBuf,
}

impl StagingDir {
    fn create(workdir: &Path) -> Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let path = workdir.join(format!(
            "{STAGING_PREFIX}{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // A leftover from a killed run would otherwise merge into this one.
        if path.exists() {
            let _ = std::fs::remove_dir_all(&path);
        }
        std::fs::create_dir_all(&path).map_err(|e| {
            cuenv_core::Error::configuration(format!(
                "create staging directory {}: {e}",
                path.display()
            ))
        })?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.path) {
            tracing::warn!(
                path = %self.path.display(),
                error = %e,
                "failed to remove cache staging directory"
            );
        }
    }
}

/// Persist a successful execution to the cache.
///
/// Failures are best-effort: callers should ignore the result so a cache-write
/// hiccup never fails the user's task.
///
/// # Errors
///
/// Returns an error if the [`Cas`] or [`ActionCache`] persistence fails.
pub async fn record(input: RecordInput<'_>) -> Result<()> {
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
            .await
            .map_err(|e| cuenv_core::Error::configuration(format!("cas put output: {e}")))?;
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
        .await
        .map_err(|e| cuenv_core::Error::configuration(format!("cas put stdout: {e}")))?;
    let stderr_digest = cache
        .cas
        .put_bytes(redacted_stderr.as_bytes())
        .await
        .map_err(|e| cuenv_core::Error::configuration(format!("cas put stderr: {e}")))?;

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
        .await
        .map_err(|e| cuenv_core::Error::configuration(format!("action cache update: {e}")))?;
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
        .map_err(|e| cuenv_core::Error::configuration(format!("invalid cache age: {e}")))?;
    Ok(age > max_age_duration)
}

fn parse_max_age(spec: &str) -> Result<Option<Duration>> {
    let raw = spec.trim();
    if raw.is_empty() {
        return Err(cuenv_core::Error::configuration(
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
        return Err(cuenv_core::Error::configuration(format!(
            "invalid cache.maxAge '{raw}': expected <int><unit> (e.g. 30m, 1h)"
        )));
    }

    let quantity: u64 = raw[..digits_len].parse().map_err(|e| {
        cuenv_core::Error::configuration(format!("invalid cache.maxAge '{raw}': {e}"))
    })?;
    let unit = raw[digits_len..].trim().to_ascii_lowercase();

    let duration = match unit.as_str() {
        "ms" => Duration::from_millis(quantity),
        "s" => Duration::from_secs(quantity),
        "m" => Duration::from_secs(multiply_checked(quantity, 60, raw)?),
        "h" => Duration::from_secs(multiply_checked(quantity, 60 * 60, raw)?),
        "d" => Duration::from_secs(multiply_checked(quantity, 24 * 60 * 60, raw)?),
        _ => {
            return Err(cuenv_core::Error::configuration(format!(
                "invalid cache.maxAge unit in '{raw}': use ms|s|m|h|d|infinite"
            )));
        }
    };

    Ok(Some(duration))
}

fn multiply_checked(quantity: u64, factor: u64, raw: &str) -> Result<u64> {
    quantity.checked_mul(factor).ok_or_else(|| {
        cuenv_core::Error::configuration(format!("cache.maxAge '{raw}' is too large to represent"))
    })
}

#[derive(Default)]
struct InputDirectoryBuilder {
    files: BTreeMap<String, FileNode>,
    directories: BTreeMap<String, Self>,
}

impl InputDirectoryBuilder {
    fn insert(&mut self, relative_path: &Path, digest: &Digest, is_executable: bool) -> Result<()> {
        let mut components = relative_path.components().peekable();
        let mut current = self;

        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(cuenv_core::Error::configuration(format!(
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
            .map_err(|e| cuenv_core::Error::configuration(format!("input root digest: {e}")))?;
        Ok((directory, digest))
    }
}

fn build_input_root_digest(hashed: &[HashedInput]) -> Result<Digest> {
    let mut builder = InputDirectoryBuilder::default();
    for input in hashed {
        let digest = Digest {
            hash: input.sha256.clone(),
            size_bytes: input.size,
        };
        builder.insert(&input.relative_path, &digest, input.is_executable)?;
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
        cuenv_core::Error::configuration(format!(
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
        cuenv_core::Error::configuration(format!(
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
                cuenv_core::Error::configuration(format!(
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

/// Express `workdir` relative to the project or module root.
///
/// Returns `None` when it is under neither. The previous behaviour — falling
/// back to the absolute path — silently baked `/home/<user>/…` into the
/// action key, guaranteeing that no other machine could ever reproduce it.
/// Declining to cache is the honest outcome.
fn normalize_workdir(workdir: &Path, project_root: &Path, module_root: &Path) -> Option<String> {
    workdir
        .strip_prefix(project_root)
        .or_else(|_| workdir.strip_prefix(module_root))
        .ok()
        .map(path_to_forward_slashes)
}

fn collect_outputs(workdir: &Path, patterns: &[String]) -> Result<Vec<PathBuf>> {
    if patterns.is_empty() {
        return Ok(Vec::new());
    }

    let mut builder = GlobSetBuilder::new();
    let mut effective = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        let trimmed = pattern.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut glob_pattern = trimmed.to_string();
        let absolute = workdir.join(trimmed);
        if absolute.is_dir() && !looks_like_glob(trimmed) {
            glob_pattern = format!("{}/**/*", trimmed.trim_end_matches('/'));
        }

        let glob = Glob::new(&glob_pattern).map_err(|e| {
            cuenv_core::Error::configuration(format!("invalid output glob '{glob_pattern}': {e}"))
        })?;
        builder.add(glob);
        effective.push(glob_pattern);
    }

    if effective.is_empty() {
        return Ok(Vec::new());
    }

    let globset = builder.build().map_err(|e| {
        cuenv_core::Error::configuration(format!("failed to build output globset: {e}"))
    })?;

    let mut resolved = Vec::new();
    for root in output_walk_roots(workdir, &effective) {
        // A declared output that the task did not produce is ordinary — the
        // glob simply matches nothing — so a missing root is skipped rather
        // than reported.
        if !root.exists() {
            continue;
        }

        let walker = WalkDir::new(&root)
            .into_iter()
            // A concurrent cache hit stages its outputs inside this workdir.
            // Capturing another task's staging tree would record outputs this
            // task never produced.
            .filter_entry(|entry| !is_staging_dir(entry.path()));

        for entry in walker {
            let entry = entry.map_err(|e| {
                cuenv_core::Error::configuration(format!(
                    "walk output tree {}: {e}",
                    root.display()
                ))
            })?;
            if entry.file_type().is_dir() {
                continue;
            }

            let relative = entry.path().strip_prefix(workdir).map_err(|e| {
                cuenv_core::Error::configuration(format!(
                    "output path '{}' not under workdir '{}': {e}",
                    entry.path().display(),
                    workdir.display()
                ))
            })?;
            if globset.is_match(relative) {
                resolved.push(relative.to_path_buf());
            }
        }
    }

    resolved.sort();
    resolved.dedup();
    Ok(resolved)
}

fn looks_like_glob(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('{') || pattern.contains('?') || pattern.contains('[')
}

/// The directories that have to be walked to satisfy `patterns`.
///
/// A glob's literal prefix bounds where its matches can live, so a task
/// declaring `target/release/app` has no reason to walk `node_modules`.
/// Walking the whole working directory instead — which is what this used to
/// do — costs a full tree traversal on every recorded task, which is exactly
/// the cost a cache is supposed to avoid.
///
/// Roots that contain one another are collapsed so no subtree is walked
/// twice, and a pattern whose first segment is already a wildcard forces the
/// whole working directory, because nothing narrower is correct.
fn output_walk_roots(workdir: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let mut bases: Vec<PathBuf> = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        let mut base = PathBuf::new();
        for segment in pattern.split('/') {
            if looks_like_glob(segment) {
                break;
            }
            if !segment.is_empty() && segment != "." {
                base.push(segment);
            }
        }
        if base.as_os_str().is_empty() {
            // Unbounded: the whole working directory is in scope, and no
            // other root can narrow that.
            return vec![workdir.to_path_buf()];
        }
        bases.push(base);
    }

    // Sorting puts a parent immediately before its descendants, so a single
    // pass collapses them.
    bases.sort();
    bases.dedup();
    let mut roots: Vec<PathBuf> = Vec::with_capacity(bases.len());
    for base in bases {
        if roots.last().is_some_and(|kept| base.starts_with(kept)) {
            continue;
        }
        roots.push(base);
    }

    roots
        .into_iter()
        .map(|base| workdir.join(base))
        .collect()
}

fn path_to_forward_slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(unix)]
fn is_executable(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::metadata(path).map_err(|e| {
        cuenv_core::Error::configuration(format!("metadata {}: {e}", path.display()))
    })?;
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
        .map_err(|e| cuenv_core::Error::configuration(format!("metadata {}: {e}", path.display())))?
        .permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    std::fs::set_permissions(path, permissions).map_err(|e| {
        cuenv_core::Error::configuration(format!("set permissions {}: {e}", path.display()))
    })?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable_if_needed(_path: &Path, _is_executable: bool) -> Result<()> {
    Ok(())
}

/// Inputs to [`record`] grouped to keep call sites self-documenting.
#[derive(Clone, Copy)]
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
#[path = "cache_tests.rs"]
mod tests;
