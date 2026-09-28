//! Writable homes for hermetic host tasks.

use cuenv_core::{Error, Result};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Resolve a writable home for a hermetic host task.
///
/// The fixed path is preferred so actions keep the same environment across
/// machines. Some runners provide that path read-only or with different
/// ownership, so fall back to a Cuenv-managed directory and include its actual
/// path in the action environment and cache key.
pub(crate) fn writable_task_home() -> Result<PathBuf> {
    let preferred = PathBuf::from(cuenv_core::environment::Environment::HERMETIC_DEFAULT_HOME);
    let mut fallbacks = Vec::with_capacity(2);
    if let Ok(cache_root) = cuenv_core::paths::cache_dir() {
        fallbacks.push(cache_root.join("task-home"));
    }
    fallbacks.push(temporary_task_home());

    match select_writable_home(&preferred, &fallbacks) {
        Ok(home) => {
            if home != preferred {
                tracing::info!(
                    requested = %preferred.display(),
                    selected = %home.display(),
                    "default hermetic task home is not writable; using Cuenv-managed home"
                );
            }
            Ok(home)
        }
        Err((path, error)) => Err(Error::io_with_path(
            "create writable hermetic task home",
            path,
            error,
        )),
    }
}

fn select_writable_home(
    preferred: &Path,
    fallbacks: &[PathBuf],
) -> std::result::Result<PathBuf, (PathBuf, io::Error)> {
    match ensure_writable_directory(preferred) {
        Ok(()) => Ok(preferred.to_path_buf()),
        Err(error) => {
            let mut last_failure = (preferred.to_path_buf(), error);
            for fallback in fallbacks {
                let result = create_private_directory(fallback)
                    .and_then(|()| ensure_writable_directory(fallback));
                match result {
                    Ok(()) => return Ok(fallback.clone()),
                    Err(error) => last_failure = (fallback.clone(), error),
                }
            }

            Err(last_failure)
        }
    }
}

fn ensure_writable_directory(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;

    static PROBE_ID: AtomicU64 = AtomicU64::new(0);
    loop {
        let suffix = PROBE_ID.fetch_add(1, Ordering::Relaxed);
        let probe = path.join(format!(
            ".cuenv-write-check-{}-{suffix}",
            std::process::id()
        ));
        match std::fs::create_dir(&probe) {
            Ok(()) => {
                std::fs::remove_dir(probe)?;
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }

    #[cfg(not(unix))]
    std::fs::create_dir_all(path)?;

    Ok(())
}

#[cfg(unix)]
fn temporary_task_home() -> PathBuf {
    // SAFETY: `geteuid` reads the current process identity and has no preconditions.
    let uid = unsafe { libc::geteuid() };
    std::env::temp_dir()
        .join(format!("cuenv-{uid}"))
        .join("task-home")
}

#[cfg(not(unix))]
fn temporary_task_home() -> PathBuf {
    std::env::temp_dir().join("cuenv").join("task-home")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn uses_a_private_writable_fallback_when_the_fixed_home_is_not_a_directory() {
        let temp = tempfile::tempdir().unwrap();
        let preferred = temp.path().join("home-file");
        fs::write(&preferred, "not a directory").unwrap();
        let fallback = temp.path().join("private-home");

        let selected = select_writable_home(&preferred, std::slice::from_ref(&fallback)).unwrap();

        assert_eq!(selected, fallback);
        fs::create_dir(selected.join(".cargo")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(selected).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
}
