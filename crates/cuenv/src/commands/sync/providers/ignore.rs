//! Ignore file sync provider.
//!
//! **DEPRECATED**: Ignore configuration has moved to .rules.cue files.
//! Use the 'rules' sync provider instead: `cuenv sync rules`

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::manifest::Base;
use std::path::Path;

use crate::commands::CommandExecutor;
use crate::commands::sync::provider::{SyncOptions, SyncProvider, SyncResult};

/// Sync provider for ignore files.
pub struct IgnoreSyncProvider;

#[async_trait]
impl SyncProvider for IgnoreSyncProvider {
    fn name(&self) -> &'static str {
        "ignore"
    }

    fn description(&self) -> &'static str {
        "Sync ignore files (.gitignore, .dockerignore, etc.)"
    }

    fn has_config(&self, _manifest: &Base) -> bool {
        // Ignore is now configured via .rules.cue files, not env.cue
        // Use the 'rules' sync provider instead
        false
    }

    async fn sync_path(
        &self,
        _path: &Path,
        _package: &str,
        _options: &SyncOptions,
        _executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        // Ignore is now configured via .rules.cue files, not env.cue
        // Use the 'rules' sync provider instead
        Ok(SyncResult::success(
            "Ignore configuration has moved to .rules.cue files. Use 'cuenv sync rules' instead.",
        ))
    }

    async fn sync_workspace(
        &self,
        _package: &str,
        _options: &SyncOptions,
        _executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        // Ignore is now configured via .rules.cue files, not env.cue
        // Use the 'rules' sync provider instead
        Ok(SyncResult::success(
            "Ignore configuration has moved to .rules.cue files. Use 'cuenv sync rules' instead.",
        ))
    }
}
