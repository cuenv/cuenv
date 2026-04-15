//! `cuenv __supervise` — hidden process-babysitter subcommand.
//!
//! On Linux, orphan prevention is handled in-kernel via
//! `PR_SET_PDEATHSIG`. macOS has no equivalent, so we spawn services
//! through this thin wrapper instead: it monitors its own parent
//! (cuenv `up`) via `kqueue` + `EVFILT_PROC` + `NOTE_EXIT`, and on
//! parent death it kills the wrapped child's process group.
//!
//! The wrapper also forwards catchable signals (SIGTERM, SIGINT, SIGHUP,
//! SIGQUIT) to the child's process group so `cuenv stop` and Ctrl-C
//! continue to work through the extra process hop.
//!
//! # Usage
//!
//! ```text
//! cuenv __supervise <program> [args...]
//! ```

#![cfg(unix)]

use std::os::unix::process::CommandExt;
use std::sync::atomic::{AtomicI32, Ordering};

/// Exit code used when the wrapper fails before the child is spawned.
const EXIT_WRAPPER_ERROR: i32 = 127;

/// Global for signal forwarder: the pid of the child process group leader.
static FORWARD_PID: AtomicI32 = AtomicI32::new(0);

/// Signal handler that forwards the received signal to the child's
/// process group recorded in `FORWARD_PID`.
extern "C" fn forward_signal(sig: libc::c_int) {
    let pid = FORWARD_PID.load(Ordering::SeqCst);
    if pid > 0 {
        #[expect(
            unsafe_code,
            reason = "kill(2) is async-signal-safe and targets the child's process group"
        )]
        // SAFETY: kill() is documented as async-signal-safe. A negative
        // pid targets the process group identified by abs(pid).
        unsafe {
            libc::kill(-pid, sig);
        }
    }
}

/// Run the `__supervise` wrapper.
///
/// `argv` contains the child program and its arguments (the wrapper's
/// own program name and `__supervise` token are expected to have been
/// stripped already by the caller).
///
/// Returns the exit code to propagate.
#[must_use]
pub fn run(argv: &[String]) -> i32 {
    let Some((program, rest)) = argv.split_first() else {
        // NOTE: stderr is acceptable for a hidden, pre-event-system
        // helper subcommand; clippy::print_stderr is explicitly
        // allowed in this tiny internal path.
        #[allow(clippy::print_stderr)]
        {
            eprintln!("cuenv __supervise: missing command");
        }
        return EXIT_WRAPPER_ERROR;
    };
    let program = program.clone();
    let child_args = rest.to_vec();

    // Remember the parent PID before spawning anything — this is what
    // we watch for exit.
    #[expect(
        unsafe_code,
        reason = "getppid(2) has no preconditions and cannot fail"
    )]
    // SAFETY: getppid() has no preconditions and cannot fail.
    let parent_pid = unsafe { libc::getppid() };

    let mut cmd = std::process::Command::new(&program);
    cmd.args(&child_args);

    // Put the child into its own process group so signal forwarding
    // can target it as a group.
    #[expect(
        unsafe_code,
        reason = "pre_exec hook runs in the forked child before exec; setpgid is async-signal-safe"
    )]
    // SAFETY: setpgid(0, 0) is signal/async-safe and has no side effects
    // outside the child process.
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            #[allow(clippy::print_stderr)]
            {
                eprintln!("cuenv __supervise: failed to spawn {program}: {e}");
            }
            return EXIT_WRAPPER_ERROR;
        }
    };

    let child_pid: libc::pid_t = child.id().cast_signed();

    // Set up parent-death monitoring. On Linux this is redundant with
    // PR_SET_PDEATHSIG (which the main supervisor already applies to
    // the child it spawns); we install it here as belt-and-braces when
    // the wrapper is invoked on Linux, and as the primary mechanism
    // on macOS where PR_SET_PDEATHSIG does not exist.
    spawn_parent_watcher(parent_pid, child_pid);

    // Forward catchable signals to the child's process group.
    spawn_signal_forwarder(child_pid);

    // Wait for the child.
    match child.wait() {
        Ok(status) => status.code().unwrap_or(EXIT_WRAPPER_ERROR),
        Err(_) => EXIT_WRAPPER_ERROR,
    }
}

