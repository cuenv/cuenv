// ============================================================================
// Result types
// ============================================================================

/// Result of generating an EditorConfig file.
#[derive(Debug)]
pub struct SyncResult {
    /// The status of the file operation.
    pub status: FileStatus,
}

/// Status of a file operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    /// File was newly created.
    Created,
    /// File existed and was updated with new content.
    Updated,
    /// File existed and content was unchanged.
    Unchanged,
    /// Would be created (dry-run mode).
    WouldCreate,
    /// Would be updated (dry-run mode).
    WouldUpdate,
}

impl std::fmt::Display for FileStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created => write!(f, "Created"),
            Self::Updated => write!(f, "Updated"),
            Self::Unchanged => write!(f, "Unchanged"),
            Self::WouldCreate => write!(f, "Would create"),
            Self::WouldUpdate => write!(f, "Would update"),
        }
    }
}
