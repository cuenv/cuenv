//! The base revision, checked out into a temporary detached `git worktree`.

use std::path::{Path, PathBuf};
use std::process::Command;

use cuenv_events::emit_stderr;
use cuenv_infrastructure::strip_control_characters;

use crate::cli::CliError;

/// A revision checked out into a temporary directory as a detached git
/// worktree. Dropping it removes the worktree and the directory.
pub(super) struct BaseCheckout {
    repository: PathBuf,
    /// The revision and the commit it names, such as `origin/main (0123456)`.
    label: String,
    /// Holds the worktree, in `base`.
    directory: tempfile::TempDir,
}

impl BaseCheckout {
    /// Check `revision` of the git repository holding `path` out. Fails when
    /// `path` is not in a git working tree or the revision names no commit.
    pub(super) fn create(path: &Path, revision: &str) -> Result<Self, CliError> {
        let shown = strip_control_characters(revision);
        let root = run(git(path).args(["rev-parse", "--show-toplevel"])).map_err(|failure| {
            CliError::config(format!(
                "cannot diff infrastructure: {} is not in a git working tree ({failure})",
                path.display()
            ))
        })?;
        let repository = Path::new(root.trim_end()).canonicalize().map_err(|error| {
            CliError::config(format!("cannot resolve the git working tree: {error}"))
        })?;
        let verify = ["rev-parse", "--verify", "--quiet", "--end-of-options"];
        let target = format!("{revision}^{{commit}}");
        let commit = run(git(&repository).args(verify).arg(target)).map_err(|_| {
            CliError::config_with_help(
                format!("cannot diff infrastructure: `{shown}` is not a commit in this repository"),
                "Pass --base a branch, tag or commit that exists locally, such as `origin/main`; \
                 fetch it first. In GitHub Actions, check out with `fetch-depth: 0` so the base \
                 branch is there.",
            )
        })?;
        let directory = tempfile::Builder::new()
            .prefix("cuenv-infrastructure-diff-")
            .tempdir()
            .map_err(|error| CliError::config(format!("cannot create the base: {error}")))?;
        // No hook of the repository runs, and Git LFS downloads nothing: the
        // diff needs the CUE files alone.
        let add = ["-c", "core.hooksPath=/dev/null", "worktree", "add"];
        let worktree = directory.path().join("base");
        let mut command = git(&repository);
        command
            .args(add)
            .arg("--detach")
            .arg(worktree)
            .arg(commit.trim());
        run(command.env("GIT_LFS_SKIP_SMUDGE", "1")).map_err(|failure| {
            CliError::config(format!(
                "cannot check out `{shown}` for the diff: {failure}"
            ))
        })?;
        let label = format!("{shown} ({})", commit.get(..7).unwrap_or(&commit));
        Ok(Self {
            repository,
            label,
            directory,
        })
    }

    /// Where `path`, in the working tree, is in the checkout.
    pub(super) fn locate(&self, path: &Path) -> Option<PathBuf> {
        let relative = path.strip_prefix(&self.repository).ok()?;
        Some(self.directory.path().join("base").join(relative))
    }

    pub(super) fn label(&self) -> &str {
        &self.label
    }
}

impl Drop for BaseCheckout {
    fn drop(&mut self) {
        let worktree = self.directory.path().join("base");
        let remove = ["worktree", "remove", "--force"];
        if let Err(failure) = run(git(&self.repository).args(remove).arg(&worktree)) {
            emit_stderr!(format!(
                "warning: could not remove the temporary worktree {} of the diff: {failure}; \
                 run `git worktree prune`",
                worktree.display()
            ));
        }
    }
}

fn git(directory: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(directory);
    command
}

/// Run git and return its standard output, or its error messages.
fn run(command: &mut Command) -> Result<String, String> {
    let output = command
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(strip_control_characters(&stderr.trim().replace('\n', "; ")))
}
