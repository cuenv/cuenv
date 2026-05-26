//! Hook-backed environment assembly for CI task execution.

use cuenv_core::Result;
use cuenv_core::cue::discovery::find_ancestor_env_files;
use cuenv_core::manifest::Project;
use cuenv_hooks::{
    ExecutionStatus, HookExecutionConfig, HookExecutionState, StateManager, compute_instance_hash,
    execute_hooks,
};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Build the project environment by merging static env with hook-generated values.
pub(super) async fn build_hook_environment(
    project_root: &Path,
    config: &Project,
    project_configs: &HashMap<PathBuf, Project>,
) -> Result<BTreeMap<String, String>> {
    let static_env = extract_static_env_vars(config);
    let hooks = collect_hooks_from_ancestors(project_root, config, project_configs)?;

    if hooks.is_empty() {
        return Ok(static_env);
    }

    let config_hash = cuenv_hooks::compute_execution_hash(&hooks, project_root);
    let instance_hash = compute_instance_hash(project_root, &config_hash);

    let state_dir = if let Ok(dir) = std::env::var("CUENV_STATE_DIR") {
        PathBuf::from(dir)
    } else {
        StateManager::default_state_dir()?
    };
    let state_manager = StateManager::new(state_dir);

    let hook_config = HookExecutionConfig {
        default_timeout_seconds: 600,
        fail_fast: true,
        state_dir: None,
    };

    let mut state = HookExecutionState::new(
        project_root.to_path_buf(),
        instance_hash,
        config_hash,
        hooks.clone(),
    );

    execute_hooks(
        hooks,
        project_root,
        &hook_config,
        &state_manager,
        &mut state,
    )
    .await?;

    match state.status {
        ExecutionStatus::Completed | ExecutionStatus::Failed => {
            Ok(collect_all_env_vars(config, &state.environment_vars))
        }
        ExecutionStatus::Running | ExecutionStatus::Cancelled => Ok(static_env),
    }
}

/// Collect `onEnter` hooks from ancestor env.cue files (root-to-leaf order).
fn collect_hooks_from_ancestors(
    project_root: &Path,
    config: &Project,
    project_configs: &HashMap<PathBuf, Project>,
) -> Result<Vec<cuenv_hooks::Hook>> {
    let ancestors = find_ancestor_env_files(project_root, "cuenv")?;
    let ancestors_len = ancestors.len();
    let mut all_hooks = Vec::new();

    for (idx, ancestor_dir) in ancestors.into_iter().enumerate() {
        let is_current_dir = idx + 1 == ancestors_len;
        let source_config = if is_current_dir {
            Some(config)
        } else {
            project_configs.get(&ancestor_dir).or_else(|| {
                ancestor_dir
                    .canonicalize()
                    .ok()
                    .and_then(|canonical| project_configs.get(&canonical))
            })
        };

        let Some(source_config) = source_config else {
            continue;
        };

        let mut hooks = source_config.on_enter_hooks();
        for hook in &mut hooks {
            resolve_hook_dir(hook, &ancestor_dir);
        }

        if !is_current_dir {
            hooks.retain(|hook| hook.propagate);
        }

        all_hooks.extend(hooks);
    }

    Ok(all_hooks)
}

/// Resolve hook.dir relative to the env.cue directory where the hook is defined.
fn resolve_hook_dir(hook: &mut cuenv_hooks::Hook, env_cue_dir: &Path) {
    let relative_dir = hook.dir.as_deref().unwrap_or(".");
    let absolute_dir = env_cue_dir.join(relative_dir);
    let resolved = absolute_dir.canonicalize().unwrap_or(absolute_dir);
    hook.dir = Some(resolved.to_string_lossy().to_string());
}

/// Extract static (non-secret) environment variables from config.
fn extract_static_env_vars(config: &Project) -> BTreeMap<String, String> {
    let mut env_vars = BTreeMap::new();
    if let Some(env) = &config.env {
        for (key, value) in &env.base {
            if value.is_secret() {
                continue;
            }
            env_vars.insert(key.clone(), value.to_string_value());
        }
    }
    env_vars
}

/// Merge static config env vars with hook-generated values (hooks win).
fn collect_all_env_vars(
    config: &Project,
    hook_env: &std::collections::HashMap<String, String>,
) -> BTreeMap<String, String> {
    let mut merged = extract_static_env_vars(config);
    for (key, value) in hook_env {
        merged.insert(key.clone(), value.clone());
    }
    merged
}
