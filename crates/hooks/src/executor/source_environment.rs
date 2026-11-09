//! Source-hook environment capture.

use crate::types::Hook;
use crate::{Error, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;
use tracing::debug;

type EnvironmentDelta = (HashMap<String, String>, Vec<String>);

/// Execute a single source hook and return the resulting environment.
///
/// This bypasses hook approval and state persistence, making it suitable for
/// runtime-backed environment materialization where the manifest itself is the
/// source of truth.
///
/// # Errors
///
/// Returns an error if the hook or source evaluation fails, or if the captured
/// environment contains invalid entries.
pub async fn capture_source_environment(
    hook: Hook,
    prior_env: &HashMap<String, String>,
    timeout_seconds: u64,
) -> Result<HashMap<String, String>> {
    if !hook.source.unwrap_or(false) {
        return Err(Error::configuration(
            "capture_source_environment requires a source hook",
        ));
    }

    let hook_result =
        super::hook_executor::execute_hook_with_timeout(hook, &timeout_seconds).await?;
    if !hook_result.success {
        return Err(Error::process(
            hook_result
                .error
                .unwrap_or_else(|| "Source hook failed".to_string()),
        ));
    }
    let (env_delta, removed_keys) =
        evaluate_shell_environment(&hook_result.stdout, prior_env, timeout_seconds).await?;

    let mut environment = prior_env.clone();
    for (key, value) in env_delta {
        environment.insert(key, value);
    }
    for key in removed_keys {
        environment.remove(&key);
    }

    Ok(environment)
}

/// Detect which shell to use for environment evaluation.
pub async fn detect_shell() -> String {
    if is_shell_capable("bash").await {
        return "bash".to_string();
    }

    if is_shell_capable("zsh").await {
        return "zsh".to_string();
    }

    "sh".to_string()
}

async fn is_shell_capable(shell: &str) -> bool {
    let check_script = "case x in x) true ;& y) true ;; esac";
    let mut command = Command::new(shell);
    command
        .arg("-c")
        .arg(check_script)
        .env_remove("CUENV_HOOK_SESSION");
    matches!(
        super::output::run_command(command, None, std::time::Duration::from_secs(3)).await,
        Ok(Some(output)) if output.status.success()
    )
}

/// Evaluate shell script and extract resulting environment variables.
pub async fn evaluate_shell_environment(
    shell_script: &str,
    prior_env: &HashMap<String, String>,
    timeout_seconds: u64,
) -> Result<EnvironmentDelta> {
    if prior_env.keys().any(|key| !is_valid_environment_name(key)) {
        return Err(Error::configuration(
            "Prior environment contains an invalid variable name",
        ));
    }
    let delimiter = format!("__CUENV_ENV_START_{}__", uuid::Uuid::new_v4().simple());

    debug!(
        "Evaluating shell script to extract environment ({} bytes)",
        shell_script.len()
    );

    let shell = if let Some(bash_path) = nix_bash_from_script(shell_script) {
        debug!("Detected Nix bash in source script");
        bash_path.to_string_lossy().into_owned()
    } else {
        detect_shell().await
    };

    debug!("Selected a shell for source evaluation");

    let env_cmd = find_env_command();
    let shell_context = ShellContext {
        shell: &shell,
        env_cmd: &env_cmd,
        prior_env,
        timeout: std::time::Duration::from_secs(timeout_seconds),
    };
    let env_before = capture_baseline_environment(&shell_context).await?;
    let filtered_script = filter_shell_script(shell_script);
    let output = evaluate_filtered_script(&shell_context, &delimiter, &filtered_script).await?;

    if !output.status.success() {
        return Err(Error::configuration("Source shell evaluation failed"));
    }

    let env_output_bytes = environment_output_bytes(&output.stdout, &delimiter)
        .ok_or_else(|| Error::configuration("Source shell did not finish environment capture"))?;
    let (env_delta, removed_keys) =
        parse_environment_delta(env_output_bytes, &env_before, prior_env)?;

    debug!(
        "Evaluated shell script and extracted {} new/changed environment variables ({} removed)",
        env_delta.len(),
        removed_keys.len()
    );
    Ok((env_delta, removed_keys))
}

pub(super) fn find_env_command() -> String {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("env");
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    "/usr/bin/env".to_string()
}

pub(super) fn nix_bash_from_script(shell_script: &str) -> Option<PathBuf> {
    for line in shell_script.lines() {
        if let Some(path) = line.strip_prefix("BASH='")
            && let Some(end) = path.find('\'')
        {
            let bash_path = PathBuf::from(&path[..end]);
            if bash_path.exists() {
                return Some(bash_path);
            }
        }
    }

    None
}

struct ShellContext<'shell> {
    shell: &'shell str,
    env_cmd: &'shell str,
    prior_env: &'shell HashMap<String, String>,
    timeout: std::time::Duration,
}

