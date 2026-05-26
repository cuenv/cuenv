//! VCS dependency sync provider.

use async_trait::async_trait;
use clap::{Arg, Command};
use cuenv_core::lockfile::{LOCKFILE_NAME, LOCKFILE_VERSION, LockedVcsDependency, Lockfile};
use cuenv_core::manifest::{Base, Project, VcsDependency};
use cuenv_core::{Error, Result};
use cuenv_ignore::{FileStatus, IgnoreFiles, IgnoreSection};
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Command as ProcessCommand;

use crate::commands::CommandExecutor;
use crate::commands::git_hooks::find_git_root;
use crate::commands::sync::provider::{SyncMode, SyncOptions, SyncProvider, SyncResult};

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
    let mut next_vcs = if scope.prunes_unconfigured() {
        BTreeMap::new()
    } else {
        next_lockfile.vcs.clone()
    };
    let mut plans = Vec::new();
    let cache_root = if options.mode == SyncMode::Write {
        None
    } else {
        Some(TempPath::new(temporary_cache_root()?))
    };
    let default_cache_root = module_root.join(".cuenv/vcs/cache");
    if options.mode == SyncMode::Write {
        ensure_managed_internal_path(&git_root, &default_cache_root)?;
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
        let path = validate_materialization_path(&git_root, &dependency.spec.path)?;
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

        let locked_by_name = existing_lockfile
            .as_ref()
            .and_then(|lockfile| lockfile.find_vcs(&dependency.name));
        let locked_by_path = existing_lockfile
            .as_ref()
            .and_then(|lockfile| find_vcs_by_materialized_path(&git_root, lockfile, &path));
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
            let temp =
                prepare_dependency(&temp_root, &plan.path, &plan.resolved, plan.locked.as_ref())?;
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

fn prune_removed_vcs_dependencies(
    git_root: &Path,
    dependencies: &[LockedVcsDependency],
) -> Result<()> {
    for dependency in dependencies {
        let target = validate_materialization_path(git_root, &dependency.path)?;
        if fs::symlink_metadata(&target).is_ok() {
            ensure_replaceable_target(&target, Some(dependency))?;
            fs::remove_dir_all(&target).map_err(|e| Error::configuration(e.to_string()))?;
        }
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.starts_with('.')
    {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency name '{}'",
            name
        )));
    }
    Ok(())
}

fn temporary_cache_root() -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "cuenv-vcs-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Error::configuration(e.to_string()))?
            .as_nanos()
    ));
    Ok(path)
}

struct TempPath {
    path: PathBuf,
}

impl TempPath {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn validate_materialization_path(git_root: &Path, path: &str) -> Result<PathBuf> {
    let rel = Path::new(path);
    if rel.is_absolute() || path.trim().is_empty() {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency path '{}': path must be relative and contained in the repository",
            path
        )));
    }
    let mut components = Vec::new();
    for component in rel.components() {
        let Component::Normal(value) = component else {
            return Err(Error::configuration(format!(
                "Invalid VCS dependency path '{}': path must not contain '.', '..', or prefixes",
                path
            )));
        };
        let value = value.to_string_lossy();
        validate_path_component(&value, path)?;
        components.push(value.into_owned());
    }
    if components.is_empty() {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency path '{}': path must not target the repository root",
            path
        )));
    }
    if components.iter().any(|component| component == ".git")
        || components == [".cuenv", "vcs", "cache"]
        || components.starts_with(&[".cuenv".to_string(), "vcs".to_string(), "cache".to_string()])
        || components == [".cuenv", "vcs", "tmp"]
        || components.starts_with(&[".cuenv".to_string(), "vcs".to_string(), "tmp".to_string()])
    {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency path '{}': path targets cuenv or git internals",
            path
        )));
    }
    let target = components
        .iter()
        .fold(git_root.to_path_buf(), |acc, part| acc.join(part));
    ensure_parent_stays_in_repo(git_root, &target)?;
    Ok(target)
}

fn validate_path_component(component: &str, original_path: &str) -> Result<()> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.starts_with('-')
        || component.contains('\\')
        || component.chars().any(|c| {
            c.is_control()
                || matches!(
                    c,
                    '*' | '?' | '[' | ']' | '!' | '#' | ' ' | '\t' | '\n' | '\r'
                )
        })
    {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency path '{}': path components may only use literal safe names",
            original_path
        )));
    }
    Ok(())
}

