//! VCS dependency sync provider.

mod git;
mod materialization;

use async_trait::async_trait;
use clap::{Arg, Command};
use cuenv_core::lockfile::{LOCKFILE_NAME, LOCKFILE_VERSION, LockedVcsDependency, Lockfile};
use cuenv_core::manifest::{Base, Project, VcsDependency};
use cuenv_core::{Error, Result};
use cuenv_ignore::{FileStatus, IgnoreFiles, IgnoreSection};
use materialization::{
    TempPath, check_materialized, ensure_managed_internal_path, install_prepared_dependency,
    locked_matches, prepare_dependency, prune_removed_vcs_dependencies, resolve_dependency,
    should_update, temporary_cache_root, validate_materialization_path, validate_name,
};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::commands::CommandExecutor;
use crate::commands::git_hooks::find_git_root;
use crate::commands::sync::provider::{SyncMode, SyncOptions, SyncProvider, SyncResult};

#[cfg(test)]
use git::run_git;
#[cfg(test)]
use materialization::validate_subdir;

const GITIGNORE_SECTION_NAME: &str = "cuenv vcs";
const FETCH_HEAD_COMMIT: &str = "FETCH_HEAD^{commit}";
const FETCH_HEAD_TREE: &str = "FETCH_HEAD^{tree}";
const HEAD_TREE: &str = "HEAD^{tree}";
const MARKER_FILE: &str = ".cuenv-vcs";

/// Sync provider for cuenv-managed Git dependencies.
pub struct VcsSyncProvider;

#[derive(Debug, Clone)]
struct CollectedVcsDependency {
    name: String,
    spec: VcsDependency,
}

struct VcsSyncInputs {
    module_root: PathBuf,
    dependencies: Vec<CollectedVcsDependency>,
}

#[async_trait]
impl SyncProvider for VcsSyncProvider {
    fn name(&self) -> &'static str {
        "vcs"
    }

    fn description(&self) -> &'static str {
        "Sync cuenv-managed VCS dependencies"
    }

    fn has_config(&self, manifest: &Base) -> bool {
        !manifest.vcs.is_empty()
    }

    fn build_command(&self) -> Command {
        self.default_command().arg(
            Arg::new("update")
                .long("update")
                .short('u')
                .help("Force re-resolution of VCS refs. Use -u for all, or -u NAME for specific dependencies.")
                .num_args(0..)
                .value_name("NAMES")
                .default_missing_value(""),
        )
    }

    fn parse_args(&self, matches: &clap::ArgMatches) -> SyncOptions {
        let mode = if matches.get_flag("dry-run") {
            SyncMode::DryRun
        } else if matches.get_flag("check") {
            SyncMode::Check
        } else {
            SyncMode::Write
        };
        let update_tools = matches
            .get_many::<String>("update")
            .map(|names| names.filter(|name| !name.is_empty()).cloned().collect());

        SyncOptions {
            mode,
            show_diff: matches.get_flag("diff"),
            ci_provider: matches.get_one::<String>("provider").cloned(),
            update_tools,
        }
    }

    async fn sync_path(
        &self,
        path: &Path,
        _package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let collected = collect_vcs_sync_inputs(path, executor, VcsSyncScope::Path)?;
        let VcsSyncInputs {
            module_root,
            dependencies,
        } = collected;
        Ok(SyncResult::success(sync_vcs_dependencies(
            VcsSyncRequest {
                module_root: &module_root,
                dependencies,
                options,
                scope: VcsSyncScope::Path,
            },
        )?))
    }

    async fn sync_workspace(
        &self,
        path: &Path,
        _package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let collected = collect_vcs_sync_inputs(path, executor, VcsSyncScope::Workspace)?;
        let VcsSyncInputs {
            module_root,
            dependencies,
        } = collected;
        Ok(SyncResult::success(sync_vcs_dependencies(
            VcsSyncRequest {
                module_root: &module_root,
                dependencies,
                options,
                scope: VcsSyncScope::Workspace,
            },
        )?))
    }
}

fn collect_vcs_sync_inputs(
    path: &Path,
    executor: &CommandExecutor,
    scope: VcsSyncScope,
) -> Result<VcsSyncInputs> {
    let module = match scope {
        VcsSyncScope::Path => executor.get_module(path)?,
        VcsSyncScope::Workspace => executor.discover_all_modules(path)?,
    };
    let module_root = module.root.clone();
    let mut dependencies = Vec::new();

    for instance in module.bases() {
        let base: Base = instance.deserialize()?;
        collect_project_vcs(&mut dependencies, &base.vcs)?;
    }
    for instance in module.projects() {
        let project: Project = instance.deserialize()?;
        collect_project_vcs(&mut dependencies, &project.vcs)?;
    }

    Ok(VcsSyncInputs {
        module_root,
        dependencies,
    })
}

