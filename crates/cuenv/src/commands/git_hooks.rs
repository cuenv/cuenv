//! Git hook-related command implementations
//!
//! This module provides functionality for running pre-push hooks with
//! the files modified between local and remote refs.

use super::env_file::{self, EnvFileStatus, find_cue_module_root};
use super::{CommandExecutor, convert_engine_error, relative_path_from_root};
use cuengine::ModuleEvalOptions;
use cuenv_core::manifest::Project;
use cuenv_core::{ModuleEvaluation, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Get the list of files changed between local and remote refs.
///
/// This runs `git diff --name-only` between the local and remote refs to get
/// the list of files that will be pushed.
pub fn get_changed_files(
    repo_root: &Path,
    remote: &str,
    local_ref: Option<&str>,
    remote_ref: Option<&str>,
) -> Result<Vec<String>> {
    let local = local_ref.unwrap_or("HEAD");

    // Determine the remote ref to compare against
    // If remote_ref is provided, use it directly
    // Otherwise, try to get the upstream tracking branch
    let remote_target = if let Some(ref_name) = remote_ref {
        format!("{remote}/{ref_name}")
    } else {
        // Try to get the upstream tracking branch for HEAD
        let upstream_output = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "@{upstream}"])
            .current_dir(repo_root)
            .output();

        match upstream_output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            _ => {
                // Fall back to remote/main or remote/master
                let main_exists = Command::new("git")
                    .args(["rev-parse", "--verify", &format!("{remote}/main")])
                    .current_dir(repo_root)
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);

                if main_exists {
                    format!("{remote}/main")
                } else {
                    format!("{remote}/master")
                }
            }
        }
    };

    // Get the merge base between local and remote
    let merge_base_output = Command::new("git")
        .args(["merge-base", local, &remote_target])
        .current_dir(repo_root)
        .output()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to run git merge-base: {e}")))?;

    let base_ref = if merge_base_output.status.success() {
        String::from_utf8_lossy(&merge_base_output.stdout)
            .trim()
            .to_string()
    } else {
        // If merge-base fails (e.g., no common ancestor), compare directly
        remote_target.clone()
    };

    // Get changed files between base and local
    let diff_output = Command::new("git")
        .args(["diff", "--name-only", &base_ref, local])
        .current_dir(repo_root)
        .output()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to run git diff: {e}")))?;

    if !diff_output.status.success() {
        let stderr = String::from_utf8_lossy(&diff_output.stderr);
        return Err(cuenv_core::Error::configuration(format!(
            "git diff failed: {stderr}"
        )));
    }

    let files: Vec<String> = String::from_utf8_lossy(&diff_output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect();

    Ok(files)
}

/// Find the git repository root directory.
pub fn find_git_root(start_path: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(start_path)
        .output()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to run git: {e}")))?;

    if !output.status.success() {
        return Err(cuenv_core::Error::configuration(
            "Not in a git repository".to_string(),
        ));
    }

    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(PathBuf::from(root))
}

/// Helper to evaluate CUE configuration.
fn evaluate_config(
    directory: &Path,
    package: &str,
    executor: Option<&CommandExecutor>,
) -> Result<Project> {
    let target_path = directory
        .canonicalize()
        .map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(directory.to_path_buf().into_boxed_path()),
            operation: "canonicalize path".to_string(),
        })?;

    // Use executor's cached module if available
    if let Some(exec) = executor {
        tracing::debug!("Using cached module evaluation from executor");
        let module = exec.get_module(&target_path)?;
        let rel_path = relative_path_from_root(&module.root, &target_path);

        let instance = module.get(&rel_path).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "No CUE instance found at path: {} (relative: {})",
                target_path.display(),
                rel_path.display()
            ))
        })?;

        return instance.deserialize();
    }

    // Legacy path: fresh evaluation
    tracing::debug!("Using fresh module evaluation (no executor)");

    let module_root = find_cue_module_root(&target_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE module found (looking for cue.mod/) starting from: {}",
            target_path.display()
        ))
    })?;

    let options = ModuleEvalOptions {
        recursive: false,
        target_dir: Some(target_path.to_string_lossy().to_string()),
        ..Default::default()
    };
    let raw_result = cuengine::evaluate_module(&module_root, package, Some(&options))
        .map_err(convert_engine_error)?;

    let module = ModuleEvaluation::from_raw(
        module_root.clone(),
        raw_result.instances,
        raw_result.projects,
    );

    let rel_path = relative_path_from_root(&module_root, &target_path);
    let instance = module.get(&rel_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE instance found at path: {} (relative: {})",
            target_path.display(),
            rel_path.display()
        ))
    })?;

    instance.deserialize()
}

