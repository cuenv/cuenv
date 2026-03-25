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
///
/// Because the isolated tempdir has no access to the parent module's schema
/// packages, we strip `import` blocks and schema type constraint references
/// (e.g. `rules.#DirectoryRules &`) so the file evaluates as plain CUE data.
/// A minimal `cue.mod/module.cue` is also created since the Go bridge
/// requires a module root.
pub fn evaluate_rules_file(file_path: &Path) -> Result<DirectoryRules> {
    let original = std::fs::read_to_string(file_path).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(file_path.to_path_buf().into_boxed_path()),
        operation: "read .rules.cue".to_string(),
    })?;

    let patched = prepare_isolated_cue(&original);

    let tempdir = tempfile::tempdir().map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: None,
        operation: "create tempdir for .rules.cue eval".to_string(),
    })?;

    // Create minimal cue.mod/module.cue required by the Go bridge
    let cue_mod_dir = tempdir.path().join("cue.mod");
    std::fs::create_dir_all(&cue_mod_dir).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(cue_mod_dir.clone().into_boxed_path()),
        operation: "create cue.mod dir".to_string(),
    })?;
    std::fs::write(
        cue_mod_dir.join("module.cue"),
        "module: \"cuenv.dev/rules-eval\"\nlanguage: version: \"v0.12.0\"\n",
    )
    .map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(cue_mod_dir.join("module.cue").into_boxed_path()),
        operation: "write cue.mod/module.cue".to_string(),
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

/// Transform `.rules.cue` content for isolated evaluation.
///
/// Strips the package declaration (replaced with `package rules`), import
/// blocks, and schema type constraint references so the file can evaluate
/// without its parent module's dependencies.
fn prepare_isolated_cue(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    result.push_str("package rules\n");

    let mut in_import_block = false;
    let mut skip_package = true;

    for line in source.lines() {
        let trimmed = line.trim();

        // Skip the original package declaration (first occurrence)
        if skip_package && trimmed.starts_with("package ") {
            skip_package = false;
            continue;
        }

        // Skip single-line imports: `import "..."`
        if trimmed.starts_with("import \"") {
            continue;
        }

        // Skip multi-line import blocks: `import ( ... )`
        if trimmed.starts_with("import (") {
            in_import_block = true;
            continue;
        }
        if in_import_block {
            if trimmed == ")" {
                in_import_block = false;
            }
            continue;
        }

        // Strip schema type constraint references on their own line
        // e.g. `rules.#DirectoryRules &` or `rules.#DirectoryRules`
        if trimmed.starts_with("rules.#") {
            // If the line is just the constraint (with optional trailing &),
            // skip it entirely
            let without_prefix = trimmed.trim_start_matches(|c: char| c.is_alphanumeric() || c == '.' || c == '#');
            let remainder = without_prefix.trim();
            if remainder.is_empty() || remainder == "&" {
                continue;
            }
        }

        // For inline constraint like `rules.#DirectoryRules & {`, strip the
        // constraint prefix, keeping only `{`
        if trimmed.starts_with("rules.#")
            && let Some(pos) = trimmed.find('&')
        {
            let after_amp = trimmed[pos + 1..].trim();
            if !after_amp.is_empty() {
                result.push_str(after_amp);
                result.push('\n');
                continue;
            }
        }

        result.push_str(line);
        result.push('\n');
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_import_and_constraint() {
        let input = r#"package cuenv

import "github.com/cuenv/cuenv/schema/rules"

rules.#DirectoryRules & {
    ignore: {
        git: [".cache"]
    }
}
"#;
        let result = prepare_isolated_cue(input);
        assert!(!result.contains("import"));
        assert!(!result.contains("rules.#DirectoryRules"));
        assert!(result.starts_with("package rules\n"));
        assert!(result.contains("ignore:"));
        assert!(result.contains('{'));
    }

    #[test]
    fn strips_standalone_constraint() {
        let input = r#"package examples

import "github.com/cuenv/cuenv/schema/rules"

rules.#DirectoryRules

ignore: {
    git: ["node_modules/"]
}
"#;
        let result = prepare_isolated_cue(input);
        assert!(!result.contains("import"));
        assert!(!result.contains("rules.#DirectoryRules"));
        assert!(result.contains("ignore:"));
    }

    #[test]
    fn handles_multi_line_import_block() {
        let input = r#"package test

import (
    "github.com/cuenv/cuenv/schema/rules"
    "strings"
)

ignore: {
    git: ["target/"]
}
"#;
        let result = prepare_isolated_cue(input);
        assert!(!result.contains("import"));
        assert!(!result.contains("strings"));
        assert!(result.contains("ignore:"));
    }
}
