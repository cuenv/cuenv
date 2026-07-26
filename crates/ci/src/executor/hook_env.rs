//! Hook-backed environment assembly for CI task execution.

use cuenv_core::Result;
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
    let hooks = collect_hooks_from_ancestors(project_root, config, project_configs);

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

/// Collect `onEnter` hooks from evaluated ancestor projects (root-to-leaf order).
fn collect_hooks_from_ancestors(
    project_root: &Path,
    config: &Project,
    project_configs: &HashMap<PathBuf, Project>,
) -> Vec<cuenv_hooks::Hook> {
    let current_dir = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut ancestors = project_configs
        .iter()
        .filter_map(|(path, ancestor_config)| {
            let normalized = path.canonicalize().unwrap_or_else(|_| path.clone());
            (normalized != current_dir && current_dir.starts_with(&normalized))
                .then_some((normalized, ancestor_config))
        })
        .collect::<Vec<_>>();
    ancestors.sort_by(|(left, _), (right, _)| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });
    ancestors.dedup_by(|(left, _), (right, _)| left == right);

    let mut all_hooks = Vec::new();

    for (ancestor_dir, ancestor_config) in ancestors {
        let mut hooks = ancestor_config.on_enter_hooks();
        for hook in &mut hooks {
            resolve_hook_dir(hook, &ancestor_dir);
        }
        hooks.retain(|hook| hook.propagate);
        all_hooks.extend(hooks);
    }

    let mut current_hooks = config.on_enter_hooks();
    for hook in &mut current_hooks {
        resolve_hook_dir(hook, &current_dir);
    }
    all_hooks.extend(current_hooks);

    all_hooks
}

/// Resolve hook.dir relative to the evaluated project directory.
fn resolve_hook_dir(hook: &mut cuenv_hooks::Hook, project_dir: &Path) {
    let relative_dir = hook.dir.as_deref().unwrap_or(".");
    let absolute_dir = project_dir.join(relative_dir);
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

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_hooks::{Hook, Hooks};
    use std::error::Error;
    use std::fs;
    use tempfile::Builder;

    type TestResult = std::result::Result<(), Box<dyn Error>>;

    fn hook(command: &str, propagate: bool) -> Hook {
        Hook {
            order: 100,
            propagate,
            command: command.to_string(),
            args: Vec::new(),
            dir: None,
            inputs: Vec::new(),
            source: None,
        }
    }

    fn project_with_hook(name: &str, command: &str, propagate: bool) -> Project {
        let mut project = Project::new(name);
        project.hooks = Some(Hooks {
            on_enter: Some(HashMap::from([(
                command.to_string(),
                hook(command, propagate),
            )])),
            ..Default::default()
        });
        project
    }

    #[test]
    fn collects_current_hooks_without_an_env_cue_filename() -> TestResult {
        let temp = Builder::new()
            .prefix("cuenv-ci-hook-arbitrary-file-")
            .tempdir()?;
        fs::write(
            temp.path().join("project.cue"),
            "package cuenv\nname: \"arbitrary\"\n",
        )?;
        let config = project_with_hook("arbitrary", "current", false);

        let hooks = collect_hooks_from_ancestors(temp.path(), &config, &HashMap::new());
        let expected_dir = temp.path().canonicalize()?.to_string_lossy().into_owned();

        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].command, "current");
        assert_eq!(hooks[0].dir.as_deref(), Some(expected_dir.as_str()));

        Ok(())
    }

    #[test]
    fn propagates_only_propagating_hooks_from_evaluated_ancestor_projects() -> TestResult {
        let temp = Builder::new()
            .prefix("cuenv-ci-hook-ancestors-")
            .tempdir()?;
        let child = temp.path().join("child");
        fs::create_dir_all(&child)?;

        let parent = project_with_hook("parent", "parent", true);
        let current = project_with_hook("child", "current", false);
        let configs = HashMap::from([(temp.path().canonicalize()?, parent)]);

        let hooks = collect_hooks_from_ancestors(&child, &current, &configs);

        assert_eq!(
            hooks
                .iter()
                .map(|hook| hook.command.as_str())
                .collect::<Vec<_>>(),
            ["parent", "current"]
        );

        Ok(())
    }
}