/// Validate a sparse-checkout `subdir`. Returns the canonical slash-joined
/// relative path to use with `git sparse-checkout set` and `git cat-file/rev-parse`.
///
/// The input is required to be in canonical form: forward-slash separated, no
/// leading/trailing whitespace, no leading/trailing slashes, no empty
/// components. Non-canonical inputs are rejected so the value can be compared
/// to the lockfile entry by string equality without re-canonicalizing.
fn validate_subdir(subdir: &str) -> Result<String> {
    if subdir.is_empty() {
        return Err(Error::configuration(
            "Invalid VCS dependency subdir: must not be empty",
        ));
    }
    if subdir.trim() != subdir {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency subdir '{}': must not have leading or trailing whitespace",
            subdir
        )));
    }
    if subdir.contains('\\') {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency subdir '{}': must use forward-slash separators",
            subdir
        )));
    }
    if subdir.starts_with('/') || subdir.ends_with('/') {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency subdir '{}': must not start or end with '/'",
            subdir
        )));
    }
    let components: Vec<&str> = subdir.split('/').collect();
    for component in &components {
        if component.is_empty() {
            return Err(Error::configuration(format!(
                "Invalid VCS dependency subdir '{}': must not contain empty components",
                subdir
            )));
        }
        if *component == "." || *component == ".." {
            return Err(Error::configuration(format!(
                "Invalid VCS dependency subdir '{}': must not contain '.' or '..'",
                subdir
            )));
        }
        validate_path_component(component, subdir)?;
    }
    Ok(components.join("/"))
}

fn ensure_parent_stays_in_repo(git_root: &Path, target: &Path) -> Result<()> {
    let canonical_root = git_root
        .canonicalize()
        .map_err(|e| Error::configuration(format!("Failed to canonicalize git root: {e}")))?;
    let mut current = canonical_root.clone();
    let relative = target.strip_prefix(git_root).map_err(|_| {
        Error::configuration(format!(
            "Invalid VCS dependency path '{}': path must stay within the repository",
            target.display()
        ))
    })?;
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(component) = component else {
            return Err(Error::configuration(format!(
                "Invalid VCS dependency path '{}': path must stay within the repository",
                target.display()
            )));
        };
        if components.peek().is_none() {
            break;
        }
        current.push(component);
        if fs::symlink_metadata(&current).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(Error::configuration(format!(
                "Invalid VCS dependency path '{}': parent path contains a symlink",
                target.display()
            )));
        }
        if current.exists() {
            let canonical = current.canonicalize().map_err(|e| {
                Error::configuration(format!(
                    "Failed to canonicalize VCS dependency parent '{}': {}",
                    current.display(),
                    e
                ))
            })?;
            if !canonical.starts_with(&canonical_root) {
                return Err(Error::configuration(format!(
                    "Invalid VCS dependency path '{}': parent resolves outside the repository",
                    target.display()
                )));
            }
        }
    }
    Ok(())
}

fn ensure_managed_internal_path(git_root: &Path, target: &Path) -> Result<()> {
    ensure_parent_stays_in_repo(git_root, target)?;
    if fs::symlink_metadata(target).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(Error::configuration(format!(
            "Refusing to write symlinked cuenv-managed path '{}'",
            target.display()
        )));
    }
    if target.exists() {
        let canonical_root = git_root
            .canonicalize()
            .map_err(|e| Error::configuration(format!("Failed to canonicalize git root: {e}")))?;
        let canonical_target = target.canonicalize().map_err(|e| {
            Error::configuration(format!(
                "Failed to canonicalize cuenv-managed path '{}': {}",
                target.display(),
                e
            ))
        })?;
        if !canonical_target.starts_with(canonical_root) {
            return Err(Error::configuration(format!(
                "Refusing to write cuenv-managed path '{}' outside the repository",
                target.display()
            )));
        }
    }
    Ok(())
}

fn should_update(name: &str, update: Option<&Vec<String>>) -> bool {
    match update {
        None => false,
        Some(names) if names.is_empty() => true,
        Some(names) => names.iter().any(|candidate| candidate == name),
    }
}

