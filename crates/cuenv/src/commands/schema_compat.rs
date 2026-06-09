//! CUE schema dependency compatibility warnings.

use cuenv_core::Result;
use cuenv_core::cue::discovery::find_cue_module_root;
use semver::Version;
use std::path::{Path, PathBuf};

const CUENV_SCHEMA_MODULE: &str = "github.com/cuenv/cuenv";

/// Return the current CLI version.
#[must_use]
pub fn cli_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Find the CUE module root from a command path.
///
/// Missing modules return `Ok(None)` so commands that support no-module mode can
/// continue without a schema dependency check.
///
/// # Errors
///
/// Returns an error if the command path cannot be canonicalized.
pub fn module_root_for_path(path: impl AsRef<Path>) -> Result<Option<PathBuf>> {
    let path = path.as_ref();
    let target_path = path.canonicalize().map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(path.to_path_buf().into_boxed_path()),
        operation: "canonicalize path".to_string(),
    })?;

    Ok(find_cue_module_root(&target_path))
}

/// Warn when the module's cuenv schema dependency differs from the running CLI.
///
/// Missing dependencies are accepted because the module might be this repository
/// itself, an older local checkout, or a module that vendors schema locally.
///
/// # Errors
///
/// Returns an error if the command path cannot be inspected or the dependency
/// metadata cannot be read.
pub fn warn_for_path(path: impl AsRef<Path>) -> Result<()> {
    if let Some(module_root) = module_root_for_path(path)? {
        warn_for_module(&module_root)?;
    }
    Ok(())
}

/// Warn when the module root's cuenv schema dependency differs from the CLI.
///
/// # Errors
///
/// Returns an error if the dependency metadata cannot be read or has an invalid
/// semantic version.
pub fn warn_for_module(module_root: &Path) -> Result<()> {
    let Some(schema_version) = read_schema_dependency_version(module_root)? else {
        return Ok(());
    };

    if versions_equal(&schema_version, cli_version())? {
        return Ok(());
    }

    let warning = mismatch_warning(&schema_version, cli_version());
    tracing::warn!(
        schema_version,
        cli_version = cli_version(),
        "cuenv schema dependency and CLI version differ"
    );
    cuenv_events::eprintln_redacted(&warning);
    Ok(())
}

fn mismatch_warning(schema_version: &str, cli_version: &str) -> String {
    format!(
        "Warning: cuenv schema dependency is {schema_version}, but the CLI is {cli_version}. Use matching cuenv schema and CLI versions to avoid incompatible behavior."
    )
}

fn read_schema_dependency_version(module_root: &Path) -> Result<Option<String>> {
    cuengine::module_dependency_version(module_root, CUENV_SCHEMA_MODULE)
        .map(|metadata| metadata.version)
        .map_err(super::module_utils::convert_engine_error)
}

fn versions_equal(left: &str, right: &str) -> Result<bool> {
    Ok(parse_version(left)? == parse_version(right)?)
}

fn parse_version(version: &str) -> Result<Version> {
    let normalized = version.trim().trim_start_matches('v');
    Version::parse(normalized).map_err(|e| {
        cuenv_core::Error::configuration(format!(
            "Invalid cuenv schema dependency version '{version}' in cue.mod/module.cue: {e}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_accepts_optional_v_prefix() {
        assert_eq!(
            parse_version("v1.2.3").expect("version"),
            parse_version("1.2.3").expect("version")
        );
    }

    #[test]
    fn versions_equal_compares_semver() {
        assert!(versions_equal("v1.2.3", "1.2.3").expect("versions"));
        assert!(!versions_equal("1.2.2", "1.2.3").expect("versions"));
    }

    #[test]
    fn mismatch_warning_names_schema_and_cli_versions() {
        let warning = mismatch_warning("v1.2.2", "1.2.3");
        assert!(warning.contains("schema dependency is v1.2.2"));
        assert!(warning.contains("CLI is 1.2.3"));
    }
}