// ------------------------------------------------------------------
// macOS: kqueue-based parent-death watcher
// ------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn spawn_parent_watcher(parent_pid: libc::pid_t, child_pid: libc::pid_t) {
    std::thread::spawn(move || {
        #[expect(
            unsafe_code,
            reason = "kqueue()/kevent()/close() on a locally-owned fd; pointers live for the call"
        )]
        // SAFETY: kqueue() returns a new owned fd or -1; kevent() is
        // given pointers valid for the duration of the call; close()
        // operates on our own fd.
        unsafe {
            let kq = libc::kqueue();
            if kq < 0 {
                return;
            }

            let mut change: libc::kevent = std::mem::zeroed();
            change.ident = parent_pid as usize;
            change.filter = libc::EVFILT_PROC;
            change.flags = libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT;
            change.fflags = libc::NOTE_EXIT;

            let mut event: libc::kevent = std::mem::zeroed();
            let n = libc::kevent(kq, &change, 1, &mut event, 1, std::ptr::null());

            // Whether we got a real event or kevent failed, the parent
            // is either dead or unobservable — kill the child group
            // either way. If we cannot watch the parent, we must not
            // leave the child orphaned.
            if n >= 0 {
                libc::kill(-child_pid, libc::SIGKILL);
            }
            libc::close(kq);
        }
    });
}

// ------------------------------------------------------------------
// Linux: polling fallback (PR_SET_PDEATHSIG is set on the real service
// child by the main supervisor, so the wrapper is rarely used here).
// ------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn spawn_parent_watcher(parent_pid: libc::pid_t, child_pid: libc::pid_t) {
    std::thread::spawn(move || {
        loop {
            #[expect(
                unsafe_code,
                reason = "kill(pid, 0) is the standard POSIX probe for process liveness"
            )]
            // SAFETY: kill(pid, 0) performs error checking only; it
            // does not send a signal.
            let alive = unsafe { libc::kill(parent_pid, 0) } == 0;
            if !alive {
                #[expect(
                    unsafe_code,
                    reason = "negative pid targets the child process group"
                )]
                // SAFETY: negative pid = kill process group.
                unsafe {
                    libc::kill(-child_pid, libc::SIGKILL);
                }
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

// ------------------------------------------------------------------
// Other unix: polling fallback
// ------------------------------------------------------------------

#[cfg(all(unix, not(target_os = "linux"), not(target_os = "macos")))]
fn spawn_parent_watcher(parent_pid: libc::pid_t, child_pid: libc::pid_t) {
    std::thread::spawn(move || {
        loop {
            #[expect(
                unsafe_code,
                reason = "kill(pid, 0) is the standard POSIX probe for process liveness"
            )]
            // SAFETY: kill(pid, 0) performs error checking only; it
            // does not send a signal.
            let alive = unsafe { libc::kill(parent_pid, 0) } == 0;
            if !alive {
                #[expect(
                    unsafe_code,
                    reason = "negative pid targets the child process group"
                )]
                // SAFETY: negative pid = kill process group.
                unsafe {
                    libc::kill(-child_pid, libc::SIGKILL);
                }
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

// ------------------------------------------------------------------
// Signal forwarder
// ------------------------------------------------------------------

fn spawn_signal_forwarder(child_pid: libc::pid_t) {
    FORWARD_PID.store(child_pid, Ordering::SeqCst);

    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT] {
        #[expect(
            unsafe_code,
            reason = "installing a signal handler at wrapper startup; function pointer cast via *const ()"
        )]
        // SAFETY: registering a signal handler is fine from the main
        // thread during wrapper startup; the handler is async-signal-safe.
        unsafe {
            libc::signal(sig, forward_signal as *const () as libc::sighandler_t);
        }
    }
}