fn locked_matches(locked: Option<&LockedVcsDependency>, spec: &VcsDependency) -> bool {
    locked.is_some_and(|locked| {
        locked.url == spec.url
            && locked.reference == spec.reference
            && locked.vendor == spec.vendor
            && locked.path == spec.path
            && locked.subdir == spec.subdir
    })
}

fn resolve_dependency(
    cache_root: &Path,
    name: &str,
    spec: &VcsDependency,
) -> Result<LockedVcsDependency> {
    validate_git_input("url", &spec.url)?;
    validate_git_input("reference", &spec.reference)?;
    let normalized_subdir = spec.subdir.as_deref().map(validate_subdir).transpose()?;
    let cache_path = cache_root.join(name);
    fs::create_dir_all(cache_root).map_err(|e| Error::configuration(e.to_string()))?;
    ensure_managed_internal_path(cache_root, &cache_path)?;
    if cache_path.exists() {
        ensure_git_dir(&cache_path)?;
        run_git(
            [
                OsStr::new("remote"),
                OsStr::new("set-url"),
                OsStr::new("origin"),
                OsStr::new(&spec.url),
            ],
            Some(&cache_path),
        )?;
    } else {
        run_git(
            [
                OsStr::new("clone"),
                OsStr::new("--no-checkout"),
                OsStr::new(&spec.url),
                cache_path.as_os_str(),
            ],
            None,
        )?;
    }
    run_git(
        [
            OsStr::new("fetch"),
            OsStr::new("--tags"),
            OsStr::new("origin"),
            OsStr::new(&spec.reference),
        ],
        Some(&cache_path),
    )?;
    let commit = git_output(
        [OsStr::new("rev-parse"), OsStr::new(FETCH_HEAD_COMMIT)],
        Some(&cache_path),
    )?;
    let tree = git_output(
        [OsStr::new("rev-parse"), OsStr::new(FETCH_HEAD_TREE)],
        Some(&cache_path),
    )?;

    let subtree = if let Some(ref subdir) = normalized_subdir {
        let object_ref = format!("{}:{}", commit, subdir);
        let object_type = git_output(
            [
                OsStr::new("cat-file"),
                OsStr::new("-t"),
                OsStr::new(&object_ref),
            ],
            Some(&cache_path),
        )
        .map_err(|_| {
            Error::configuration(format!(
                "VCS dependency '{}': subdir '{}' not found at reference '{}'",
                name, subdir, spec.reference
            ))
        })?;
        if object_type != "tree" {
            return Err(Error::configuration(format!(
                "VCS dependency '{}': subdir '{}' must be a tree (directory), got {} at reference '{}'",
                name, subdir, object_type, spec.reference
            )));
        }
        Some(git_output(
            [OsStr::new("rev-parse"), OsStr::new(&object_ref)],
            Some(&cache_path),
        )?)
    } else {
        None
    };

    Ok(LockedVcsDependency {
        url: spec.url.clone(),
        reference: spec.reference.clone(),
        commit,
        tree,
        vendor: spec.vendor,
        path: spec.path.clone(),
        subdir: normalized_subdir,
        subtree,
    })
}

fn prepare_dependency(
    temp_root: &Path,
    target: &Path,
    locked: &LockedVcsDependency,
    previous: Option<&LockedVcsDependency>,
) -> Result<TempPath> {
    validate_git_input("url", &locked.url)?;
    validate_git_input("reference", &locked.reference)?;
    validate_git_input("commit", &locked.commit)?;
    ensure_replaceable_target(target, previous)?;
    fs::create_dir_all(temp_root).map_err(|e| Error::configuration(e.to_string()))?;
    if let Some(ref subdir) = locked.subdir {
        return prepare_subdir_dependency(temp_root, target, locked, subdir);
    }
    let temp_target = temporary_target_path(temp_root, target, "tmp")?;
    let temp_guard = TempPath::new(temp_target);
    let temp_target = temp_guard.path();
    run_git(
        [
            OsStr::new("clone"),
            OsStr::new("--no-checkout"),
            OsStr::new(&locked.url),
            temp_target.as_os_str(),
        ],
        None,
    )?;
    run_git(
        [
            OsStr::new("fetch"),
            OsStr::new("origin"),
            OsStr::new(&locked.commit),
        ],
        Some(temp_target),
    )?;
    run_git(
        [
            OsStr::new("checkout"),
            OsStr::new("--detach"),
            OsStr::new(&locked.commit),
        ],
        Some(temp_target),
    )?;
    verify_checked_out_tree(temp_target, locked)?;
    ensure_dependency_does_not_reserve_marker(temp_target, locked)?;
    if locked.vendor {
        let git_dir = temp_target.join(".git");
        if git_dir.exists() {
            fs::remove_dir_all(&git_dir).map_err(|e| Error::configuration(e.to_string()))?;
        }
    }
    write_ownership_marker(temp_target, locked)?;
    Ok(temp_guard)
}