fn collect_project_vcs(
    dependencies: &mut Vec<CollectedVcsDependency>,
    vcs: &HashMap<String, VcsDependency>,
) -> Result<()> {
    for (name, spec) in vcs {
        validate_name(name)?;
        dependencies.push(CollectedVcsDependency {
            name: name.clone(),
            spec: spec.clone(),
        });
    }
    Ok(())
}

fn sync_vcs_dependencies(request: VcsSyncRequest<'_>) -> Result<String> {
    let VcsSyncRequest {
        module_root,
        dependencies,
        options,
        scope,
    } = request;
    if dependencies.is_empty() && !scope.prunes_unconfigured() {
        return Ok("No VCS dependencies configured.".to_string());
    }

    let git_root = find_git_root(module_root)?;
    let lockfile_path = module_root.join(LOCKFILE_NAME);
    let existing_lockfile = Lockfile::load(&lockfile_path)?;
    if dependencies.is_empty()
        && scope.prunes_unconfigured()
        && existing_lockfile
            .as_ref()
            .is_none_or(|lockfile| lockfile.vcs.is_empty())
    {
        check_gitignore(&git_root, &[])?;
        if options.mode == SyncMode::Write {
            sync_gitignore(&git_root, &[])?;
        }
        return Ok("No VCS dependencies configured.".to_string());
    }
    let mut next_lockfile = existing_lockfile.clone().unwrap_or_default();
    next_lockfile.version = LOCKFILE_VERSION;
    let mut outputs = Vec::new();
    let VcsPlanOutput { next_vcs, plans } = plan_vcs_dependencies(VcsPlanRequest {
        git_root: &git_root,
        module_root,
        dependencies,
        existing_lockfile: existing_lockfile.as_ref(),
        options,
        scope,
    })?;

    let removed_vcs =
        removed_vcs_dependencies(existing_lockfile.as_ref(), &git_root, &next_vcs, scope);
    next_lockfile.vcs = next_vcs;
    let gitignore_paths = gitignore_paths_from_lockfile(&git_root, module_root, &next_lockfile)?;

    if options.mode == SyncMode::Check {
        for plan in &plans {
            check_materialized(&plan.path, &plan.resolved)?;
            outputs.push(format!("{}: in sync", plan.name));
        }
        check_gitignore(&git_root, &gitignore_paths)?;
        if lockfile_is_changed(existing_lockfile.as_ref(), &next_lockfile) {
            return Err(Error::configuration(
                "VCS dependencies are out of sync. Run 'cuenv sync vcs' to update cuenv.lock.",
            ));
        }
    } else if options.mode == SyncMode::Write {
        let temp_root = module_root.join(".cuenv/vcs/tmp");
        ensure_managed_internal_path(&git_root, &temp_root)?;
        let mut prepared = Vec::new();
        for plan in plans {
            let temp = prepare_dependency(PrepareDependencyRequest {
                temp_root: &temp_root,
                target: &plan.path,
                locked: &plan.resolved,
                previous: plan.locked.as_ref(),
            })?;
            prepared.push((plan, temp));
        }
        prune_removed_vcs_dependencies(&git_root, &removed_vcs)?;
        for dependency in &removed_vcs {
            outputs.push(format!("{}: Removed stale checkout", dependency.path));
        }
        for (plan, temp) in &prepared {
            install_prepared_dependency(&plan.path, temp.path())?;
            outputs.push(format!(
                "{}: Synced {} to {}",
                plan.name, plan.resolved.commit, plan.resolved.path
            ));
        }
        next_lockfile.save(&lockfile_path)?;
        sync_gitignore(&git_root, &gitignore_paths)?;
    } else {
        for plan in &plans {
            outputs.push(format!(
                "{}: Would sync {} at {}",
                plan.name, plan.resolved.commit, plan.resolved.path
            ));
        }
        for dependency in &removed_vcs {
            outputs.push(format!(
                "{}: Would remove stale checkout at {}",
                dependency.path, dependency.path
            ));
        }
        if lockfile_is_changed(existing_lockfile.as_ref(), &next_lockfile) {
            outputs.push("Would update cuenv.lock".to_string());
        }
        let result = sync_gitignore_section(&git_root, &gitignore_paths, &SyncMode::DryRun)?;
        if result.files.iter().any(|file| {
            matches!(
                file.status,
                FileStatus::WouldCreate | FileStatus::WouldUpdate
            )
        }) {
            outputs.push("Would update VCS .gitignore entries".to_string());
        }
    }

    if outputs.is_empty() {
        return Ok("No VCS dependencies configured.".to_string());
    }
    Ok(outputs.join("\n"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VcsSyncScope {
    Path,
    Workspace,
}

struct VcsSyncPlan {
    name: String,
    path: PathBuf,
    resolved: LockedVcsDependency,
    locked: Option<LockedVcsDependency>,
}

struct VcsPlanOutput {
    next_vcs: BTreeMap<String, LockedVcsDependency>,
    plans: Vec<VcsSyncPlan>,
}

struct VcsPlanRequest<'a> {
    git_root: &'a Path,
    module_root: &'a Path,
    dependencies: Vec<CollectedVcsDependency>,
    existing_lockfile: Option<&'a Lockfile>,
    options: &'a SyncOptions,
    scope: VcsSyncScope,
}

#[derive(Clone, Copy)]
struct PrepareDependencyRequest<'a> {
    temp_root: &'a Path,
    target: &'a Path,
    locked: &'a LockedVcsDependency,
    previous: Option<&'a LockedVcsDependency>,
}

#[derive(Clone, Copy)]
struct PrepareSubdirDependencyRequest<'a> {
    temp_root: &'a Path,
    target: &'a Path,
    locked: &'a LockedVcsDependency,
    subdir: &'a str,
}

