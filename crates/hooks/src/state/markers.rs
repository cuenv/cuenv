use super::StateManager;
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::debug;

impl StateManager {
    /// Compute a key for directory-only lookups (used for fast status checks).
    /// This hashes just the canonicalized directory path, without config hash.
    #[must_use]
    pub fn compute_directory_key(path: &Path) -> String {
        let mut hasher = Sha256::new();
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        hasher.update(canonical.to_string_lossy().as_bytes());
        format!("{:x}", hasher.finalize())[..16].to_string()
    }

    /// Get the path for a directory marker file
    pub(super) fn directory_marker_path(&self, directory_key: &str) -> PathBuf {
        self.state_dir.join(format!("{}.marker", directory_key))
    }

    /// Create a marker file linking directory to instance hash.
    /// Called when hooks start to enable fast status lookups.
    pub async fn create_directory_marker(
        &self,
        directory_path: &Path,
        instance_hash: &str,
    ) -> Result<()> {
        self.ensure_state_dir().await?;
        let dir_key = Self::compute_directory_key(directory_path);
        let marker_path = self.directory_marker_path(&dir_key);

        fs::write(&marker_path, instance_hash)
            .await
            .map_err(|e| Error::Io {
                source: e,
                path: Some(marker_path.into_boxed_path()),
                operation: "write_marker".to_string(),
            })?;

        debug!(
            "Created directory marker for {} -> {}",
            directory_path.display(),
            instance_hash
        );
        Ok(())
    }

    /// Remove marker file for a directory.
    /// Called when hooks complete/fail and display timeout expires.
    pub async fn remove_directory_marker(&self, directory_path: &Path) -> Result<()> {
        let dir_key = Self::compute_directory_key(directory_path);
        let marker_path = self.directory_marker_path(&dir_key);

        if marker_path.exists() {
            fs::remove_file(&marker_path).await.ok();
            debug!("Removed directory marker for {}", directory_path.display());
        }
        Ok(())
    }

    /// Fast synchronous check: does a marker exist for this directory?
    /// This is the hot path for Starship - just a single stat() syscall.
    #[must_use]
    pub fn has_active_marker(&self, directory_path: &Path) -> bool {
        let dir_key = Self::compute_directory_key(directory_path);
        self.directory_marker_path(&dir_key).exists()
    }

    /// Read the instance hash from a marker file (if it exists).
    pub async fn get_marker_instance_hash(&self, directory_path: &Path) -> Option<String> {
        let dir_key = Self::compute_directory_key(directory_path);
        let marker_path = self.directory_marker_path(&dir_key);
        fs::read_to_string(&marker_path)
            .await
            .ok()
            .map(|s| s.trim().to_string())
    }

    /// Read the instance hash from a marker file synchronously.
    /// This is the sync equivalent of `get_marker_instance_hash` for the fast path.
    #[must_use]
    pub fn get_marker_instance_hash_sync(&self, directory_path: &Path) -> Option<String> {
        let dir_key = Self::compute_directory_key(directory_path);
        let marker_path = self.directory_marker_path(&dir_key);
        std::fs::read_to_string(&marker_path)
            .ok()
            .map(|s| s.trim().to_string())
    }
}
