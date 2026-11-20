//! Re-exec boundary for callers that protect their own process memory.

use super::super::SandboxExchange;
use super::{LinuxSandboxPlan, probe_plan};
use cuenv_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::io::{self, Read, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "worker.rs"]
mod worker;

const PROBE: &str = "__cuenv_linux_sandbox_probe";
const EXECUTE: &str = "__cuenv_linux_sandbox_execute";
const MAX_PLAN_BYTES: u32 = 1024 * 1024;
const MAX_TASK_BYTES: usize = 16 * 1024 * 1024;
const READY: u8 = b'R';
static CLI_LAUNCHER: OnceLock<PathBuf> = OnceLock::new();

pub fn dispatch() {
    match std::env::args_os().nth(1).as_deref() {
        Some(mode) if mode == PROBE => {
            let input = io::stdin().lock().take(u64::from(MAX_PLAN_BYTES));
            let result = serde_json::from_reader(input)
                .map_err(|error| error.to_string())
                .and_then(|plan| probe_plan(&plan));
            if let Err(reason) = result {
                cuenv_events::eprintln_redacted(&reason);
                std::process::exit(125);
            }
            std::process::exit(0);
        }
        Some(mode) if mode == EXECUTE => match execute() {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                cuenv_events::eprintln_redacted(&format!("strict sandbox launch failed: {error}"));
                std::process::exit(125);
            }
        },
        _ => {}
    }
}

pub fn register() {
    if let Ok(executable) = std::env::current_exe() {
        let _ = CLI_LAUNCHER.set(executable);
    }
}

pub fn probe_support(plan: &LinuxSandboxPlan) -> std::result::Result<(), String> {
    let Some(executable) = CLI_LAUNCHER.get() else {
        return probe_plan(plan);
    };
    let payload = serde_json::to_vec(plan).map_err(|error| error.to_string())?;
    if payload.len() > MAX_PLAN_BYTES as usize {
        return Err("strict sandbox probe plan is too large".to_string());
    }
    let (mut parent, child) = UnixStream::pair().map_err(|error| error.to_string())?;
    let child: OwnedFd = child.into();
    let mut probe = Command::new(executable);
    probe.arg(PROBE).env_clear().stdin(Stdio::from(child));
    terminate_with_parent(&mut probe);
    let child = probe
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not launch strict sandbox probe: {error}"))?;
    let write = parent.write_all(&payload);
    drop(parent);
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    write.map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "strict sandbox probe failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[derive(Deserialize, Serialize)]
struct LaunchSetup {
    plan: LinuxSandboxPlan,
    task: TaskLayout,
}

#[derive(Deserialize, Serialize)]
struct TaskLayout {
    bytes: usize,
    arguments: usize,
    variables: usize,
}

pub fn finish(
    plan: &LinuxSandboxPlan,
    command: tokio::process::Command,
) -> Result<(
    tokio::process::Command,
    Option<Stdio>,
    Option<SandboxExchange>,
)> {
    let Some(executable) = CLI_LAUNCHER.get() else {
        return Ok((command, None, None));
    };
    let (stdin, transfer) = handoff(plan, command.as_std())
        .map_err(|error| Error::io("prepare strict sandbox launch request", error))?;

    // The helper is execed without task arguments or environment. Only
    // the confined, non-dumpable worker receives the private task data.
    let mut launcher = tokio::process::Command::new(executable);
    launcher
        .arg(EXECUTE)
        .env_clear()
        .current_dir("/")
        .kill_on_drop(true);
    terminate_with_parent(launcher.as_std_mut());
    Ok((launcher, Some(stdin), Some(transfer)))
}

fn handoff(plan: &LinuxSandboxPlan, task: &Command) -> io::Result<(Stdio, SandboxExchange)> {
    let mut payload = Vec::new();
    let mut arguments = 0;
    let mut variables = 0;
    for arg in std::iter::once(task.get_program()).chain(task.get_args()) {
        append_field(&mut payload, arg)?;
        arguments += 1;
    }
    for (key, value) in task.get_envs() {
        let Some(value) = value else { continue };
        let key = key.as_bytes();
        if key.is_empty() || key.contains(&b'=') || key.contains(&0) {
            return Err(io::Error::other("invalid strict sandbox environment name"));
        }
        payload.extend_from_slice(key);
        payload.push(b'=');
        append_field(&mut payload, value)?;
        variables += 1;
    }
    if payload.len() > MAX_TASK_BYTES {
        return Err(io::Error::other("strict sandbox task data is too large"));
    }
    let setup = LaunchSetup {
        plan: plan.clone(),
        task: TaskLayout {
            bytes: payload.len(),
            arguments,
            variables,
        },
    };
    let setup = serde_json::to_vec(&setup)?;
    let size = u32::try_from(setup.len()).map_err(io::Error::other)?;
    if size > MAX_PLAN_BYTES {
        return Err(io::Error::other("strict sandbox plan is too large"));
    }
    let (parent, child) = UnixStream::pair()?;
    parent.set_nonblocking(true)?;
    let child: OwnedFd = child.into();
    let transfer = Box::pin(async move {
        let mut stream = tokio::net::UnixStream::from_std(parent)?;
        stream.write_all(&size.to_le_bytes()).await?;
        stream.write_all(&setup).await?;
        if stream.read_u8().await? != READY {
            return Err(io::Error::other(
                "invalid strict sandbox readiness response",
            ));
        }
        stream.write_all(&payload).await?;
        stream.shutdown().await
    });
    Ok((Stdio::from(child), transfer))
}

fn append_field(payload: &mut Vec<u8>, field: &OsStr) -> io::Result<()> {
    let field = field.as_bytes();
    if field.contains(&0) {
        return Err(io::Error::other(
            "strict sandbox task data contains a NUL byte",
        ));
    }
    payload.extend_from_slice(field);
    payload.push(0);
    Ok(())
}

fn terminate_with_parent(command: &mut Command) {
    let parent = rustix::process::getpid().as_raw_pid();
    #[expect(unsafe_code, reason = "Required for Linux launcher lifetime binding")]
    // SAFETY: This callback uses only async-signal-safe Linux syscalls.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

fn execute() -> io::Result<i32> {
    let setup = {
        let mut stdin = io::stdin().lock();
        let mut size = [0; 4];
        stdin.read_exact(&mut size)?;
        let size = u32::from_le_bytes(size);
        if size > MAX_PLAN_BYTES {
            return Err(io::Error::other("strict sandbox plan is too large"));
        }
        let mut setup = vec![0; size as usize];
        stdin.read_exact(&mut setup)?;
        serde_json::from_slice(&setup)?
    };
    worker::run(&setup)
}
