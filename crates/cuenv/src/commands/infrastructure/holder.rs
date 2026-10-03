//! Description of the run that holds a state lock, shown to whoever finds
//! the project locked.

/// Describe this run: the subcommand (`apply`, `state remove`, ...), user,
/// host, process and, in GitHub Actions, the workflow run.
#[must_use]
pub(super) fn describe(command: &str) -> String {
    let continuous_integration = match (
        std::env::var("GITHUB_SERVER_URL"),
        std::env::var("GITHUB_REPOSITORY"),
        std::env::var("GITHUB_RUN_ID"),
    ) {
        (Ok(server), Ok(repository), Ok(run)) => {
            format!(", {server}/{repository}/actions/runs/{run}")
        }
        _ => String::new(),
    };
    format!(
        "cuenv infrastructure {command} by {} on {}, process {}{continuous_integration}",
        user_name(),
        host_name(),
        std::process::id()
    )
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn environment(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok().as_deref().and_then(non_empty))
}

/// The real user: the account of the real user identifier, then the
/// environment, then the numeric identifier.
fn user_name() -> String {
    #[cfg(unix)]
    {
        let identifier = system::real_user_identifier();
        system::account_name(identifier)
            .or_else(|| environment(&["USER", "LOGNAME"]))
            .unwrap_or_else(|| format!("user {identifier}"))
    }
    #[cfg(not(unix))]
    {
        environment(&["USERNAME", "USER"]).unwrap_or_else(|| "an unknown user".to_string())
    }
}

/// The host: the system host name, then `/etc/hostname`, then the
/// environment.
fn host_name() -> String {
    #[cfg(unix)]
    let system_name = system::host_name();
    #[cfg(not(unix))]
    let system_name: Option<String> = None;
    system_name
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .as_deref()
                .and_then(non_empty)
        })
        .or_else(|| environment(&["HOSTNAME", "COMPUTERNAME"]))
        .unwrap_or_else(|| "an unknown host".to_string())
}

#[cfg(unix)]
mod system {
    use std::ffi::CStr;

    /// The real user identifier of this process.
    #[expect(unsafe_code, reason = "getuid has no safe standard library wrapper")]
    pub(super) fn real_user_identifier() -> libc::uid_t {
        // SAFETY: getuid takes no arguments, cannot fail and has no side
        // effects; POSIX specifies it as always successful.
        unsafe { libc::getuid() }
    }

    /// The account name of a user identifier, from the password database.
    #[expect(
        unsafe_code,
        reason = "getpwuid_r has no safe standard library wrapper"
    )]
    pub(super) fn account_name(identifier: libc::uid_t) -> Option<String> {
        let mut buffer = vec![0; 16 * 1024];
        // SAFETY: passwd is a plain C struct of integers and pointers, for
        // which all-zero bytes (null pointers, zero integers) are valid.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer refers to live, writable memory owned by
        // this frame; the buffer length passed is the buffer's real length;
        // getpwuid_r writes only within them and is thread-safe.
        let status = unsafe {
            libc::getpwuid_r(
                identifier,
                &raw mut entry,
                buffer.as_mut_ptr(),
                buffer.len(),
                &raw mut result,
            )
        };
        if status != 0 || result.is_null() || entry.pw_name.is_null() {
            return None;
        }
        // SAFETY: on success pw_name points to a NUL-terminated string
        // inside `buffer`, which outlives this borrow.
        let name = unsafe { CStr::from_ptr(entry.pw_name) };
        name.to_str()
            .ok()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToString::to_string)
    }

    /// The system host name.
    #[expect(
        unsafe_code,
        reason = "gethostname has no safe standard library wrapper"
    )]
    pub(super) fn host_name() -> Option<String> {
        let mut buffer = [0_u8; 256];
        // SAFETY: the pointer and length describe `buffer`, which is live and
        // writable; gethostname writes at most that many bytes.
        let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
        if status != 0 {
            return None;
        }
        let end = buffer
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(buffer.len());
        std::str::from_utf8(&buffer[..end])
            .ok()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToString::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_command_user_host_and_process() {
        let description = describe("destroy");
        assert!(description.starts_with("cuenv infrastructure destroy by "));
        assert!(!description.contains("unknown user"));
        assert!(!description.contains("unknown host"));
        assert!(description.contains(&format!("process {}", std::process::id())));
    }

    #[cfg(unix)]
    #[test]
    fn reads_the_real_host_name() {
        assert!(system::host_name().is_some());
    }
}
