//! Per-action exec roots: the directory a sandboxed task actually runs in.
//!
//! A task under [`Sandbox::Dir`](cuenv_core::tasks::Sandbox::Dir) does not run
//! in the project. It runs in a directory built from its declared inputs, and
//! only its declared outputs come back. This isolates relative workspace
//! access; it does not prevent a command from opening an absolute host path.
//!
//! Strict shared-cache publication needs an OS-level filesystem boundary in
//! addition to this execution root, so remote uploads remain disabled.

use cuenv_core::{Error, Result};
use cuenv_vcs::HashedInput;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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

    /// Resolve and create the action working directory inside this root.
    ///
    /// # Errors
    ///
    /// Returns an error when `relative` escapes the root or the directory
    /// cannot be created.
    pub fn workdir(&self, relative: &Path) -> Result<PathBuf> {
        let workdir = safe_join(self.path(), relative)?;
        std::fs::create_dir_all(&workdir)
            .map_err(|e| Error::io_with_path("create action workdir", workdir.clone(), e))?;
        Ok(workdir)
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

struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn create(parent: &Path, prefix: &str) -> Result<Self> {
        static INVOCATION: AtomicU64 = AtomicU64::new(0);

        loop {
            let suffix = INVOCATION.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!("{prefix}{}-{suffix}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(Error::io_with_path(
                        "create output scratch directory",
                        path,
                        error,
                    ));
                }
            }
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.path.display(),
                %error,
                "could not remove output scratch directory"
            );
        }
    }
}

/// Build an exec root under `cache_root` holding exactly `inputs`.
///
/// Each input is copied and the staged copy is verified against the digest
/// produced during input resolution. Hard links are deliberately forbidden:
/// writing a hard-linked sandbox input would mutate the source workspace.
///
/// # Errors
///
/// Returns an error if the root cannot be created, if an input's destination
/// escapes the root, or if an input can be neither linked nor copied.
pub fn prepare(cache_root: &Path, action_digest: &str, inputs: &[HashedInput]) -> Result<ExecRoot> {
    static INVOCATION: AtomicU64 = AtomicU64::new(0);

    let parent = cache_root.join("exec");
    std::fs::create_dir_all(&parent)
        .map_err(|e| Error::io_with_path("create exec root parent", parent.clone(), e))?;
    let path = loop {
        let suffix = INVOCATION.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!("{action_digest}-{}-{suffix}", std::process::id()));
        match std::fs::create_dir(&candidate) {
            Ok(()) => break candidate,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(Error::io_with_path("create exec root", candidate, error));
            }
        }
    };

    let root = ExecRoot { path };
    for input in inputs {
        let destination = safe_join(root.path(), &input.relative_path)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::io_with_path("create exec root directory", parent.to_path_buf(), e)
            })?;
        }
        place(input, &destination)?;
    }

    tracing::debug!(
        path = %root.path().display(),
        inputs = inputs.len(),
        "prepared exec root"
    );
    Ok(root)
}

/// Inputs to [`project_outputs`].
pub struct ProjectOutputs<'a> {
    /// Root containing the completed task's outputs.
    pub exec_root: &'a Path,
    /// Live workspace directory that owns the declarations.
    pub workdir: &'a Path,
    /// Paths produced by this invocation.
    pub resolved: &'a [PathBuf],
    /// Paths currently owned by the declarations in the live workspace.
    pub existing: &'a [PathBuf],
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
/// An output may name a directory. `outputs: ["dist"]` is how most real tasks
/// describe what they produce, and since isolation is the default, refusing a
/// directory here would fail builds that have always worked.
///
/// # Errors
///
/// Returns an error if an output cannot be copied into `workdir`.
pub fn project_outputs(input: ProjectOutputs<'_>) -> Result<()> {
    let ProjectOutputs {
        exec_root,
        workdir,
        resolved,
        existing,
    } = input;
    reject_symlink_base(workdir)?;
    reject_overlapping_paths(resolved)?;

    // Validate and copy every new output before changing the live workspace.
    // A deep symlink or read error therefore leaves the previous good output
    // untouched.
    let staging = ScratchDir::create(workdir, ".cuenv-project-")?;
    for relative in resolved {
        let source = secure_source(exec_root, relative)?;
        let destination = safe_join(staging.path(), relative)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::io_with_path("create staged output directory", parent.to_path_buf(), e)
            })?;
        }
        project_entry(&source, &destination)?;
    }

    commit_staged_outputs(staging.path(), workdir, resolved, existing)
}

