//! CUE module cuenv version compatibility checks.

use cuenv_core::Result;
use cuenv_core::cue::discovery::find_cue_module_root;
use semver::Version;
use std::path::{Path, PathBuf};

use super::sync::SyncMode;

const CUENV_MODULE_NAMESPACE: &str = "github.com/cuenv/cuenv";

/// Result of checking or syncing the cuenv version marker in `module.cue`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleVersionSync {
    /// The marker is already current.
    InSync,
    /// The marker is missing and would be added.
    Missing {
        /// CLI version that would be written.
        version: String,
    },
    /// The marker exists but would be updated.
    Stale {
        /// Version currently recorded in `cue.mod/module.cue`.
        existing: String,
        /// CLI version that would be written.
        current: String,
    },
    /// The marker was written.
    Updated {
        /// CLI version that was written.
        version: String,
    },
}

impl ModuleVersionSync {
    /// Human-friendly status message.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::InSync => "module.cue cuenv version marker is in sync".to_string(),
            Self::Missing { version } => {
                format!("module.cue cuenv version marker missing; would set to {version}")
            }
            Self::Stale { existing, current } => {
                format!("module.cue cuenv version marker is {existing}; would update to {current}")
            }
            Self::Updated { version } => {
                format!("module.cue cuenv version marker updated to {version}")
            }
        }
    }
}

/// Return the current CLI version.
#[must_use]
pub fn cli_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Find the CUE module root from a command path.
///
/// Missing modules return `Ok(None)` so commands that support no-module mode can
/// continue without a compatibility check.
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

/// Ensure the module does not require a newer cuenv CLI.
///
/// # Errors
///
/// Returns an error if the command path cannot be inspected, if the module
/// marker is invalid, or if the module requires a newer cuenv CLI.
pub fn ensure_compatible_for_path(path: impl AsRef<Path>) -> Result<()> {
    if let Some(module_root) = module_root_for_path(path)? {
        ensure_compatible_module(&module_root)?;
    }
    Ok(())
}

/// Ensure the module root does not require a newer cuenv CLI.
///
/// # Errors
///
/// Returns an error if the module marker cannot be read, if the marker is
/// invalid, or if the module requires a newer cuenv CLI.
pub fn ensure_compatible_module(module_root: &Path) -> Result<()> {
    let Some(required) = read_module_version(module_root)? else {
        return Ok(());
    };

    let required_version = parse_version(&required)?;
    let current_version = parse_version(cli_version())?;
    if required_version > current_version {
        return Err(cuenv_core::Error::configuration(format!(
            "Project requires cuenv {required}; this CLI is {}. Upgrade cuenv to {required} or newer.",
            cli_version()
        )));
    }

    Ok(())
}

/// Sync the module's cuenv version marker according to the selected sync mode.
///
/// # Errors
///
/// Returns an error if no CUE module can be found from the path, if the module
/// marker is invalid, if the module requires a newer cuenv CLI, or if check or
/// write mode cannot complete.
pub fn sync_marker_for_path(path: impl AsRef<Path>, mode: &SyncMode) -> Result<ModuleVersionSync> {
    let module_root = module_root_for_path(path)?.ok_or_else(|| {
        cuenv_core::Error::configuration("No CUE module found (looking for cue.mod/)")
    })?;
    sync_marker_for_module(&module_root, mode)
}

/// Sync the module's cuenv version marker according to the selected sync mode.
///
/// # Errors
///
/// Returns an error if the module marker cannot be read or formatted, if the
/// marker is invalid, if the module requires a newer cuenv CLI, if `--check`
/// detects a missing or stale marker, or if writing `module.cue` fails.
pub fn sync_marker_for_module(module_root: &Path, mode: &SyncMode) -> Result<ModuleVersionSync> {
    let current = cli_version().to_string();
    let existing = read_module_version(module_root)?;

    if let Some(existing_version) = existing.as_deref() {
        let existing_semver = parse_version(existing_version)?;
        let current_semver = parse_version(&current)?;
        if existing_semver > current_semver {
            return Err(cuenv_core::Error::configuration(format!(
                "Project requires cuenv {existing_version}; this CLI is {current}. Upgrade cuenv to {existing_version} or newer."
            )));
        }
        if versions_equal(existing_version, &current)? {
            return Ok(ModuleVersionSync::InSync);
        }
    }

    match mode {
        SyncMode::Check => Err(cuenv_core::Error::configuration(
            marker_status(existing.as_deref(), &current).message(),
        )),
        SyncMode::DryRun => Ok(marker_status(existing.as_deref(), &current)),
        SyncMode::Write => {
            let formatted = cuengine::format_module_with_custom_version(
                module_root,
                CUENV_MODULE_NAMESPACE,
                &current,
            )
            .map_err(super::module_utils::convert_engine_error)?;
            let module_file = module_root.join("cue.mod").join("module.cue");
            std::fs::write(&module_file, formatted).map_err(|e| cuenv_core::Error::Io {
                source: e,
                path: Some(module_file.into_boxed_path()),
                operation: "write module.cue".to_string(),
            })?;
            Ok(ModuleVersionSync::Updated { version: current })
        }
    }
}

fn marker_status(existing: Option<&str>, current: &str) -> ModuleVersionSync {
    match existing {
        Some(existing) => ModuleVersionSync::Stale {
            existing: existing.to_string(),
            current: current.to_string(),
        },
        None => ModuleVersionSync::Missing {
            version: current.to_string(),
        },
    }
}

fn read_module_version(module_root: &Path) -> Result<Option<String>> {
    cuengine::module_custom_version(module_root, CUENV_MODULE_NAMESPACE)
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
            "Invalid cuenv version marker '{version}' in cue.mod/module.cue: {e}"
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
    fn marker_status_reports_missing_and_stale() {
        assert_eq!(
            marker_status(None, "1.2.3"),
            ModuleVersionSync::Missing {
                version: "1.2.3".to_string()
            }
        );
        assert_eq!(
            marker_status(Some("1.2.2"), "1.2.3"),
            ModuleVersionSync::Stale {
                existing: "1.2.2".to_string(),
                current: "1.2.3".to_string()
            }
        );
    }

    #[test]
    fn semver_precedence_detects_newer_required_version() {
        let required = parse_version("1.2.4").expect("required");
        let current = parse_version("1.2.3").expect("current");
        assert!(required > current);
    }
}
