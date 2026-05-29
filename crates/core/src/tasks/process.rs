//! Host process execution support for tasks.

use super::TaskResult;
use super::process_registry::global_registry;
use crate::{Error, OutputCapture, Result};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Outcome of a single host process attempt.
///
/// A timeout is a hard policy violation, not an ordinary failure: callers must
/// be able to tell the two apart without inspecting stderr text (so a task that
/// merely prints "timed out" is never mistaken for one that exceeded its
/// deadline, and so timed-out attempts are never retried).
pub enum TaskAttempt {
    /// The process ran to completion (the inner result may still be a failure).
    Completed(TaskResult),
    /// The process exceeded its timeout and its process group was terminated.
    TimedOut(TaskResult),
}

/// Run a host task process in captured or inherited-output mode.
pub async fn run_task_process(
    name: &str,
    command: Command,
    capture_output: OutputCapture,
    timeout: Option<Duration>,
) -> Result<TaskAttempt> {
    if capture_output.should_capture() {
        run_captured_process(name, command, timeout).await
    } else {
        run_inherited_process(name, command, timeout).await
    }
}

async fn run_captured_process(
    name: &str,
    mut command: Command,
    timeout: Option<Duration>,
) -> Result<TaskAttempt> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let start_time = std::time::Instant::now();

    #[cfg(unix)]
    setup_process_group(&mut command);

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Io {
            source: e,
            path: None,
            operation: format!("spawn task {}", name),
        })?;

    let child_pid = child.id();
    if let Some(pid) = child_pid {
        global_registry().register(pid, name.to_string()).await;
    }

    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    let name_for_stdout = name.to_string();
    let stdout_task = tokio::spawn(async move {
        let mut lines = Vec::new();
        if let Some(stdout) = stdout_handle {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                cuenv_events::emit_task_output!(name_for_stdout, "stdout", line);
                lines.push(line);
            }
        }
        lines
    });

    let name_for_stderr = name.to_string();
    let stderr_task = tokio::spawn(async move {
        let mut lines = Vec::new();
        if let Some(stderr) = stderr_handle {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                cuenv_events::emit_task_output!(name_for_stderr, "stderr", line);
                lines.push(line);
            }
        }
        lines
    });

    let status = match wait_or_terminate(name, &mut child, timeout).await? {
        WaitOutcome::Exited(status) => status,
        WaitOutcome::TimedOut(timeout) => {
            if let Some(pid) = child_pid {
                global_registry().unregister(pid).await;
            }
            let stdout = stdout_task.await.unwrap_or_default().join("\n");
            let stderr = stderr_task.await.unwrap_or_default().join("\n");
            return Ok(TaskAttempt::TimedOut(timeout_result(TimedOutTask {
                name,
                timeout,
                start_time,
                stdout,
                stderr,
            })));
        }
    };

    if let Some(pid) = child_pid {
        global_registry().unregister(pid).await;
    }

    let stdout_lines = stdout_task.await.unwrap_or_default();
    let stderr_lines = stderr_task.await.unwrap_or_default();
    let duration_ms = u64::try_from(start_time.elapsed().as_millis()).unwrap_or(u64::MAX);
    let stdout = stdout_lines.join("\n");
    let stderr = stderr_lines.join("\n");
    let exit_code = status.code().unwrap_or(-1);
    let success = status.success();

    cuenv_events::emit_task_completed!(name, success, Some(exit_code), duration_ms);

    if !success {
        tracing::warn!(task = %name, exit = exit_code, "Task failed");
        tracing::error!(task = %name, "Task stdout:\n{}", stdout);
        tracing::error!(task = %name, "Task stderr:\n{}", stderr);
    }

    Ok(TaskAttempt::Completed(TaskResult {
        name: name.to_string(),
        exit_code: Some(exit_code),
        stdout,
        stderr,
        success,
    }))
}

async fn run_inherited_process(
    name: &str,
    mut command: Command,
    timeout: Option<Duration>,
) -> Result<TaskAttempt> {
    #[cfg(unix)]
    setup_process_group(&mut command);

    let mut child = command
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::inherit())
        .spawn()
        .map_err(|e| Error::Io {
            source: e,
            path: None,
            operation: format!("spawn task {}", name),
        })?;

    let child_pid = child.id();
    if let Some(pid) = child_pid {
        global_registry().register(pid, name.to_string()).await;
    }

    let start_time = std::time::Instant::now();
    let status = match wait_or_terminate(name, &mut child, timeout).await? {
        WaitOutcome::Exited(status) => status,
        WaitOutcome::TimedOut(timeout) => {
            if let Some(pid) = child_pid {
                global_registry().unregister(pid).await;
            }
            return Ok(TaskAttempt::TimedOut(timeout_result(TimedOutTask {
                name,
                timeout,
                start_time,
                stdout: String::new(),
                stderr: String::new(),
            })));
        }
    };

    if let Some(pid) = child_pid {
        global_registry().unregister(pid).await;
    }

    let exit_code = status.code().unwrap_or(-1);
    let success = status.success();

    if !success {
        tracing::warn!(task = %name, exit = exit_code, "Task failed");
    }

    Ok(TaskAttempt::Completed(TaskResult {
        name: name.to_string(),
        exit_code: Some(exit_code),
        stdout: String::new(),
        stderr: String::new(),
        success,
    }))
}

