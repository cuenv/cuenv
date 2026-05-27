use super::ToolActivationResolveOptions;
use crate::tools::{Arch, Os, Platform, ToolExtract};
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub(super) struct ToolPathIndex {
    pub(super) all_bin_dirs: Vec<PathBuf>,
    pub(super) all_lib_dirs: Vec<PathBuf>,
    pub(super) all_include_dirs: Vec<PathBuf>,
    pub(super) all_pkgconfig_dirs: Vec<PathBuf>,
    pub(super) tool_bin_dirs: BTreeMap<String, Vec<PathBuf>>,
    pub(super) tool_lib_dirs: BTreeMap<String, Vec<PathBuf>>,
    pub(super) file_env_exports: BTreeMap<String, PathBuf>,
}

impl ToolPathIndex {
    pub(super) fn collect(options: &ToolActivationResolveOptions<'_>) -> Result<Self> {
        Self::collect_with(options, nix_profile_path_for_project)
    }

    pub(super) fn collect_with<F>(
        options: &ToolActivationResolveOptions<'_>,
        nix_profile_path_for_project: F,
    ) -> Result<Self>
    where
        F: Fn(&Path) -> Result<PathBuf>,
    {
        let mut index = Self::default();
        let mut all_bin_seen = HashSet::new();
        let mut all_lib_seen = HashSet::new();
        let mut all_include_seen = HashSet::new();
        let mut all_pkgconfig_seen = HashSet::new();
        let platform_key = options.platform.to_string();
        let lockfile_dir = options.lockfile_path.parent().unwrap_or(Path::new("."));
        let mut nix_profile_path: Option<Option<PathBuf>> = None;

        for (name, tool) in &options.lockfile.tools {
            let Some(platform_data) = tool.platforms.get(&platform_key) else {
                continue;
            };

            match platform_data.provider.as_str() {
                "nix" => {
                    let profile_path = nix_profile_path
                        .get_or_insert_with(|| nix_profile_path_for_project(lockfile_dir).ok());
                    let Some(profile_path) = profile_path.as_ref() else {
                        continue;
                    };
                    add_existing_dir(
                        &mut index.all_bin_dirs,
                        &mut all_bin_seen,
                        profile_path.join("bin"),
                    );
                    add_existing_dir(
                        &mut index.all_lib_dirs,
                        &mut all_lib_seen,
                        profile_path.join("lib"),
                    );
                    add_existing_dir(
                        &mut index.all_include_dirs,
                        &mut all_include_seen,
                        profile_path.join("include"),
                    );
                    add_existing_dir(
                        &mut index.all_pkgconfig_dirs,
                        &mut all_pkgconfig_seen,
                        profile_path.join("lib").join("pkgconfig"),
                    );
                    add_tool_existing_dir(&mut index.tool_bin_dirs, name, profile_path.join("bin"));
                    add_tool_existing_dir(&mut index.tool_lib_dirs, name, profile_path.join("lib"));
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
                    collect_github_tool_dirs(
                        options,
                        &mut index,
                        name,
                        &tool.version,
                        platform_data,
                    )?;
                }
                provider_name => {
                    collect_default_provider_dirs(
                        options,
                        &mut index,
                        name,
                        &tool.version,
                        provider_name,
                    );
                }
            }
        }

        Ok(index)
    }
}

fn collect_github_tool_dirs(
    options: &ToolActivationResolveOptions<'_>,
    index: &mut ToolPathIndex,
    name: &str,
    version: &str,
    platform_data: &crate::lockfile::LockedToolPlatform,
) -> Result<()> {
    let tool_dir = options.cache_dir.join("github").join(name).join(version);
    let extract: Vec<ToolExtract> = platform_data
        .source
        .get("extract")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();

    if extract.is_empty() {
        collect_legacy_github_dirs(index, name, platform_data, &tool_dir);
        return Ok(());
    }

    for item in extract {
        collect_extract_dirs(index, name, &tool_dir, item)?;
    }

    Ok(())
}

fn collect_legacy_github_dirs(
    index: &mut ToolPathIndex,
    name: &str,
    platform_data: &crate::lockfile::LockedToolPlatform,
    tool_dir: &Path,
) {
    let mut all_bin_seen = index.all_bin_dirs.iter().cloned().collect();
    let mut all_lib_seen = index.all_lib_dirs.iter().cloned().collect();
    let path_hint_is_lib = platform_data
        .source
        .get("path")
        .and_then(|v| v.as_str())
        .is_some_and(path_looks_like_library);

    if path_hint_is_lib {
        let lib_dir = tool_dir.join("lib");
        add_existing_dir(&mut index.all_lib_dirs, &mut all_lib_seen, lib_dir.clone());
        add_tool_existing_dir(&mut index.tool_lib_dirs, name, lib_dir);
    } else {
        let bin_dir = tool_dir.join("bin");
        add_existing_dir(&mut index.all_bin_dirs, &mut all_bin_seen, bin_dir.clone());
        add_tool_existing_dir(&mut index.tool_bin_dirs, name, bin_dir);
    }
}

