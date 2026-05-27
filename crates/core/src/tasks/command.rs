use crate::environment::Environment;
use crate::{Error, Result};
use std::process::Stdio;
use tokio::process::Command;

/// Execute an arbitrary command with the cuenv environment
///
/// If `secrets` is provided, output will be captured and redacted before printing.
pub async fn execute_command(
    command: &str,
    args: &[String],
    environment: &Environment,
) -> Result<i32> {
    execute_command_with_redaction(command, args, environment, &[]).await
}

/// Execute a command with secret redaction
///
/// Secret values in stdout/stderr are replaced with [REDACTED].
pub async fn execute_command_with_redaction(
    command: &str,
    args: &[String],
    environment: &Environment,
    secrets: &[String],
) -> Result<i32> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    tracing::info!("Executing command: {} {:?}", command, args);
    let mut cmd = Command::new(command);
    cmd.args(args);

    let env_vars = environment.merge_with_system_hermetic();
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    if secrets.is_empty() {
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());
        cmd.stdin(Stdio::inherit());
        let status = cmd.status().await.map_err(|e| {
            Error::configuration(format!("Failed to execute command '{}': {}", command, e))
        })?;
        return Ok(status.code().unwrap_or(1));
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.stdin(Stdio::inherit());

    let mut child = cmd.spawn().map_err(|e| {
        Error::configuration(format!("Failed to execute command '{}': {}", command, e))
    })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::execution("stdout pipe not available"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::execution("stderr pipe not available"))?;

    let mut sorted_secrets: Vec<&str> = secrets.iter().map(String::as_str).collect();
    sorted_secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let sorted_secrets: Vec<String> = sorted_secrets.into_iter().map(String::from).collect();

    let secrets_clone = sorted_secrets.clone();
    let stdout_task = tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut redacted = line;
            for secret in &secrets_clone {
                if secret.len() >= 4 {
                    redacted = redacted.replace(secret, "[REDACTED]");
                }
            }
            cuenv_events::emit_stdout!(&redacted);
        }
    });

    let stderr_task = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut redacted = line;
            for secret in &sorted_secrets {
                if secret.len() >= 4 {
                    redacted = redacted.replace(secret, "[REDACTED]");
                }
            }
            cuenv_events::emit_stderr!(&redacted);
        }
    });

    let status = child.wait().await.map_err(|e| {
        Error::configuration(format!("Failed to wait for command '{}': {}", command, e))
    })?;

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    Ok(status.code().unwrap_or(1))
}
