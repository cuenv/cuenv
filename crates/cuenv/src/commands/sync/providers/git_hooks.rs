//! Git hooks sync provider.
//!
//! Syncs git hook scripts from CUE configuration.
//! Pre-push hooks defined in `hooks.prePush` are converted to synthetic tasks
//! that can be run via `cuenv task hooks.pre-push.<name>`.

use async_trait::async_trait;
use cuenv_core::Result;
use std::path::Path;

use crate::commands::git_hooks;
use crate::commands::sync::provider::{SyncMode, SyncProvider, SyncRequest, SyncResult, SyncScope};

/// Sync provider for git hooks.
pub struct GitHooksSyncProvider;

#[async_trait]
impl SyncProvider for GitHooksSyncProvider {
    fn name(&self) -> &'static str {
        "git-hooks"
    }

    async fn sync(&self, request: SyncRequest<'_>) -> Result<SyncResult> {
        match request.scope {
            SyncScope::Path => sync_path(request),
            SyncScope::Workspace => sync_workspace(request),
        }
    }
}

fn sync_path(request: SyncRequest<'_>) -> Result<SyncResult> {
    let SyncRequest {
        path,
        options,
        executor,
        ..
    } = request;
    let Ok(git_root) = git_hooks::find_git_root(path) else {
        return Ok(SyncResult::success(
            "Not in a git repository. Skipping git hooks sync.",
        ));
    };

    // Get path-local module and collect pre-push hooks
    let module = executor.module(path)?;
    let mut all_pre_push_hooks = std::collections::HashMap::new();

    for instance in module.projects() {
        if let Ok(project) = instance.deserialize::<cuenv_core::manifest::Project>() {
            let hooks = project.pre_push_hooks_map();
            for (name, hook) in hooks {
                let hook_name = format_hook_name(&instance.path, &project.name, name);
                all_pre_push_hooks.insert(hook_name, hook);
            }
        }
    }

    if all_pre_push_hooks.is_empty() {
        return Ok(SyncResult::success(
            "No pre-push hooks configured in this project.",
        ));
    }

    let output = sync_pre_push_hook(&git_root, &options.mode)?;

    Ok(SyncResult::success(output))
}

fn sync_workspace(request: SyncRequest<'_>) -> Result<SyncResult> {
    let SyncRequest {
        path,
        options,
        executor,
        ..
    } = request;

    // For git hooks, we sync at the repo root level
    // Find git root and sync there
    let Ok(git_root) = git_hooks::find_git_root(path) else {
        return Ok(SyncResult::success(
            "Not in a git repository. Skipping git hooks sync.",
        ));
    };

    // Get all projects across the workspace and collect pre-push hooks
    let module = executor.discover_all_modules(path)?;
    let mut all_pre_push_hooks = std::collections::HashMap::new();

    for instance in module.projects() {
        if let Ok(project) = instance.deserialize::<cuenv_core::manifest::Project>() {
            let hooks = project.pre_push_hooks_map();
            for (name, hook) in hooks {
                // Prefix with project name if not at root
                let hook_name = format_hook_name(&instance.path, &project.name, name);
                all_pre_push_hooks.insert(hook_name, hook);
            }
        }
    }

    if all_pre_push_hooks.is_empty() {
        return Ok(SyncResult::success(
            "No pre-push hooks configured in any project.",
        ));
    }

    let output = sync_pre_push_hook(&git_root, &options.mode)?;

    Ok(SyncResult::success(output))
}

/// Format a hook name, prefixing with project name when the instance is not at the module root.
fn format_hook_name(instance_path: &std::path::Path, project_name: &str, name: String) -> String {
    if instance_path.as_os_str().is_empty() || instance_path == std::path::Path::new(".") {
        name
    } else {
        format!("{project_name}:{name}")
    }
}

