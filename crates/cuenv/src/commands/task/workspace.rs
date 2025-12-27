//! Workspace setup and global task building
//!
//! Handles injecting workspace setup tasks (install hooks, setup dependencies)
//! and building the global task registry from all discovered projects.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use cuenv_core::Result;
use cuenv_core::manifest::{HookItem, Project};
use cuenv_core::tasks::discovery::{EvalFn, TaskDiscovery};
use cuenv_core::tasks::{TaskDefinition, TaskGroup, TaskIndex, Tasks};

use crate::commands::CommandExecutor;

use super::discovery::evaluate_manifest;
use super::normalization::{
    canonicalize_dep_for_task_name, compute_project_id, normalize_definition_deps,
    set_default_project_root, task_fqdn,
};
use super::resolution::{get_task_mut_by_name_or_path, resolve_task_refs_in_manifest};

/// Context for a project during global task building.
#[derive(Clone)]
pub struct ProjectCtx {
    pub root: PathBuf,
    pub id: String,
    pub manifest: Project,
    pub is_current: bool,
}

/// Inject workspace setup tasks into a project manifest.
///
/// For each enabled workspace that is referenced by tasks in the manifest:
/// 1. Expands `beforeInstall` hooks into concrete tasks
/// 2. Ensures a `<workspace>.setup` task exists
/// 3. Wires tasks that use the workspace to depend on setup
///
/// This creates the dependency chain: hooks -> install -> setup -> user tasks
#[allow(clippy::too_many_lines)]
pub fn inject_workspace_setup_tasks(
    manifest: &mut Project,
    discovery: &TaskDiscovery,
    manifest_project_id: &str,
) -> Result<()> {
    // NOTE: This is long because it needs to translate workspace config + hook steps into
    // concrete tasks while carefully avoiding dependency cycles.
    #[allow(clippy::too_many_lines)]
    fn add_setup_dep_to_definition(
        task_name: &str,
        task_def: &mut TaskDefinition,
        ws_name: &str,
        setup_task_name: &str,
    ) {
        // Avoid cycles: never make install/setup/hooks depend on setup.
        let install_name = format!("{ws_name}.install");
        let setup_name = format!("{ws_name}.setup");
        let hooks_prefix = format!("{ws_name}.hooks.");

        match task_def {
            TaskDefinition::Single(task) => {
                if !task
                    .workspaces
                    .as_ref()
                    .is_some_and(|ws| ws.iter().any(|w| w == ws_name))
                {
                    return;
                }

                if task_name == install_name
                    || task_name == setup_name
                    || task_name.starts_with(&hooks_prefix)
                {
                    return;
                }

                if !task.depends_on.contains(&setup_task_name.to_string()) {
                    task.depends_on.push(setup_task_name.to_string());
                }
            }
            TaskDefinition::Group(group) => match group {
                TaskGroup::Sequential(tasks) => {
                    for (i, sub_task) in tasks.iter_mut().enumerate() {
                        let sub_name = format!("{task_name}[{i}]");
                        add_setup_dep_to_definition(&sub_name, sub_task, ws_name, setup_task_name);
                    }
                }
                TaskGroup::Parallel(group) => {
                    for (name, sub_task) in &mut group.tasks {
                        let sub_name = format!("{task_name}.{name}");
                        add_setup_dep_to_definition(&sub_name, sub_task, ws_name, setup_task_name);
                    }
                }
            },
        }
    }

    let Some(workspaces) = &manifest.workspaces else {
        return Ok(());
    };

    // Clone to avoid borrow issues
    let workspaces = workspaces.clone();

    for (ws_name, config) in &workspaces {
        if !config.enabled {
            continue;
        }

        // Only inject if this workspace is actually referenced by tasks.
        let workspace_used = manifest
            .tasks
            .values()
            .any(|task_def| task_def.uses_workspace(ws_name));
        if !workspace_used {
            continue;
        }

        let install_task_name = format!("{ws_name}.install");
        let setup_task_name = format!("{ws_name}.setup");

        // Expand beforeInstall hook steps into concrete tasks.
        let mut all_hook_task_names: Vec<String> = Vec::new();
        let mut previous_step_task_names: Vec<String> = Vec::new();

        if let Some(hooks) = &config.hooks
            && let Some(before_install) = &hooks.before_install
        {
            for (step_idx, hook_item) in before_install.iter().enumerate() {
                match hook_item {
                    HookItem::Task(task) => {
                        let hook_task_name = format!("{ws_name}.hooks.beforeInstall[{step_idx}]");
                        let mut hook_task = task.as_ref().clone();
                        hook_task
                            .depends_on
                            .extend(previous_step_task_names.clone());
                        manifest.tasks.insert(
                            hook_task_name.clone(),
                            TaskDefinition::Single(Box::new(hook_task)),
                        );
                        all_hook_task_names.push(hook_task_name.clone());
                        previous_step_task_names = vec![hook_task_name];
                    }
                    HookItem::TaskRef(task_ref) => {
                        let hook_task_name = format!("{ws_name}.hooks.beforeInstall[{step_idx}]");
                        let mut hook_task = cuenv_core::tasks::Task::from_task_ref(&task_ref.ref_);
                        hook_task
                            .depends_on
                            .extend(previous_step_task_names.clone());
                        manifest.tasks.insert(
                            hook_task_name.clone(),
                            TaskDefinition::Single(Box::new(hook_task)),
                        );
                        all_hook_task_names.push(hook_task_name.clone());
                        previous_step_task_names = vec![hook_task_name];
                    }
                    HookItem::Match(match_hook) => {
                        let matched_tasks = discovery.match_tasks(&match_hook.matcher).map_err(|e| {
                            cuenv_core::Error::configuration(format!(
                                "Workspace '{ws_name}' beforeInstall matcher has invalid configuration: {e}"
                            ))
                        })?;

                        let step_name = match_hook
                            .name
                            .as_deref()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map_or_else(|| format!("match[{step_idx}]"), ToString::to_string);

                        if matched_tasks.is_empty() {
                            tracing::info!(
                                "Workspace '{}' beforeInstall matcher '{}' matched no tasks",
                                ws_name,
                                step_name
                            );
                        } else {
                            let matched_display: Vec<String> = matched_tasks
                                .iter()
                                .map(|m| {
                                    if let Some(project_name) = &m.project_name {
                                        format!("{project_name}:{}", m.task_name)
                                    } else {
                                        format!("{}:{}", m.project_root.display(), m.task_name)
                                    }
                                })
                                .collect();

                            tracing::info!(
                                "Workspace '{}' beforeInstall matcher '{}' matched {} task(s): {}",
                                ws_name,
                                step_name,
                                matched_display.len(),
                                matched_display.join(", ")
                            );
                        }

                        let mut step_task_names: Vec<String> = Vec::new();
                        let mut prev_in_step: Option<String> = None;

                        for (i, matched) in matched_tasks.iter().enumerate() {
                            let hook_task_name =
                                format!("{ws_name}.hooks.beforeInstall.{step_name}[{i}]");

                            let mut task = matched.task.clone();
                            task.project_root = Some(
                                fs::canonicalize(&matched.project_root)
                                    .unwrap_or_else(|_| matched.project_root.clone()),
                            );

                            // Canonicalize deps relative to the matched task name (not the synthetic hook name)
                            task.depends_on = task
                                .depends_on
                                .iter()
                                .map(|d| canonicalize_dep_for_task_name(d, &matched.task_name))
                                .collect();

                            // Ensure this step runs after previous hook step(s). These deps live in the
                            // current project even though this task executes in a matched project_root.
                            for dep in &previous_step_task_names {
                                task.depends_on.push(task_fqdn(manifest_project_id, dep));
                            }

                            // Respect matcher.parallel: chain within this step if parallel == false
                            if !match_hook.matcher.parallel {
                                if let Some(prev_name) = &prev_in_step {
                                    task.depends_on
                                        .push(task_fqdn(manifest_project_id, prev_name));
                                }
                                prev_in_step = Some(hook_task_name.clone());
                            }

                            manifest.tasks.insert(
                                hook_task_name.clone(),
                                TaskDefinition::Single(Box::new(task)),
                            );
                            all_hook_task_names.push(hook_task_name.clone());
                            step_task_names.push(hook_task_name);
                        }

                        // Next step depends on all tasks from this step
                        previous_step_task_names = step_task_names;
                    }
                }
            }
        }

        // Wire: hooks -> install
        if !all_hook_task_names.is_empty()
            && let Some(install_task) =
                get_task_mut_by_name_or_path(&mut manifest.tasks, &install_task_name)
        {
            for hook_name in &all_hook_task_names {
                if !install_task.depends_on.contains(hook_name) {
                    install_task.depends_on.push(hook_name.clone());
                }
            }
        }

        // Ensure <ws>.setup exists
        if !manifest.tasks.contains_key(&setup_task_name) {
            let setup_task = cuenv_core::tasks::Task {
                command: String::new(),
                script: Some("true".to_string()),
                hermetic: false,
                depends_on: vec![install_task_name.clone()],
                ..Default::default()
            };
            manifest.tasks.insert(
                setup_task_name.clone(),
                TaskDefinition::Single(Box::new(setup_task)),
            );
        }

        // Wire: any task that uses this workspace -> <ws>.setup
        for (task_name, task_def) in &mut manifest.tasks {
            add_setup_dep_to_definition(task_name, task_def, ws_name, &setup_task_name);
        }
    }

    Ok(())
}

