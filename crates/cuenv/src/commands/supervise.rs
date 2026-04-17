//! `cuenv __supervise` — hidden process-babysitter subcommand.
//!
//! On Linux, orphan prevention is handled in-kernel via
//! `PR_SET_PDEATHSIG`. macOS has no equivalent, so we spawn services
//! through this thin wrapper instead: it monitors its own parent
//! (cuenv `up`) via `kqueue` + `EVFILT_PROC` + `NOTE_EXIT`, and on
//! parent death it kills its own process group (which the service
//! inherits), cleanly terminating the whole subtree.
//!
//! The wrapper also forwards catchable signals (SIGTERM, SIGINT, SIGHUP,
//! SIGQUIT) to the child pid so `cuenv stop` and Ctrl-C continue to
//! work through the extra process hop.
//!
//! # Process-group layout
//!
//! The outer supervisor (`cuenv_services::supervisor::spawn_process`)
//! calls `setpgid(0, 0)` on the wrapper, so the wrapper is a process
//! group leader with `pgid == wrapper_pid`. The wrapper does NOT set
//! a new pgid on the service child — the service inherits the
//! wrapper's pgid. This keeps both processes in a single group so that
//! `kill(-wrapper_pid, SIGTERM)` from `cuenv stop` naturally reaches
//! both the wrapper and the service.
//!
//! # Usage
//!
//! ```text
//! cuenv __supervise <program> [args...]
//! ```

use std::sync::atomic::{AtomicI32, Ordering};

/// Exit code used when the wrapper fails before the child is spawned.
const EXIT_WRAPPER_ERROR: i32 = 127;

/// Global for signal forwarder: the pid of the supervised child.
static FORWARD_PID: AtomicI32 = AtomicI32::new(0);

/// Signal handler that forwards the received signal to the child pid
/// recorded in `FORWARD_PID`. Uses a positive pid so we target only
/// the service process (redundant when the signal arrived via a pgid
/// broadcast, but correct when the wrapper is signalled directly).
extern "C" fn forward_signal(sig: libc::c_int) {
    let pid = FORWARD_PID.load(Ordering::SeqCst);
    if pid > 0 {
        #[expect(
            unsafe_code,
            reason = "kill(2) is async-signal-safe; positive pid targets a single process"
        )]
        // SAFETY: kill() is documented as async-signal-safe. A positive
        // pid targets exactly that process.
        unsafe {
            libc::kill(pid, sig);
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
    // Initialize a minimal tracing subscriber so diagnostic output from
    // this hidden helper goes through the tracing pipeline rather than
    // directly to stderr. Failing to init is not fatal — another
    // subscriber may already be set.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .try_init();

    let Some((program, rest)) = argv.split_first() else {
        tracing::error!("cuenv __supervise: missing command");
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

    // Deliberately do NOT call setpgid(0, 0) here: the service must
    // inherit the wrapper's pgid so that `kill(-wrapper_pid, sig)` from
    // the outer supervisor reaches both processes in a single group
    // broadcast. See module-level doc for the pgid layout.

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("cuenv __supervise: failed to spawn {program}: {e}");
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

    // Forward catchable signals to the child.
    spawn_signal_forwarder(child_pid);

    // Wait for the child.
    match child.wait() {
        Ok(status) => status.code().unwrap_or(EXIT_WRAPPER_ERROR),
        Err(_) => EXIT_WRAPPER_ERROR,
    }
}

/// Kill the wrapper's process group, which includes the wrapper itself,
/// the supervised child, and any descendants of the child. Called when
/// the outer parent dies and we must cleanly tear down the whole
/// subtree.
fn kill_group() {
    #[expect(
        unsafe_code,
        reason = "getpid/kill on our own pgid are safe; SIGKILL terminates the whole group"
    )]
    // SAFETY: getpid() cannot fail. The wrapper is its own pgid leader
    // (ensured by the outer supervisor's setpgid(0, 0) pre_exec), so
    // -getpid() == -pgid and targets exactly our group.
    unsafe {
        let pgid = libc::getpid();
        libc::kill(-pgid, libc::SIGKILL);
    }
}

// ------------------------------------------------------------------
// macOS: kqueue-based parent-death watcher
// ------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn spawn_parent_watcher(parent_pid: libc::pid_t, _child_pid: libc::pid_t) {
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
                tracing::warn!(
                    "cuenv __supervise: kqueue() failed (errno {}); killing group",
                    std::io::Error::last_os_error()
                );
                kill_group();
                return;
            }

            let mut change: libc::kevent = std::mem::zeroed();
            change.ident = usize::try_from(parent_pid).expect("pid fits in usize");
            change.filter = libc::EVFILT_PROC;
            change.flags = libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT;
            change.fflags = libc::NOTE_EXIT;

            let mut event: libc::kevent = std::mem::zeroed();
            let n = libc::kevent(
                kq,
                &raw const change,
                1,
                &raw mut event,
                1,
                std::ptr::null(),
            );
            if n < 0 {
                tracing::warn!(
                    "cuenv __supervise: kevent() failed (errno {}); killing group",
                    std::io::Error::last_os_error()
                );
            }

            // Whether we observed the parent's NOTE_EXIT or kevent
            // failed, the parent is either dead or unobservable — kill
            // the whole group either way. If we cannot watch the
            // parent, we must not leave the child orphaned.
            libc::close(kq);
            kill_group();
        }
    });
}

// ------------------------------------------------------------------
// Linux: polling fallback (PR_SET_PDEATHSIG is set on the real service
// child by the main supervisor, so the wrapper is rarely used here).
// ------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn spawn_parent_watcher(parent_pid: libc::pid_t, _child_pid: libc::pid_t) {
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
                kill_group();
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
fn spawn_parent_watcher(parent_pid: libc::pid_t, _child_pid: libc::pid_t) {
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
                kill_group();
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
