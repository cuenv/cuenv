//! Source-hook environment capture.

use super::execute_hook_with_timeout;
use crate::types::Hook;
use crate::{Error, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;
use tracing::{debug, warn};

/// Execute a single source hook and return the resulting environment.
///
/// This bypasses hook approval and state persistence, making it suitable for
/// runtime-backed environment materialization where the manifest itself is the
/// source of truth.
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

    let hook_result = execute_hook_with_timeout(hook, &timeout_seconds).await?;
    let (env_delta, removed_keys) =
        evaluate_shell_environment(&hook_result.stdout, prior_env).await?;

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
    match Command::new(shell)
        .arg("-c")
        .arg(check_script)
        .output()
        .await
    {
        Ok(output) => output.status.success(),
        Err(_) => false,
    }
}

/// Evaluate shell script and extract resulting environment variables.
pub async fn evaluate_shell_environment(
    shell_script: &str,
    prior_env: &HashMap<String, String>,
) -> Result<(HashMap<String, String>, Vec<String>)> {
    const DELIMITER: &str = "__CUENV_ENV_START__";

    debug!(
        "Evaluating shell script to extract environment ({} bytes)",
        shell_script.len()
    );

    tracing::trace!("Raw shell script from hook:\n{}", shell_script);

    let shell = if let Some(bash_path) = nix_bash_from_script(shell_script) {
        debug!("Detected Nix bash in script: {}", bash_path.display());
        bash_path.to_string_lossy().into_owned()
    } else {
        detect_shell().await
    };

    debug!("Using shell: {}", shell);

    let env_cmd = find_env_command();
    let env_before = capture_baseline_environment(&shell, &env_cmd, prior_env).await?;
    let filtered_script = filter_shell_script(shell_script);
    tracing::trace!("Filtered shell script:\n{}", filtered_script);

    let output =
        evaluate_filtered_script(&shell, &env_cmd, DELIMITER, &filtered_script, prior_env).await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            "Shell script evaluation finished with error (exit code {:?}): {}",
            output.status.code(),
            stderr
        );
    }

    let env_output_bytes = environment_output_bytes(&output.stdout, DELIMITER);
    let (env_delta, removed_keys) =
        parse_environment_delta(env_output_bytes, &env_before, prior_env);

    if env_delta.is_empty() && removed_keys.is_empty() && !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::configuration(format!(
            "Shell script evaluation failed and no environment captured. Error: {}",
            stderr
        )));
    }

    debug!(
        "Evaluated shell script and extracted {} new/changed environment variables ({} removed)",
        env_delta.len(),
        removed_keys.len()
    );
    Ok((env_delta, removed_keys))
}

fn find_env_command() -> String {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("env");
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    "/usr/bin/env".to_string()
}

fn nix_bash_from_script(shell_script: &str) -> Option<PathBuf> {
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

async fn capture_baseline_environment(
    shell: &str,
    env_cmd: &str,
    prior_env: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    let mut cmd_before = Command::new(shell);
    cmd_before.arg("-c");
    cmd_before.arg(format!("{env_cmd} -0"));
    cmd_before.stdout(Stdio::piped());
    cmd_before.stderr(Stdio::piped());
    apply_prior_env(&mut cmd_before, prior_env);

    let output_before = cmd_before
        .output()
        .await
        .map_err(|e| Error::configuration(format!("Failed to get initial environment: {}", e)))?;

    Ok(parse_env_output(&output_before.stdout))
}

fn filter_shell_script(shell_script: &str) -> String {
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
    shell: &str,
    env_cmd: &str,
    delimiter: &str,
    filtered_script: &str,
    prior_env: &HashMap<String, String>,
) -> Result<std::process::Output> {
    let mut cmd = Command::new(shell);
    cmd.arg("-c");
    cmd.arg(format!(
        "{}\necho -ne '\\0{}\\0'; {env_cmd} -0",
        filtered_script, delimiter
    ));
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    apply_prior_env(&mut cmd, prior_env);

    cmd.output()
        .await
        .map_err(|e| Error::configuration(format!("Failed to evaluate shell environment: {}", e)))
}

fn apply_prior_env(cmd: &mut Command, prior_env: &HashMap<String, String>) {
    for (key, value) in prior_env {
        cmd.env(key, value);
    }
}

fn environment_output_bytes<'a>(stdout: &'a [u8], delimiter: &str) -> &'a [u8] {
    let delimiter_bytes = format!("\0{delimiter}\0").into_bytes();
    if let Some(idx) = stdout
        .windows(delimiter_bytes.len())
        .position(|window| window == delimiter_bytes)
    {
        return &stdout[idx + delimiter_bytes.len()..];
    }

    debug!("Environment delimiter not found in hook output");
    let len = stdout.len();
    let start = len.saturating_sub(1000);
    let tail = String::from_utf8_lossy(&stdout[start..]);
    warn!(
        "Delimiter missing. Tail of stdout (last 1000 bytes):\n{}",
        tail
    );

    &[]
}

fn parse_env_output(bytes: &[u8]) -> HashMap<String, String> {
    String::from_utf8_lossy(bytes)
        .split('\0')
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn parse_environment_delta(
    env_output_bytes: &[u8],
    env_before: &HashMap<String, String>,
    prior_env: &HashMap<String, String>,
) -> (HashMap<String, String>, Vec<String>) {
    let env_output = String::from_utf8_lossy(env_output_bytes);
    let mut env_delta = HashMap::new();
    let mut post_env_keys = std::collections::HashSet::new();

    for line in env_output.split('\0') {
        if let Some((key, value)) = line.split_once('=') {
            if should_skip_env_key(key) {
                continue;
            }

            if !key.is_empty() {
                post_env_keys.insert(key.to_string());
            }

            if !key.is_empty() && env_before.get(key) != Some(&value.to_string()) {
                env_delta.insert(key.to_string(), value.to_string());
            }
        }
    }

    let removed_keys = prior_env
        .keys()
        .filter(|key| !should_skip_env_key(key) && !post_env_keys.contains(key.as_str()))
        .cloned()
        .collect();

    (env_delta, removed_keys)
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