/// Sparse-checkout subdir path: only the requested `subdir` of the repo lands
/// at `target`, with no `.git` directory.
fn prepare_subdir_dependency(
    temp_root: &Path,
    target: &Path,
    locked: &LockedVcsDependency,
    subdir: &str,
) -> Result<TempPath> {
    let expected_subtree = locked.subtree.as_deref().ok_or_else(|| {
        Error::configuration(format!(
            "VCS dependency '{}': locked entry has subdir but no subtree hash",
            locked.path
        ))
    })?;
    let clone_target = temporary_target_path(temp_root, target, "clone")?;
    let clone_guard = TempPath::new(clone_target);
    let clone_path = clone_guard.path();
    run_git(
        [
            OsStr::new("clone"),
            OsStr::new("--no-checkout"),
            OsStr::new("--filter=blob:none"),
            OsStr::new(&locked.url),
            clone_path.as_os_str(),
        ],
        None,
    )?;
    run_git(
        [
            OsStr::new("sparse-checkout"),
            OsStr::new("init"),
            OsStr::new("--cone"),
        ],
        Some(clone_path),
    )?;
    run_git(
        [
            OsStr::new("sparse-checkout"),
            OsStr::new("set"),
            OsStr::new("--"),
            OsStr::new(subdir),
        ],
        Some(clone_path),
    )?;
    run_git(
        [
            OsStr::new("fetch"),
            OsStr::new("origin"),
            OsStr::new(&locked.commit),
        ],
        Some(clone_path),
    )?;
    run_git(
        [
            OsStr::new("checkout"),
            OsStr::new("--detach"),
            OsStr::new(&locked.commit),
        ],
        Some(clone_path),
    )?;
    let subtree = git_output(
        [
            OsStr::new("rev-parse"),
            OsStr::new(&format!("HEAD:{}", subdir)),
        ],
        Some(clone_path),
    )
    .map_err(|_| {
        Error::configuration(format!(
            "VCS dependency '{}': subdir '{}' not present at locked commit",
            locked.path, subdir
        ))
    })?;
    if subtree != expected_subtree {
        return Err(Error::configuration(format!(
            "VCS dependency '{}': subdir tree {} does not match locked subtree {}",
            locked.path, subtree, expected_subtree
        )));
    }

    let extracted_source = clone_path.join(subdir);
    if !extracted_source.is_dir() {
        return Err(Error::configuration(format!(
            "VCS dependency '{}': sparse checkout did not materialize subdir '{}'",
            locked.path, subdir
        )));
    }
    let extracted_target = temporary_target_path(temp_root, target, "tmp")?;
    let extracted_guard = TempPath::new(extracted_target);
    fs::rename(&extracted_source, extracted_guard.path())
        .map_err(|e| Error::configuration(e.to_string()))?;
    drop(clone_guard);

    ensure_dependency_does_not_reserve_marker(extracted_guard.path(), locked)?;
    write_ownership_marker(extracted_guard.path(), locked)?;
    Ok(extracted_guard)
}

fn install_prepared_dependency(target: &Path, prepared: &Path) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| Error::configuration("VCS target has no parent"))?;
    fs::create_dir_all(parent).map_err(|e| Error::configuration(e.to_string()))?;
    replace_target_with_prepared_checkout(target, prepared)
}

fn validate_git_input(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.starts_with('-') || value.chars().any(|c| c.is_control()) {
        return Err(Error::configuration(format!(
            "Invalid VCS dependency {label} '{}'",
            value
        )));
    }
    Ok(())
}

