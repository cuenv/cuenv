use crate::{Error, Result};
use std::path::{Path, PathBuf};
use tokio::fs;

fn is_reserved_state_identifier(instance_hash: &str) -> bool {
    let upper = instance_hash.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || matches!(
            upper.as_bytes(),
            [b'C', b'O', b'M', b'1'..=b'9'] | [b'L', b'P', b'T', b'1'..=b'9']
        )
}

fn validate_state_directory(path: &Path, metadata: &std::fs::Metadata) -> Result<()> {
    if !metadata.file_type().is_dir() {
        return Err(Error::configuration(format!(
            "Hook state directory {} must be a directory, not a symlink or file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != effective_uid() || metadata.mode() & 0o077 != 0 {
            return Err(Error::configuration(format!(
                "Hook state directory {} must be owned by the current user and private (mode 0700 or stricter)",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

#[cfg(unix)]
fn secure_default_state_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let directory = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(Error::Io {
                source,
                path: Some(path.to_path_buf().into_boxed_path()),
                operation: "open default state directory without following symlinks".to_string(),
            });
        }
    };
    let metadata = directory.metadata().map_err(|source| Error::Io {
        source,
        path: Some(path.to_path_buf().into_boxed_path()),
        operation: "inspect default state directory".to_string(),
    })?;
    if metadata.uid() != effective_uid() || metadata.mode() & 0o022 != 0 {
        return Err(Error::configuration(format!(
            "Default hook state directory {} must be owned by the current user and not writable by group or others",
            path.display()
        )));
    }
    if metadata.mode() & 0o077 != 0 {
        let permissions = std::fs::Permissions::from_mode(metadata.mode() & !0o077);
        directory
            .set_permissions(permissions)
            .map_err(|source| Error::Io {
                source,
                path: Some(path.to_path_buf().into_boxed_path()),
                operation: "make default state directory private".to_string(),
            })?;
    }
    Ok(())
}

/// Manages persistent state for hook execution sessions
#[derive(Debug, Clone)]
pub struct StateManager {
    pub(super) state_dir: PathBuf,
}

impl StateManager {
    /// Create a new state manager with the specified state directory
    #[must_use]
    pub fn new(state_dir: PathBuf) -> Self {
        Self { state_dir }
    }

    /// Get the default state directory.
    ///
    /// Uses platform-appropriate paths:
    /// - Linux: `~/.local/state/cuenv/hooks`
    /// - macOS: `~/Library/Application Support/cuenv/hooks`
    /// - Windows: `%APPDATA%\cuenv\hooks`
    ///
    /// # Errors
    ///
    /// Returns an error when no platform state or data directory is available.
    pub fn default_state_dir() -> Result<PathBuf> {
        let base = dirs::state_dir()
            .or_else(dirs::data_dir)
            .ok_or_else(|| Error::configuration("Could not determine state directory"))?;
        Ok(base.join("cuenv").join("hooks"))
    }

    /// Create a state manager using the default state directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the default path is unavailable or insecure.
    pub fn with_default_dir() -> Result<Self> {
        let state_dir = Self::default_state_dir()?;
        #[cfg(unix)]
        secure_default_state_directory(&state_dir)?;
        Ok(Self::new(state_dir))
    }

    /// Get the state directory path
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Ensure the state directory exists and is private to the current user.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created or verified private.
    pub async fn ensure_state_dir(&self) -> Result<()> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder
            .create(&self.state_dir)
            .await
            .map_err(|source| Error::Io {
                source,
                path: Some(self.state_dir.clone().into_boxed_path()),
                operation: "create state directory".to_string(),
            })?;
        let metadata = fs::symlink_metadata(&self.state_dir)
            .await
            .map_err(|source| Error::Io {
                source,
                path: Some(self.state_dir.clone().into_boxed_path()),
                operation: "inspect state directory".to_string(),
            })?;
        validate_state_directory(&self.state_dir, &metadata)?;
        Ok(())
    }

    pub(super) async fn state_dir_exists(&self) -> Result<bool> {
        match fs::symlink_metadata(&self.state_dir).await {
            Ok(metadata) => {
                validate_state_directory(&self.state_dir, &metadata)?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(Error::Io {
                source,
                path: Some(self.state_dir.clone().into_boxed_path()),
                operation: "inspect state directory".to_string(),
            }),
        }
    }

    pub(super) fn state_dir_exists_sync(&self) -> Result<bool> {
        match std::fs::symlink_metadata(&self.state_dir) {
            Ok(metadata) => {
                validate_state_directory(&self.state_dir, &metadata)?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(Error::Io {
                source,
                path: Some(self.state_dir.clone().into_boxed_path()),
                operation: "inspect state directory".to_string(),
            }),
        }
    }

    /// Generate a state file path for a valid instance identifier.
    ///
    /// # Errors
    ///
    /// Returns an error if the identifier is empty or is not a safe filename.
    pub fn try_state_file_path(&self, instance_hash: &str) -> Result<PathBuf> {
        if instance_hash.is_empty()
            || !instance_hash
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || is_reserved_state_identifier(instance_hash)
        {
            return Err(Error::configuration(
                "Invalid hook state instance identifier",
            ));
        }
        Ok(self.state_dir.join(format!("{}.json", instance_hash)))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn managed_default_permissions_are_tightened_only_when_safe() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let default = directory.path().join("hooks");
        std::fs::create_dir(&default).unwrap();
        std::fs::set_permissions(&default, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::secure_default_state_directory(&default).unwrap();
        assert_eq!(
            std::fs::metadata(&default).unwrap().permissions().mode() & 0o777,
            0o700
        );

        std::fs::set_permissions(&default, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(super::secure_default_state_directory(&default).is_err());
        assert_eq!(
            std::fs::metadata(&default).unwrap().permissions().mode() & 0o777,
            0o775
        );

        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&default, &link).unwrap();
        assert!(super::secure_default_state_directory(&link).is_err());
        assert_eq!(
            std::fs::metadata(default).unwrap().permissions().mode() & 0o777,
            0o775
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn replacing_state_with_an_open_reader_never_removes_the_old_state() {
        use crate::state::{HookExecutionIdentity, HookExecutionState, StateManager};
        use std::io::Read;

        let directory = tempfile::tempdir().unwrap();
        let manager = StateManager::new(directory.path().to_path_buf());
        let mut state = HookExecutionState::new(
            HookExecutionIdentity {
                session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
                directory_path: directory.path().to_path_buf(),
                instance_hash: "reader".to_string(),
                config_hash: "config".to_string(),
            },
            Vec::new(),
        );
        manager.seed_test_state(&state).await.unwrap();
        let path = manager.try_state_file_path("reader").unwrap();
        let mut reader = std::fs::File::open(path).unwrap();
        state.completed_hooks = 1;
        let result = manager.seed_test_state(&state).await;
        let persisted = manager.load_state("reader").await.unwrap().unwrap();
        if result.is_ok() {
            assert_eq!(persisted.completed_hooks, 1);
        } else {
            assert_eq!(persisted.completed_hooks, 0);
        }
        let mut old = String::new();
        reader.read_to_string(&mut old).unwrap();
        let old: HookExecutionState = serde_json::from_str(&old).unwrap();
        assert_eq!(old.completed_hooks, 0);
    }
}
