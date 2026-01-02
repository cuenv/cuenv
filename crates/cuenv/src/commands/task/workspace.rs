//! Workspace setup and global task building
//!
//! Handles auto-detecting workspaces from lockfiles and injecting setup tasks.
//! Workspaces are detected from:
//! - `bun.lock` → Bun
//! - `package-lock.json` → npm
//! - `pnpm-lock.yaml` → pnpm
//! - `yarn.lock` → Yarn
//! - `Cargo.lock` → Cargo
//! - `deno.lock` → Deno
//!
//! For each detected workspace, we inject:
//! - `{manager}.install` task (runs the package manager's install command)
//! - `{manager}.setup` task (depends on install, used as dependency anchor)
//!
//! Tasks are auto-associated by command: if a task uses `bun` and we detected
//! a Bun workspace, the task automatically depends on `bun.setup`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use cuenv_core::manifest::Project;
use cuenv_core::tasks::discovery::{EvalFn, TaskDiscovery};
use cuenv_core::tasks::{Input, Task, TaskDefinition, TaskGroup, TaskIndex, Tasks};
use cuenv_core::Result;
use cuenv_workspaces::{detect_from_command, detect_package_managers, PackageManager};

use crate::commands::CommandExecutor;

use super::discovery::evaluate_manifest;
use super::normalization::{
    compute_project_id, normalize_definition_deps, set_default_project_root, task_fqdn,
};
use super::resolution::resolve_task_refs_in_manifest;

/// Context for a project during global task building.
#[derive(Clone)]
pub struct ProjectCtx {
    pub root: PathBuf,
    pub id: String,
    pub manifest: Project,
    pub is_current: bool,
}

/// Install task configuration for a package manager.
struct InstallTaskConfig {
    command: &'static str,
    args: Vec<&'static str>,
    inputs: Vec<&'static str>,
    outputs: Vec<&'static str>,
}

/// Returns the install task configuration for a package manager.
fn install_config_for_manager(manager: PackageManager) -> InstallTaskConfig {
    match manager {
        PackageManager::Npm => InstallTaskConfig {
            command: "npm",
            args: vec!["install"],
            inputs: vec!["package.json", "package-lock.json"],
            outputs: vec!["node_modules"],
        },
        PackageManager::Bun => InstallTaskConfig {
            command: "bun",
            args: vec!["install"],
            inputs: vec!["package.json", "bun.lock"],
            outputs: vec!["node_modules"],
        },
        PackageManager::Pnpm => InstallTaskConfig {
            command: "pnpm",
            args: vec!["install"],
            inputs: vec!["package.json", "pnpm-lock.yaml"],
            outputs: vec!["node_modules"],
        },
        PackageManager::YarnClassic | PackageManager::YarnModern => InstallTaskConfig {
            command: "yarn",
            args: vec!["install"],
            inputs: vec!["package.json", "yarn.lock"],
            outputs: vec!["node_modules"],
        },
        PackageManager::Cargo => InstallTaskConfig {
            command: "cargo",
            args: vec!["fetch"],
            inputs: vec!["Cargo.toml", "Cargo.lock"],
            outputs: vec![],
        },
        PackageManager::Deno => InstallTaskConfig {
            command: "deno",
            args: vec!["cache", "**/*.ts"],
            inputs: vec!["deno.json", "deno.lock"],
            outputs: vec![],
        },
    }
}

/// Returns the canonical workspace name for a package manager.
fn workspace_name_for_manager(manager: PackageManager) -> &'static str {
    match manager {
        PackageManager::Npm => "npm",
        PackageManager::Bun => "bun",
        PackageManager::Pnpm => "pnpm",
        PackageManager::YarnClassic | PackageManager::YarnModern => "yarn",
        PackageManager::Cargo => "cargo",
        PackageManager::Deno => "deno",
    }
}

