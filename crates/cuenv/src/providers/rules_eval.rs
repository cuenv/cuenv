//! Shared utility for evaluating `.rules.cue` files in isolation.
//!
//! CUE ignores dotfiles during package loading, so `.rules.cue` cannot be
//! evaluated directly within its parent directory. This module copies the
//! content into a temporary directory as `package rules` and evaluates it
//! there, avoiding unification with `env.cue` in the same folder.

use std::path::Path;

use cuenv_core::Result;
use cuenv_core::manifest::DirectoryRules;

/// Evaluate a `.rules.cue` file and return the parsed configuration.
///
/// The file is copied into a temporary directory with its package declaration
/// rewritten to `package rules`, then evaluated in isolation via the CUE
/// engine.
pub fn evaluate_rules_file(file_path: &Path) -> Result<DirectoryRules> {
    let original = std::fs::read_to_string(file_path).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(file_path.to_path_buf().into_boxed_path()),
        operation: "read .rules.cue".to_string(),
    })?;

    // Rewrite the package declaration so CUE evaluates it as an independent
    // `rules` package rather than merging with the surrounding directory.
    let mut patched = String::new();
    for (i, line) in original.lines().enumerate() {
        if i == 0 && line.trim_start().starts_with("package ") {
            patched.push_str("package rules\n");
        } else {
            patched.push_str(line);
            patched.push('\n');
        }
    }
    if !patched.starts_with("package ") {
        patched = format!("package rules\n{}", original);
    }

    let tempdir = tempfile::tempdir().map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: None,
        operation: "create tempdir for .rules.cue eval".to_string(),
    })?;
    let temp_path = tempdir.path().join("rules_eval.cue");
    std::fs::write(&temp_path, patched).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(temp_path.clone().into_boxed_path()),
        operation: "write temp rules file".to_string(),
    })?;

    cuengine::evaluate_cue_package_typed(tempdir.path(), "rules").map_err(|e| {
        cuenv_core::Error::configuration(format!(
            "Failed to parse .rules.cue [isolated-eval] at {}: {}",
            file_path.display(),
            e
        ))
    })
}
