//! Git hooks sync provider.
//!
//! Syncs git hook scripts from CUE configuration.
//! Pre-push hooks defined in `hooks.prePush` are converted to synthetic tasks
//! that can be run via `cuenv task hooks.pre-push.<name>`.

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::manifest::Base;
use std::path::Path;

use crate::commands::CommandExecutor;
use crate::commands::git_hooks::find_git_root;
use crate::commands::sync::provider::{SyncMode, SyncOptions, SyncProvider, SyncResult};

/// Sync provider for git hooks.
pub struct GitHooksSyncProvider;

#[async_trait]
impl SyncProvider for GitHooksSyncProvider {
    fn name(&self) -> &'static str {
        "git-hooks"
    }

    fn description(&self) -> &'static str {
        "Sync git hook scripts (pre-push, pre-commit)"
    }

    fn has_config(&self, _manifest: &Base) -> bool {
        // Git hooks config is on Project, not Base
        // For now return false and check during sync
        false
    }

    async fn sync_path(
        &self,
        _path: &Path,
        package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        // Git hooks always aggregate from all projects (like CODEOWNERS)
        // since there's only one .git/hooks/pre-push per repo
        self.sync_workspace(package, options, executor).await
    }

    async fn sync_workspace(
        &self,
        _package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let cwd = std::env::current_dir().map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to get current directory: {e}"))
        })?;

        // For git hooks, we sync at the repo root level
        // Find git root and sync there
        let Ok(git_root) = find_git_root(&cwd) else {
            return Ok(SyncResult::success(
                "Not in a git repository. Skipping git hooks sync.",
            ));
        };

        // Get all projects and collect pre-push hooks
        let module = executor.get_module(&cwd)?;
        let mut all_pre_push_hooks = std::collections::HashMap::new();

        for instance in module.projects() {
            if let Ok(project) = instance.deserialize::<cuenv_core::manifest::Project>() {
                let hooks = project.pre_push_hooks_map();
                for (name, hook) in hooks {
                    // Prefix with project name if not at root
                    let hook_name = if instance.path.as_os_str().is_empty()
                        || instance.path == std::path::Path::new(".")
                    {
                        name
                    } else {
                        format!("{}:{}", project.name, name)
                    };
                    all_pre_push_hooks.insert(hook_name, hook);
                }
            }
        }

        if all_pre_push_hooks.is_empty() {
            return Ok(SyncResult::success(
                "No pre-push hooks configured in any project.",
            ));
        }

        let dry_run = options.mode == SyncMode::DryRun;
        let check = options.mode == SyncMode::Check;

        let output = sync_pre_push_hook(&git_root, &all_pre_push_hooks, dry_run, check)?;

        Ok(SyncResult::success(output))
    }
}

/// Sync the pre-push hook script.
fn sync_pre_push_hook(
    git_root: &Path,
    _hooks: &std::collections::HashMap<String, cuenv_hooks::Hook>,
    dry_run: bool,
    check: bool,
) -> Result<String> {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let hooks_dir = git_root.join(".git/hooks");
    let pre_push_path = hooks_dir.join("pre-push");

    // Generate the hook script content
    let hook_script = generate_pre_push_script();

    // Check mode
    if check {
        if pre_push_path.exists() {
            let existing = fs::read_to_string(&pre_push_path).map_err(|e| cuenv_core::Error::Io {
                source: e,
                path: Some(pre_push_path.clone().into_boxed_path()),
                operation: "read pre-push hook".to_string(),
            })?;
            if existing == hook_script {
                return Ok("pre-push: in sync".to_string());
            }
            return Err(cuenv_core::Error::configuration(
                "pre-push hook out of sync. Run 'cuenv sync git-hooks' to update.",
            ));
        }
        return Err(cuenv_core::Error::configuration(
            "pre-push hook missing. Run 'cuenv sync git-hooks' to create.",
        ));
    }

    // Check if unchanged
    if pre_push_path.exists() && !dry_run {
        let existing = fs::read_to_string(&pre_push_path).map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(pre_push_path.clone().into_boxed_path()),
            operation: "read pre-push hook".to_string(),
        })?;
        if existing == hook_script {
            return Ok("pre-push: unchanged".to_string());
        }
    }

    // Dry-run mode
    if dry_run {
        if pre_push_path.exists() {
            return Ok("pre-push: Would update".to_string());
        }
        return Ok("pre-push: Would create".to_string());
    }

    // Create hooks directory if needed
    if !hooks_dir.exists() {
        fs::create_dir_all(&hooks_dir).map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(hooks_dir.clone().into_boxed_path()),
            operation: "create hooks directory".to_string(),
        })?;
    }

    // Write the hook script
    let existed = pre_push_path.exists();
    fs::write(&pre_push_path, &hook_script).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(pre_push_path.clone().into_boxed_path()),
        operation: "write pre-push hook".to_string(),
    })?;

    // Make executable
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&pre_push_path)
            .map_err(|e| cuenv_core::Error::Io {
                source: e,
                path: Some(pre_push_path.clone().into_boxed_path()),
                operation: "get hook permissions".to_string(),
            })?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&pre_push_path, perms).map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(pre_push_path.clone().into_boxed_path()),
            operation: "set hook permissions".to_string(),
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

    #[test]
    fn test_generate_pre_push_script() {
        let script = generate_pre_push_script();
        assert!(script.contains("#!/bin/sh"));
        assert!(script.contains("cuenv task hooks.pre-push"));
    }
}