struct VcsSyncRequest<'a> {
    module_root: &'a Path,
    dependencies: Vec<CollectedVcsDependency>,
    options: &'a SyncOptions,
    scope: VcsSyncScope,
}

impl VcsSyncScope {
    fn prunes_unconfigured(self) -> bool {
        matches!(self, Self::Workspace)
    }
}

fn plan_vcs_dependencies(request: VcsPlanRequest<'_>) -> Result<VcsPlanOutput> {
    let VcsPlanRequest {
        git_root,
        module_root,
        dependencies,
        existing_lockfile,
        options,
        scope,
    } = request;
    let mut next_vcs = if scope.prunes_unconfigured() {
        BTreeMap::new()
    } else {
        existing_lockfile
            .map(|lockfile| lockfile.vcs.clone())
            .unwrap_or_default()
    };
    let mut plans = Vec::new();
    let cache_root = if options.mode == SyncMode::Write {
        None
    } else {
        Some(TempPath::new(temporary_cache_root()?))
    };
    let default_cache_root = module_root.join(".cuenv/vcs/cache");
    if options.mode == SyncMode::Write {
        ensure_managed_internal_path(git_root, &default_cache_root)?;
    }

    let mut seen_names = HashMap::new();
    let mut seen_paths = HashMap::new();
    for dependency in dependencies {
        if let Some(previous_spec) =
            seen_names.insert(dependency.name.clone(), dependency.spec.clone())
        {
            if previous_spec == dependency.spec {
                continue;
            }
            return Err(Error::configuration(format!(
                "VCS dependency '{}' is declared multiple times with different configuration",
                dependency.name
            )));
        }
        let path = validate_materialization_path(git_root, &dependency.spec.path)?;
        let path_key = path.clone();
        if let Some((previous_name, previous_path)) = overlapping_vcs_path(&seen_paths, &path_key) {
            return Err(Error::configuration(format!(
                "VCS dependencies '{}' and '{}' use overlapping paths '{}' and '{}'",
                previous_name,
                dependency.name,
                previous_path.display(),
                dependency.spec.path
            )));
        }
        seen_paths.insert(path_key, dependency.name.clone());

        let locked_by_name =
            existing_lockfile.and_then(|lockfile| lockfile.find_vcs(&dependency.name));
        let locked_by_path = existing_lockfile
            .and_then(|lockfile| find_vcs_by_materialized_path(git_root, lockfile, &path));
        let locked = locked_by_name.or(locked_by_path);
        let should_update = should_update(&dependency.name, options.update_tools.as_ref());
        if options.mode == SyncMode::Check && !locked_matches(locked_by_name, &dependency.spec) {
            return Err(Error::configuration(format!(
                "VCS dependency '{}' is out of sync with cuenv.lock. Run 'cuenv sync vcs' to update.",
                dependency.name
            )));
        }
        let resolved = if !should_update && locked_matches(locked, &dependency.spec) {
            locked.cloned().ok_or_else(|| {
                Error::configuration(format!(
                    "VCS dependency '{}' is missing from cuenv.lock. Run 'cuenv sync vcs' to update.",
                    dependency.name
                ))
            })?
        } else {
            resolve_dependency(
                cache_root
                    .as_ref()
                    .map_or(default_cache_root.as_path(), TempPath::path),
                &dependency.name,
                &dependency.spec,
            )?
        };

        plans.push(VcsSyncPlan {
            name: dependency.name.clone(),
            path,
            resolved: resolved.clone(),
            locked: locked.cloned(),
        });
        next_vcs.insert(dependency.name, resolved);
    }

    Ok(VcsPlanOutput { next_vcs, plans })
}