/// Inject workspace setup tasks based on auto-detection from lockfiles.
///
/// This function:
/// 1. Detects package managers from lockfiles in the project directory
/// 2. For each detected manager, creates `{manager}.install` and `{manager}.setup` tasks
/// 3. Auto-associates tasks by command: if a task uses `bun`, it depends on `bun.setup`
///
/// This replaces the old explicit `workspaces:` configuration approach.
pub fn inject_detected_workspace_tasks(manifest: &mut Project, project_root: &Path) {
    // Detect package managers from lockfiles
    let detected_managers = detect_package_managers(project_root).unwrap_or_else(|e| {
        tracing::debug!(
            "Failed to detect package managers in {}: {}",
            project_root.display(),
            e
        );
        Vec::new()
    });

    if detected_managers.is_empty() {
        return;
    }

    tracing::debug!(
        "Detected package managers in {}: {:?}",
        project_root.display(),
        detected_managers
    );

    // Track which workspace names we've injected (to avoid duplicates for yarn variants)
    let mut injected_workspaces: HashSet<&str> = HashSet::new();

    for manager in &detected_managers {
        let ws_name = workspace_name_for_manager(*manager);

        // Skip if we've already injected this workspace (e.g., YarnClassic and YarnModern both map to "yarn")
        if injected_workspaces.contains(ws_name) {
            continue;
        }
        injected_workspaces.insert(ws_name);

        let install_task_name = format!("{ws_name}.install");
        let setup_task_name = format!("{ws_name}.setup");

        // Create the install task if it doesn't exist
        if !manifest.tasks.contains_key(&install_task_name) {
            let config = install_config_for_manager(*manager);
            let install_task = Task {
                command: config.command.to_string(),
                args: config.args.iter().map(|s| (*s).to_string()).collect(),
                hermetic: false,
                inputs: config
                    .inputs
                    .iter()
                    .map(|s| Input::Path((*s).to_string()))
                    .collect(),
                outputs: config.outputs.iter().map(|s| (*s).to_string()).collect(),
                description: Some(format!("Install {ws_name} dependencies")),
                ..Default::default()
            };
            manifest.tasks.insert(
                install_task_name.clone(),
                TaskDefinition::Single(Box::new(install_task)),
            );
            tracing::debug!("Injected task: {}", install_task_name);
        }

        // Create the setup task if it doesn't exist
        if !manifest.tasks.contains_key(&setup_task_name) {
            let setup_task = Task {
                command: String::new(),
                script: Some("true".to_string()),
                hermetic: false,
                depends_on: vec![install_task_name.clone()],
                description: Some(format!("{ws_name} workspace setup complete")),
                ..Default::default()
            };
            manifest.tasks.insert(
                setup_task_name.clone(),
                TaskDefinition::Single(Box::new(setup_task)),
            );
            tracing::debug!("Injected task: {}", setup_task_name);
        }
    }

    // Auto-associate tasks by command: if a task's command matches a detected manager,
    // add a dependency on the corresponding setup task
    let ws_names: Vec<&str> = injected_workspaces.iter().copied().collect();
    for (task_name, task_def) in &mut manifest.tasks {
        auto_associate_task_by_command(task_name, task_def, &ws_names);
    }
}

/// Auto-associate a task with workspace setup based on its command.
///
/// If the task's command matches a detected package manager (e.g., "bun run dev"),
/// we add a dependency on the corresponding setup task (e.g., "bun.setup").
fn auto_associate_task_by_command(
    task_name: &str,
    task_def: &mut TaskDefinition,
    workspace_names: &[&str],
) {
    match task_def {
        TaskDefinition::Single(task) => {
            // Skip synthetic install/setup tasks to avoid cycles
            for ws_name in workspace_names {
                if task_name == format!("{ws_name}.install")
                    || task_name == format!("{ws_name}.setup")
                {
                    return;
                }
            }

            // Detect package manager from command
            let Some(detected_manager) = detect_from_command(&task.command) else {
                return;
            };

            let ws_name = workspace_name_for_manager(detected_manager);

            // Only add dependency if this workspace was detected
            if !workspace_names.contains(&ws_name) {
                return;
            }

            let setup_task_name = format!("{ws_name}.setup");

            // Add dependency if not already present
            if !task.depends_on.contains(&setup_task_name) {
                task.depends_on.push(setup_task_name);
                tracing::debug!(
                    "Auto-associated task '{}' with workspace '{}'",
                    task_name,
                    ws_name
                );
            }
        }
        TaskDefinition::Group(group) => match group {
            TaskGroup::Sequential(tasks) => {
                for (i, sub_task) in tasks.iter_mut().enumerate() {
                    let sub_name = format!("{task_name}[{i}]");
                    auto_associate_task_by_command(&sub_name, sub_task, workspace_names);
                }
            }
            TaskGroup::Parallel(group) => {
                for (name, sub_task) in &mut group.tasks {
                    let sub_name = format!("{task_name}.{name}");
                    auto_associate_task_by_command(&sub_name, sub_task, workspace_names);
                }
            }
        },
    }
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

    // Use executor's cached module if available (single evaluation for all projects).
    // All projects must use `package cuenv` - this is enforced by the CUE schema.
    if let Some(exec) = executor {
        tracing::debug!("Using cached module for global task registry build");
        let module = exec.get_module(module_root)?;

        // Iterate through all Project instances and add them directly
        for instance in module.projects() {
            match instance.deserialize::<Project>() {
                Ok(project) => {
                    let project_root = module.root.join(&instance.path);
                    discovery.add_project(project_root, project);
                }
                Err(e) => {
                    tracing::warn!(
                        path = %instance.path.display(),
                        error = %e,
                        "Failed to deserialize project for global task registry"
                    );
                }
            }
        }
    } else {
        // Legacy path: use EvalFn for per-project evaluation (when no executor available)
        tracing::debug!("Using legacy EvalFn for global task registry build");
        let eval_fn: EvalFn =
            Box::new(move |project_path: &Path| evaluate_manifest(project_path, "cuenv", None));

        discovery = discovery.with_eval_fn(eval_fn);
        discovery.discover().map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to discover projects: {e}"))
        })?;
    }

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

    // Inject workspace setup tasks (auto-detected from lockfiles) and resolve TaskRefs
    for p in &mut projects {
        inject_detected_workspace_tasks(&mut p.manifest, &p.root);
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
