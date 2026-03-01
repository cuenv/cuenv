//! Module utilities for CUE evaluation and error conversion.

use cuenv_core::ModuleEvaluation;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;

/// Convert cuengine error to `cuenv_core` error.
#[must_use]
pub fn convert_engine_error(err: cuengine::CueEngineError) -> cuenv_core::Error {
    match err {
        cuengine::CueEngineError::Configuration { message } => {
            cuenv_core::Error::configuration(message)
        }
        cuengine::CueEngineError::Ffi { function, message } => {
            cuenv_core::Error::ffi(function, message)
        }
        cuengine::CueEngineError::CueParse { path, message } => {
            cuenv_core::Error::cue_parse(&path, message)
        }
        cuengine::CueEngineError::Validation { message } => cuenv_core::Error::validation(message),
        cuengine::CueEngineError::Cache { message } => cuenv_core::Error::configuration(message),
    }
}

/// Compute the relative path from module root to target directory.
///
/// Returns the path suitable for looking up instances in `ModuleEvaluation`.
/// Returns `"."` for the module root itself.
#[must_use]
pub fn relative_path_from_root(module_root: &Path, target: &Path) -> PathBuf {
    target.strip_prefix(module_root).map_or_else(
        |_| PathBuf::from("."),
        |p| {
            if p.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        },
    )
}

/// A guard that provides access to the loaded `ModuleEvaluation`.
///
/// This wrapper around `MutexGuard` holds a `HashMap<PathBuf, ModuleEvaluation>`
/// and a lookup key. The keyed entry is guaranteed to exist in the map while
/// the guard is held.
pub struct ModuleGuard<'a> {
    pub(super) guard: MutexGuard<'a, HashMap<PathBuf, ModuleEvaluation>>,
    pub(super) key: PathBuf,
}

impl std::ops::Deref for ModuleGuard<'_> {
    type Target = ModuleEvaluation;

    fn deref(&self) -> &Self::Target {
        self.guard.get(&self.key).unwrap_or_else(|| {
            unreachable!("ModuleGuard invariant violated: module should be loaded")
        })
    }
}
