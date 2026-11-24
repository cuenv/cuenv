//! Host process execution support for tasks.

use super::{TaskResult, process_registry};
use cuenv_core::{Error, OutputCapture, Result};
use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::process::Command;

/// The executable that tethers task processes to cuenv, if any.
static TASK_SUPERVISOR: OnceLock<PathBuf> = OnceLock::new();

/// Run every task process under `<executable> __supervise`, which kills the
/// task's process group when cuenv dies, even by `kill -9` (issue 458).
///
/// For macOS, which has no parent-death signal; on Linux the task itself
/// asks the kernel for one. The cuenv binary passes its own executable.
/// Without this call, as in embedders and tests whose executable has no
/// `__supervise`, tasks are spawned directly. Only the first call counts.
pub fn supervise_tasks_with(executable: PathBuf) {
    let _ = TASK_SUPERVISOR.set(executable);
}

/// A command that runs `program` as a task, under the supervisor when one
/// is configured. Arguments, environment and working directory added to
/// the command reach `program` unchanged through the wrapper, which shares
/// its process group.
pub fn task_command(program: impl AsRef<OsStr>) -> Command {
    match TASK_SUPERVISOR.get() {
        Some(supervisor) => {
            let mut command = Command::new(supervisor);
            command.arg("__supervise").arg(program);
            command
        }
        None => Command::new(program),
    }
}

/// The error for a task process that could not start. A program missing from
/// `PATH` is named, since the bare `No such file or directory` from the OS
/// does not say which program was looked for.
fn spawn_error(task: &str, command: &Command, error: std::io::Error) -> Error {
    if error.kind() != std::io::ErrorKind::NotFound {
        return Error::io(format!("spawn task {task}"), error);
    }
    let std_command = command.as_std();
    // Under the supervisor the task's own program follows `__supervise`.
    let program = if TASK_SUPERVISOR.get().is_some() {
        std_command.get_args().nth(1)
    } else {
        Some(std_command.get_program())
    };
    match program {
        Some(program) => Error::configuration(format!(
            "task {task}: program `{}` was not found; install it or provide it through the project runtime",
            program.to_string_lossy()
        )),
        None => Error::io(format!("spawn task {task}"), error),
    }
}

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
    /// Bind stdin to `/dev/null` and mark inherited non-stdio file descriptors
    /// (`>= 3`) close-on-exec. Enabled for hermetic and confined runs.
    pub(super) isolate_stdio_and_fds: bool,
    /// Control socket supplied as the Linux sandbox launcher’s standard input.
    pub(super) launcher_stdin: Option<Stdio>,
    pub(super) launcher_exchange: Option<super::sandbox::SandboxExchange>,
}

/// Run a host task process in captured or inherited-output mode.
pub async fn run_task_process(
    name: &str,
    command: Command,
    options: ProcessOptions,
) -> Result<TaskAttempt> {
    if options.capture_output.should_capture() {
        run_captured_process(name, command, options).await
    } else {
        run_inherited_process(name, command, options).await
    }
}