/// Sync the pre-push hook script.
fn sync_pre_push_hook(git_root: &Path, mode: &SyncMode) -> Result<String> {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let hooks_dir = git_root.join(".git/hooks");
    let pre_push_path = hooks_dir.join("pre-push");

    // Generate the hook script content
    let hook_script = generate_pre_push_script();

    let existed = pre_push_path.try_exists().map_err(|e| {
        cuenv_core::Error::io_with_path("check pre-push hook", pre_push_path.clone(), e)
    })?;

    // Dry-run mode
    if *mode == SyncMode::DryRun {
        if existed {
            return Ok("pre-push: Would update".to_string());
        }
        return Ok("pre-push: Would create".to_string());
    }

    if existed {
        let existing = fs::read_to_string(&pre_push_path).map_err(|e| {
            cuenv_core::Error::io_with_path("read pre-push hook", pre_push_path.clone(), e)
        })?;
        if existing == hook_script {
            #[cfg(unix)]
            {
                match rustix::fs::access(&pre_push_path, rustix::fs::Access::EXEC_OK) {
                    Ok(()) => {}
                    Err(rustix::io::Errno::ACCESS) if *mode == SyncMode::Check => {
                        return Err(cuenv_core::Error::configuration(
                            "pre-push hook is not executable. Run 'cuenv sync git-hooks' to update.",
                        ));
                    }
                    Err(rustix::io::Errno::ACCESS) => {
                        let mut permissions = fs::metadata(&pre_push_path)
                            .map_err(|e| {
                                cuenv_core::Error::io_with_path(
                                    "get hook permissions",
                                    pre_push_path.clone(),
                                    e,
                                )
                            })?
                            .permissions();
                        permissions.set_mode(permissions.mode() | 0o100);
                        fs::set_permissions(&pre_push_path, permissions).map_err(|e| {
                            cuenv_core::Error::io_with_path(
                                "set hook permissions",
                                pre_push_path.clone(),
                                e,
                            )
                        })?;
                        rustix::fs::access(&pre_push_path, rustix::fs::Access::EXEC_OK).map_err(
                            |e| {
                                cuenv_core::Error::io_with_path(
                                    "verify hook execute access",
                                    pre_push_path.clone(),
                                    e.into(),
                                )
                            },
                        )?;
                        return Ok("pre-push: Updated permissions".to_string());
                    }
                    Err(error) => {
                        return Err(cuenv_core::Error::io_with_path(
                            "check hook execute access",
                            pre_push_path.clone(),
                            error.into(),
                        ));
                    }
                }
            }
            return Ok(match mode {
                SyncMode::Check => "pre-push: in sync",
                SyncMode::Write => "pre-push: unchanged",
                SyncMode::DryRun => "pre-push: Would update",
            }
            .to_string());
        }
        if *mode == SyncMode::Check {
            return Err(cuenv_core::Error::configuration(
                "pre-push hook out of sync. Run 'cuenv sync git-hooks' to update.",
            ));
        }
    } else if *mode == SyncMode::Check {
        return Err(cuenv_core::Error::configuration(
            "pre-push hook missing. Run 'cuenv sync git-hooks' to create.",
        ));
    }

    // Create hooks directory if needed
    if !hooks_dir.exists() {
        fs::create_dir_all(&hooks_dir).map_err(|e| {
            cuenv_core::Error::io_with_path("create hooks directory", hooks_dir.clone(), e)
        })?;
    }

    // Write the hook script
    fs::write(&pre_push_path, &hook_script).map_err(|e| {
        cuenv_core::Error::io_with_path("write pre-push hook", pre_push_path.clone(), e)
    })?;

    // Make executable
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&pre_push_path)
            .map_err(|e| {
                cuenv_core::Error::io_with_path("get hook permissions", pre_push_path.clone(), e)
            })?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&pre_push_path, perms).map_err(|e| {
            cuenv_core::Error::io_with_path("set hook permissions", pre_push_path.clone(), e)
        })?;
        rustix::fs::access(&pre_push_path, rustix::fs::Access::EXEC_OK).map_err(|e| {
            cuenv_core::Error::io_with_path(
                "verify hook execute access",
                pre_push_path.clone(),
                e.into(),
            )
        })?;
    }

    if existed {
        Ok("pre-push: Updated".to_string())
    } else {
        Ok("pre-push: Created".to_string())
    }
}