fn gitignore_paths_from_lockfile(
    git_root: &Path,
    module_root: &Path,
    lockfile: &Lockfile,
) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for dependency in lockfile.vcs.values() {
        if dependency.vendor {
            paths.push(format!(
                "{}/{}",
                dependency.path.trim_end_matches('/'),
                MARKER_FILE
            ));
        } else {
            paths.push(format!("{}/", dependency.path.trim_end_matches('/')));
        }
    }
    if !lockfile.vcs.is_empty() {
        paths.push(format!(
            "{}/",
            relative_gitignore_path(git_root, &module_root.join(".cuenv/vcs/cache"),)?
        ));
        paths.push(format!(
            "{}/",
            relative_gitignore_path(git_root, &module_root.join(".cuenv/vcs/tmp"),)?
        ));
    }
    Ok(paths)
}

fn relative_gitignore_path(git_root: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(git_root).map_err(|_| {
        Error::configuration(format!(
            "Unable to derive gitignore path for '{}' outside '{}'",
            path.display(),
            git_root.display()
        ))
    })?;
    let path = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if path.is_empty() {
        return Err(Error::configuration(
            "Unable to derive gitignore path for repository root",
        ));
    }
    Ok(escape_gitignore_path(&path))
}

fn escape_gitignore_path(path: &str) -> String {
    let mut escaped = String::new();
    for ch in path.chars() {
        if matches!(ch, '\\' | ' ' | '#' | '!' | '*' | '?' | '[' | ']') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn overlapping_vcs_path<'a>(
    seen_paths: &'a HashMap<PathBuf, String>,
    path: &Path,
) -> Option<(&'a str, &'a Path)> {
    seen_paths
        .iter()
        .find(|(seen_path, _)| path.starts_with(seen_path) || seen_path.starts_with(path))
        .map(|(seen_path, name)| (name.as_str(), seen_path.as_path()))
}

fn find_vcs_by_materialized_path<'a>(
    git_root: &Path,
    lockfile: &'a Lockfile,
    path: &Path,
) -> Option<&'a LockedVcsDependency> {
    lockfile.vcs.values().find(|dependency| {
        validate_materialization_path(git_root, &dependency.path)
            .is_ok_and(|locked_path| locked_path == path)
    })
}

fn removed_vcs_dependencies(
    existing: Option<&Lockfile>,
    git_root: &Path,
    next_vcs: &BTreeMap<String, LockedVcsDependency>,
    scope: VcsSyncScope,
) -> Vec<LockedVcsDependency> {
    if !scope.prunes_unconfigured() {
        return Vec::new();
    }
    existing.map_or_else(Vec::new, |lockfile| {
        lockfile
            .vcs
            .iter()
            .filter(|(_, dependency)| {
                let Ok(old_path) = validate_materialization_path(git_root, &dependency.path) else {
                    return true;
                };
                let path_still_configured = next_vcs.values().any(|next| {
                    validate_materialization_path(git_root, &next.path)
                        .is_ok_and(|new_path| new_path == old_path)
                });
                !path_still_configured
            })
            .map(|(_, dependency)| dependency.clone())
            .collect()
    })
}

fn check_gitignore(git_root: &Path, paths: &[String]) -> Result<()> {
    let gitignore_path = git_root.join(".gitignore");
    ensure_managed_internal_path(git_root, &gitignore_path)?;
    let result = sync_gitignore_section(git_root, paths, &SyncMode::Check)?;
    if result.files.iter().any(|file| {
        matches!(
            file.status,
            FileStatus::WouldCreate | FileStatus::WouldUpdate
        )
    }) {
        return Err(Error::configuration(
            "VCS .gitignore entries are out of sync. Run 'cuenv sync vcs' to update.",
        ));
    }
    Ok(())
}

fn sync_gitignore(git_root: &Path, paths: &[String]) -> Result<()> {
    let gitignore_path = git_root.join(".gitignore");
    ensure_managed_internal_path(git_root, &gitignore_path)?;
    sync_gitignore_section(git_root, paths, &SyncMode::Write)?;
    Ok(())
}

fn sync_gitignore_section(
    git_root: &Path,
    paths: &[String],
    mode: &SyncMode,
) -> Result<cuenv_ignore::SyncResult> {
    let section = IgnoreSection::new(GITIGNORE_SECTION_NAME).patterns(paths.iter().cloned());
    IgnoreFiles::builder()
        .directory(git_root)
        .require_git_repo(false)
        .dry_run(mode != &SyncMode::Write)
        .section(section)
        .generate()
        .map_err(|e| Error::configuration(format!("Failed to sync VCS .gitignore entries: {e}")))
}

fn lockfile_is_changed(existing: Option<&Lockfile>, next: &Lockfile) -> bool {
    existing != Some(next)
}

#[cfg(test)]
#[path = "vcs_tests.rs"]
mod tests;
