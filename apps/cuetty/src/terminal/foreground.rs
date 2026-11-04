//! What a terminal's foreground process is doing, beyond the name the platform
//! gives it. A shell waiting at its prompt and one running a script share a name, so
//! only the arguments it was started with can tell them apart.

/// The leader of a terminal's foreground process group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ForegroundProcess {
    pub(super) name: String,
    /// The arguments after `argv[0]`. `None` when they were not read, which says
    /// nothing about what the process is doing.
    pub(super) arguments: Option<Vec<String>>,
}

/// What a shell was started to do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ShellRun<'a> {
    /// Read commands from the terminal.
    Interactive,
    /// Run this script, then exit.
    Script(&'a str),
    /// Run commands given to it some other way, such as `-c`, then exit.
    Commands,
}

/// Long options that give a shell its commands, across the shells Cuetty recognises.
const COMMAND_OPTIONS: &[&str] = &["command", "commands", "stdin"];

/// Long options whose value is the next argument, which is not a script. Anything
/// not listed reads as a script, which errs towards not pressing Enter for a shell.
const VALUE_OPTIONS: &[&str] = &[
    "config",
    "env-config",
    "init-command",
    "init-file",
    "plugin-config",
    "rcfile",
];

/// Reads what a shell was started to do from its arguments. It says nothing of what
/// an interactive shell is doing right now: a builtin such as `read` or a subshell
/// runs with the same arguments as the prompt.
pub(super) fn shell_run(arguments: &[String]) -> ShellRun<'_> {
    let mut arguments = arguments.iter().map(String::as_str);
    while let Some(argument) = arguments.next() {
        if matches!(argument, "-" | "--") {
            return arguments
                .next()
                .map_or(ShellRun::Interactive, ShellRun::Script);
        }
        if let Some(option) = argument.strip_prefix("--") {
            let (name, value) = option
                .split_once('=')
                .map_or((option, None), |(name, value)| (name, Some(value)));
            if COMMAND_OPTIONS.contains(&name) {
                return ShellRun::Commands;
            }
            if value.is_none() && VALUE_OPTIONS.contains(&name) {
                arguments.next();
            }
        } else if let Some(flags) = argument.strip_prefix('-') {
            // In bash, zsh, and `sh`, `c` and `s` take commands from elsewhere, and a
            // longer word like pwsh's `-NonInteractive` is no prompt either.
            if flags.contains(['c', 's']) {
                return ShellRun::Commands;
            }
            if takes_shell_option_name(flags) {
                arguments.next();
            }
        } else if let Some(flags) = argument.strip_prefix('+') {
            if takes_shell_option_name(flags) {
                arguments.next();
            }
        } else {
            return ShellRun::Script(argument);
        }
    }
    ShellRun::Interactive
}

/// Whether short flags end in the `-o` or `-O` that names a shell option next, as
/// `-eo pipefail` does. Mixed case marks a whole word, as in pwsh's `-NoLogo`.
fn takes_shell_option_name(flags: &str) -> bool {
    flags == "O" || (flags.ends_with('o') && flags.chars().all(|flag| flag.is_ascii_lowercase()))
}