/// Generate the pre-push hook script content.
fn generate_pre_push_script() -> String {
    let mut script = String::from(
        r"#!/bin/sh
# Generated by cuenv - do not edit
# Source: hooks.prePush in env.cue
#
# This hook runs cuenv pre-push hook tasks before pushing.
# Each task is filtered by its inputs to only run when relevant files changed.

set -e

",
    );

    // Add the main logic that runs the aggregator task
    script.push_str(r#"# Run the pre-push hooks aggregator task
# This task depends on all individual pre-push hooks and will run them in parallel
# Each hook task filters itself based on changed files via CUENV_CHANGED_FILES

# Read stdin for refs being pushed (standard git pre-push input)
while read local_ref local_sha remote_ref remote_sha
do
    # Get changed files between local and remote
    if [ "$remote_sha" = "0000000000000000000000000000000000000000" ]; then
        # New branch - compare against default branch
        remote_branch=$(git symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's@^refs/remotes/origin/@@' || echo "main")
        base_sha=$(git merge-base "$local_sha" "origin/$remote_branch" 2>/dev/null || echo "$local_sha~1")
    else
        base_sha="$remote_sha"
    fi

    # Get changed files, with error logging
    if ! changed_files=$(git diff --name-only "$base_sha" "$local_sha" 2>&1); then
        echo "Warning: Failed to get changed files: $changed_files" >&2
        changed_files=""
    fi

    if [ -z "$changed_files" ]; then
        echo "No files changed. Skipping pre-push hooks."
        continue
    fi

    export CUENV_CHANGED_FILES="$changed_files"
    export CUENV_PRE_PUSH_LOCAL_SHA="$local_sha"
    export CUENV_PRE_PUSH_REMOTE_SHA="$remote_sha"

    echo "Running pre-push hooks..."
    cuenv task hooks.pre-push
    exit_code=$?

    if [ $exit_code -ne 0 ]; then
        echo "Pre-push hooks failed. Push aborted."
        exit $exit_code
    fi
done

exit 0
"#);

    script
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::path::PathBuf;
    #[cfg(unix)]
    use std::time::{Duration, SystemTime};
    #[cfg(unix)]
    use tempfile::TempDir;

    #[cfg(unix)]
    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    #[cfg(unix)]
    fn matching_hook(mode: u32) -> TestResult<(TempDir, PathBuf)> {
        let repo = TempDir::new()?;
        let hooks = repo.path().join(".git/hooks");
        fs::create_dir_all(&hooks)?;
        let hook = hooks.join("pre-push");
        fs::write(&hook, generate_pre_push_script())?;
        let times = fs::FileTimes::new()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000));
        fs::File::options()
            .write(true)
            .open(&hook)?
            .set_times(times)?;
        fs::set_permissions(&hook, fs::Permissions::from_mode(mode))?;
        Ok((repo, hook))
    }

    #[test]
    fn test_generate_pre_push_script() {
        let script = generate_pre_push_script();
        assert!(script.contains("#!/bin/sh"));
        assert!(script.contains("cuenv task hooks.pre-push"));
    }

    #[cfg(unix)]
    #[test]
    fn check_reports_missing_execute_permission_without_changes() -> TestResult {
        let (repo, hook) = matching_hook(0o640)?;
        let before = fs::metadata(&hook)?;

        let error = sync_pre_push_hook(repo.path(), &SyncMode::Check)
            .err()
            .ok_or("expected missing execute permission to be rejected")?;
        assert!(error.to_string().contains("not executable"));
        assert_eq!(fs::read_to_string(&hook)?, generate_pre_push_script());
        let after = fs::metadata(&hook)?;
        assert_eq!(after.permissions().mode() & 0o777, 0o640);
        assert_eq!(after.modified()?, before.modified()?);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn write_repairs_permissions_without_rewriting_hook() -> TestResult {
        let (repo, hook) = matching_hook(0o640)?;
        let before = fs::metadata(&hook)?;

        assert_eq!(
            sync_pre_push_hook(repo.path(), &SyncMode::Write)?,
            "pre-push: Updated permissions"
        );
        assert_eq!(fs::read_to_string(&hook)?, generate_pre_push_script());
        let after = fs::metadata(&hook)?;
        assert_eq!(after.permissions().mode() & 0o777, 0o740);
        assert_eq!(after.modified()?, before.modified()?);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn dry_run_does_not_repair_permissions_or_rewrite_hook() -> TestResult {
        let (repo, hook) = matching_hook(0o640)?;
        let before = fs::metadata(&hook)?;

        assert_eq!(
            sync_pre_push_hook(repo.path(), &SyncMode::DryRun)?,
            "pre-push: Would update"
        );
        assert_eq!(fs::read_to_string(&hook)?, generate_pre_push_script());
        let after = fs::metadata(&hook)?;
        assert_eq!(after.permissions().mode() & 0o777, 0o640);
        assert_eq!(after.modified()?, before.modified()?);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn executable_hook_keeps_restrictive_permissions() -> TestResult {
        let (repo, hook) = matching_hook(0o700)?;
        let before = fs::metadata(&hook)?;

        assert_eq!(
            sync_pre_push_hook(repo.path(), &SyncMode::Check)?,
            "pre-push: in sync"
        );
        assert_eq!(
            sync_pre_push_hook(repo.path(), &SyncMode::Write)?,
            "pre-push: unchanged"
        );
        assert_eq!(fs::read_to_string(&hook)?, generate_pre_push_script());
        let after = fs::metadata(&hook)?;
        assert_eq!(after.permissions().mode() & 0o777, 0o700);
        assert_eq!(after.modified()?, before.modified()?);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn group_execute_does_not_replace_owner_execute() -> TestResult {
        let (repo, hook) = matching_hook(0o650)?;
        let before = fs::metadata(&hook)?;

        // Privileged users may execute any file with at least one execute bit.
        if rustix::fs::access(&hook, rustix::fs::Access::EXEC_OK).is_ok() {
            assert_eq!(
                sync_pre_push_hook(repo.path(), &SyncMode::Check)?,
                "pre-push: in sync"
            );
            assert_eq!(
                sync_pre_push_hook(repo.path(), &SyncMode::Write)?,
                "pre-push: unchanged"
            );
            assert_eq!(fs::metadata(&hook)?.permissions().mode() & 0o777, 0o650);
            return Ok(());
        }
        assert!(sync_pre_push_hook(repo.path(), &SyncMode::Check).is_err());
        assert_eq!(fs::metadata(&hook)?.permissions().mode() & 0o777, 0o650);
        sync_pre_push_hook(repo.path(), &SyncMode::Write)?;
        let after = fs::metadata(&hook)?;
        assert_eq!(after.permissions().mode() & 0o777, 0o750);
        assert_eq!(after.modified()?, before.modified()?);
        assert_eq!(fs::read_to_string(&hook)?, generate_pre_push_script());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn new_hook_is_installed_with_standard_permissions() -> TestResult {
        let repo = TempDir::new()?;
        assert_eq!(
            sync_pre_push_hook(repo.path(), &SyncMode::Write)?,
            "pre-push: Created"
        );
        let hook = repo.path().join(".git/hooks/pre-push");
        assert_eq!(fs::read_to_string(&hook)?, generate_pre_push_script());
        assert_eq!(fs::metadata(hook)?.permissions().mode() & 0o777, 0o755);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn existing_hook_read_errors_are_reported() -> TestResult {
        let repo = TempDir::new()?;
        fs::create_dir_all(repo.path().join(".git/hooks/pre-push"))?;
        for mode in [SyncMode::Check, SyncMode::Write] {
            let error = sync_pre_push_hook(repo.path(), &mode)
                .err()
                .ok_or("expected directory read to fail")?;
            assert!(error.to_string().contains("read pre-push hook"));
        }
        Ok(())
    }
}
