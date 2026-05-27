//! Tool activation planning and environment mutation helpers.
//!
//! Tool activation is inferred from lockfile tool metadata so every execution
//! path (`tools activate`, `exec`, `task`, CI) applies the same environment
//! mutations.

use super::{Platform, default_cache_dir};
use crate::lockfile::Lockfile;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

mod path_index;

use path_index::ToolPathIndex;

/// A configured activation step from runtime/lockfile configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolActivationStep {
    /// Environment variable to mutate (for example `PATH`).
    pub var: String,
    /// Mutation operation.
    pub op: ToolActivationOperation,
    /// Separator for joining values (defaults to `:`).
    #[serde(default = "default_separator")]
    pub separator: String,
    /// Source reference that resolves to one or more paths.
    pub from: ToolActivationSource,
}

fn default_separator() -> String {
    ":".to_string()
}

/// Mutation operation for tool activation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ToolActivationOperation {
    /// Replace the variable with the resolved value.
    Set,
    /// Prepend the resolved value before the current value.
    Prepend,
    /// Append the resolved value after the current value.
    Append,
}

/// Activation source selector.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolActivationSource {
    /// All bin directories for tools available on the current platform.
    AllBinDirs,
    /// All lib directories for tools available on the current platform.
    AllLibDirs,
    /// Bin directory for a specific tool.
    ToolBinDir { tool: String },
    /// Lib directory for a specific tool.
    ToolLibDir { tool: String },
}

/// A resolved activation step with a concrete value to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedToolActivationStep {
    /// Environment variable to mutate.
    pub var: String,
    /// Mutation operation.
    pub op: ToolActivationOperation,
    /// Separator for joining values.
    pub separator: String,
    /// Resolved value (already joined using `separator`).
    pub value: String,
}

/// Options for resolving tool activation.
#[derive(Debug, Clone)]
pub struct ToolActivationResolveOptions<'a> {
    /// Lockfile containing tools and activation config.
    pub lockfile: &'a Lockfile,
    /// Absolute path to the loaded lockfile.
    pub lockfile_path: &'a Path,
    /// Target platform to resolve against.
    pub platform: Platform,
    /// Cache directory for provider-backed tools.
    pub cache_dir: PathBuf,
}

impl<'a> ToolActivationResolveOptions<'a> {
    /// Create options using current platform and default cache directory.
    #[must_use]
    pub fn new(lockfile: &'a Lockfile, lockfile_path: &'a Path) -> Self {
        Self {
            lockfile,
            lockfile_path,
            platform: Platform::current(),
            cache_dir: default_cache_dir(),
        }
    }

    /// Override platform.
    #[must_use]
    pub fn with_platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    /// Override cache directory.
    #[must_use]
    pub fn with_cache_dir(mut self, cache_dir: PathBuf) -> Self {
        self.cache_dir = cache_dir;
        self
    }
}

/// Validate lockfile activation configuration for the selected platform.
///
/// # Errors
///
/// Returns an error when:
/// - an explicit activation step has an empty variable name
/// - a per-tool reference targets an unknown tool
/// - a per-tool reference targets a Nix tool on this platform (unsupported in v1)
pub fn validate_tool_activation(options: &ToolActivationResolveOptions<'_>) -> Result<()> {
    if options.lockfile.tools.is_empty() {
        return Ok(());
    }

    // Auto-inferred mode: no explicit activation config required.
    if options.lockfile.tools_activation.is_empty() {
        return Ok(());
    }

    let platform_key = options.platform.to_string();

    for step in &options.lockfile.tools_activation {
        if step.var.trim().is_empty() {
            return Err(Error::configuration(
                "Tool activation entry has an empty `var` value.",
            ));
        }

        match &step.from {
            ToolActivationSource::ToolBinDir { tool }
            | ToolActivationSource::ToolLibDir { tool } => {
                let Some(locked_tool) = options.lockfile.tools.get(tool) else {
                    return Err(Error::configuration(format!(
                        "Tool activation references unknown tool '{}'.",
                        tool
                    )));
                };

                if let Some(platform_data) = locked_tool.platforms.get(&platform_key)
                    && platform_data.provider == "nix"
                {
                    return Err(Error::configuration(format!(
                        "Tool activation per-tool references do not support Nix tools yet ('{}'). \
                         Use allBinDirs/allLibDirs instead.",
                        tool
                    )));
                }
            }
            ToolActivationSource::AllBinDirs | ToolActivationSource::AllLibDirs => {}
        }
    }

    Ok(())
}

