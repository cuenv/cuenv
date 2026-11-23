//! Child-side process supervision for the strict Linux sandbox.

use std::io;

const SETUP_FAILURE: libc::c_int = 125;
const CLOSE_RANGE_CLOEXEC: u32 = 1 << 2;

// Linux uapi capability.h and securebits.h.
const CAPABILITY_VERSION_3: u32 = 0x2008_0522;
const SECBIT_NOROOT_LOCKED: libc::c_ulong = 0b11;

#[repr(C)]
struct CapabilityHeader {
    version: u32,
    pid: libc::pid_t,
}

#[repr(C)]
struct CapabilityData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

/// Revoke namespace setup privileges before giving control to the task.
/// UID 0 is retained so the mapped UID can still write to the exec root.
///
/// SAFETY: Called after entering the user namespace, before running any
/// untrusted code. Performs only syscalls with stack-allocated arguments.
#[expect(
    unsafe_code,
    reason = "Required to drop Linux namespace setup privileges"
)]
pub(super) unsafe fn drop_setup_privileges() -> io::Result<()> {
    let header = CapabilityHeader {
        version: CAPABILITY_VERSION_3,
        pid: 0,
    };
    let capabilities = [const {
        CapabilityData {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        }
    }; 2];
    // SAFETY: capset reads the version 3 header and two capability words.
    unsafe {
        if libc::prctl(libc::PR_SET_SECUREBITS, SECBIT_NOROOT_LOCKED, 0, 0, 0) != 0
            || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            || libc::syscall(libc::SYS_capset, &raw const header, capabilities.as_ptr()) != 0
            || libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Establish a trusted PID 1 before returning to the task's exec path.
/// The launcher only mirrors the exit status once Linux has shut down the
/// PID namespace. This also contains descendants that change process group.
///
/// SAFETY: Called in the single-threaded launcher after unsharing the PID
/// namespace for its children. Only the worker returns to Rust's exec path.
#[expect(
    unsafe_code,
    reason = "Required to launch and supervise a Linux PID namespace"
)]
pub(super) unsafe fn run() -> io::Result<()> {
    // SAFETY: The launcher PID is live, and the fd remains stack-managed.
    unsafe {
        if libc::syscall(libc::SYS_close_range, 3_u32, u32::MAX, CLOSE_RANGE_CLOEXEC) != 0 {
            return Err(io::Error::last_os_error());
        }
        let launcher_fd = libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0);
        if launcher_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let launcher_fd = libc::c_int::try_from(launcher_fd)
            .map_err(|_| io::Error::from_raw_os_error(libc::EBADF))?;

        let init = fork_child();
        if init < 0 {
            let error = io::Error::last_os_error();
            libc::close(launcher_fd);
            return Err(error);
        }
        if init > 0 {
            supervise(init);
        }

        // PID 1 sees its parent outside the namespace as 0. Poll the launcher's
        // pidfd after installing the death signal to close the fork/prctl race.
        let mut parent = libc::pollfd {
            fd: launcher_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
            || libc::poll(&raw mut parent, 1, 0) != 0
        {
            libc::_exit(SETUP_FAILURE);
        }
        libc::close(launcher_fd);

        let worker = fork_child();
        if worker < 0 {
            return Err(io::Error::last_os_error());
        }
        if worker > 0 {
            supervise(worker);
        }
    }
    Ok(())
}

#[expect(
    unsafe_code,
    reason = "Required to fork without running registered atfork handlers"
)]
pub(super) unsafe fn fork_child() -> libc::pid_t {
    // SAFETY: Raw clone with only SIGCHLD preserves fork's private address
    // space and stack, with no atfork callbacks in the already-forked child.
    unsafe {
        #[cfg(not(target_arch = "s390x"))]
        let result = libc::syscall(libc::SYS_clone, libc::SIGCHLD, 0, 0, 0, 0);
        #[cfg(target_arch = "s390x")]
        let result = libc::syscall(libc::SYS_clone, 0, libc::SIGCHLD, 0, 0, 0);
        libc::pid_t::try_from(result).unwrap_or(-1)
    }
}

#[expect(
    unsafe_code,
    reason = "Required to wait for child exit without returning to exec"
)]
unsafe fn supervise(child: libc::pid_t) -> ! {
    // SAFETY: Both supervisors have just forked their specific child.
    unsafe {
        // Only the worker may hold the exec status pipe and task stdio.
        if libc::syscall(libc::SYS_close_range, 0_u32, u32::MAX, 0_u32) != 0 {
            libc::_exit(SETUP_FAILURE);
        }
        let mut status = 0;
        loop {
            if libc::waitpid(child, &raw mut status, 0) == child {
                if libc::WIFEXITED(status) {
                    libc::_exit(libc::WEXITSTATUS(status));
                }
                if libc::WIFSIGNALED(status) {
                    libc::_exit(128 + libc::WTERMSIG(status));
                }
            } else if io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                libc::_exit(SETUP_FAILURE);
            }
        }
    }
}