/// Execute pre-push hooks with the files changed between local and remote.
///
/// This function:
/// 1. Finds the git repository root
/// 2. Gets the list of changed files between local and remote refs
/// 3. Loads the CUE configuration and extracts pre-push hooks
/// 4. For each hook, filters its inputs to only include changed files
/// 5. Runs hooks that have matching changed files
pub async fn execute_pre_push(
    path: &str,
    package: &str,
    remote: &str,
    local_ref: Option<&str>,
    remote_ref: Option<&str>,
    executor: Option<&CommandExecutor>,
) -> Result<String> {
    use std::fmt::Write;

    // Validate path and find env.cue
    let directory = match env_file::find_env_file(Path::new(path), package)? {
        EnvFileStatus::Match(dir) => dir,
        EnvFileStatus::Missing => {
            return Err(cuenv_core::Error::configuration(format!(
                "No env.cue file found in '{path}'"
            )));
        }
        EnvFileStatus::PackageMismatch { found_package } => {
            let message = match found_package {
                Some(found) => format!(
                    "env.cue in '{path}' uses package '{found}', expected '{package}'"
                ),
                None => format!(
                    "env.cue in '{path}' is missing a package declaration (expected '{package}')"
                ),
            };
            return Err(cuenv_core::Error::configuration(message));
        }
    };

    // Find git root
    let git_root = find_git_root(&directory)?;

    // Get changed files
    let changed_files = get_changed_files(&git_root, remote, local_ref, remote_ref)?;

    if changed_files.is_empty() {
        return Ok("No files changed between local and remote. Nothing to check.".to_string());
    }

    // Evaluate CUE configuration
    let config = evaluate_config(&directory, package, executor)?;

    // Get pre-push hooks
    let hooks = config.pre_push_hooks();

    if hooks.is_empty() {
        return Ok("No pre-push hooks configured.".to_string());
    }

    let mut output = String::new();
    writeln!(&mut output, "Changed files ({} total):", changed_files.len()).ok();
    for file in &changed_files {
        writeln!(&mut output, "  - {file}").ok();
    }
    writeln!(&mut output).ok();

    // For each hook, check if any of its inputs match the changed files
    let mut hooks_to_run = Vec::new();

    for hook in &hooks {
        // Check if any input patterns match the changed files
        let matching_files = filter_matching_files(&hook.inputs, &changed_files, &git_root)?;

        if !matching_files.is_empty() {
            hooks_to_run.push((hook.clone(), matching_files));
        }
    }

    if hooks_to_run.is_empty() {
        return Ok(format!(
            "Changed files ({} total) don't match any pre-push hook inputs. Skipping.",
            changed_files.len()
        ));
    }

    writeln!(
        &mut output,
        "Running {} pre-push hook(s) with matching files:",
        hooks_to_run.len()
    )
    .ok();

    let mut all_success = true;

    for (hook, matching_files) in hooks_to_run {
        writeln!(&mut output, "\n[{}]", hook.command).ok();
        writeln!(&mut output, "  Matching files: {}", matching_files.len()).ok();

        // Build the command with matching files as arguments (if not already specified)
        let mut cmd = Command::new(&hook.command);

        // Add hook args
        for arg in &hook.args {
            cmd.arg(arg);
        }

        // If hook has no inputs specified, pass all matching files as positional args
        // This allows hooks to receive the changed files they should check
        if hook.inputs.is_empty() {
            for file in &matching_files {
                cmd.arg(file);
            }
        }

        // Set working directory
        let work_dir = if let Some(dir) = &hook.dir {
            if dir == "." {
                git_root.clone()
            } else {
                git_root.join(dir)
            }
        } else {
            git_root.clone()
        };
        cmd.current_dir(&work_dir);

        // Set CUENV_CHANGED_FILES environment variable with newline-separated list
        cmd.env("CUENV_CHANGED_FILES", matching_files.join("\n"));

        // Execute
        let result = cmd
            .output()
            .map_err(|e| cuenv_core::Error::configuration(format!("Failed to run hook: {e}")))?;

        let stdout = String::from_utf8_lossy(&result.stdout);
        let stderr = String::from_utf8_lossy(&result.stderr);

        if !stdout.is_empty() {
            writeln!(&mut output, "  stdout:\n{}", indent_lines(&stdout, "    ")).ok();
        }
        if !stderr.is_empty() {
            writeln!(&mut output, "  stderr:\n{}", indent_lines(&stderr, "    ")).ok();
        }

        if result.status.success() {
            writeln!(&mut output, "  Status: OK").ok();
        } else {
            writeln!(
                &mut output,
                "  Status: FAILED (exit code: {:?})",
                result.status.code()
            )
            .ok();
            all_success = false;
        }
    }

    if !all_success {
        return Err(cuenv_core::Error::configuration(format!(
            "Pre-push hooks failed:\n{output}"
        )));
    }

    Ok(output)
}

