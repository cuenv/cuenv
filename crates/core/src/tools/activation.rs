//! Tool activation planning and environment mutation helpers.
//!
//! Tool activation is inferred from lockfile tool metadata so every execution
//! path (`tools activate`, `exec`, `task`, CI) applies the same environment
//! mutations.

use super::{Platform, default_cache_dir};
use crate::lockfile::Lockfile;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

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

#[derive(Debug, Default)]
struct ToolPathIndex {
    all_bin_dirs: Vec<PathBuf>,
    all_lib_dirs: Vec<PathBuf>,
    all_include_dirs: Vec<PathBuf>,
    all_pkgconfig_dirs: Vec<PathBuf>,
    tool_bin_dirs: BTreeMap<String, Vec<PathBuf>>,
    tool_lib_dirs: BTreeMap<String, Vec<PathBuf>>,
    file_env_exports: BTreeMap<String, PathBuf>,
}

impl ToolPathIndex {
    fn collect(options: &ToolActivationResolveOptions<'_>) -> Result<Self> {
        let mut index = Self::default();
        let mut all_bin_seen = HashSet::new();
        let mut all_lib_seen = HashSet::new();
        let mut all_include_seen = HashSet::new();
        let mut all_pkgconfig_seen = HashSet::new();
        let platform_key = options.platform.to_string();
        let lockfile_dir = options.lockfile_path.parent().unwrap_or(Path::new("."));
        let nix_profile_path = nix_profile_path_for_project(lockfile_dir)?;

        for (name, tool) in &options.lockfile.tools {
            let Some(platform_data) = tool.platforms.get(&platform_key) else {
                continue;
            };

            match platform_data.provider.as_str() {
                "nix" => {
                    add_existing_dir(
                        &mut index.all_bin_dirs,
                        &mut all_bin_seen,
                        nix_profile_path.join("bin"),
                    );
                    add_existing_dir(
                        &mut index.all_lib_dirs,
                        &mut all_lib_seen,
                        nix_profile_path.join("lib"),
                    );
                    add_existing_dir(
                        &mut index.all_include_dirs,
                        &mut all_include_seen,
                        nix_profile_path.join("include"),
                    );
                    add_existing_dir(
                        &mut index.all_pkgconfig_dirs,
                        &mut all_pkgconfig_seen,
                        nix_profile_path.join("lib").join("pkgconfig"),
                    );
                    add_tool_existing_dir(
                        &mut index.tool_bin_dirs,
                        name,
                        nix_profile_path.join("bin"),
                    );
                    add_tool_existing_dir(
                        &mut index.tool_lib_dirs,
                        name,
                        nix_profile_path.join("lib"),
                    );
                }
                "rustup" => {
                    let toolchain = platform_data
                        .source
                        .get("toolchain")
                        .and_then(|v| v.as_str())
                        .unwrap_or("stable");
                    let rustup_dir = rustup_toolchain_dir(toolchain, &options.platform);
                    add_existing_dir(
                        &mut index.all_bin_dirs,
                        &mut all_bin_seen,
                        rustup_dir.join("bin"),
                    );
                    add_existing_dir(
                        &mut index.all_lib_dirs,
                        &mut all_lib_seen,
                        rustup_dir.join("lib"),
                    );
                    add_existing_dir(
                        &mut index.all_include_dirs,
                        &mut all_include_seen,
                        rustup_dir.join("include"),
                    );
                    add_existing_dir(
                        &mut index.all_pkgconfig_dirs,
                        &mut all_pkgconfig_seen,
                        rustup_dir.join("lib").join("pkgconfig"),
                    );
                    add_tool_existing_dir(&mut index.tool_bin_dirs, name, rustup_dir.join("bin"));
                    add_tool_existing_dir(&mut index.tool_lib_dirs, name, rustup_dir.join("lib"));
                }
                "github" => {
                    let tool_dir = options
                        .cache_dir
                        .join("github")
                        .join(name)
                        .join(&tool.version);
                    let extract: Vec<super::ToolExtract> = platform_data
                        .source
                        .get("extract")
                        .cloned()
                        .and_then(|value| serde_json::from_value(value).ok())
                        .unwrap_or_default();

                    if extract.is_empty() {
                        // Legacy fallback: assume binary unless source path hints library.
                        let path_hint_is_lib = platform_data
                            .source
                            .get("path")
                            .and_then(|v| v.as_str())
                            .is_some_and(path_looks_like_library);
                        if path_hint_is_lib {
                            let lib_dir = tool_dir.join("lib");
                            add_existing_dir(
                                &mut index.all_lib_dirs,
                                &mut all_lib_seen,
                                lib_dir.clone(),
                            );
                            add_tool_existing_dir(&mut index.tool_lib_dirs, name, lib_dir);
                        } else {
                            let bin_dir = tool_dir.join("bin");
                            add_existing_dir(
                                &mut index.all_bin_dirs,
                                &mut all_bin_seen,
                                bin_dir.clone(),
                            );
                            add_tool_existing_dir(&mut index.tool_bin_dirs, name, bin_dir);
                        }
                        continue;
                    }

                    for item in extract {
                        match item {
                            super::ToolExtract::Bin { .. } => {
                                let bin_dir = tool_dir.join("bin");
                                add_existing_dir(
                                    &mut index.all_bin_dirs,
                                    &mut all_bin_seen,
                                    bin_dir.clone(),
                                );
                                add_tool_existing_dir(&mut index.tool_bin_dirs, name, bin_dir);
                            }
                            super::ToolExtract::Lib { path, env } => {
                                let lib_dir = tool_dir.join("lib");
                                add_existing_dir(
                                    &mut index.all_lib_dirs,
                                    &mut all_lib_seen,
                                    lib_dir.clone(),
                                );
                                add_tool_existing_dir(
                                    &mut index.tool_lib_dirs,
                                    name,
                                    lib_dir.clone(),
                                );
                                if let Some(var) = env {
                                    let file_name = Path::new(&path)
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or(path.as_str());
                                    let file_path = lib_dir.join(file_name);
                                    upsert_file_env_export(
                                        &mut index.file_env_exports,
                                        &var,
                                        file_path,
                                    )?;
                                }
                            }
                            super::ToolExtract::Include { .. } => {
                                add_existing_dir(
                                    &mut index.all_include_dirs,
                                    &mut all_include_seen,
                                    tool_dir.join("include"),
                                );
                            }
                            super::ToolExtract::PkgConfig { .. } => {
                                add_existing_dir(
                                    &mut index.all_pkgconfig_dirs,
                                    &mut all_pkgconfig_seen,
                                    tool_dir.join("lib").join("pkgconfig"),
                                );
                            }
                            super::ToolExtract::File { path, env } => {
                                if let Some(var) = env {
                                    let file_name = Path::new(&path)
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or(path.as_str());
                                    let file_path = tool_dir.join("files").join(file_name);
                                    upsert_file_env_export(
                                        &mut index.file_env_exports,
                                        &var,
                                        file_path,
                                    )?;
                                }
                            }
                        }
                    }
                }
                provider_name => {
                    let tool_dir = options
                        .cache_dir
                        .join(provider_name)
                        .join(name)
                        .join(&tool.version);
                    let bin_dir = tool_dir.join("bin");
                    let lib_dir = tool_dir.join("lib");
                    let include_dir = tool_dir.join("include");
                    let pkgconfig_dir = tool_dir.join("lib").join("pkgconfig");
                    add_existing_dir(&mut index.all_bin_dirs, &mut all_bin_seen, bin_dir.clone());
                    add_existing_dir(&mut index.all_lib_dirs, &mut all_lib_seen, lib_dir.clone());
                    add_existing_dir(
                        &mut index.all_include_dirs,
                        &mut all_include_seen,
                        include_dir.clone(),
                    );
                    add_existing_dir(
                        &mut index.all_pkgconfig_dirs,
                        &mut all_pkgconfig_seen,
                        pkgconfig_dir,
                    );
                    add_tool_existing_dir(&mut index.tool_bin_dirs, name, bin_dir);
                    add_tool_existing_dir(&mut index.tool_lib_dirs, name, lib_dir);

                    // Some providers store binaries directly in the tool root.
                    if tool_dir.join(name).exists() || tool_dir.join(format!("{name}.exe")).exists()
                    {
                        add_existing_dir(
                            &mut index.all_bin_dirs,
                            &mut all_bin_seen,
                            tool_dir.clone(),
                        );
                        add_tool_existing_dir(&mut index.tool_bin_dirs, name, tool_dir);
                    }
                }
            }
        }

        Ok(index)
    }
}