/// Build the global task registry from all discovered projects.
///
/// This function:
/// 1. Discovers all projects in the module
/// 2. Assigns unique IDs to each project
/// 3. Injects workspace setup tasks
/// 4. Resolves `TaskRef` placeholders
/// 5. Normalizes all dependencies to FQDNs
/// 6. Builds a unified task registry keyed by FQDN
///
/// Returns the global tasks and the current project's ID.
#[allow(clippy::too_many_lines)]
pub fn build_global_tasks(
    module_root: &Path,
    current_project_root: &Path,
    current_manifest: &Project,
    executor: Option<&CommandExecutor>,
) -> Result<(Tasks, String)> {
    let mut discovery = TaskDiscovery::new(module_root.to_path_buf());

    // Note: We intentionally don't use the executor's cached module here.
    // The executor evaluates with recursive: true, which can fail when the CLI
    // package differs from some subdirectories. Instead, we use fresh per-project
    // evaluation with the CLI-specified package.
    let package = executor.map_or("cuenv", |e| e.package()).to_string();
    tracing::debug!(
        "Using EvalFn for global task registry build (package: {})",
        package
    );
    let eval_fn: EvalFn =
        Box::new(move |project_path: &Path| evaluate_manifest(project_path, &package, None));

    discovery = discovery.with_eval_fn(eval_fn);
    discovery.discover().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to discover projects: {e}"))
    })?;

    let current_root = fs::canonicalize(current_project_root)
        .unwrap_or_else(|_| current_project_root.to_path_buf());

    // Build project contexts
    let mut project_id_by_name: HashMap<String, String> = HashMap::new();
    let mut used_project_ids: HashSet<String> = HashSet::new();
    let mut projects: Vec<ProjectCtx> = Vec::new();
    for p in discovery.projects() {
        let root = fs::canonicalize(&p.project_root).unwrap_or_else(|_| p.project_root.clone());
        let is_current = root == current_root;
        let mut manifest = if is_current {
            current_manifest.clone()
        } else {
            p.manifest.clone()
        };
        manifest = manifest.with_implicit_tasks();

        // Prefer the explicit `name` field for stable cross-project refs, but ensure uniqueness.
        let base_id = compute_project_id(&manifest, &root, module_root);
        let mut id = base_id.clone();
        if used_project_ids.contains(&id) {
            // Disambiguate collisions (common in repos that layer multiple env.cue files for the same package)
            // by suffixing with a path-derived identifier.
            let rel = root
                .strip_prefix(module_root)
                .unwrap_or(&root)
                .to_string_lossy()
                .replace(['/', '\\'], ".");
            let mut candidate = format!("{base_id}.{rel}");
            let mut i = 2;
            while used_project_ids.contains(&candidate) {
                candidate = format!("{base_id}.{rel}.{i}");
                i += 1;
            }
            id = candidate;
        }
        used_project_ids.insert(id.clone());

        // Map manifest `name` (used by TaskRef: "#name:task") to this unique project id.
        let trimmed = manifest.name.trim();
        if !trimmed.is_empty() {
            project_id_by_name
                .entry(trimmed.to_string())
                .or_insert_with(|| id.clone());
        }
        projects.push(ProjectCtx {
            root,
            id,
            manifest,
            is_current,
        });
    }

    // Index roots -> ids (used to scope relative dependencies by task.project_root)
    let mut id_by_root: HashMap<PathBuf, String> = HashMap::new();
    for p in &projects {
        id_by_root.insert(p.root.clone(), p.id.clone());
    }

    let current_project_id = projects.iter().find(|p| p.is_current).map_or_else(
        || compute_project_id(current_manifest, &current_root, module_root),
        |p| p.id.clone(),
    );

    // Inject workspace setup tasks and resolve TaskRefs (hooks)
    for p in &mut projects {
        inject_workspace_setup_tasks(&mut p.manifest, &discovery, &p.id)?;
        resolve_task_refs_in_manifest(&mut p.manifest, &discovery, &p.id, &project_id_by_name);
    }

    // Build global tasks keyed by FQDN
    let mut global: HashMap<String, TaskDefinition> = HashMap::new();
    for p in &projects {
        let idx = TaskIndex::build(&p.manifest.tasks)?;
        for entry in idx.list() {
            let mut def = entry.definition.clone();
            set_default_project_root(&mut def, &p.root);
            normalize_definition_deps(&mut def, &id_by_root, &project_id_by_name, &p.id);
            let fqdn = task_fqdn(&p.id, &entry.name);
            if global.contains_key(&fqdn) {
                return Err(cuenv_core::Error::configuration(format!(
                    "Duplicate task FQDN detected: '{fqdn}'",
                )));
            }
            global.insert(fqdn, def);
        }
    }

    Ok((Tasks { tasks: global }, current_project_id))
}