async fn run_captured_process(
    name: &str,
    mut command: Command,
    options: ProcessOptions,
) -> Result<TaskAttempt> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let start_time = std::time::Instant::now();

    #[cfg(unix)]
    setup_process_group(&mut command, &options);

    if let Some(stdin) = options.launcher_stdin {
        command.stdin(stdin);
    } else if options.isolate_stdio_and_fds {
        command.stdin(Stdio::null());
    }

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| spawn_error(name, &command, e))?;

    let child_pid = child.id();
    // Declared after `child`, so it is dropped first, while the leader is
    // not yet reaped and its process group id cannot have been reused.
    let mut guard = ProcessGroupGuard::new(child_pid);
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

    let status = match wait_or_terminate(
        name,
        &mut child,
        WaitOptions {
            timeout: options.timeout,
            exchange: options.launcher_exchange,
        },
    )
    .await?
    {
        WaitOutcome::Exited(status) => {
            // Reap any lingering background descendants in the task's process
            // group before collecting outputs or publishing cache entries.
            reap_process_group(child_pid).await;
            guard.disarm();
            status
        }
        WaitOutcome::TimedOut(timeout) => {
            guard.disarm();
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
    options: ProcessOptions,
) -> Result<TaskAttempt> {
    #[cfg(unix)]
    setup_process_group(&mut command, &options);

    let stdin = if let Some(stdin) = options.launcher_stdin {
        stdin
    } else if options.isolate_stdio_and_fds {
        Stdio::null()
    } else {
        Stdio::inherit()
    };

    let mut child = command
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(stdin)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| spawn_error(name, &command, e))?;

    let child_pid = child.id();
    // Dropped before `child`; see `run_captured_process`.
    let mut guard = ProcessGroupGuard::new(child_pid);
    if let Some(pid) = child_pid {
        process_registry::global_registry()
            .register(pid, name.to_string())
            .await;
    }

    let start_time = std::time::Instant::now();
    let status = match wait_or_terminate(
        name,
        &mut child,
        WaitOptions {
            timeout: options.timeout,
            exchange: options.launcher_exchange,
        },
    )
    .await?
    {
        WaitOutcome::Exited(status) => {
            reap_process_group(child_pid).await;
            guard.disarm();
            status
        }
        WaitOutcome::TimedOut(timeout) => {
            guard.disarm();
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

struct WaitOptions {
    timeout: Option<Duration>,
    exchange: Option<super::sandbox::SandboxExchange>,
}

/// Wait for `child` to exit, terminating its whole process group if `timeout`
/// elapses first. This is the single place both capture modes share their
/// timeout handling, so the SIGTERM/SIGKILL ladder lives in exactly one path.
async fn wait_or_terminate(
    name: &str,
    child: &mut tokio::process::Child,
    options: WaitOptions,
) -> Result<WaitOutcome> {
    let wait = async {
        if let Some(exchange) = options.exchange {
            tokio::select! {
                status = child.wait() => return status,
                transferred = exchange => {
                    if let Err(error) = transferred {
                        tracing::debug!(task = %name, %error, "strict sandbox launch exchange ended");
                    }
                }
            }
        }
        child.wait().await
    };

    let Some(timeout) = options.timeout else {
        let status = wait
            .await
            .map_err(|e| Error::io(format!("wait for task {name}"), e))?;
        return Ok(WaitOutcome::Exited(status));
    };

    if let Ok(status) = tokio::time::timeout(timeout, wait).await {
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

/// Kills a task's process group when the future running the task is
/// dropped before the task finished.
///
/// Fail-fast aborts the futures of sibling tasks (issue 458). Dropping a
/// future does not stop the processes it started, so without this they
/// would keep running, and writing into the workspace, after cuenv reported
/// the failure and exited. The guard signals only its own group, while the
/// group leader is still unreaped, so the group id cannot belong to anyone
/// else; it never walks the process-wide registry.
struct ProcessGroupGuard {
    /// The group leader's pid, until the task has finished.
    pid: Option<u32>,
}

impl ProcessGroupGuard {
    const fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }

    /// The task finished and its group was reaped: nothing to kill.
    const fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        let Some(pid) = self.pid.take() else {
            return;
        };
        #[cfg(unix)]
        if let Ok(pgid) = i32::try_from(pid) {
            #[expect(
                unsafe_code,
                reason = "Required to stop an abandoned task's process group"
            )]
            // SAFETY: `kill` takes plain integers. The child was placed in its
            // own process group by `setup_process_group`, and its leader has
            // not been reaped, so `-pgid` names that group alone.
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
        forget_registration(pid);
    }
}

/// Remove `pid` from the process registry without awaiting, so a later
/// shutdown never signals a pid that may have been reused.
fn forget_registration(pid: u32) {
    let registry = process_registry::global_registry();
    if registry.try_unregister(pid) {
        return;
    }
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move { registry.unregister(pid).await });
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
        #[expect(
            unsafe_code,
            reason = "Required to terminate lingering task descendants"
        )]
        // SAFETY: The child was placed in its own process group by `setup_process_group`.
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
        }
        for _ in 0..10 {
            if !process_group_alive(pgid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        #[expect(
            unsafe_code,
            reason = "Required to terminate lingering task descendants"
        )]
        // SAFETY: Escalate to SIGKILL for stubborn descendants.
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
/// marks inherited non-stdio file descriptors (`>= 3`) close-on-exec.
#[cfg(unix)]
fn setup_process_group(cmd: &mut Command, options: &ProcessOptions) {
    let close_extra_fds = options.isolate_stdio_and_fds;
    #[expect(
        unsafe_code,
        reason = "Required for POSIX process group, parent-death signal and FD hygiene"
    )]
    // SAFETY: the child-side pre-exec closure only calls async-signal-safe
    // `setpgid`, `prctl`, `close_range`, and `fcntl`.
    unsafe {
        cmd.pre_exec(move || {
            libc::setpgid(0, 0);
            // Die with cuenv, as services do: a task nobody supervises any
            // more must not keep writing into the workspace after a
            // `kill -9`, an OOM kill or a hard-stopped CI job (issue 458).
            // A strict sandbox may run this hook in a namespace process
            // below the one cuenv spawned; the signal then follows that
            // process, which the sandbox tethers to cuenv itself.
            #[cfg(target_os = "linux")]
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if close_extra_fds {
                mark_non_stdio_fds_close_on_exec()?;
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
fn mark_non_stdio_fds_close_on_exec() -> std::io::Result<()> {
    // SAFETY: Each call handles scalar descriptor numbers or initializes a live stack limit.
    unsafe {
        #[cfg(target_os = "linux")]
        {
            // Preserve the exec-status pipe so Rust can still report a failed
            // exec to its parent. The pipe and inherited FDs close on exec.
            const CLOSE_RANGE_CLOEXEC: u32 = 1 << 2;
            if libc::syscall(libc::SYS_close_range, 3_u32, u32::MAX, CLOSE_RANGE_CLOEXEC) == 0 {
                return Ok(());
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
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags != -1 && libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) == -1 {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn options(capture_output: OutputCapture) -> ProcessOptions {
        ProcessOptions {
            capture_output,
            timeout: None,
            isolate_stdio_and_fds: true,
            launcher_stdin: None,
            launcher_exchange: None,
        }
    }

    /// A task that, left alone, writes `survived` after a while, from the
    /// shell and from a background descendant.
    fn slow_writer(directory: &std::path::Path) -> Command {
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "(sleep 0.5; touch descendant-survived) & sleep 0.5; touch survived; wait",
            ])
            .current_dir(directory);
        command
    }

    #[tokio::test]
    async fn an_abandoned_task_stops_with_its_descendants()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        for capture in [OutputCapture::Capture, OutputCapture::Stream] {
            let directory = tempfile::TempDir::new()?;
            let running =
                run_task_process("abandoned", slow_writer(directory.path()), options(capture));
            // Fail-fast abandons a sibling the same way: its future is dropped.
            assert!(
                tokio::time::timeout(Duration::from_millis(150), running)
                    .await
                    .is_err()
            );
            tokio::time::sleep(Duration::from_millis(900)).await;
            assert!(!directory.path().join("survived").exists(), "{capture:?}");
            assert!(
                !directory.path().join("descendant-survived").exists(),
                "{capture:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_finished_task_is_not_signalled_again()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::TempDir::new()?;
        let attempt = run_task_process(
            "finished",
            slow_writer(directory.path()),
            options(OutputCapture::Capture),
        )
        .await?;
        assert!(matches!(attempt, TaskAttempt::Completed(ref result) if result.success));
        assert!(directory.path().join("survived").exists());
        Ok(())
    }
}
