//! Shared archive extraction for cuenv tool providers.
//!
//! Consolidates the zip / tar.gz / tar.xz / macOS `.pkg` extraction logic
//! previously duplicated between the GitHub and URL tool providers:
//!
//! - **Atomic extraction**: archives unpack into a temporary sibling
//!   directory that atomically replaces the destination, so failed
//!   extractions never leave partial trees behind.
//! - **Single-root normalization**: archives whose content lives under a
//!   single top-level directory (`tool-1.2.3/...`) are flattened so the
//!   destination itself becomes the tool root.
//! - **Deterministic binary selection**: [`extract_binary`] returns the
//!   primary binary, preferring `bin/` entries whose name matches the
//!   destination directory (or its parent for versioned cache layouts).
//! - **macOS `.pkg` support**: payloads are expanded with `pkgutil`/`cpio`
//!   on macOS hosts; other hosts report a descriptive error.

#[cfg(any(target_os = "macos", test))]
mod entry_paths;
mod extraction;
#[cfg(target_os = "macos")]
mod pkg;

pub use extraction::{
    ArchiveError, ArchiveSource, Result, ensure_executable, extract_binary, extract_from_pkg,
    extract_from_tar_gz, extract_from_tar_xz, extract_from_zip, file_looks_like_library,
    find_main_binary, find_primary_binary_in_prefix, looks_like_prefix_install,
    path_looks_like_library,
};