/// Copy one output entry, recursing into directories.
///
/// Copying rather than linking: the workspace copy is the user's to edit, and
/// a hard link would write through to the exec root of any concurrent action
/// sharing the same blob.
fn project_entry(source: &Path, destination: &Path) -> Result<()> {
    let source_metadata = std::fs::symlink_metadata(source)
        .map_err(|e| Error::io_with_path("inspect projected output", source.to_path_buf(), e))?;
    if source_metadata.file_type().is_symlink() {
        return Err(Error::configuration(format!(
            "refusing to project symlink output '{}'",
            source.display()
        )));
    }

    if source_metadata.is_dir() {
        remove_existing(destination)?;
        std::fs::create_dir_all(destination).map_err(|e| {
            Error::io_with_path("create projected directory", destination.to_path_buf(), e)
        })?;
        let entries = std::fs::read_dir(source)
            .map_err(|e| Error::io_with_path("read output directory", source.to_path_buf(), e))?;
        for entry in entries {
            let entry = entry.map_err(|e| {
                Error::io_with_path("read output directory entry", source.to_path_buf(), e)
            })?;
            project_entry(&entry.path(), &destination.join(entry.file_name()))?;
        }
        return Ok(());
    }

    remove_existing(destination)?;
    std::fs::copy(source, destination)
        .map(|_| ())
        .map_err(|e| Error::io_with_path("project output", destination.to_path_buf(), e))
}

/// Install a complete staged output set and remove stale owned paths.
///
/// Existing outputs move to a same-filesystem backup before the new set is
/// installed. Any installation failure restores that backup, so a cache hit
/// or successful task cannot leave a half-updated workspace.
pub(crate) fn commit_staged_outputs(
    staging: &Path,
    workdir: &Path,
    resolved: &[PathBuf],
    existing: &[PathBuf],
) -> Result<()> {
    reject_symlink_base(workdir)?;
    reject_overlapping_paths(resolved)?;

    let backup = ScratchDir::create(workdir, ".cuenv-backup-")?;
    let owned_roots = collapse_owned_roots(existing, resolved);
    let mut backed_up = Vec::new();

    for relative in &owned_roots {
        match backup_owned_output(backup.path(), workdir, relative) {
            Ok(Some(paths)) => backed_up.push(paths),
            Ok(None) => {}
            Err(error) => {
                rollback_outputs(&[], &backed_up);
                return Err(error);
            }
        }
    }

    let mut installed = Vec::new();
    for relative in resolved {
        match install_staged_output(staging, workdir, relative) {
            Ok(destination) => installed.push(destination),
            Err(error) => {
                rollback_outputs(&installed, &backed_up);
                return Err(error);
            }
        }
    }

    Ok(())
}

fn backup_owned_output(
    backup: &Path,
    workdir: &Path,
    relative: &Path,
) -> Result<Option<(PathBuf, PathBuf)>> {
    let destination = secure_destination(workdir, relative)?;
    match std::fs::symlink_metadata(&destination) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Error::io_with_path(
                "inspect existing output",
                destination,
                error,
            ));
        }
    }
    let saved = safe_join(backup, relative)?;
    if let Some(parent) = saved.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::io_with_path("create output backup parent", parent.to_path_buf(), e)
        })?;
    }
    std::fs::rename(&destination, &saved)
        .map_err(|e| Error::io_with_path("backup existing output", destination.clone(), e))?;
    Ok(Some((saved, destination)))
}

fn install_staged_output(staging: &Path, workdir: &Path, relative: &Path) -> Result<PathBuf> {
    let source = safe_join(staging, relative)?;
    let destination = secure_destination(workdir, relative)?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            Error::io_with_path("create output directory", parent.to_path_buf(), error)
        })?;
    }
    std::fs::rename(&source, &destination).map_err(|error| {
        Error::io_with_path("install projected output", destination.clone(), error)
    })?;
    Ok(destination)
}

fn collapse_owned_roots(existing: &[PathBuf], resolved: &[PathBuf]) -> Vec<PathBuf> {
    let mut candidates = existing
        .iter()
        .chain(resolved)
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    candidates.sort_by_key(|path| path.components().count());

    candidates.into_iter().fold(Vec::new(), |mut roots, path| {
        if !roots.iter().any(|root: &PathBuf| path.starts_with(root)) {
            roots.push(path);
        }
        roots
    })
}

