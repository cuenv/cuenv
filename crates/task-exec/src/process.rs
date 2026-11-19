//! Host process execution support for tasks.

use super::{TaskResult, process_registry};
use cuenv_core::{Error, OutputCapture, Result};
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

pub struct ProcessOptions {
    pub(super) capture_output: OutputCapture,
    pub(super) timeout: Option<Duration>,
    /// Bind stdin to `/dev/null` and close inherited non-stdio file descriptors
    /// (`>= 3`) in `pre_exec`. Enabled for hermetic / confined runs.
    pub(super) isolate_stdio_and_fds: bool,
}

/// Run a host task process in captured or inherited-output mode.
pub async fn run_task_process(
    name: &str,
    command: Command,
    options: ProcessOptions,
) -> Result<TaskAttempt> {
    if options.capture_output.should_capture() {
        run_captured_process(
            name,
            command,
            options.timeout,
            options.isolate_stdio_and_fds,
        )
        .await
    } else {
        run_inherited_process(
            name,
            command,
            options.timeout,
            options.isolate_stdio_and_fds,
        )
        .await
    }
}

async fn run_captured_process(
    name: &str,
    mut command: Command,
    timeout: Option<Duration>,
    isolate_stdio_and_fds: bool,
) -> Result<TaskAttempt> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let start_time = std::time::Instant::now();

    #[cfg(unix)]
    setup_process_group(&mut command, isolate_stdio_and_fds);

    if isolate_stdio_and_fds {
        command.stdin(Stdio::null());
    }

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::io(format!("spawn task {}", name), e))?;

    let child_pid = child.id();
    if let Some(pid) = child_pid {
        process_registry::global_registry()
            .register(pid, name.to_string())
            .await;
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
        WaitOutcome::Exited(status) => {
            // Reap any lingering background descendants in the task's process
            // group before collecting outputs or publishing cache entries.
            reap_process_group(child_pid).await;
            status
        }
        WaitOutcome::TimedOut(timeout) => {
            if let Some(pid) = child_pid {
                process_registry::global_registry().unregister(pid).await;
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
        process_registry::global_registry().unregister(pid).await;
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
        from_cache: false,
        cache_source: None,
    }))
}

async fn run_inherited_process(
    name: &str,
    mut command: Command,
    timeout: Option<Duration>,
    isolate_stdio_and_fds: bool,
) -> Result<TaskAttempt> {
    #[cfg(unix)]
    setup_process_group(&mut command, isolate_stdio_and_fds);

    let stdin = if isolate_stdio_and_fds {
        Stdio::null()
    } else {
        Stdio::inherit()
    };

    let mut child = command
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(stdin)
        .spawn()
        .map_err(|e| Error::io(format!("spawn task {}", name), e))?;

    let child_pid = child.id();
    if let Some(pid) = child_pid {
        process_registry::global_registry()
            .register(pid, name.to_string())
            .await;
    }

    let start_time = std::time::Instant::now();
    let status = match wait_or_terminate(name, &mut child, timeout).await? {
        WaitOutcome::Exited(status) => {
            reap_process_group(child_pid).await;
            status
        }
        WaitOutcome::TimedOut(timeout) => {
            if let Some(pid) = child_pid {
                process_registry::global_registry().unregister(pid).await;
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
        process_registry::global_registry().unregister(pid).await;
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
        from_cache: false,
        cache_source: None,
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
        from_cache: false,
        cache_source: None,
    }
}

fn format_duration(duration: Duration) -> String {
    if duration.as_millis() < 1_000 {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{}s", duration.as_secs())
    }
}

/// Ensure no lingering descendants in the child's process group remain alive
/// after the lead process has exited.
async fn reap_process_group(child_pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = child_pid
        && let Ok(pgid) = i32::try_from(pid)
    {
        // Fast path: if no processes remain in `-pgid`, `kill(-pgid, 0)` returns
        // -1 with ESRCH immediately.
        if !process_group_alive(pgid) {
            return;
        }
        // SAFETY: The child was placed in its own process group by `setup_process_group`.
        #[expect(
            unsafe_code,
            reason = "Required to terminate lingering task descendants"
        )]
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
        }
        for _ in 0..10 {
            if !process_group_alive(pgid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // SAFETY: Escalate to SIGKILL for stubborn descendants.
        #[expect(
            unsafe_code,
            reason = "Required to terminate lingering task descendants"
        )]
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
        for _ in 0..25 {
            if !process_group_alive(pgid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    #[cfg(not(unix))]
    let _ = child_pid;
}

#[cfg(unix)]
fn process_group_alive(pgid: i32) -> bool {
    // SAFETY: Signal 0 probes whether any process exists in `-pgid`.
    #[expect(unsafe_code, reason = "Required to probe process group liveness")]
    let rc = unsafe { libc::kill(-pgid, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
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
            reap_process_group(child_pid).await;
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
            reap_process_group(child_pid).await;
            return Ok(());
        }
    }

    child
        .kill()
        .await
        .map_err(|e| Error::io("kill timed-out task", e))
}

/// Set up process group on Unix so we can kill the entire process tree on Ctrl-C
/// and reap lingering descendants on exit. When `close_extra_fds` is `true`,
/// closes all inherited non-stdio file descriptors (`>= 3`) using strictly
/// async-signal-safe syscalls.
#[cfg(unix)]
fn setup_process_group(cmd: &mut Command, close_extra_fds: bool) {
    // SAFETY: the child-side pre-exec closure only calls async-signal-safe
    // `setpgid` and `close`/`close_range`; it never allocates or acquires locks.
    #[expect(
        unsafe_code,
        reason = "Required for POSIX process group and FD hygiene"
    )]
    unsafe {
        cmd.pre_exec(move || {
            libc::setpgid(0, 0);
            if close_extra_fds {
                close_non_stdio_fds();
            }
            Ok(())
        });
    }
}

#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "Required for async-signal-safe FD closing after fork"
)]
fn close_non_stdio_fds() {
    unsafe {
        #[cfg(target_os = "linux")]
        {
            // Try close_range(3, ~0U, 0) first (Linux >= 5.9).
            if libc::syscall(libc::SYS_close_range, 3_u32, u32::MAX, 0_u32) == 0 {
                return;
            }
        }
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        let max_fd = if libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) == 0 {
            libc::c_int::try_from(limit.rlim_cur.min(4096)).unwrap_or(4096)
        } else {
            1024
        };
        for fd in 3..max_fd {
            let _ = libc::close(fd);
        }
    }
}
