//! A temporary file that is removed when dropped.

use std::path::{Path, PathBuf};
use uuid::Uuid;

pub struct TemporaryFile {
    path: PathBuf,
}

impl TemporaryFile {
    pub fn new() -> cuenv_cas::Result<Self> {
        let path = std::env::temp_dir().join(format!("cuenv-remote-cas-{}.tmp", Uuid::new_v4()));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| cuenv_cas::Error::io(error, &path, "create temporary file"))?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