fn rollback_outputs(installed: &[PathBuf], backed_up: &[(PathBuf, PathBuf)]) {
    for path in installed.iter().rev() {
        if let Err(error) = remove_existing(path) {
            tracing::error!(path = %path.display(), %error, "could not remove partially installed output");
        }
    }
    for (saved, destination) in backed_up.iter().rev() {
        if let Some(parent) = destination.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            tracing::error!(path = %parent.display(), %error, "could not recreate output parent during rollback");
            continue;
        }
        if let Err(error) = std::fs::rename(saved, destination) {
            tracing::error!(
                from = %saved.display(),
                to = %destination.display(),
                %error,
                "could not restore output during rollback"
            );
        }
    }
}

fn reject_overlapping_paths(paths: &[PathBuf]) -> Result<()> {
    let mut sorted = paths.to_vec();
    sorted.sort();
    for (index, path) in sorted.iter().enumerate() {
        if sorted
            .iter()
            .skip(index + 1)
            .any(|candidate| candidate.starts_with(path))
        {
            return Err(Error::configuration(format!(
                "declared outputs overlap at '{}'",
                path.display()
            )));
        }
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

fn secure_destination(base: &Path, relative: &Path) -> Result<PathBuf> {
    reject_symlink_base(base)?;
    let destination = safe_join(base, relative)?;
    reject_symlink_parents(base, relative, "project output")?;
    Ok(destination)
}

fn secure_source(base: &Path, relative: &Path) -> Result<PathBuf> {
    reject_symlink_base(base)?;
    let source = safe_join(base, relative)?;
    reject_symlink_parents(base, relative, "read output")?;
    Ok(source)
}

fn reject_symlink_base(base: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(base)
        .map_err(|error| Error::io_with_path("inspect output root", base, error))?;
    if metadata.file_type().is_symlink() {
        return Err(Error::configuration(format!(
            "refusing to use symlink output root '{}'",
            base.display()
        )));
    }
    Ok(())
}

fn reject_symlink_parents(base: &Path, relative: &Path, operation: &str) -> Result<()> {
    let mut current = base.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            continue;
        };
        current.push(name);
        if components.peek().is_none() {
            break;
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(Error::configuration(format!(
                    "refusing to {operation} through symlink parent '{}'",
                    current.display()
                )));
            }
            Ok(metadata) if !metadata.is_dir() => break,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(Error::io_with_path("inspect output parent", current, error));
            }
        }
    }
    Ok(())
}

fn remove_existing(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(path)
                .map_err(|e| Error::io_with_path("replace projected output", path.to_path_buf(), e))
        }
        Ok(_) => std::fs::remove_file(path)
            .map_err(|e| Error::io_with_path("replace projected output", path.to_path_buf(), e)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::io_with_path(
            "inspect projected output",
            path.to_path_buf(),
            error,
        )),
    }
}

/// Copy one input and verify the copied bytes still match the resolved digest.
fn place(input: &HashedInput, destination: &Path) -> Result<()> {
    std::fs::copy(&input.absolute_path, destination).map_err(|e| {
        Error::io_with_path("stage input into exec root", destination.to_path_buf(), e)
    })?;

    let mut file = std::fs::File::open(destination)
        .map_err(|e| Error::io_with_path("verify staged input", destination.to_path_buf(), e))?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|e| {
            Error::io_with_path("verify staged input", destination.to_path_buf(), e)
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        size += count as u64;
    }
    let hash = hex::encode(hasher.finalize());
    if hash != input.sha256 || size != input.size {
        return Err(Error::configuration(format!(
            "input '{}' changed while preparing the execution root",
            input.relative_path.display()
        )));
    }
    normalize_executable(destination, input.is_executable)?;
    Ok(())
}

#[cfg(unix)]
fn normalize_executable(path: &Path, is_executable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)
        .map_err(|e| Error::io_with_path("inspect staged input mode", path, e))?
        .permissions();
    let mode = if is_executable {
        permissions.mode() | 0o111
    } else {
        permissions.mode() & !0o111
    };
    permissions.set_mode(mode);
    std::fs::set_permissions(path, permissions)
        .map_err(|e| Error::io_with_path("normalize staged input mode", path, e))
}