fn temporary_target_path(temp_root: &Path, target: &Path, kind: &str) -> Result<PathBuf> {
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dependency");
    let path = temp_root.join(format!(
        ".{file_name}.cuenv-{kind}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Error::configuration(e.to_string()))?
            .as_nanos()
    ));
    if fs::symlink_metadata(&path).is_ok() {
        return Err(Error::configuration(format!(
            "Refusing to reuse existing VCS temporary path '{}'",
            path.display()
        )));
    }
    Ok(path)
}

fn ensure_replaceable_target(target: &Path, previous: Option<&LockedVcsDependency>) -> Result<()> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(Error::configuration(format!(
                "Refusing to overwrite symlinked VCS target '{}'",
                target.display()
            )));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::configuration(e.to_string())),
    }
    let marker = read_ownership_marker(target).map_err(|_| {
        Error::configuration(format!(
            "Refusing to overwrite unmanaged VCS target '{}'",
            target.display()
        ))
    })?;
    if marker.trim().is_empty() {
        return Err(Error::configuration(format!(
            "Refusing to overwrite VCS target '{}' with an invalid ownership marker",
            target.display()
        )));
    }
    let previous = previous.ok_or_else(|| {
        Error::configuration(format!(
            "Refusing to overwrite VCS target '{}' without an existing lock entry",
            target.display()
        ))
    })?;
    if marker.trim() != previous.commit {
        return Err(Error::configuration(format!(
            "Refusing to overwrite VCS target '{}' with ownership marker {}, expected {}",
            target.display(),
            marker.trim(),
            previous.commit
        )));
    }
    validate_git_input("marker", marker.trim())?;
    if is_snapshot_materialization(previous) {
        let expected = expected_snapshot_tree(previous);
        let actual = vendored_tree_hash(target)?;
        if actual != expected {
            return Err(Error::configuration(format!(
                "Refusing to overwrite modified VCS target '{}': tree {}, expected {}",
                target.display(),
                actual,
                expected
            )));
        }
    } else {
        ensure_git_dir(target)?;
        ensure_marker_is_excluded(target)?;
        let status = git_output(
            [
                OsStr::new("status"),
                OsStr::new("--porcelain"),
                OsStr::new("--ignored"),
            ],
            Some(target),
        )?;
        if !git_status_is_clean_or_marker_only(&status) {
            return Err(Error::configuration(format!(
                "Refusing to overwrite dirty VCS checkout '{}'",
                target.display()
            )));
        }
    }
    Ok(())
}

fn ensure_dependency_does_not_reserve_marker(
    path: &Path,
    locked: &LockedVcsDependency,
) -> Result<()> {
    if fs::symlink_metadata(path.join(MARKER_FILE)).is_ok() {
        return Err(Error::configuration(format!(
            "VCS dependency '{}' contains reserved cuenv ownership marker '{}'",
            locked.path, MARKER_FILE
        )));
    }
    Ok(())
}

fn write_ownership_marker(path: &Path, locked: &LockedVcsDependency) -> Result<()> {
    let mut marker = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path.join(MARKER_FILE))
        .map_err(|e| Error::configuration(e.to_string()))?;
    marker
        .write_all(locked.commit.as_bytes())
        .map_err(|e| Error::configuration(e.to_string()))?;
    if is_git_checkout_materialization(locked) {
        ensure_marker_is_excluded(path)?;
    }
    Ok(())
}

