//! Workspace setup and global task building
//!
//! Uses the ContributorEngine to inject workspace setup tasks based on auto-detection.
//! Workspaces are detected from lockfiles (bun.lock, package-lock.json, etc.)
//!
//! Contributors inject tasks with the `cuenv:contributor:` prefix:
//! - `cuenv:contributor:{manager}.workspace.install` (package manager install)
//! - `cuenv:contributor:{manager}.workspace.setup` (depends on install, dependency anchor)
//!
//! Tasks are auto-associated by command: if a task uses `bun` and we detected
//! a Bun workspace, the task automatically depends on `cuenv:contributor:bun.workspace.setup`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use cuenv_core::Result;
use cuenv_core::contributors::{
    ContributorContext, ContributorEngine, builtin_workspace_contributors,
};
use cuenv_core::manifest::Project;
use cuenv_core::tasks::{TaskDefinition, TaskIndex, Tasks};
use cuenv_task_discovery::{EvalFn, TaskDiscovery};

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

/// Apply workspace contributors to inject setup tasks.
///
/// Uses the ContributorEngine to:
/// 1. Detect package managers from lockfiles
/// 2. Inject `cuenv:contributor:{manager}.workspace.install` and `.setup` tasks
/// 3. Auto-associate user tasks by command
pub fn apply_workspace_contributors(manifest: &mut Project, project_root: &Path) {
    // Create context with workspace detection
    let context = ContributorContext::detect(project_root).with_task_commands(&manifest.tasks);

    // Get built-in workspace contributors
    let contributors = builtin_workspace_contributors();

    // Apply contributors using the engine
    let engine = ContributorEngine::new(&contributors, context);
    match engine.apply(&mut manifest.tasks) {
        Ok(injected) => {
            if injected > 0 {
                tracing::debug!(
                    "ContributorEngine injected {} tasks in {}",
                    injected,
                    project_root.display()
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                "Failed to apply workspace contributors in {}: {}",
                project_root.display(),
                e
            );
        }
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

    // Apply workspace contributors (auto-detected from lockfiles) and resolve TaskRefs
    for p in &mut projects {
        apply_workspace_contributors(&mut p.manifest, &p.root);
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