#[cfg(not(unix))]
fn normalize_executable(_path: &Path, _is_executable: bool) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn hashed(absolute: &Path, relative: &str) -> HashedInput {
        let bytes = fs::read(absolute).unwrap();
        let digest = cuenv_cas::Digest::of_bytes(&bytes);
        HashedInput {
            relative_path: PathBuf::from(relative),
            absolute_path: absolute.to_path_buf(),
            sha256: digest.hash,
            size: digest.size_bytes,
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
    fn concurrent_roots_for_one_action_are_unique() {
        let tmp = TempDir::new().unwrap();
        let first = prepare(tmp.path(), "abc123", &[]).unwrap();
        let second = prepare(tmp.path(), "abc123", &[]).unwrap();
        assert_ne!(first.path(), second.path());
        assert!(first.path().exists());
        assert!(second.path().exists());
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
    fn mutating_a_staged_input_does_not_mutate_the_workspace() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source.txt");
        fs::write(&source, "original").unwrap();
        let digest = cuenv_cas::Digest::of_bytes(b"original");
        let input = HashedInput {
            relative_path: PathBuf::from("source.txt"),
            absolute_path: source.clone(),
            sha256: digest.hash,
            size: digest.size_bytes,
            is_executable: false,
        };

        let root = prepare(tmp.path(), "abc123", &[input]).unwrap();
        fs::write(root.path().join("source.txt"), "changed").unwrap();

        assert_eq!(fs::read_to_string(source).unwrap(), "original");
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

        project_outputs(ProjectOutputs {
            exec_root: &exec_root,
            workdir: &workdir,
            resolved: &[PathBuf::from("target/app")],
            existing: &[],
        })
        .unwrap();

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

        project_outputs(ProjectOutputs {
            exec_root: &exec_root,
            workdir: &workdir,
            resolved: &[],
            existing: &[],
        })
        .unwrap();
        assert!(!workdir.join("target/app").exists());
    }

    #[test]
    fn a_declared_output_directory_comes_back_whole() {
        // `outputs: ["dist"]` is how most real tasks describe what they
        // produce. Isolation is the default, so refusing a directory here
        // would break builds that have always worked.
        let tmp = TempDir::new().unwrap();
        let exec_root = tmp.path().join("exec");
        let workdir = tmp.path().join("workdir");
        fs::create_dir_all(exec_root.join("dist/nested")).unwrap();
        fs::create_dir_all(workdir.join("dist")).unwrap();
        fs::write(exec_root.join("dist/app.js"), "built").unwrap();
        fs::write(exec_root.join("dist/nested/chunk.js"), "chunk").unwrap();
        fs::write(workdir.join("dist/stale.js"), "stale").unwrap();

        project_outputs(ProjectOutputs {
            exec_root: &exec_root,
            workdir: &workdir,
            resolved: &[PathBuf::from("dist")],
            existing: &[PathBuf::from("dist")],
        })
        .unwrap();

        assert_eq!(
            fs::read_to_string(workdir.join("dist/app.js")).unwrap(),
            "built"
        );
        assert_eq!(
            fs::read_to_string(workdir.join("dist/nested/chunk.js")).unwrap(),
            "chunk"
        );
        assert!(!workdir.join("dist/stale.js").exists());
    }

    #[test]
    fn a_projected_directory_replaces_a_stale_file_of_the_wrong_kind() {
        let tmp = TempDir::new().unwrap();
        let exec_root = tmp.path().join("exec");
        let workdir = tmp.path().join("workdir");
        fs::create_dir_all(exec_root.join("dist")).unwrap();
        fs::create_dir_all(&workdir).unwrap();
        fs::write(exec_root.join("dist/app.js"), "built").unwrap();
        // The workspace holds a *file* named `dist` from some earlier run.
        fs::write(workdir.join("dist"), "stale file").unwrap();

        project_outputs(ProjectOutputs {
            exec_root: &exec_root,
            workdir: &workdir,
            resolved: &[PathBuf::from("dist")],
            existing: &[PathBuf::from("dist")],
        })
        .unwrap();
        assert_eq!(
            fs::read_to_string(workdir.join("dist/app.js")).unwrap(),
            "built"
        );
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

        project_outputs(ProjectOutputs {
            exec_root: &exec_root,
            workdir: &workdir,
            resolved: &[PathBuf::from("out")],
            existing: &[PathBuf::from("out")],
        })
        .unwrap();
        assert_eq!(fs::read_to_string(workdir.join("out")).unwrap(), "fresh");
    }

    #[test]
    fn an_absent_output_removes_the_previous_owned_path() {
        let tmp = TempDir::new().unwrap();
        let exec_root = tmp.path().join("exec");
        let workdir = tmp.path().join("workdir");
        fs::create_dir_all(&exec_root).unwrap();
        fs::create_dir_all(&workdir).unwrap();
        fs::write(workdir.join("stale.txt"), "old").unwrap();

        project_outputs(ProjectOutputs {
            exec_root: &exec_root,
            workdir: &workdir,
            resolved: &[],
            existing: &[PathBuf::from("stale.txt")],
        })
        .unwrap();

        assert!(!workdir.join("stale.txt").exists());
    }
}