fn read_ownership_marker(path: &Path) -> Result<String> {
    let marker_path = path.join(MARKER_FILE);
    let metadata = fs::symlink_metadata(&marker_path).map_err(|e| {
        Error::configuration(format!(
            "Unable to read VCS ownership marker '{}': {}",
            marker_path.display(),
            e
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::configuration(format!(
            "Refusing to read symlinked or non-file VCS ownership marker '{}'",
            marker_path.display()
        )));
    }
    fs::read_to_string(&marker_path).map_err(|e| {
        Error::configuration(format!(
            "Unable to read VCS ownership marker '{}': {}",
            marker_path.display(),
            e
        ))
    })
}

fn ensure_git_dir(path: &Path) -> Result<()> {
    let git_dir = path.join(".git");
    let metadata = fs::symlink_metadata(&git_dir).map_err(|e| {
        Error::configuration(format!(
            "Refusing to use malformed VCS checkout '{}' without a git directory: {}",
            path.display(),
            e
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::configuration(format!(
            "Refusing to use VCS checkout '{}' with a symlinked or non-directory .git path",
            path.display()
        )));
    }
    Ok(())
}

fn ensure_marker_is_excluded(path: &Path) -> Result<()> {
    ensure_git_dir(path)?;
    let info_dir = path.join(".git/info");
    let metadata = fs::symlink_metadata(&info_dir).map_err(|e| {
        Error::configuration(format!(
            "Refusing to update malformed VCS checkout '{}' without .git/info: {}",
            path.display(),
            e
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::configuration(format!(
            "Refusing to update VCS checkout '{}' with a symlinked or non-directory .git/info path",
            path.display()
        )));
    }
    let exclude_path = path.join(".git/info/exclude");
    if fs::symlink_metadata(&exclude_path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(Error::configuration(format!(
            "Refusing to update symlinked VCS exclude file '{}'",
            exclude_path.display()
        )));
    }
    let existing = fs::read_to_string(&exclude_path).unwrap_or_default();
    if existing.lines().any(|line| line.trim() == MARKER_FILE) {
        return Ok(());
    }
    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(MARKER_FILE);
    next.push('\n');
    fs::write(exclude_path, next).map_err(|e| Error::configuration(e.to_string()))?;
    Ok(())
}

fn git_status_is_clean_or_marker_only(status: &str) -> bool {
    status.lines().all(|line| {
        line.trim().is_empty()
            || line == format!("?? {MARKER_FILE}")
            || line == format!("!! {MARKER_FILE}")
    })
}

fn replace_target_with_prepared_checkout(target: &Path, prepared: &Path) -> Result<()> {
    match fs::symlink_metadata(target) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::rename(prepared, target).map_err(|e| Error::configuration(e.to_string()))?;
            return Ok(());
        }
        Err(e) => return Err(Error::configuration(e.to_string())),
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(Error::configuration(format!(
                "Refusing to replace symlinked VCS target '{}'",
                target.display()
            )));
        }
        Ok(_) => {}
    }

    let backup = temporary_backup_path(target)?;
    fs::rename(target, &backup).map_err(|e| Error::configuration(e.to_string()))?;
    if let Err(rename_error) = fs::rename(prepared, target) {
        let restore_result = fs::rename(&backup, target);
        return Err(Error::configuration(format!(
            "Failed to replace VCS target '{}': {}; restore {}",
            target.display(),
            rename_error,
            if restore_result.is_ok() {
                "succeeded"
            } else {
                "failed"
            }
        )));
    }
    fs::remove_dir_all(&backup).map_err(|e| Error::configuration(e.to_string()))?;
    Ok(())
}

fn temporary_backup_path(target: &Path) -> Result<PathBuf> {
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dependency");
    let path = target.with_file_name(format!(
        ".{file_name}.cuenv-backup-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Error::configuration(e.to_string()))?
            .as_nanos()
    ));
    if fs::symlink_metadata(&path).is_ok() {
        return Err(Error::configuration(format!(
            "Refusing to reuse existing VCS backup path '{}'",
            path.display()
        )));
    }
    Ok(path)
}

fn verify_checked_out_tree(path: &Path, locked: &LockedVcsDependency) -> Result<()> {
    let tree = git_output([OsStr::new("rev-parse"), OsStr::new(HEAD_TREE)], Some(path))?;
    if tree != locked.tree {
        return Err(Error::configuration(format!(
            "VCS dependency '{}' resolved tree {}, expected {}",
            locked.path, tree, locked.tree
        )));
    }
    Ok(())
}

fn is_snapshot_materialization(locked: &LockedVcsDependency) -> bool {
    locked.vendor || locked.subdir.is_some()
}

fn is_git_checkout_materialization(locked: &LockedVcsDependency) -> bool {
    !locked.vendor && locked.subdir.is_none()
}

/// Tree object the snapshot content on disk should hash to. When a `subdir`
/// is set we expect the subtree, not the full repo root tree.
fn expected_snapshot_tree(locked: &LockedVcsDependency) -> &str {
    locked.subtree.as_deref().unwrap_or(locked.tree.as_str())
}

fn check_materialized(path: &Path, locked: &LockedVcsDependency) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(Error::configuration(format!(
                "VCS dependency '{}' is a symlink",
                locked.path
            )));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::configuration(format!(
                "VCS dependency '{}' is missing",
                locked.path
            )));
        }
        Err(e) => return Err(Error::configuration(e.to_string())),
    }
    let marker = read_ownership_marker(path).map_err(|_| {
        Error::configuration(format!(
            "VCS dependency '{}' is missing cuenv ownership marker",
            locked.path
        ))
    })?;
    if marker.trim() != locked.commit {
        return Err(Error::configuration(format!(
            "VCS dependency '{}' marker is {}, expected {}",
            locked.path,
            marker.trim(),
            locked.commit
        )));
    }
    if is_snapshot_materialization(locked) {
        let expected = expected_snapshot_tree(locked);
        let actual = vendored_tree_hash(path)?;
        if actual != expected {
            return Err(Error::configuration(format!(
                "VCS dependency '{}' has tree {}, expected {}",
                locked.path, actual, expected
            )));
        }
        return Ok(());
    }
    let head = git_output([OsStr::new("rev-parse"), OsStr::new("HEAD")], Some(path))?;
    if head != locked.commit {
        return Err(Error::configuration(format!(
            "VCS dependency '{}' is checked out at {}, expected {}",
            locked.path, head, locked.commit
        )));
    }
    verify_checked_out_tree(path, locked)?;
    let status = git_output(
        [
            OsStr::new("status"),
            OsStr::new("--porcelain"),
            OsStr::new("--ignored"),
        ],
        Some(path),
    )?;
    if !git_status_is_clean_or_marker_only(&status) {
        return Err(Error::configuration(format!(
            "VCS dependency '{}' has uncommitted changes",
            locked.path
        )));
    }
    Ok(())
}

