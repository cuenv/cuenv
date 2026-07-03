use serde::{Deserialize, Serialize};

// =============================================================================
// Task Cache Policy
// =============================================================================

/// Cache mode for task result caching.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TaskCacheMode {
    /// Never read or write cache entries.
    #[default]
    Never,
    /// Read from cache only.
    Read,
    /// Write to cache only.
    Write,
    /// Read from and write to cache.
    ReadWrite,
}

impl TaskCacheMode {
    /// Returns true when this mode allows cache reads.
    #[must_use]
    pub const fn allows_read(self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }

    /// Returns true when this mode allows cache writes.
    #[must_use]
    pub const fn allows_write(self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}

/// Cache policy controls for a single task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskCachePolicy {
    /// Cache mode for the task. Default is `never`.
    #[serde(default)]
    pub mode: TaskCacheMode,
    /// Maximum age for cache reads (for example: "1h", "30m", "infinite").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age: Option<String>,
}
