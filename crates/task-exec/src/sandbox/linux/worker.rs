//! Receive private task data and exec only after namespace setup.

use super::super::{enter_linux_sandbox, supervisor};
use super::{LaunchSetup, MAX_TASK_BYTES, READY, TaskLayout};
use std::io;
use std::ptr::null_mut;

const SETUP_FAILURE: libc::c_int = 125;
type TaskPointer = *mut libc::c_char;

// The Rust Unix process backend likewise installs an explicit environment
// and then uses execvp after its fork/exec setup callbacks.
#[expect(
    unsafe_code,
    reason = "Required to exec with the task's own PATH and environment"
)]
unsafe extern "C" {
    static mut environ: *mut TaskPointer;
}

struct TaskPayload {
    data: Vec<u8>,
    argv: Vec<TaskPointer>,
    envp: Vec<TaskPointer>,
}

impl TaskPayload {
    fn prepare(layout: &TaskLayout) -> io::Result<Self> {
        let fields = layout.arguments.saturating_add(layout.variables);
        if layout.arguments == 0 || fields > layout.bytes || layout.bytes > MAX_TASK_BYTES {
            return Err(io::Error::other("invalid strict sandbox task layout"));
        }
        Ok(Self {
            data: vec![0; layout.bytes],
            argv: vec![null_mut(); layout.arguments + 1],
            envp: vec![null_mut(); layout.variables + 1],
        })
    }

    #[expect(
        unsafe_code,
        reason = "Required to exchange handoff data and exec in the confined child"
    )]
    fn receive_and_exec(&mut self) -> io::Result<()> {
        // SAFETY: fd 0 is the worker side of the socket. This function runs
        // after the host launcher closed its copy and the worker is confined.
        unsafe { send_ready(libc::STDIN_FILENO)? };
        read_exact(libc::STDIN_FILENO, &mut self.data)?;
        let data = parse_fields(self.data.as_mut_slice(), &mut self.argv)?;
        let data = parse_fields(data, &mut self.envp)?;
        if !data.is_empty() {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        // SAFETY: Both pointer arrays are NUL-terminated and refer to the
        // still-live task buffer. The caller is the single-threaded worker.
        unsafe {
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
            if null < 0 {
                return Err(io::Error::last_os_error());
            }
            let installed = libc::dup2(null, libc::STDIN_FILENO);
            let error = io::Error::last_os_error();
            libc::close(null);
            if installed < 0 {
                return Err(error);
            }

            let previous = environ;
            environ = self.envp.as_mut_ptr();
            libc::execvp(self.argv[0], self.argv.as_ptr().cast());
            let error = io::Error::last_os_error();
            environ = previous;
            Err(error)
        }
    }
}

fn parse_fields<'data>(
    mut data: &'data mut [u8],
    pointers: &mut [TaskPointer],
) -> io::Result<&'data mut [u8]> {
    let fields = pointers.len() - 1; // The final pointer stays null.
    for pointer in &mut pointers[..fields] {
        let end = data
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        let (field, remaining) = data.split_at_mut(end + 1);
        *pointer = field.as_mut_ptr().cast();
        data = remaining;
    }
    Ok(data)
}

#[expect(
    unsafe_code,
    reason = "Required for a child that enters Linux namespaces before exec"
)]
pub(super) fn run(setup: &LaunchSetup) -> io::Result<i32> {
    let mut payload = TaskPayload::prepare(&setup.task)?;
    let parent = rustix::process::getpid().as_raw_pid();
    let mut gate = [0; 2];
    // SAFETY: The pipe descriptors belong to this invocation. The child only
    // enters the sandbox, transfers bytes with syscalls and execs or exits.
    let child = unsafe {
        if libc::pipe2(gate.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
            return Err(io::Error::last_os_error());
        }
        let child = supervisor::fork_child();
        if child < 0 {
            let error = io::Error::last_os_error();
            libc::close(gate[0]);
            libc::close(gate[1]);
            return Err(error);
        }
        if child == 0 {
            libc::close(gate[1]);
            let result = enter_linux_sandbox(&setup.plan, parent)
                .and_then(|()| wait_for_helper(gate[0]))
                .and_then(|()| payload.receive_and_exec());
            let message = match result {
                Ok(()) => b"strict sandbox worker returned without executing\n".as_slice(),
                Err(_) => b"strict sandbox worker could not start the task\n".as_slice(),
            };
            libc::write(libc::STDERR_FILENO, message.as_ptr().cast(), message.len());
            libc::_exit(SETUP_FAILURE);
        }
        libc::close(gate[0]);
        libc::close(libc::STDIN_FILENO);
        child
    };

    // Release the confined worker only after the host helper has detached
    // its copy of the socket and has become non-dumpable.
    let release =
        rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable)
            .map_err(io::Error::from)
            .and_then(|()| write_byte(gate[1]));
    // SAFETY: The helper owns the write end. Closing also wakes the worker
    // with EOF if the helper failed to release it.
    unsafe { libc::close(gate[1]) };
    let status = wait_for_child(child)?;
    release?;
    Ok(status)
}

#[expect(unsafe_code, reason = "Required for the confined pre-exec handoff")]
fn wait_for_helper(gate: libc::c_int) -> io::Result<()> {
    let result = read_exact(gate, &mut [0; 1]);
    // SAFETY: The worker owns the read end of the startup gate.
    unsafe { libc::close(gate) };
    result
}

#[expect(
    unsafe_code,
    reason = "Required for async-signal-safe input after fork"
)]
fn read_exact(fd: libc::c_int, mut bytes: &mut [u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        // SAFETY: The provided slice is writable for the syscall length.
        let result = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        match result {
            0 => return Err(io::Error::from_raw_os_error(libc::EPIPE)),
            -1 => {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EINTR) {
                    return Err(error);
                }
            }
            count => {
                let count =
                    usize::try_from(count).map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
                bytes = &mut bytes[count..];
            }
        }
    }
    Ok(())
}

#[expect(unsafe_code, reason = "Required to signal from the confined worker")]
unsafe fn send_ready(fd: libc::c_int) -> io::Result<()> {
    let ready = [READY];
    loop {
        // SAFETY: The socket is connected and the byte lives until send returns.
        if unsafe { libc::send(fd, ready.as_ptr().cast(), 1, libc::MSG_NOSIGNAL) } == 1 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR) {
            return Err(error);
        }
    }
}

#[expect(
    unsafe_code,
    reason = "Required to release the worker over the setup pipe"
)]
fn write_byte(fd: libc::c_int) -> io::Result<()> {
    let release = [READY];
    loop {
        // SAFETY: The pipe is open and the byte lives until write returns.
        if unsafe { libc::write(fd, release.as_ptr().cast(), 1) } == 1 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR) {
            return Err(error);
        }
    }
}

fn wait_for_child(pid: libc::pid_t) -> io::Result<i32> {
    use rustix::process::{Pid, WaitOptions, waitpid};

    let child = Pid::from_raw(pid).ok_or_else(|| io::Error::from_raw_os_error(libc::ECHILD))?;
    loop {
        match waitpid(Some(child), WaitOptions::empty()) {
            Ok(Some((_, status))) => {
                return Ok(status
                    .exit_status()
                    .or_else(|| status.terminating_signal().map(|signal| 128 + signal))
                    .unwrap_or(SETUP_FAILURE));
            }
            Ok(None) | Err(rustix::io::Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
    }
}