fn vendored_tree_hash(path: &Path) -> Result<String> {
    let temp = std::env::temp_dir().join(format!(
        "cuenv-vcs-tree-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Error::configuration(e.to_string()))?
            .as_nanos()
    ));
    fs::create_dir_all(&temp).map_err(|e| Error::configuration(e.to_string()))?;
    let result = vendored_tree_hash_with_git_dir(path, &temp);
    let cleanup = fs::remove_dir_all(&temp);
    match (result, cleanup) {
        (Ok(tree), Ok(())) => Ok(tree),
        (Ok(_), Err(e)) => Err(Error::configuration(e.to_string())),
        (Err(e), _) => Err(e),
    }
}

fn vendored_tree_hash_with_git_dir(path: &Path, temp: &Path) -> Result<String> {
    run_git([OsStr::new("init"), OsStr::new("-q")], Some(temp))?;
    let git_dir = temp.join(".git");
    run_git(
        [
            OsStr::new("--git-dir"),
            git_dir.as_os_str(),
            OsStr::new("--work-tree"),
            path.as_os_str(),
            OsStr::new("add"),
            OsStr::new("--all"),
            OsStr::new("--force"),
            OsStr::new("--"),
            OsStr::new("."),
        ],
        None,
    )?;
    run_git(
        [
            OsStr::new("--git-dir"),
            git_dir.as_os_str(),
            OsStr::new("--work-tree"),
            path.as_os_str(),
            OsStr::new("rm"),
            OsStr::new("--cached"),
            OsStr::new("--ignore-unmatch"),
            OsStr::new("--"),
            OsStr::new(MARKER_FILE),
        ],
        None,
    )?;
    git_output(
        [
            OsStr::new("--git-dir"),
            git_dir.as_os_str(),
            OsStr::new("--work-tree"),
            path.as_os_str(),
            OsStr::new("write-tree"),
        ],
        None,
    )
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

fn run_git<I, S>(args: I, cwd: Option<&Path>) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git_command(args, cwd)
        .output()
        .map_err(|e| Error::configuration(e.to_string()))?;
    if output.status.success() {
        return Ok(());
    }
    Err(Error::configuration(format!(
        "git command failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

fn git_output<I, S>(args: I, cwd: Option<&Path>) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git_command(args, cwd)
        .output()
        .map_err(|e| Error::configuration(e.to_string()))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    Err(Error::configuration(format!(
        "git command failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

fn git_command<I, S>(args: I, cwd: Option<&Path>) -> ProcessCommand
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = ProcessCommand::new("git");
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command.args(args);
    command
}

#[cfg(test)]
#[path = "vcs_tests.rs"]
mod tests;
