//! What the filesystem records about a file that git also stores.

/// Whether file metadata has an executable bit set.
#[cfg(unix)]
pub fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

/// Whether file metadata has an executable bit set: never, where there is
/// no such bit.
#[cfg(not(unix))]
pub fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}