/// Filter files that match any of the input patterns.
fn filter_matching_files(
    input_patterns: &[String],
    changed_files: &[String],
    repo_root: &Path,
) -> Result<Vec<String>> {
    use glob::Pattern;

    // If no input patterns specified, all changed files match
    if input_patterns.is_empty() {
        return Ok(changed_files.to_vec());
    }

    let mut matching = Vec::new();

    for file in changed_files {
        for pattern in input_patterns {
            // Handle glob patterns
            let normalized_pattern = if pattern.ends_with('/') || pattern.ends_with("/**") {
                // Directory pattern - match any file inside
                format!("{}**/*", pattern.trim_end_matches("**/*").trim_end_matches('/'))
            } else if !pattern.contains('*') && !pattern.contains('?') {
                // Plain path - could be a directory or file
                // Check if it's a directory in the repo
                let full_path = repo_root.join(pattern);
                if full_path.is_dir() {
                    format!("{}/**/*", pattern)
                } else {
                    pattern.clone()
                }
            } else {
                pattern.clone()
            };

            match Pattern::new(&normalized_pattern) {
                Ok(glob_pattern) => {
                    if glob_pattern.matches(file) {
                        matching.push(file.clone());
                        break; // File matched, no need to check more patterns
                    }
                }
                Err(_) => {
                    // If pattern is invalid, try exact match
                    if file == pattern || file.starts_with(&format!("{pattern}/")) {
                        matching.push(file.clone());
                        break;
                    }
                }
            }
        }
    }

    Ok(matching)
}

/// Indent each line of text with the given prefix.
fn indent_lines(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Install git hooks into the .git/hooks directory.
pub async fn execute_install(path: &str, force: bool) -> Result<String> {
    use std::fmt::Write;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let directory = Path::new(path)
        .canonicalize()
        .map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(Path::new(path).to_path_buf().into_boxed_path()),
            operation: "canonicalize path".to_string(),
        })?;

    let git_root = find_git_root(&directory)?;
    let hooks_dir = git_root.join(".git/hooks");

    // Create hooks directory if it doesn't exist
    if !hooks_dir.exists() {
        fs::create_dir_all(&hooks_dir).map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(hooks_dir.clone().into_boxed_path()),
            operation: "create hooks directory".to_string(),
        })?;
    }

    let pre_push_hook = hooks_dir.join("pre-push");

    // Check if hook already exists
    if pre_push_hook.exists() && !force {
        return Err(cuenv_core::Error::configuration(format!(
            "pre-push hook already exists at {}. Use --force to overwrite.",
            pre_push_hook.display()
        )));
    }

    // Write the hook script
    let hook_script = r#"#!/bin/sh
