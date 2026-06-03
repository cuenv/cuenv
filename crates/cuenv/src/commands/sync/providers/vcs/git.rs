//! Git subprocess helpers for VCS dependency sync.

use cuenv_core::{Error, Result};
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command as ProcessCommand;

pub(super) fn run_git<I, S>(args: I, cwd: Option<&Path>) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git_command(args, cwd)
        .output()
        .map_err(|e| Error::configuration(e.to_string()))?;
    if output.status.success() {
        return Ok(());
    }
    Err(Error::configuration(format!(
        "git command failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

pub(super) fn git_output<I, S>(args: I, cwd: Option<&Path>) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git_command(args, cwd)
        .output()
        .map_err(|e| Error::configuration(e.to_string()))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    Err(Error::configuration(format!(
        "git command failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

/// Run a git command and return raw stdout bytes without trimming.
///
/// Used for `git ls-tree -z`, whose records are NUL-delimited and may contain
/// names with spaces or newlines that trimming would corrupt.
pub(super) fn git_output_bytes<I, S>(args: I, cwd: Option<&Path>) -> Result<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git_command(args, cwd)
        .output()
        .map_err(|e| Error::configuration(e.to_string()))?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    Err(Error::configuration(format!(
        "git command failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

fn git_command<I, S>(args: I, cwd: Option<&Path>) -> ProcessCommand
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = ProcessCommand::new("git");
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command.args(args);
    command
}