/// Resolve configured tool activation steps to concrete values for the selected platform.
///
/// # Errors
///
/// Returns validation errors from [`validate_tool_activation`].
pub fn resolve_tool_activation(
    options: &ToolActivationResolveOptions<'_>,
) -> Result<Vec<ResolvedToolActivationStep>> {
    let path_index = ToolPathIndex::collect(options)?;
    if options.lockfile.tools_activation.is_empty() {
        let mut resolved = vec![
            ResolvedToolActivationStep {
                var: "PATH".to_string(),
                op: ToolActivationOperation::Prepend,
                separator: ":".to_string(),
                value: join_paths(&path_index.all_bin_dirs, ":"),
            },
            ResolvedToolActivationStep {
                var: "DYLD_LIBRARY_PATH".to_string(),
                op: ToolActivationOperation::Prepend,
                separator: ":".to_string(),
                value: join_paths(&path_index.all_lib_dirs, ":"),
            },
            ResolvedToolActivationStep {
                var: "LD_LIBRARY_PATH".to_string(),
                op: ToolActivationOperation::Prepend,
                separator: ":".to_string(),
                value: join_paths(&path_index.all_lib_dirs, ":"),
            },
            ResolvedToolActivationStep {
                var: "CPATH".to_string(),
                op: ToolActivationOperation::Prepend,
                separator: ":".to_string(),
                value: join_paths(&path_index.all_include_dirs, ":"),
            },
            ResolvedToolActivationStep {
                var: "PKG_CONFIG_PATH".to_string(),
                op: ToolActivationOperation::Prepend,
                separator: ":".to_string(),
                value: join_paths(&path_index.all_pkgconfig_dirs, ":"),
            },
        ];
        for (var, value) in &path_index.file_env_exports {
            resolved.push(ResolvedToolActivationStep {
                var: var.clone(),
                op: ToolActivationOperation::Set,
                separator: ":".to_string(),
                value: value.to_string_lossy().to_string(),
            });
        }
        return Ok(resolved);
    }

    validate_tool_activation(options)?;
    let mut resolved = Vec::with_capacity(options.lockfile.tools_activation.len());

    for step in &options.lockfile.tools_activation {
        let paths = match &step.from {
            ToolActivationSource::AllBinDirs => path_index.all_bin_dirs.clone(),
            ToolActivationSource::AllLibDirs => path_index.all_lib_dirs.clone(),
            ToolActivationSource::ToolBinDir { tool } => path_index
                .tool_bin_dirs
                .get(tool)
                .cloned()
                .unwrap_or_default(),
            ToolActivationSource::ToolLibDir { tool } => path_index
                .tool_lib_dirs
                .get(tool)
                .cloned()
                .unwrap_or_default(),
        };

        resolved.push(ResolvedToolActivationStep {
            var: step.var.clone(),
            op: step.op.clone(),
            separator: step.separator.clone(),
            value: join_paths(&paths, &step.separator),
        });
    }

    Ok(resolved)
}