# cuenv pre-push hook
# This hook runs cuenv git-hooks pre-push to validate changes before pushing

# Read stdin for refs being pushed (standard git pre-push input)
while read local_ref local_sha remote_ref remote_sha
do
    # Run cuenv pre-push hook with the refs
    cuenv git-hooks pre-push --local-ref "$local_sha" --remote-ref "${remote_ref#refs/heads/}"
    exit_code=$?

    if [ $exit_code -ne 0 ]; then
        echo "cuenv pre-push hook failed. Push aborted."
        exit $exit_code
    fi
done

exit 0
"#;

    fs::write(&pre_push_hook, hook_script).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(pre_push_hook.clone().into_boxed_path()),
        operation: "write pre-push hook".to_string(),
    })?;

    // Make executable
    let mut perms = fs::metadata(&pre_push_hook)
        .map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(pre_push_hook.clone().into_boxed_path()),
            operation: "get hook permissions".to_string(),
        })?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&pre_push_hook, perms).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(pre_push_hook.clone().into_boxed_path()),
        operation: "set hook permissions".to_string(),
    })?;

    let mut output = String::new();
    writeln!(&mut output, "Installed pre-push hook at: {}", pre_push_hook.display()).ok();
    writeln!(&mut output, "\nThe hook will run `cuenv git-hooks pre-push` before each push.").ok();
    writeln!(&mut output, "Configure hooks in your env.cue under hooks.prePush").ok();

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_filter_matching_files_empty_patterns() {
        let patterns: Vec<String> = vec![];
        let files = vec!["src/main.rs".to_string(), "Cargo.toml".to_string()];
        let temp = TempDir::new().unwrap();

        let result = filter_matching_files(&patterns, &files, temp.path()).unwrap();
        assert_eq!(result, files);
    }

    #[test]
    fn test_filter_matching_files_glob_pattern() {
        let patterns = vec!["src/**/*.rs".to_string()];
        let files = vec![
            "src/main.rs".to_string(),
            "src/lib/utils.rs".to_string(),
            "Cargo.toml".to_string(),
            "README.md".to_string(),
        ];
        let temp = TempDir::new().unwrap();

        let result = filter_matching_files(&patterns, &files, temp.path()).unwrap();
        assert_eq!(result, vec!["src/main.rs", "src/lib/utils.rs"]);
    }

    #[test]
    fn test_filter_matching_files_exact_match() {
        let patterns = vec!["Cargo.toml".to_string()];
        let files = vec![
            "src/main.rs".to_string(),
            "Cargo.toml".to_string(),
            "README.md".to_string(),
        ];
        let temp = TempDir::new().unwrap();

        let result = filter_matching_files(&patterns, &files, temp.path()).unwrap();
        assert_eq!(result, vec!["Cargo.toml"]);
    }

    #[test]
    fn test_filter_matching_files_multiple_patterns() {
        let patterns = vec!["src/**/*.rs".to_string(), "*.toml".to_string()];
        let files = vec![
            "src/main.rs".to_string(),
            "Cargo.toml".to_string(),
            "README.md".to_string(),
        ];
        let temp = TempDir::new().unwrap();

        let result = filter_matching_files(&patterns, &files, temp.path()).unwrap();
        assert_eq!(result, vec!["src/main.rs", "Cargo.toml"]);
    }

    #[test]
    fn test_indent_lines() {
        let text = "line1\nline2\nline3";
        let result = indent_lines(text, "  ");
        assert_eq!(result, "  line1\n  line2\n  line3");
    }
}
