//! The base revision, checked out into a temporary detached `git worktree`.

use std::path::{Path, PathBuf};
use std::process::Command;

use cuenv_events::emit_stderr;
use cuenv_infrastructure::strip_control_characters;

use crate::cli::CliError;

/// The top directory of the git working tree that holds `path`.
///
/// # Errors
///
/// Returns an error when `path` is not inside a git working tree or git
/// cannot run.
pub(super) fn repository_root(path: &Path) -> Result<PathBuf, CliError> {
    let root = git(path, &["rev-parse", "--show-toplevel"]).map_err(|failure| {
        CliError::config_with_help(
            format!(
                "cannot diff infrastructure: {} is not in a git working tree ({failure})",
                path.display()
            ),
            "`cuenv infrastructure diff` compares the working tree with a git revision; run it \
             inside a git repository.",
        )
    })?;
    PathBuf::from(root.trim_end_matches(['\n', '\r']))
        .canonicalize()
        .map_err(|error| CliError::config(format!("cannot resolve the git working tree: {error}")))
}

/// A revision checked out into a temporary directory as a detached git
/// worktree. Dropping it removes the worktree and the directory.
#[derive(Debug)]
pub(super) struct BaseCheckout {
    repository: PathBuf,
    worktree: PathBuf,
    commit: String,
    /// Holds the worktree; deleted once `drop` has removed the worktree.
    directory: tempfile::TempDir,
}

impl BaseCheckout {
    /// Check `revision` of the repository at `repository` out.
    ///
    /// Git hooks do not run and Git LFS does not download anything: the
    /// checkout only needs the CUE files.
    ///
    /// # Errors
    ///
    /// Returns an error when the revision does not name a commit, or git
    /// cannot create the worktree.
    pub(super) fn create(repository: &Path, revision: &str) -> Result<Self, CliError> {
        let commit = git(
            repository,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &format!("{revision}^{{commit}}"),
            ],
        )
        .map(|commit| commit.trim().to_string())
        .map_err(|_| {
            CliError::config_with_help(
                format!(
                    "cannot diff infrastructure: `{}` is not a commit in this repository",
                    strip_control_characters(revision)
                ),
                "Pass --base a branch, tag or commit that exists locally, such as `origin/main`; \
                 fetch it first. In GitHub Actions, check out with `fetch-depth: 0` so the base \
                 branch is there.",
            )
        })?;
        let directory = tempfile::Builder::new()
            .prefix("cuenv-infrastructure-diff-")
            .tempdir()
            .map_err(|error| {
                CliError::config(format!(
                    "cannot create a directory for the base revision: {error}"
                ))
            })?;
        // An empty hooks directory: no hook of the repository runs.
        let hooks = directory.path().join("hooks");
        let worktree = directory.path().join("base");
        std::fs::create_dir(&hooks).map_err(|error| {
            CliError::config(format!(
                "cannot create a directory for the base revision: {error}"
            ))
        })?;
        let hooks_setting = format!("core.hooksPath={}", hooks.display());
        let worktree_text = worktree.to_string_lossy();
        Command::new("git")
            .current_dir(repository)
            .env("GIT_LFS_SKIP_SMUDGE", "1")
            .args([
                "-c",
                &hooks_setting,
                "worktree",
                "add",
                "--detach",
                "--quiet",
            ])
            .args([worktree_text.as_ref(), commit.as_str()])
            .output()
            .map_err(|error| error.to_string())
            .and_then(|output| succeeded(&output))
            .map_err(|failure| {
                CliError::config(format!(
                    "cannot check out `{}` for the diff: {failure}",
                    strip_control_characters(revision)
                ))
            })?;
        Ok(Self {
            repository: repository.to_path_buf(),
            worktree,
            commit,
            directory,
        })
    }

    /// The top directory of the checked-out revision.
    pub(super) fn root(&self) -> &Path {
        &self.worktree
    }

    /// The commit the revision resolved to.
    pub(super) fn commit(&self) -> &str {
        &self.commit
    }
}

impl Drop for BaseCheckout {
    fn drop(&mut self) {
        let worktree = self.worktree.to_string_lossy().into_owned();
        if git(
            &self.repository,
            &["worktree", "remove", "--force", &worktree],
        )
        .is_ok()
        {
            return;
        }
        // Remove the files, then let git forget the worktree whose directory
        // is gone.
        let _ = std::fs::remove_dir_all(&self.worktree);
        if let Err(failure) = git(&self.repository, &["worktree", "prune"]) {
            emit_stderr!(format!(
                "warning: could not remove the temporary worktree of the diff from {}: \
                 {failure}; run `git worktree prune`",
                self.directory.path().display()
            ));
        }
    }
}

/// Run git in `directory` and return its standard output.
fn git(directory: &Path, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    succeeded(&output)?;
    String::from_utf8(output.stdout).map_err(|_| "git printed a path that is not UTF-8".to_string())
}

fn succeeded(output: &std::process::Output) -> Result<(), String> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = stderr
        .lines()
        .map(|line| strip_control_characters(line.trim()))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    Err(if message.is_empty() {
        format!("git exited with {}", output.status)
    } else {
        message
    })
}