fn collect_extract_dirs(
    index: &mut ToolPathIndex,
    name: &str,
    tool_dir: &Path,
    item: ToolExtract,
) -> Result<()> {
    let mut all_bin_seen = index.all_bin_dirs.iter().cloned().collect();
    let mut all_lib_seen = index.all_lib_dirs.iter().cloned().collect();
    let mut all_include_seen = index.all_include_dirs.iter().cloned().collect();
    let mut all_pkgconfig_seen = index.all_pkgconfig_dirs.iter().cloned().collect();

    match item {
        ToolExtract::Bin { .. } => {
            let bin_dir = tool_dir.join("bin");
            add_existing_dir(&mut index.all_bin_dirs, &mut all_bin_seen, bin_dir.clone());
            add_tool_existing_dir(&mut index.tool_bin_dirs, name, bin_dir);
        }
        ToolExtract::Lib { path, env } => {
            let lib_dir = tool_dir.join("lib");
            add_existing_dir(&mut index.all_lib_dirs, &mut all_lib_seen, lib_dir.clone());
            add_tool_existing_dir(&mut index.tool_lib_dirs, name, lib_dir.clone());
            if let Some(var) = env {
                let file_name = Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(path.as_str());
                upsert_file_env_export(&mut index.file_env_exports, &var, lib_dir.join(file_name))?;
            }
        }
        ToolExtract::Include { .. } => {
            add_existing_dir(
                &mut index.all_include_dirs,
                &mut all_include_seen,
                tool_dir.join("include"),
            );
        }
        ToolExtract::PkgConfig { .. } => {
            add_existing_dir(
                &mut index.all_pkgconfig_dirs,
                &mut all_pkgconfig_seen,
                tool_dir.join("lib").join("pkgconfig"),
            );
        }
        ToolExtract::File { path, env } => {
            if let Some(var) = env {
                let file_name = Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(path.as_str());
                upsert_file_env_export(
                    &mut index.file_env_exports,
                    &var,
                    tool_dir.join("files").join(file_name),
                )?;
            }
        }
    }

    Ok(())
}

fn collect_default_provider_dirs(
    options: &ToolActivationResolveOptions<'_>,
    index: &mut ToolPathIndex,
    name: &str,
    version: &str,
    provider_name: &str,
) {
    let mut all_bin_seen = index.all_bin_dirs.iter().cloned().collect();
    let mut all_lib_seen = index.all_lib_dirs.iter().cloned().collect();
    let mut all_include_seen = index.all_include_dirs.iter().cloned().collect();
    let mut all_pkgconfig_seen = index.all_pkgconfig_dirs.iter().cloned().collect();
    let tool_dir = options
        .cache_dir
        .join(provider_name)
        .join(name)
        .join(version);
    let bin_dir = tool_dir.join("bin");
    let lib_dir = tool_dir.join("lib");
    let include_dir = tool_dir.join("include");
    let pkgconfig_dir = tool_dir.join("lib").join("pkgconfig");

    add_existing_dir(&mut index.all_bin_dirs, &mut all_bin_seen, bin_dir.clone());
    add_existing_dir(&mut index.all_lib_dirs, &mut all_lib_seen, lib_dir.clone());
    add_existing_dir(
        &mut index.all_include_dirs,
        &mut all_include_seen,
        include_dir,
    );
    add_existing_dir(
        &mut index.all_pkgconfig_dirs,
        &mut all_pkgconfig_seen,
        pkgconfig_dir,
    );
    add_tool_existing_dir(&mut index.tool_bin_dirs, name, bin_dir);
    add_tool_existing_dir(&mut index.tool_lib_dirs, name, lib_dir);

    if tool_dir.join(name).exists() || tool_dir.join(format!("{name}.exe")).exists() {
        add_existing_dir(&mut index.all_bin_dirs, &mut all_bin_seen, tool_dir.clone());
        add_tool_existing_dir(&mut index.tool_bin_dirs, name, tool_dir);
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
        Arch::Arm64 => "aarch64",
        Arch::X86_64 => "x86_64",
    };

    let os = match platform.os {
        Os::Darwin => "apple-darwin",
        Os::Linux => "unknown-linux-gnu",
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