fn add_existing_dir(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, dir: PathBuf) {
    if !dir.exists() {
        return;
    }
    if seen.insert(dir.clone()) {
        paths.push(dir);
    }
}

fn add_tool_existing_dir(map: &mut BTreeMap<String, Vec<PathBuf>>, tool: &str, dir: PathBuf) {
    if !dir.exists() {
        return;
    }
    let dirs = map.entry(tool.to_string()).or_default();
    if !dirs.contains(&dir) {
        dirs.push(dir);
    }
}

fn path_looks_like_library(path: &str) -> bool {
    let ext_is = |target: &str| {
        Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case(target))
    };
    ext_is("dylib") || ext_is("so") || path.to_ascii_lowercase().contains(".so.") || ext_is("dll")
}

fn upsert_file_env_export(
    exports: &mut BTreeMap<String, PathBuf>,
    var: &str,
    path: PathBuf,
) -> Result<()> {
    match exports.get(var) {
        Some(existing) if existing != &path => Err(Error::configuration(format!(
            "Conflicting file env export for '{}': '{}' vs '{}'",
            var,
            existing.display(),
            path.display()
        ))),
        Some(_) => Ok(()),
        None => {
            exports.insert(var.to_string(), path);
            Ok(())
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

fn rustup_toolchain_dir(toolchain: &str, platform: &Platform) -> PathBuf {
    let rustup_home = std::env::var("RUSTUP_HOME").map_or_else(
        |_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".rustup")
        },
        PathBuf::from,
    );
    rustup_home
        .join("toolchains")
        .join(format!("{toolchain}-{}", rustup_host_triple(platform)))
}

fn rustup_host_triple(platform: &Platform) -> String {
    let arch = match platform.arch {
        super::Arch::Arm64 => "aarch64",
        super::Arch::X86_64 => "x86_64",
    };

    let os = match platform.os {
        super::Os::Darwin => "apple-darwin",
        super::Os::Linux => "unknown-linux-gnu",
    };

    format!("{arch}-{os}")
}

fn nix_profile_path_for_project(project_root: &Path) -> Result<PathBuf> {
    let cache = crate::paths::cache_dir()?;
    let project_id = project_profile_id(project_root);
    Ok(cache.join("nix-profiles").join(project_id))
}

fn project_profile_id(project_root: &Path) -> String {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lockfile::{LockedTool, LockedToolPlatform, Lockfile};
    use std::collections::BTreeMap;

    #[test]
    fn test_validate_missing_activation_is_allowed() {
        let mut lockfile = Lockfile::new();
        lockfile.tools.insert(
            "jq".to_string(),
            LockedTool {
                version: "1.7.1".to_string(),
                platforms: BTreeMap::from([(
                    "darwin-arm64".to_string(),
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
        let mut lockfile = Lockfile::new();
        lockfile.tools.insert(
            "rust".to_string(),
            LockedTool {
                version: "1.0.0".to_string(),
                platforms: BTreeMap::from([(
                    "darwin-arm64".to_string(),
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
