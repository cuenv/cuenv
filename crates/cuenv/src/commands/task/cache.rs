//! The task cache a run uses: where it lives, whether it is enabled, and the
//! runtime identity its keys include.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cuenv_core::lockfile::{LOCKFILE_NAME, LockedRuntime, Lockfile};
use cuenv_core::manifest::Runtime;
use cuenv_core::tasks::TaskCacheMode;
use cuenv_task_exec::cache::TaskCacheConfig;

use super::remote_cache;
use crate::commands::relative_path_from_root;

/// Resolve the on-disk root for the local CAS + action cache.
///
/// Resolution order:
/// 1. `$CUENV_CACHE_DIR` (explicit override)
/// 2. `$XDG_CACHE_HOME/cuenv` or the platform default
/// 3. `<project>/.cuenv-cache`
pub(super) fn resolve_cache_root(project_root: &Path) -> PathBuf {
    if let Some(env) = std::env::var_os("CUENV_CACHE_DIR")
        && !env.is_empty()
    {
        return PathBuf::from(env);
    }
    if let Some(d) = dirs::cache_dir() {
        return d.join("cuenv");
    }
    project_root.join(".cuenv-cache")
}

/// Everything `build_task_cache` needs that is not the cache's own settings.
pub(super) struct TaskCacheContext<'a> {
    /// Root of the project whose task is running.
    pub(super) project_root: &'a Path,
    /// Root of the VCS workspace, which bounds path-shaped project references.
    pub(super) hasher_root: &'a Path,
    /// Project roots by name and by module-relative path.
    pub(super) project_roots: BTreeMap<String, PathBuf>,
    /// Runtime identity folded into every action key.
    pub(super) runtime_identity: RuntimeCacheIdentity,
}

/// Construct the [`TaskCacheConfig`] used by the executor.
///
/// Returns `None` if the local CAS or action cache cannot be opened (e.g.
/// permissions). In that case the executor falls back to the no-cache code
/// path so the user's command still works — degraded, not broken.
pub(super) async fn build_task_cache(
    context: TaskCacheContext<'_>,
    cache_config: Option<&cuenv_core::manifest::Cache>,
) -> Option<TaskCacheConfig> {
    let TaskCacheContext {
        project_root,
        hasher_root,
        project_roots,
        runtime_identity,
    } = context;
    let cache_override = cache_override();
    let cache_off = cache_override == CacheOverride::Off;
    if cache_off {
        tracing::debug!(
            "task result cache disabled by CUENV_CACHE=off; input isolation remains enabled"
        );
    }

    let root = resolve_cache_root(project_root);
    let cas = match cuenv_cas::LocalCas::open(&root) {
        Ok(c) => Arc::new(c) as Arc<dyn cuenv_cas::Cas>,
        Err(e) => {
            tracing::warn!(error = %e, root = %root.display(), "task cache disabled: cannot open CAS");
            return None;
        }
    };
    let action_cache = match cuenv_cas::LocalActionCache::open(&root) {
        Ok(ac) => Arc::new(ac) as Arc<dyn cuenv_cas::ActionCache>,
        Err(e) => {
            tracing::warn!(error = %e, root = %root.display(), "task cache disabled: cannot open action cache");
            return None;
        }
    };
    // Stack a remote cache behind the local one when configured. Every
    // failure in here degrades to local-only: a cache is an optimization, so
    // an unreachable server should make a build slower, not broken.
    let layers = if cache_off {
        remote_cache::CacheLayers { cas, action_cache }
    } else {
        remote_cache::build(cache_config, cas, action_cache).await
    };

    // Rooted at the VCS workspace, not the project: a task may declare an
    // input in a sibling CUE module, and a hasher that cannot see outside its
    // own module can only answer such a reference by declining to cache.
    // Patterns are prefixed with each project's workspace-relative path before
    // they reach the walker, so this widens what is reachable without widening
    // what is walked.
    // The store — and the exec roots materialized inside it — may sit inside
    // the workspace (`<project>/.cuenv-cache`, or `CUENV_CACHE_DIR` pointed
    // at the checkout). Its contents are never anyone's inputs.
    let vcs_hasher = Arc::new(cuenv_vcs::WalkHasher::new(hasher_root).excluding(&root))
        as Arc<dyn cuenv_vcs::VcsHasher>;
    Some(TaskCacheConfig {
        cas: layers.cas,
        action_cache: layers.action_cache,
        vcs_hasher,
        vcs_hasher_root: hasher_root.to_path_buf(),
        cache_root: root,
        project_roots,
        action_semantics_version: cuenv_cas::ACTION_SEMANTICS_VERSION,
        runtime_identity_properties: runtime_identity.properties,
        cache_disabled_reason: if cache_off {
            Some("disabled by CUENV_CACHE=off".to_string())
        } else {
            runtime_identity.cache_disabled_reason
        },
        secret_salt: secret_cache_salt(),
        mode_override: match cache_override {
            CacheOverride::None | CacheOverride::Off => None,
            CacheOverride::ReadOnly => Some(TaskCacheMode::Read),
            CacheOverride::WriteOnly => Some(TaskCacheMode::Write),
        },
    })
}