/// Whether a child exited on its own or was terminated for exceeding its timeout.
enum WaitOutcome {
    Exited(std::process::ExitStatus),
    TimedOut(Duration),
}

/// Wait for `child` to exit, terminating its whole process group if `timeout`
/// elapses first. This is the single place both capture modes share their
/// timeout handling, so the SIGTERM/SIGKILL ladder lives in exactly one path.
async fn wait_or_terminate(
    name: &str,
    child: &mut tokio::process::Child,
    timeout: Option<Duration>,
) -> Result<WaitOutcome> {
    let Some(timeout) = timeout else {
        let status = child.wait().await.map_err(|e| Error::Io {
            source: e,
            path: None,
            operation: format!("wait for task {name}"),
        })?;
        return Ok(WaitOutcome::Exited(status));
    };

    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => {
            let status = status.map_err(|e| Error::Io {
                source: e,
                path: None,
                operation: format!("wait for task {name}"),
            })?;
            Ok(WaitOutcome::Exited(status))
        }
        Err(_) => {
            let child_pid = child.id();
            terminate_child(child, child_pid).await?;
            Ok(WaitOutcome::TimedOut(timeout))
        }
    }
}

/// Inputs for building a timed-out [`TaskResult`].
struct TimedOutTask<'a> {
    name: &'a str,
    timeout: Duration,
    start_time: std::time::Instant,
    stdout: String,
    stderr: String,
}

fn timeout_result(task: TimedOutTask<'_>) -> TaskResult {
    let duration_ms = u64::try_from(task.start_time.elapsed().as_millis()).unwrap_or(u64::MAX);
    let message = format!("Task timed out after {}", format_duration(task.timeout));
    cuenv_events::emit_task_output!(task.name, "stderr", &message);
    cuenv_events::emit_task_completed!(task.name, false, None, duration_ms);
    tracing::warn!(task = %task.name, timeout_ms = task.timeout.as_millis(), "Task timed out");

    TaskResult {
        name: task.name.to_string(),
        exit_code: None,
        stdout: task.stdout,
        stderr: if task.stderr.is_empty() {
            message
        } else {
            format!("{}\n{message}", task.stderr)
        },
        success: false,
    }
}

fn format_duration(duration: Duration) -> String {
    if duration.as_millis() < 1_000 {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{}s", duration.as_secs())
    }
}

async fn terminate_child(child: &mut tokio::process::Child, child_pid: Option<u32>) -> Result<()> {
    #[cfg(unix)]
    if let Some(pid) = child_pid {
        let pgid = i32::try_from(pid)
            .map_err(|e| Error::execution(format!("invalid child pid {pid}: {e}")))?;
        // SAFETY: The child was started in its own process group by `setup_process_group`.
        // Sending SIGTERM to `-pgid` targets that process group so descendants exit too.
        #[expect(unsafe_code, reason = "Required for POSIX process group termination")]
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
        }
        if tokio::time::timeout(Duration::from_millis(500), child.wait())
            .await
            .is_ok()
        {
            return Ok(());
        }
        // SAFETY: Same process group as above; SIGKILL is the final fallback after SIGTERM.
        #[expect(unsafe_code, reason = "Required for POSIX process group termination")]
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
        if tokio::time::timeout(Duration::from_millis(500), child.wait())
            .await
            .is_ok()
        {
            return Ok(());
        }
    }

    child.kill().await.map_err(|e| Error::Io {
        source: e,
        path: None,
        operation: "kill timed-out task".to_string(),
    })
}

/// Set up process group on Unix so we can kill the entire process tree on Ctrl-C.
///
/// This creates a new process group with the spawned process as the leader,
/// allowing us to send signals to all descendants when terminating.
#[cfg(unix)]
fn setup_process_group(cmd: &mut Command) {
    // SAFETY: setpgid(0, 0) creates a new process group with this process as leader.
    // This is safe to call in the pre-spawn hook as it only affects the child process.
    // It allows us to send signals to the entire process group when terminating.
    #[expect(unsafe_code, reason = "Required for POSIX process group management")]
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
}