async fn capture_baseline_environment(
    context: &ShellContext<'_>,
) -> Result<HashMap<String, String>> {
    let mut cmd_before = Command::new(context.shell);
    cmd_before.arg("-c");
    cmd_before.arg(format!("{} -0", shell_quote(context.env_cmd)));
    cmd_before.stdout(Stdio::piped());
    cmd_before.stderr(Stdio::piped());
    apply_prior_env(&mut cmd_before, context.prior_env);

    let output_before = super::output::run_command(cmd_before, None, context.timeout)
        .await?
        .ok_or_else(|| Error::configuration("Baseline environment capture timed out"))?;

    if !output_before.status.success() {
        return Err(Error::configuration(
            "Failed to capture baseline environment",
        ));
    }

    Ok(parse_env_output(&output_before.stdout))
}

pub(super) fn filter_shell_script(shell_script: &str) -> String {
    shell_script
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty()
                && !trimmed.starts_with("✓")
                && !trimmed.starts_with("sh:")
                && !trimmed.starts_with("bash:")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn evaluate_filtered_script(
    context: &ShellContext<'_>,
    delimiter: &str,
    filtered_script: &str,
) -> Result<std::process::Output> {
    if filtered_script.contains('\0') {
        return Err(Error::configuration("Source script contains a null byte"));
    }
    let mut cmd = Command::new(context.shell);
    cmd.arg("-c");
    cmd.arg(source_evaluation_command(delimiter, context.env_cmd));
    apply_prior_env(&mut cmd, context.prior_env);
    super::output::run_command(cmd, Some(filtered_script.as_bytes()), context.timeout)
        .await?
        .ok_or_else(|| Error::configuration("Source shell evaluation timed out"))
}

pub(super) fn source_evaluation_command(delimiter: &str, env_command: &str) -> String {
    // A fresh, non-exported variable avoids passing the script through argv or
    // accidentally changing an inherited environment variable.
    let variable = format!("__cuenv_source_{}", uuid::Uuid::new_v4().simple());
    format!(
        "set -e\nunset {variable}\n{variable}=$(command cat) || exit 1\neval \"${variable}\" 1>&2\nunset {variable}\nprintf '\\0%s\\0' '{delimiter}'\n{} -0",
        shell_quote(env_command)
    )
}

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn apply_prior_env(cmd: &mut Command, prior_env: &HashMap<String, String>) {
    for (key, value) in prior_env {
        cmd.env(key, value);
    }
    cmd.env_remove("CUENV_HOOK_SESSION");
}

pub(super) fn environment_output_bytes<'a>(stdout: &'a [u8], delimiter: &str) -> Option<&'a [u8]> {
    let delimiter_bytes = format!("\0{delimiter}\0").into_bytes();
    let mut matches = stdout
        .windows(delimiter_bytes.len())
        .enumerate()
        .filter_map(|(index, window)| (window == delimiter_bytes).then_some(index));
    let first = matches.next()?;
    if first != 0 || matches.next().is_some() {
        return None;
    }
    Some(&stdout[first + delimiter_bytes.len()..])
}

pub(super) fn is_valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'a'..=b'z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

pub(super) fn parse_env_output(bytes: &[u8]) -> HashMap<String, String> {
    String::from_utf8_lossy(bytes)
        .split('\0')
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

pub(super) fn parse_environment_delta(
    env_output_bytes: &[u8],
    env_before: &HashMap<String, String>,
    prior_env: &HashMap<String, String>,
) -> Result<EnvironmentDelta> {
    let env_output = String::from_utf8_lossy(env_output_bytes);
    let mut env_delta = HashMap::new();
    let mut post_env_keys = std::collections::HashSet::new();

    for line in env_output.split('\0') {
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if should_skip_env_key(key) {
                continue;
            }

            if !is_valid_environment_name(key) {
                if env_before.get(key).is_none_or(|before| before != value) {
                    return Err(Error::configuration(
                        "Source shell returned an invalid environment variable name",
                    ));
                }
                continue;
            }

            if !post_env_keys.insert(key.to_string()) {
                return Err(Error::configuration(
                    "Source shell returned a duplicate environment variable",
                ));
            }

            if env_before.get(key).is_none_or(|before| before != value) {
                env_delta.insert(key.to_string(), value.to_string());
            }
        } else {
            return Err(Error::configuration(
                "Source shell returned a malformed environment entry",
            ));
        }
    }

    let removed_keys = prior_env
        .keys()
        .filter(|key| !should_skip_env_key(key) && !post_env_keys.contains(key.as_str()))
        .cloned()
        .collect();

    Ok((env_delta, removed_keys))
}

fn should_skip_env_key(key: &str) -> bool {
    key.starts_with("BASH_FUNC_")
        || key == "PS1"
        || key == "PS2"
        || key == "_"
        || key == "PWD"
        || key == "OLDPWD"
        || key == "SHLVL"
        || key.starts_with("BASH")
}
