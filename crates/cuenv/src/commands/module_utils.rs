//! Module utilities for CUE evaluation and error conversion.

use cuenv_core::ModuleEvaluation;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;

/// Convert cuengine error to `cuenv_core` error
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
/// This wrapper around `MutexGuard` ensures the inner `Option` is always `Some`
/// by the time it's constructed, providing direct access to the module.
pub struct ModuleGuard<'a> {
    pub(super) guard: MutexGuard<'a, Option<ModuleEvaluation>>,
}

impl std::ops::Deref for ModuleGuard<'_> {
    type Target = ModuleEvaluation;

    fn deref(&self) -> &Self::Target {
        // SAFETY: ModuleGuard is only constructed after ensuring the Option is Some.
        // This unwrap_or_else with unreachable! documents the invariant while avoiding expect().
        self.guard.as_ref().unwrap_or_else(|| {
            unreachable!("ModuleGuard invariant violated: module should be loaded")
        })
    }
}