/// The arguments of the foreground process group's leader on the terminal `main_fd`
/// controls, or of `shell_pid` when no group can be read, as the name's lookup does.
#[cfg(unix)]
pub(super) fn leader_arguments(main_fd: std::os::fd::RawFd, shell_pid: u32) -> Option<Vec<String>> {
    // SAFETY: tcgetpgrp only reads the terminal's foreground group from a file
    // descriptor passed by value, and reports a bad descriptor by returning -1.
    let group = unsafe { libc::tcgetpgrp(main_fd) };
    let leader = u32::try_from(group).unwrap_or(shell_pid);
    process_arguments(leader)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn process_arguments(pid: u32) -> Option<Vec<String>> {
    let path = if cfg!(target_os = "freebsd") {
        format!("/compat/linux/proc/{pid}/cmdline")
    } else {
        format!("/proc/{pid}/cmdline")
    };
    std::fs::read(path)
        .ok()
        .and_then(|bytes| parse_cmdline(&bytes))
}

#[cfg(target_os = "macos")]
fn process_arguments(pid: u32) -> Option<Vec<String>> {
    let mut name = [
        libc::CTL_KERN,
        libc::KERN_PROCARGS2,
        i32::try_from(pid).ok()?,
    ];
    let name_length = libc::c_uint::try_from(name.len()).ok()?;
    let mut size: libc::size_t = 0;
    // SAFETY: `name` holds `name_length` integers, a null buffer asks only for the
    // size, which goes to a valid `size`, and nothing is being written to the sysctl.
    let sized = unsafe {
        libc::sysctl(
            name.as_mut_ptr(),
            name_length,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if sized != 0 {
        return None;
    }
    let mut buffer = vec![0_u8; size];
    // SAFETY: as above, and `buffer` is writable for the `size` bytes passed, which
    // the call lowers to however many it wrote.
    let read = unsafe {
        libc::sysctl(
            name.as_mut_ptr(),
            name_length,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 {
        return None;
    }
    buffer.truncate(size);
    parse_procargs2(&buffer)
}

fn lossy_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The arguments in Linux's `/proc/<pid>/cmdline`: each ends in a NUL. A process
/// that is gone or exiting has none at all, not even `argv[0]`.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn parse_cmdline(bytes: &[u8]) -> Option<Vec<String>> {
    let terminated = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    (!terminated.is_empty()).then(|| {
        terminated
            .split(|byte| *byte == 0)
            .skip(1)
            .map(lossy_string)
            .collect()
    })
}

/// The arguments in macOS's `KERN_PROCARGS2`: `argc` as a native integer, the
/// executable's path, NUL padding, then `argv` with each ending in a NUL.
#[cfg(any(target_os = "macos", test))]
fn parse_procargs2(bytes: &[u8]) -> Option<Vec<String>> {
    let (count, rest) = bytes.split_first_chunk::<4>()?;
    let count = usize::try_from(i32::from_ne_bytes(*count)).ok()?;
    let path_length = rest.iter().position(|byte| *byte == 0)?;
    let after_path = &rest[path_length..];
    let padding = after_path.iter().take_while(|byte| **byte == 0).count();
    let arguments: Vec<String> = after_path[padding..]
        .split(|byte| *byte == 0)
        .take(count)
        .skip(1)
        .map(lossy_string)
        .collect();
    (count > 0 && arguments.len() == count - 1).then_some(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(arguments: &[&str]) -> Vec<String> {
        arguments.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn options_alone_leave_a_shell_at_its_prompt() {
        for arguments in [
            &[][..],
            &["-l"],
            &["-il"],
            &["--login"],
            &["--norc", "--noprofile"],
            // `nix develop` starts bash this way.
            &["--rcfile", "/tmp/nix-shell.abc"],
            &["--rcfile=/tmp/nix-shell.abc"],
            &[
                "--config",
                "/etc/nu/config.nu",
                "--env-config",
                "/etc/nu/env.nu",
            ],
            &["-o", "vi"],
            &["+o", "history"],
            &["-O", "extglob"],
            &["--"],
        ] {
            assert_eq!(
                shell_run(&owned(arguments)),
                ShellRun::Interactive,
                "{arguments:?}"
            );
        }
    }

    #[test]
    fn a_script_operand_is_found_past_the_options() {
        for arguments in [
            &["deploy.sh"][..],
            &["deploy.sh", "--force"],
            &["-e", "deploy.sh"],
            &["-eo", "pipefail", "deploy.sh"],
            &["--rcfile", "x", "deploy.sh"],
            &["--", "deploy.sh"],
            &["-", "deploy.sh"],
        ] {
            assert_eq!(
                shell_run(&owned(arguments)),
                ShellRun::Script("deploy.sh"),
                "{arguments:?}"
            );
        }
    }

    #[test]
    fn commands_from_elsewhere_are_not_a_prompt() {
        for arguments in [
            &["-c", "make"][..],
            &["-lc", "make"],
            &["-s"],
            &["--command", "make"],
            &["--command=make"],
            // Nushell's spelling.
            &["--commands", "ls"],
            &["-NonInteractive"],
        ] {
            assert_eq!(
                shell_run(&owned(arguments)),
                ShellRun::Commands,
                "{arguments:?}"
            );
        }
    }

    #[test]
    fn an_option_it_does_not_know_errs_towards_a_script() {
        for arguments in [&["--unknown-value", "x"][..], &["-NoLogo", "x"]] {
            assert_eq!(shell_run(&owned(arguments)), ShellRun::Script("x"));
        }
    }

    #[test]
    fn cmdline_drops_argv_zero_and_keeps_empty_arguments() {
        assert_eq!(parse_cmdline(b"-zsh\0"), Some(vec![]));
        assert_eq!(
            parse_cmdline(b"bash\0deploy.sh\0\0x\0"),
            Some(vec![
                "deploy.sh".to_string(),
                String::new(),
                "x".to_string()
            ])
        );
    }

    #[test]
    fn an_exiting_process_has_no_cmdline_to_read() {
        assert_eq!(parse_cmdline(b""), None);
    }

    fn procargs(argc: i32, tail: &[u8]) -> Vec<u8> {
        let mut bytes = argc.to_ne_bytes().to_vec();
        bytes.extend_from_slice(tail);
        bytes
    }

    #[test]
    fn procargs_skips_the_executable_path_and_its_padding() {
        assert_eq!(
            parse_procargs2(&procargs(2, b"/bin/bash\0\0\0bash\0deploy.sh\0HOME=/x\0")),
            Some(vec!["deploy.sh".to_string()])
        );
        assert_eq!(
            parse_procargs2(&procargs(1, b"/bin/zsh\0\0-zsh\0HOME=/x\0")),
            Some(vec![])
        );
    }

    #[test]
    fn malformed_procargs_read_as_unknown() {
        assert_eq!(parse_procargs2(b"ab"), None);
        assert_eq!(parse_procargs2(&procargs(0, b"/bin/zsh\0\0")), None);
        assert_eq!(parse_procargs2(&procargs(-1, b"/bin/zsh\0\0zsh\0")), None);
        assert_eq!(parse_procargs2(&procargs(3, b"/bin/zsh\0\0zsh\0")), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_live_process_reports_the_arguments_it_was_started_with() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        // The arguments appear with the exec, a moment after the fork returns.
        let want = Some(vec!["30".to_string()]);
        let seen = (0..200).find_map(|_| {
            let arguments = process_arguments(child.id());
            if arguments == want {
                return arguments;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            None
        });
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(seen, want);
    }
}
