//! Host process execution support for tasks.

use super::{TaskResult, process_registry};
use cuenv_core::{Error, OutputCapture, Result};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

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

pub struct ProcessOptions {
    pub(super) capture_output: OutputCapture,
    pub(super) timeout: Option<Duration>,
}

/// Run a host task process in captured or inherited-output mode.
pub async fn run_task_process(
    name: &str,
    command: Command,
    options: ProcessOptions,
) -> Result<TaskAttempt> {
    if options.capture_output.should_capture() {
        run_captured_process(name, command, options.timeout).await
    } else {
        run_inherited_process(name, command, options.timeout).await
    }
}

async fn run_captured_process(
    name: &str,
    mut command: Command,
    timeout: Option<Duration>,
) -> Result<TaskAttempt> {
    let start_time = std::time::Instant::now();
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut process = TaskProcess::spawn(name, &mut command).await?;
    let stdout_task = Capture::spawn(name, "stdout", process.child.stdout.take());
    let stderr_task = Capture::spawn(name, "stderr", process.child.stderr.take());

    let outcome = wait_or_terminate(name, &mut process.child, timeout).await?;
    drop(process);
    let (stdout, stderr) = tokio::join!(stdout_task.finish(), stderr_task.finish());
    let status = match outcome {
        WaitOutcome::Exited(status) => status,
        WaitOutcome::TimedOut(timeout) => {
            return Ok(TaskAttempt::TimedOut(timeout_result(TimedOutTask {
                name,
                timeout,
                start_time,
                stdout,
                stderr,
            })));
        }
    };
    let duration_ms = u64::try_from(start_time.elapsed().as_millis()).unwrap_or(u64::MAX);
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
    command
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::inherit());
    let mut process = TaskProcess::spawn(name, &mut command).await?;
    let start_time = std::time::Instant::now();
    let outcome = wait_or_terminate(name, &mut process.child, timeout).await?;
    drop(process);
    let status = match outcome {
        WaitOutcome::Exited(status) => status,
        WaitOutcome::TimedOut(timeout) => {
            return Ok(TaskAttempt::TimedOut(timeout_result(TimedOutTask {
                name,
                timeout,
                start_time,
                stdout: String::new(),
                stderr: String::new(),
            })));
        }
    };

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

struct TaskProcess {
    child: Child,
    registered_pid: Option<u32>,
    registry: Arc<process_registry::ProcessRegistry>,
    registration: Arc<()>,
}

impl TaskProcess {
    async fn spawn(name: &str, command: &mut Command) -> Result<Self> {
        #[cfg(unix)]
        command.process_group(0);
        let child = command
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| Error::io(format!("spawn task {name}"), error))?;
        let process = Self {
            registered_pid: child.id(),
            child,
            registry: process_registry::global_registry(),
            registration: Arc::new(()),
        };
        if let Some(pid) = process.registered_pid {
            process
                .registry
                .register_tracked(pid, name.to_string(), Arc::downgrade(&process.registration))
                .await;
        }
        Ok(process)
    }
}

impl Drop for TaskProcess {
    fn drop(&mut self) {
        // Tokio returns no id after reaping, so the cancellation path never
        // signals a group by a PID the completed child no longer owns.
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            process_registry::signal_process_group(pid, rustix::process::Signal::KILL);
        }
        if let Some(pid) = self.registered_pid {
            self.registry.forget_tracked(pid, &self.registration);
        }
    }
}

struct Capture(JoinHandle<Vec<String>>);

impl Capture {
    fn spawn(
        name: &str,
        stream: &'static str,
        output: Option<impl AsyncRead + Unpin + Send + 'static>,
    ) -> Self {
        let name = name.to_string();
        Self(tokio::spawn(async move {
            let mut lines = Vec::new();
            if let Some(output) = output {
                let mut reader = BufReader::new(output).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    cuenv_events::emit_task_output!(name, stream, line);
                    lines.push(line);
                }
            }
            lines
        }))
    }

    async fn finish(mut self) -> String {
        (&mut self.0).await.unwrap_or_default().join("\n")
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.0.abort();
    }
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
    child: &mut Child,
    timeout: Option<Duration>,
) -> Result<WaitOutcome> {
    let Some(timeout) = timeout else {
        let status = child
            .wait()
            .await
            .map_err(|e| Error::io(format!("wait for task {name}"), e))?;
        return Ok(WaitOutcome::Exited(status));
    };

    if let Ok(status) = tokio::time::timeout(timeout, child.wait()).await {
        let status = status.map_err(|e| Error::io(format!("wait for task {name}"), e))?;
        Ok(WaitOutcome::Exited(status))
    } else {
        let child_pid = child.id();
        terminate_child(child, child_pid).await?;
        Ok(WaitOutcome::TimedOut(timeout))
    }
}

/// Inputs for building a timed-out [`TaskResult`].
struct TimedOutTask<'task> {
    name: &'task str,
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

async fn terminate_child(child: &mut Child, child_pid: Option<u32>) -> Result<()> {
    #[cfg(unix)]
    if let Some(pid) = child_pid {
        process_registry::signal_process_group(pid, rustix::process::Signal::TERM);
        if tokio::time::timeout(Duration::from_millis(500), child.wait())
            .await
            .is_ok()
        {
            return Ok(());
        }
        process_registry::signal_process_group(pid, rustix::process::Signal::KILL);
        if tokio::time::timeout(Duration::from_millis(500), child.wait())
            .await
            .is_ok()
        {
            return Ok(());
        }
    }

    child
        .kill()
        .await
        .map_err(|e| Error::io("kill timed-out task", e))
}
