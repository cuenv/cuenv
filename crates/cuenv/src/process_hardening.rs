//! Protection of cuenv's own memory and environment from other processes of
//! the same user.
//!
//! cuenv holds resolved secrets in its memory and in its environment block,
//! and it starts third-party programs (Terraform providers, task commands)
//! that run as the same user. On Linux a process may read the environment
//! (`/proc/<pid>/environ`), the memory (`/proc/<pid>/mem`) and the open files
//! of any other process of the same user that is *dumpable*, so a child could
//! read its parent's variables however carefully the parent built the child's
//! own environment. Clearing the dumpable flag makes the kernel refuse those
//! accesses to everyone but the superuser; `execve` sets the flag again, so
//! the programs cuenv starts are unaffected and stay inspectable as usual.
//!
//! The cost: a same-user debugger cannot attach to cuenv, and cuenv writes no
//! core dump. Reading what `ps` shows (`/proc/<pid>/cmdline`, `status`,
//! `stat`) is still allowed, which is all cuenv's own process discovery uses.
//!
//! This is hygiene against a program that merely looks around, not a sandbox:
//! a hostile program running as the same user can still use any credential
//! files in the home directory.

/// Why the process could not be hardened.
#[derive(Debug, thiserror::Error)]
#[error("could not make this process non-dumpable: {0}")]
pub struct HardeningError(#[from] std::io::Error);

/// Stop other processes of the same user from reading this process's
/// environment and memory. Call it first thing at startup.
///
/// A no-op on platforms other than Linux, which have no equivalent switch
/// (macOS protects other processes' memory behind entitlements, and offers no
/// way to hide the environment block of a same-user process).
///
/// # Errors
///
/// Returns the operating system's error if the kernel refuses; the process
/// then keeps running unprotected.
#[cfg(target_os = "linux")]
pub fn restrict_process_inspection() -> Result<(), HardeningError> {
    rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable)
        .map_err(|error| HardeningError(error.into()))
}

/// See the Linux version.
///
/// # Errors
///
/// Never.
#[cfg(not(target_os = "linux"))]
pub fn restrict_process_inspection() -> Result<(), HardeningError> {
    Ok(())
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    /// Set in the probe process's environment: which probe to run.
    const PROBE_VARIABLE: &str = "CUENV_DUMPABLE_PROBE";
    /// Exit status of the probe when the grandchild could not read the
    /// environment of the probe.
    const DENIED: i32 = 17;
    /// Exit status of the probe when the grandchild could read it.
    const READABLE: i32 = 18;
    /// Exit status of the probe when the grandchild could not even read
    /// the probe's command line.
    const COMMAND_LINE_DENIED: i32 = 19;

    /// What a probe process does before it reports.
    #[derive(Debug, Clone, Copy)]
    enum ProbeMode {
        /// Harden the process first.
        Hardened,
        /// Leave the process as it is (the control).
        Control,
    }

    /// The probe: runs in a process of its own (see `probe`), optionally
    /// hardens itself, and reports whether a child can read its environment.
    fn run_probe(mode: ProbeMode) -> ! {
        if matches!(mode, ProbeMode::Hardened) {
            restrict_process_inspection().expect("hardening");
        }
        let read = |file: &str| {
            Command::new("sh")
                .arg("-c")
                .arg(format!("cat /proc/$PPID/{file} > /dev/null"))
                .status()
                .expect("sh")
                .success()
        };
        let code = if !read("cmdline") {
            COMMAND_LINE_DENIED
        } else if read("environ") {
            READABLE
        } else {
            DENIED
        };
        std::process::exit(code);
    }

    /// Entry point of the probe processes; a no-op in the normal test run.
    #[test]
    fn probe() {
        match std::env::var(PROBE_VARIABLE).as_deref() {
            Ok("hardened") => run_probe(ProbeMode::Hardened),
            Ok("control") => run_probe(ProbeMode::Control),
            _ => {}
        }
    }

    /// Run the probe as a separate process, as an unprivileged user when the
    /// test itself runs as the superuser (the superuser can always read
    /// everything, which would hide the effect).
    fn probe_status(mode: &str) -> i32 {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "process_hardening::tests::probe",
                "--test-threads=1",
            ])
            .env(PROBE_VARIABLE, mode);
        if rustix::process::geteuid().is_root() {
            command.uid(65534).gid(65534);
        }
        command
            .status()
            .expect("probe process")
            .code()
            .expect("probe exit code")
    }

    #[test]
    fn a_hardened_process_environment_is_not_readable_by_its_children() {
        assert_eq!(
            probe_status("control"),
            READABLE,
            "without hardening a child reads its parent's environment (the control)"
        );
        assert_eq!(probe_status("hardened"), DENIED);
    }
}