/// Apply a resolved activation step against the current value.
///
/// Returns `None` for no-op mutations (prepend/append with empty resolved value).
#[must_use]
pub fn apply_resolved_tool_activation(
    current: Option<&str>,
    step: &ResolvedToolActivationStep,
) -> Option<String> {
    match step.op {
        ToolActivationOperation::Set => Some(step.value.clone()),
        ToolActivationOperation::Prepend => {
            if step.value.is_empty() {
                return None;
            }
            match current {
                Some(existing) if !existing.is_empty() => {
                    Some(format!("{}{}{}", step.value, step.separator, existing))
                }
                _ => Some(step.value.clone()),
            }
        }
        ToolActivationOperation::Append => {
            if step.value.is_empty() {
                return None;
            }
            match current {
                Some(existing) if !existing.is_empty() => {
                    Some(format!("{}{}{}", existing, step.separator, step.value))
                }
                _ => Some(step.value.clone()),
            }
        }
    }
}

fn join_paths(paths: &[PathBuf], separator: &str) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(separator)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lockfile::{LockedTool, LockedToolPlatform, Lockfile};
    use std::collections::BTreeMap;
    use std::fs;

    fn current_platform_key() -> String {
        Platform::current().to_string()
    }

    #[test]
    fn test_validate_missing_activation_is_allowed() {
        let platform_key = current_platform_key();
        let mut lockfile = Lockfile::new();
        lockfile.tools.insert(
            "jq".to_string(),
            LockedTool {
                version: "1.7.1".to_string(),
                platforms: BTreeMap::from([(
                    platform_key,
                    LockedToolPlatform {
                        provider: "github".to_string(),
                        digest: "sha256:abc".to_string(),
                        source: serde_json::json!({
                            "type": "github",
                            "repo": "jqlang/jq",
                            "tag": "jq-1.7.1",
                            "asset": "jq-macos-arm64",
                        }),
                        size: None,
                        dependencies: vec![],
                    },
                )]),
            },
        );

        let temp = tempfile::tempdir().unwrap();
        let lockfile_path = temp.path().join("cuenv.lock");
        let options = ToolActivationResolveOptions::new(&lockfile, &lockfile_path);
        let result = validate_tool_activation(&options);

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_rejects_per_tool_nix_reference() {
        let platform_key = current_platform_key();
        let mut lockfile = Lockfile::new();
        lockfile.tools.insert(
            "rust".to_string(),
            LockedTool {
                version: "1.0.0".to_string(),
                platforms: BTreeMap::from([(
                    platform_key,
                    LockedToolPlatform {
                        provider: "nix".to_string(),
                        digest: "sha256:def".to_string(),
                        source: serde_json::json!({
                            "type": "nix",
                            "flake": "nixpkgs",
                            "package": "rustc",
                        }),
                        size: None,
                        dependencies: vec![],
                    },
                )]),
            },
        );
        lockfile.tools_activation = vec![ToolActivationStep {
            var: "PATH".to_string(),
            op: ToolActivationOperation::Prepend,
            separator: ":".to_string(),
            from: ToolActivationSource::ToolBinDir {
                tool: "rust".to_string(),
            },
        }];

        let temp = tempfile::tempdir().unwrap();
        let lockfile_path = temp.path().join("cuenv.lock");
        let options = ToolActivationResolveOptions::new(&lockfile, &lockfile_path);
        let result = validate_tool_activation(&options);

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("do not support Nix tools")
        );
    }

    #[test]
    fn test_collect_with_failing_nix_profile_lookup_still_collects_non_nix_tools() {
        let platform_key = current_platform_key();
        let mut lockfile = Lockfile::new();
        lockfile.tools.insert(
            "jq".to_string(),
            LockedTool {
                version: "1.7.1".to_string(),
                platforms: BTreeMap::from([(
                    platform_key,
                    LockedToolPlatform {
                        provider: "github".to_string(),
                        digest: "sha256:abc".to_string(),
                        source: serde_json::json!({
                            "type": "github",
                            "repo": "jqlang/jq",
                            "tag": "jq-1.7.1",
                            "asset": "jq",
                        }),
                        size: None,
                        dependencies: vec![],
                    },
                )]),
            },
        );

        let temp = tempfile::tempdir().unwrap();
        let lockfile_path = temp.path().join("cuenv.lock");
        let cache_dir = temp.path().join("cache");
        let bin_dir = cache_dir
            .join("github")
            .join("jq")
            .join("1.7.1")
            .join("bin");
        fs::create_dir_all(&bin_dir).unwrap();

        let options =
            ToolActivationResolveOptions::new(&lockfile, &lockfile_path).with_cache_dir(cache_dir);
        let index = ToolPathIndex::collect_with(&options, |_| {
            Err(Error::configuration("Could not determine cache directory"))
        })
        .unwrap();

        assert_eq!(index.all_bin_dirs, vec![bin_dir.clone()]);
        assert_eq!(index.tool_bin_dirs.get("jq"), Some(&vec![bin_dir]));
    }

    #[test]
    fn test_collect_with_failing_nix_profile_lookup_skips_nix_tools() {
        let platform_key = current_platform_key();
        let mut lockfile = Lockfile::new();
        lockfile.tools.insert(
            "jq".to_string(),
            LockedTool {
                version: "1.7.1".to_string(),
                platforms: BTreeMap::from([(
                    platform_key.clone(),
                    LockedToolPlatform {
                        provider: "github".to_string(),
                        digest: "sha256:abc".to_string(),
                        source: serde_json::json!({
                            "type": "github",
                            "repo": "jqlang/jq",
                            "tag": "jq-1.7.1",
                            "asset": "jq",
                        }),
                        size: None,
                        dependencies: vec![],
                    },
                )]),
            },
        );
        lockfile.tools.insert(
            "rust".to_string(),
            LockedTool {
                version: "1.85.0".to_string(),
                platforms: BTreeMap::from([(
                    platform_key,
                    LockedToolPlatform {
                        provider: "nix".to_string(),
                        digest: "sha256:def".to_string(),
                        source: serde_json::json!({
                            "type": "nix",
                            "flake": "nixpkgs",
                            "package": "rustc",
                        }),
                        size: None,
                        dependencies: vec![],
                    },
                )]),
            },
        );

        let temp = tempfile::tempdir().unwrap();
        let lockfile_path = temp.path().join("cuenv.lock");
        let cache_dir = temp.path().join("cache");
        let bin_dir = cache_dir
            .join("github")
            .join("jq")
            .join("1.7.1")
            .join("bin");
        fs::create_dir_all(&bin_dir).unwrap();

        let options =
            ToolActivationResolveOptions::new(&lockfile, &lockfile_path).with_cache_dir(cache_dir);
        let index = ToolPathIndex::collect_with(&options, |_| {
            Err(Error::configuration("Could not determine cache directory"))
        })
        .unwrap();

        assert_eq!(index.all_bin_dirs, vec![bin_dir.clone()]);
        assert_eq!(index.tool_bin_dirs.get("jq"), Some(&vec![bin_dir]));
        assert!(!index.tool_bin_dirs.contains_key("rust"));
        assert!(index.all_lib_dirs.is_empty());
    }

    #[test]
    fn test_resolve_tool_activation_includes_file_env_exports() {
        let platform = Platform::current();
        let platform_key = platform.to_string();
        let mut lockfile = Lockfile::new();
        lockfile.tools.insert(
            "foundationdb".to_string(),
            LockedTool {
                version: "7.3.63".to_string(),
                platforms: BTreeMap::from([(
                    platform_key,
                    LockedToolPlatform {
                        provider: "github".to_string(),
                        digest: "sha256:abc".to_string(),
                        source: serde_json::json!({
                            "type": "github",
                            "repo": "apple/foundationdb",
                            "tag": "7.3.63",
                            "asset": "FoundationDB.pkg",
                            "extract": [
                                {"kind": "bin", "path": "bin/fdbcli"},
                                {"kind": "lib", "path": "lib/libfdb_c.dylib", "env": "FDB_CLIENT_LIB"},
                                {"kind": "file", "path": "etc/fdb.cluster", "env": "FDB_CLUSTER_FILE"},
                                {"kind": "include", "path": "include/foundationdb/fdb_c.h"},
                                {"kind": "pkgconfig", "path": "lib/pkgconfig/foundationdb.pc"}
                            ],
                        }),
                        size: None,
                        dependencies: vec![],
                    },
                )]),
            },
        );

        let temp = tempfile::tempdir().unwrap();
        let lockfile_path = temp.path().join("cuenv.lock");
        let cache_dir = temp.path().join("cache");
        let tool_dir = cache_dir.join("github").join("foundationdb").join("7.3.63");
        let bin_dir = tool_dir.join("bin");
        let lib_dir = tool_dir.join("lib");
        let files_dir = tool_dir.join("files");
        let include_dir = tool_dir.join("include");
        let pkgconfig_dir = lib_dir.join("pkgconfig");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::create_dir_all(&lib_dir).unwrap();
        fs::create_dir_all(&files_dir).unwrap();
        fs::create_dir_all(&include_dir).unwrap();
        fs::create_dir_all(&pkgconfig_dir).unwrap();
        fs::write(bin_dir.join("fdbcli"), "").unwrap();
        fs::write(lib_dir.join("libfdb_c.dylib"), "").unwrap();
        fs::write(files_dir.join("fdb.cluster"), "").unwrap();
        fs::write(include_dir.join("fdb_c.h"), "").unwrap();
        fs::write(pkgconfig_dir.join("foundationdb.pc"), "").unwrap();

        let options = ToolActivationResolveOptions::new(&lockfile, &lockfile_path)
            .with_platform(platform)
            .with_cache_dir(cache_dir);
        let steps = resolve_tool_activation(&options).unwrap();

        assert!(
            steps
                .iter()
                .any(|step| step.var == "PATH" && step.value == bin_dir.to_string_lossy())
        );
        assert!(steps.iter().any(|step| {
            step.var == "FDB_CLIENT_LIB"
                && step.op == ToolActivationOperation::Set
                && step.value == lib_dir.join("libfdb_c.dylib").to_string_lossy()
        }));
        assert!(steps.iter().any(|step| {
            step.var == "FDB_CLUSTER_FILE"
                && step.op == ToolActivationOperation::Set
                && step.value == files_dir.join("fdb.cluster").to_string_lossy()
        }));
        assert!(
            steps
                .iter()
                .any(|step| step.var == "CPATH" && step.value == include_dir.to_string_lossy())
        );
        assert!(steps.iter().any(|step| {
            step.var == "PKG_CONFIG_PATH" && step.value == pkgconfig_dir.to_string_lossy()
        }));
    }

    #[test]
    fn test_apply_activation_operations() {
        let set_step = ResolvedToolActivationStep {
            var: "PATH".to_string(),
            op: ToolActivationOperation::Set,
            separator: ":".to_string(),
            value: "/a:/b".to_string(),
        };
        let prepend_step = ResolvedToolActivationStep {
            var: "PATH".to_string(),
            op: ToolActivationOperation::Prepend,
            separator: ":".to_string(),
            value: "/tools".to_string(),
        };
        let append_step = ResolvedToolActivationStep {
            var: "PATH".to_string(),
            op: ToolActivationOperation::Append,
            separator: ":".to_string(),
            value: "/tail".to_string(),
        };

        let set_value = apply_resolved_tool_activation(None, &set_step).unwrap();
        assert_eq!(set_value, "/a:/b");

        let prepend_value =
            apply_resolved_tool_activation(Some("/usr/bin"), &prepend_step).unwrap();
        assert_eq!(prepend_value, "/tools:/usr/bin");

        let append_value = apply_resolved_tool_activation(Some("/usr/bin"), &append_step).unwrap();
        assert_eq!(append_value, "/usr/bin:/tail");
    }
}
