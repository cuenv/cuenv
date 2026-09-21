//! Per-action exec roots: the directory a sandboxed task actually runs in.
//!
//! A task under [`Sandbox::Dir`](cuenv_core::tasks::Sandbox::Dir) does not run
//! in the project. It runs in a directory built from exactly its declared
//! inputs, and only its declared outputs come back. That is what turns
//! `inputs` from documentation into a contract: an undeclared read fails
//! instead of quietly producing a cache entry that is wrong on the next
//! machine.
//!
//! This is the same trade Bazel and buck2 make. The cost is a directory of
//! links per action; the return is that an entry recorded here means something,
//! which is the precondition for ever uploading one to a shared cache.

use cuenv_core::{Error, Result};
use cuenv_vcs::HashedInput;
use std::path::{Component, Path, PathBuf};

/// A materialized exec root, removed when dropped.
///
/// Dropping cleans up on the success path *and* on every error path, which is
/// the reason this is a guard rather than a pair of functions: a task that
/// fails mid-run would otherwise leave its inputs behind, and the next run of
/// the same action would find a directory it did not build.
#[derive(Debug)]
pub struct ExecRoot {
    path: PathBuf,
}

impl ExecRoot {
    /// The directory the task should run in.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ExecRoot {
    fn drop(&mut self) {
        // A failure to clean up is worth knowing about but never worth
        // failing a task that already finished.
        if let Err(error) = std::fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.path.display(),
                error = %error,
                "could not remove exec root"
            );
        }
    }
}

/// Build an exec root under `cache_root` holding exactly `inputs`.
///
/// Each input is hard-linked from its real location when the filesystem
/// allows it and copied otherwise. A hard link is not a weaker choice than a
/// copy here: the input was hashed before this call, and a task that mutates
/// a declared input is misdeclared either way — copying would merely hide it
/// until the next run produced a different key for the same source.
///
/// # Errors
///
/// Returns an error if the root cannot be created, if an input's destination
/// escapes the root, or if an input can be neither linked nor copied.
pub fn prepare(cache_root: &Path, action_digest: &str, inputs: &[HashedInput]) -> Result<ExecRoot> {
    let path = cache_root.join("exec").join(action_digest);
    // A leftover root from an interrupted run would leak undeclared files
    // into this one, which is exactly what the sandbox exists to prevent.
    if path.exists() {
        std::fs::remove_dir_all(&path)
            .map_err(|e| Error::io_with_path("remove stale exec root", path.clone(), e))?;
    }
    std::fs::create_dir_all(&path)
        .map_err(|e| Error::io_with_path("create exec root", path.clone(), e))?;

    let root = ExecRoot { path };
    for input in inputs {
        let destination = safe_join(root.path(), &input.relative_path)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::io_with_path("create exec root directory", parent.to_path_buf(), e)
            })?;
        }
        place(&input.absolute_path, &destination)?;
    }

    tracing::debug!(
        path = %root.path().display(),
        inputs = inputs.len(),
        "prepared exec root"
    );
    Ok(root)
}

/// Copy the task's declared outputs out of the exec root and into `workdir`.
///
/// Without this the user would run a build and find nothing built: the exec
/// root is transient, so an output that stays there is an output that was
/// thrown away. Only declared outputs are projected, which is the other half
/// of the contract — a task that writes somewhere it did not declare loses
/// that write, visibly, on the first run rather than mysteriously on the
/// hundredth.
///
/// # Errors
///
/// Returns an error if an output cannot be copied into `workdir`.
pub fn project_outputs(exec_root: &Path, workdir: &Path, outputs: &[PathBuf]) -> Result<()> {
    for relative in outputs {
        let source = safe_join(exec_root, relative)?;
        if !source.exists() {
            continue;
        }
        let destination = safe_join(workdir, relative)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::io_with_path("create output directory", parent.to_path_buf(), e)
            })?;
        }
        // Replacing rather than linking: the workspace copy is the user's to
        // edit, and a hard link would silently write through to the CAS-fed
        // exec root of any concurrent action sharing the same blob.
        if destination.exists() {
            std::fs::remove_file(&destination).map_err(|e| {
                Error::io_with_path("replace projected output", destination.clone(), e)
            })?;
        }
        std::fs::copy(&source, &destination)
            .map_err(|e| Error::io_with_path("project output", destination.clone(), e))?;
    }
    Ok(())
}

/// Join `relative` onto `base`, refusing anything that escapes it.
///
/// `..` in a declared input or output would write outside the sandbox, which
/// would defeat the entire point of having one.
fn safe_join(base: &Path, relative: &Path) -> Result<PathBuf> {
    let mut joined = base.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(name) => joined.push(name),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::configuration(format!(
                    "path '{}' escapes its sandbox root",
                    relative.display()
                )));
            }
        }
    }
    Ok(joined)
}

