use super::StateManager;
use crate::types::ExecutionStatus;
use crate::{Error, Result};
use chrono::Utc;
use tokio::fs;
use tracing::{debug, error, info, warn};

impl StateManager {
    /// Clean up the entire state directory
    pub async fn cleanup_state_directory(&self) -> Result<usize> {
        if !self.state_dir.exists() {
            return Ok(0);
        }

        let mut cleaned_count = 0;
        let mut dir = fs::read_dir(&self.state_dir).await.map_err(|e| Error::Io {
            source: e,
            path: Some(self.state_dir.clone().into_boxed_path()),
            operation: "read_dir".to_string(),
        })?;

        while let Some(entry) = dir.next_entry().await.map_err(|e| Error::Io {
            source: e,
            path: Some(self.state_dir.clone().into_boxed_path()),
            operation: "next_entry".to_string(),
        })? {
            let path = entry.path();

            let extension = path.extension().and_then(|s| s.to_str());

            if extension == Some("json") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    match self.load_state(stem).await {
                        Ok(Some(state)) if state.is_complete() => {
                            if let Err(e) = fs::remove_file(&path).await {
                                warn!("Failed to remove state file {}: {}", path.display(), e);
                            } else {
                                cleaned_count += 1;
                                debug!("Cleaned up state file: {}", path.display());
                                self.remove_directory_marker(&state.directory_path)
                                    .await
                                    .ok();
                            }
                        }
                        Ok(Some(_)) => {
                            debug!("Keeping active state file: {}", path.display());
                        }
                        Ok(None) => {}
                        Err(e) => {
                            warn!("Failed to parse state file {}: {}", path.display(), e);
                            if let Err(rm_err) = fs::remove_file(&path).await {
                                error!(
                                    "Failed to remove corrupted state file {}: {}",
                                    path.display(),
                                    rm_err
                                );
                            } else {
                                cleaned_count += 1;
                                info!("Removed corrupted state file: {}", path.display());
                            }
                        }
                    }
                }
            } else if extension == Some("marker")
                && let Ok(instance_hash) = fs::read_to_string(&path).await
            {
                let instance_hash = instance_hash.trim();
                let cleanup_reason = match self.load_state(instance_hash).await {
                    Ok(None) => Some("orphaned"),
                    Ok(Some(state)) if state.is_complete() && !state.should_display_completed() => {
                        Some("expired")
                    }
                    _ => None,
                };
                if let Some(reason) = cleanup_reason
                    && fs::remove_file(&path).await.is_ok()
                {
                    cleaned_count += 1;
                    debug!("Cleaned up {reason} marker: {}", path.display());
                }
            }
        }

        if cleaned_count > 0 {
            info!(
                "Cleaned up {} state/marker files from directory",
                cleaned_count
            );
        }

        Ok(cleaned_count)
    }

    /// Clean up orphaned state files (states without corresponding processes)
    pub async fn cleanup_orphaned_states(&self, max_age: chrono::Duration) -> Result<usize> {
        let cutoff = Utc::now() - max_age;
        let mut cleaned_count = 0;

        for state in self.list_active_states().await? {
            if state.status == ExecutionStatus::Running && state.started_at < cutoff {
                warn!(
                    "Found orphaned running state for {} (started {}), removing",
                    state.directory_path.display(),
                    state.started_at
                );
                self.remove_state(&state.instance_hash).await?;
                self.remove_directory_marker(&state.directory_path)
                    .await
                    .ok();
                cleaned_count += 1;
            }
        }

        if cleaned_count > 0 {
            info!("Cleaned up {} orphaned state files", cleaned_count);
        }

        Ok(cleaned_count)
    }
}