/// How `CUENV_CACHE` overrides the per-task cache policy for a whole run.
///
/// Without this the only way to bust a bad entry is editing CUE, which is a
/// poor answer when a cache is misbehaving and you want to know whether the
/// cache is the reason. moon has `MOON_CACHE` for the same purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CacheOverride {
    /// Honour each task's declared policy.
    None,
    /// Ignore the cache entirely for this run.
    Off,
    /// Read existing entries but record nothing.
    ReadOnly,
    /// Ignore existing entries but record fresh ones — the way to refresh a
    /// poisoned entry without discarding the whole store.
    WriteOnly,
}

/// Parse `CUENV_CACHE`.
///
/// Accepts `off`/`false`/`0`, `read`, `write`, and `read-write`/`on`/`true`.
/// An unrecognised value warns and is ignored rather than failing the run: a
/// typo in an environment variable should not stop a build.
pub(super) fn cache_override() -> CacheOverride {
    let Ok(raw) = std::env::var("CUENV_CACHE") else {
        return CacheOverride::None;
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "off" | "false" | "0" | "none" => CacheOverride::Off,
        "read" | "read-only" => CacheOverride::ReadOnly,
        "write" | "write-only" => CacheOverride::WriteOnly,
        // Empty and the explicit "on" spellings both mean "leave each task's
        // declared policy alone".
        "" | "read-write" | "rw" | "on" | "true" | "1" => CacheOverride::None,
        other => {
            tracing::warn!(
                value = other,
                "ignoring unrecognised CUENV_CACHE; expected off, read, write or read-write"
            );
            CacheOverride::None
        }
    }
}

/// Salt used to fingerprint secret-derived environment values into action
/// keys.
///
/// Read from `CUENV_SECRET_SALT`, the same variable the CI secret pipeline
/// already uses. An empty value counts as unset: an empty salt would make
/// fingerprints trivially reversible by anyone who can read the store.
pub(super) fn secret_cache_salt() -> Option<String> {
    std::env::var("CUENV_SECRET_SALT")
        .ok()
        .filter(|salt| !salt.is_empty())
}

#[derive(Debug, Clone, Default)]
pub(super) struct RuntimeCacheIdentity {
    pub(super) properties: BTreeMap<String, String>,
    pub(super) cache_disabled_reason: Option<String>,
}

pub(super) fn resolve_runtime_cache_identity(
    module_root: &Path,
    project_root: &Path,
    runtime: Option<&Runtime>,
) -> RuntimeCacheIdentity {
    let mut identity = RuntimeCacheIdentity::default();
    let Some(runtime) = runtime else {
        return identity;
    };

    match runtime {
        Runtime::Nix(nix_runtime) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "nix".to_string());

            let lockfile_path = module_root.join(LOCKFILE_NAME);
            let lockfile = match Lockfile::load(&lockfile_path) {
                Ok(Some(lockfile)) => lockfile,
                Ok(None) => {
                    identity.cache_disabled_reason = Some(format!(
                        "runtime is nix but {} is missing",
                        lockfile_path.display()
                    ));
                    return identity;
                }
                Err(e) => {
                    identity.cache_disabled_reason = Some(format!(
                        "runtime is nix but {} could not be read: {}",
                        lockfile_path.display(),
                        e
                    ));
                    return identity;
                }
            };

            let project_path = relative_path_from_root(module_root, project_root);
            let project_key = project_path.to_string_lossy().into_owned();
            let Some(locked_runtime) = lockfile.find_runtime(&project_key) else {
                identity.cache_disabled_reason = Some(format!(
                    "runtime is nix but lockfile has no runtime entry for project '{}'",
                    project_key
                ));
                return identity;
            };

            let LockedRuntime::Nix(locked_nix) = locked_runtime;

            if locked_nix.flake != nix_runtime.flake || locked_nix.output != nix_runtime.output {
                identity.cache_disabled_reason = Some(format!(
                    "runtime lock mismatch for project '{}': expected flake='{}' output='{}', got flake='{}' output='{}'",
                    project_key,
                    nix_runtime.flake,
                    nix_runtime.output.as_deref().unwrap_or(""),
                    locked_nix.flake,
                    locked_nix.output.as_deref().unwrap_or("")
                ));
                return identity;
            }

            identity
                .properties
                .insert("runtime.nix.digest".to_string(), locked_nix.digest.clone());
            identity
                .properties
                .insert("runtime.nix.flake".to_string(), locked_nix.flake.clone());
            if let Some(output) = &locked_nix.output {
                identity
                    .properties
                    .insert("runtime.nix.output".to_string(), output.clone());
            }
            identity.properties.insert(
                "runtime.nix.lockfile".to_string(),
                locked_nix.lockfile.clone(),
            );
            identity
        }
        Runtime::Devenv(_) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "devenv".to_string());
            identity
        }
        Runtime::Container(_) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "container".to_string());
            identity
        }
        Runtime::Dagger(_) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "dagger".to_string());
            identity
        }
        Runtime::Oci(_) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "oci".to_string());
            identity
        }
        Runtime::Tools(_) => {
            identity
                .properties
                .insert("runtime.kind".to_string(), "tools".to_string());
            identity
        }
    }
}
