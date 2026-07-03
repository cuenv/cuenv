//! Traversal validation for archive entry paths.
//!
//! Mirrors the guarantee the zip path gets from `enclosed_name()`: an entry
//! may not be absolute and may not contain `..` components, so extraction
//! cannot write outside the destination directory. Used by the macOS `.pkg`
//! cpio flow, which has no library-level equivalent; kept platform-neutral
//! so the validation logic is unit-tested on every platform.

use std::path::{Component, Path};

/// Returns `true` when `entry` cannot escape the extraction root: it is
/// non-empty, relative, and free of `..` components. `.` components are
/// allowed (cpio listings routinely emit `./usr/...` and a bare `.`).
pub fn is_safe_entry(entry: &str) -> bool {
    if entry.is_empty() {
        return false;
    }
    Path::new(entry)
        .components()
        .all(|component| matches!(component, Component::CurDir | Component::Normal(_)))
}

/// Scan listing lines (e.g. `cpio -it` output) and return the first unsafe
/// entry, ignoring blank lines.
pub fn find_unsafe_entry<'a>(lines: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    lines
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .find(|line| !is_safe_entry(line))
}