/// Hard-link `source` to `destination`, falling back to a copy.
///
/// Cross-device links, filesystems without link support and link-count limits
/// all surface as ordinary errors, so the fallback is unconditional rather
/// than conditional on an error kind that varies by platform.
fn place(source: &Path, destination: &Path) -> Result<()> {
    if std::fs::hard_link(source, destination).is_ok() {
        return Ok(());
    }
    std::fs::copy(source, destination)
        .map(|_| ())
        .map_err(|e| Error::io_with_path("stage input into exec root", destination.to_path_buf(), e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn hashed(absolute: &Path, relative: &str) -> HashedInput {
        HashedInput {
            relative_path: PathBuf::from(relative),
            absolute_path: absolute.to_path_buf(),
            sha256: "0".repeat(64),
            size: 0,
            is_executable: false,
        }
    }

    #[test]
    fn an_exec_root_holds_exactly_the_declared_inputs() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::write(workspace.join("src/main.rs"), "declared").unwrap();
        fs::write(workspace.join("secret.txt"), "undeclared").unwrap();

        let inputs = vec![hashed(&workspace.join("src/main.rs"), "src/main.rs")];
        let root = prepare(tmp.path(), "abc123", &inputs).unwrap();

        assert_eq!(
            fs::read_to_string(root.path().join("src/main.rs")).unwrap(),
            "declared"
        );
        assert!(
            !root.path().join("secret.txt").exists(),
            "an undeclared file must not be reachable"
        );
    }

    #[test]
    fn a_stale_root_is_rebuilt_rather_than_reused() {
        // An interrupted run leaves files behind; reusing them would leak
        // undeclared inputs into the next run of the same action.
        let tmp = TempDir::new().unwrap();
        let stale = tmp.path().join("exec/abc123");
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("leftover.txt"), "from a previous run").unwrap();

        let root = prepare(tmp.path(), "abc123", &[]).unwrap();
        assert!(!root.path().join("leftover.txt").exists());
    }

    #[test]
    fn dropping_the_root_removes_it() {
        let tmp = TempDir::new().unwrap();
        let path = {
            let root = prepare(tmp.path(), "abc123", &[]).unwrap();
            root.path().to_path_buf()
        };
        assert!(!path.exists(), "the exec root must not outlive its guard");
    }

    #[test]
    fn an_input_may_not_escape_the_root() {
        let tmp = TempDir::new().unwrap();
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, "elsewhere").unwrap();

        let inputs = vec![hashed(&outside, "../escaped.txt")];
        let error = prepare(tmp.path(), "abc123", &inputs).unwrap_err();
        assert!(
            error.to_string().contains("escapes its sandbox root"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn declared_outputs_come_back_and_undeclared_ones_do_not() {
        let tmp = TempDir::new().unwrap();
        let exec_root = tmp.path().join("exec");
        let workdir = tmp.path().join("workdir");
        fs::create_dir_all(exec_root.join("target")).unwrap();
        fs::create_dir_all(&workdir).unwrap();
        fs::write(exec_root.join("target/app"), "built").unwrap();
        fs::write(exec_root.join("scratch.tmp"), "noise").unwrap();

        project_outputs(&exec_root, &workdir, &[PathBuf::from("target/app")]).unwrap();

        assert_eq!(
            fs::read_to_string(workdir.join("target/app")).unwrap(),
            "built"
        );
        assert!(
            !workdir.join("scratch.tmp").exists(),
            "an undeclared write is lost, visibly, on the first run"
        );
    }

    #[test]
    fn a_declared_output_the_task_never_produced_is_not_an_error() {
        let tmp = TempDir::new().unwrap();
        let exec_root = tmp.path().join("exec");
        let workdir = tmp.path().join("workdir");
        fs::create_dir_all(&exec_root).unwrap();
        fs::create_dir_all(&workdir).unwrap();

        project_outputs(&exec_root, &workdir, &[PathBuf::from("target/app")]).unwrap();
        assert!(!workdir.join("target/app").exists());
    }

    #[test]
    fn projecting_replaces_an_existing_workspace_file() {
        let tmp = TempDir::new().unwrap();
        let exec_root = tmp.path().join("exec");
        let workdir = tmp.path().join("workdir");
        fs::create_dir_all(&exec_root).unwrap();
        fs::create_dir_all(&workdir).unwrap();
        fs::write(exec_root.join("out"), "fresh").unwrap();
        fs::write(workdir.join("out"), "stale").unwrap();

        project_outputs(&exec_root, &workdir, &[PathBuf::from("out")]).unwrap();
        assert_eq!(fs::read_to_string(workdir.join("out")).unwrap(), "fresh");
    }
}
