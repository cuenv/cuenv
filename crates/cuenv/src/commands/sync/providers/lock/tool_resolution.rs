use cuenv_core::Result;
use cuenv_core::lockfile::{LockedTool, LockedToolPlatform, Lockfile};
use cuenv_core::manifest::{GitHubExtract, SourceConfig, SourceOverride};
use cuenv_core::tools::{
    Platform as ToolPlatform, ResolvedTool, ToolExtract, ToolRegistry, ToolResolveRequest,
    ToolSource,
};
use std::collections::HashMap;
use tracing::{debug, info};

use crate::commands::sync::provider::{SyncMode, SyncOptions};

const VERSION_TEMPLATE: &str = "{version}";
const OS_TEMPLATE: &str = "{os}";
const ARCH_TEMPLATE: &str = "{arch}";

/// Collected tool specification from manifest.
#[derive(Debug, Clone)]
pub(super) struct CollectedTool {
    pub(super) name: String,
    pub(super) version: String,
    pub(super) source: Option<SourceConfig>,
    pub(super) overrides: Vec<SourceOverride>,
    pub(super) platforms: Vec<String>,
}

pub(super) struct ToolLockResolutionRequest<'a> {
    pub(super) tools: &'a [CollectedTool],
    pub(super) platforms: &'a [String],
    pub(super) flakes: HashMap<String, String>,
    pub(super) existing_lockfile: Option<&'a Lockfile>,
    pub(super) options: &'a SyncOptions,
    pub(super) github_token: Option<&'a str>,
}

/// Key type for tool deduplication: (name, version, source_hash).
pub(super) type ToolIdentityKey = (String, String, String);

/// Create a tool registry with available providers.
fn create_registry(flakes: HashMap<String, String>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    registry.register(cuenv_tools_nix::NixToolProvider::with_flakes(flakes));
    registry.register(cuenv_tools_github::GitHubToolProvider::new());
    registry.register(cuenv_tools_rustup::RustupToolProvider::new());
    registry.register(cuenv_tools_url::UrlToolProvider::new());
    registry.register(cuenv_tools_oci::OciToolProvider::new());

    registry
}

/// Check if a tool should be force-updated based on the update_tools option.
///
/// Returns true if the tool should be re-resolved from the provider,
/// false if it should use the cached lockfile resolution.
fn should_update_tool(tool_name: &str, update_tools: Option<&Vec<String>>) -> bool {
    match update_tools {
        None => false,                                       // No -u flag: use cache
        Some(tools) if tools.is_empty() => true,             // -u alone: update all
        Some(tools) => tools.iter().any(|t| t == tool_name), // -u <names>: update if listed
    }
}

/// Get a valid cached resolution from the lockfile if it's still valid.
///
/// Returns the cached platform resolution if:
/// 1. The locked version matches the requested version
/// 2. The locked platform exists
/// 3. The locked source configuration matches the resolved manifest source
///
/// Returns `None` if the cache is stale and the tool needs to be re-resolved.
fn get_valid_cached_resolution<'a>(
    locked_tool: &'a LockedTool,
    manifest_version: &str,
    platform_str: &str,
    resolved_source_config: &serde_json::Value,
) -> Option<&'a LockedToolPlatform> {
    if locked_tool.version != manifest_version {
        debug!(
            locked_version = %locked_tool.version,
            %manifest_version,
            "Cache miss: version mismatch"
        );
        return None;
    }

    let Some(locked_platform) = locked_tool.platforms.get(platform_str) else {
        debug!(%platform_str, "Cache miss: platform not in lockfile");
        return None;
    };

    if locked_platform.source != *resolved_source_config {
        debug!(
            %platform_str,
            "Cache miss: source config changed"
        );
        return None;
    }

    Some(locked_platform)
}

/// Create a unique identity key for tool deduplication.
///
/// Tools are considered identical if they have the same name, version, and source.
/// This prevents resolving the same tool multiple times when defined in multiple projects.
pub(super) fn tool_identity_key(
    name: &str,
    version: &str,
    source: Option<&SourceConfig>,
) -> ToolIdentityKey {
    let source_hash = source.map(|s| format!("{s:?}")).unwrap_or_default();
    (name.to_string(), version.to_string(), source_hash)
}

pub(super) async fn resolve_tool_locks(
    lockfile: &mut Lockfile,
    request: ToolLockResolutionRequest<'_>,
) -> Result<()> {
    let ToolLockResolutionRequest {
        tools,
        platforms,
        flakes,
        existing_lockfile,
        options,
        github_token,
    } = request;

    if tools.is_empty() {
        return Ok(());
    }

    info!(
        "Resolving {} unique tools for {} platforms",
        tools.len(),
        platforms.len()
    );

    let registry = create_registry(flakes);

    for tool in tools {
        debug!(name = %tool.name, version = %tool.version, "Resolving tool");

        for platform_str in &tool.platforms {
            let platform = ToolPlatform::parse(platform_str).ok_or_else(|| {
                cuenv_core::Error::configuration(format!(
                    "Invalid platform '{}': expected format 'os-arch' (e.g., 'darwin-arm64')",
                    platform_str
                ))
            })?;

            let source_config =
                resolve_source_for_platform(tool.source.as_ref(), &tool.overrides, &platform)
                    .ok_or_else(|| {
                        cuenv_core::Error::configuration(format!(
                            "Tool '{}' has no source configured for platform '{}'. \
                                 Specify a source (github, nix, rustup, url, or oci) in your tool definition.",
                            tool.name, platform_str
                        ))
                    })?;

            let (provider_name, _tool_source, config) =
                source_config_to_tool_source(&tool.version, &source_config, &platform);

            let force_update = should_update_tool(&tool.name, options.update_tools.as_ref());

            debug!(
                tool = %tool.name,
                %platform_str,
                manifest_version = %tool.version,
                config = %config,
                "Checking lockfile cache"
            );

            let cached = if force_update {
                debug!(tool = %tool.name, "Skipping cache: force update requested");
                None
            } else if options.mode == SyncMode::Check {
                debug!(tool = %tool.name, "Skipping cache: check mode");
                None
            } else if let Some(existing) = existing_lockfile {
                if let Some(locked_tool) = existing.find_tool(&tool.name) {
                    get_valid_cached_resolution(locked_tool, &tool.version, platform_str, &config)
                } else {
                    debug!(tool = %tool.name, "Cache miss: tool not in lockfile");
                    None
                }
            } else {
                debug!(tool = %tool.name, "Cache miss: no existing lockfile");
                None
            };

            if let Some(locked_platform) = cached {
                debug!(
                    tool = %tool.name,
                    %platform_str,
                    "Using cached resolution from lockfile"
                );

                lockfile
                    .upsert_tool_platform(
                        &tool.name,
                        &tool.version,
                        platform_str,
                        locked_platform.clone(),
                    )
                    .map_err(|e| {
                        cuenv_core::Error::configuration(format!(
                            "Failed to add tool '{}' to lockfile: {}",
                            tool.name, e
                        ))
                    })?;
                continue;
            }

            let Some(provider) = registry.get(&provider_name) else {
                return Err(cuenv_core::Error::configuration(format!(
                    "No provider '{}' registered for tool '{}'",
                    provider_name, tool.name
                )));
            };

            info!(
                tool = %tool.name,
                %platform_str,
                provider = %provider_name,
                "Resolving tool from provider"
            );

            let resolved = provider
                .resolve(&ToolResolveRequest {
                    tool_name: &tool.name,
                    version: &tool.version,
                    platform: &platform,
                    config: &config,
                    token: github_token,
                })
                .await
                .map_err(|e| {
                    cuenv_core::Error::configuration(format!(
                        "Failed to resolve tool '{}' for platform '{}': {}",
                        tool.name, platform_str, e
                    ))
                })?;

            debug!(
                tool = %tool.name,
                %platform_str,
                provider = %provider_name,
                "Resolved tool"
            );

            let resolved_source = serde_json::to_value(&resolved.source).map_err(|e| {
                cuenv_core::Error::configuration(format!(
                    "Failed to serialize resolved source for '{}': {}",
                    tool.name, e
                ))
            })?;
            let locked_platform = LockedToolPlatform {
                provider: provider_name.clone(),
                digest: format!("sha256:{}", compute_tool_digest(&resolved)),
                source: resolved_source,
                size: None,
                dependencies: vec![],
            };

            lockfile
                .upsert_tool_platform(&tool.name, &resolved.version, platform_str, locked_platform)
                .map_err(|e| {
                    cuenv_core::Error::configuration(format!(
                        "Failed to add tool '{}' to lockfile: {}",
                        tool.name, e
                    ))
                })?;
        }
    }

    Ok(())
}

/// Check if the lockfile contains any Nix tools for the current platform.
pub(super) fn has_nix_tools(lockfile: &Lockfile) -> bool {
    let platform = ToolPlatform::current().to_string();
    lockfile.tools.values().any(|tool| {
        tool.platforms
            .get(&platform)
            .is_some_and(|p| p.provider == "nix")
    })
}

/// Resolve the source configuration for a specific platform.
///
/// Applies overrides based on OS and architecture, returning the
/// appropriate source configuration. Returns None if no source is configured.
fn resolve_source_for_platform(
    default_source: Option<&SourceConfig>,
    overrides: &[SourceOverride],
    platform: &ToolPlatform,
) -> Option<SourceConfig> {
    let mut best_match: Option<&SourceConfig> = None;
    let mut best_specificity = 0;

    for override_config in overrides {
        let os_matches = override_config
            .os
            .as_ref()
            .map_or(true, |os| os == &platform.os.to_string());
        let arch_matches = override_config
            .arch
            .as_ref()
            .map_or(true, |arch| arch == &platform.arch.to_string());

        if os_matches && arch_matches {
            let specificity =
                u8::from(override_config.os.is_some()) + u8::from(override_config.arch.is_some());
            if specificity > best_specificity {
                best_specificity = specificity;
                best_match = Some(&override_config.source);
            }
        }
    }

    best_match.cloned().or_else(|| default_source.cloned())
}

/// Convert `SourceConfig` to `ToolSource` and provider config.
pub(super) fn source_config_to_tool_source(
    version: &str,
    config: &SourceConfig,
    platform: &ToolPlatform,
) -> (String, ToolSource, serde_json::Value) {
    match config {
        SourceConfig::Oci { image, path } => (
            "oci".to_string(),
            ToolSource::Oci {
                image: image.clone(),
                path: path.clone(),
            },
            serde_json::json!({ "type": "oci", "image": image, "path": path }),
        ),
        SourceConfig::GitHub {
            repo,
            tag_prefix,
            tag,
            asset,
            path,
            extract,
        } => {
            let tag_template = tag
                .clone()
                .unwrap_or_else(|| format!("{}{}", tag_prefix, version));
            let resolved_tag = expand_source_template(&tag_template, version, platform);
            let resolved_asset = expand_source_template(asset, version, platform);
            let resolve_context = ExtractResolveContext {
                version,
                platform,
                legacy_path: path.as_deref(),
            };
            let resolved_extract = resolve_extract_templates(extract, &resolve_context);
            (
                "github".to_string(),
                ToolSource::GitHub {
                    repo: repo.clone(),
                    tag: resolved_tag.clone(),
                    asset: resolved_asset.clone(),
                    extract: resolved_extract.clone(),
                },
                serde_json::json!({
                    "type": "github",
                    "repo": repo,
                    "tag": resolved_tag,
                    "asset": resolved_asset,
                    "extract": resolved_extract,
                }),
            )
        }
        SourceConfig::Nix {
            flake,
            package,
            output,
        } => (
            "nix".to_string(),
            ToolSource::Nix {
                flake: flake.clone(),
                package: package.clone(),
                output: output.clone(),
            },
            serde_json::json!({
                "type": "nix",
                "flake": flake,
                "package": package,
                "output": output,
            }),
        ),
        SourceConfig::Rustup {
            toolchain,
            profile,
            components,
            targets,
        } => (
            "rustup".to_string(),
            ToolSource::Rustup {
                toolchain: toolchain.clone(),
                profile: Some(profile.clone()),
                components: components.clone(),
                targets: targets.clone(),
            },
            serde_json::json!({
                "type": "rustup",
                "toolchain": toolchain,
                "profile": profile,
                "components": components,
                "targets": targets,
            }),
        ),
        SourceConfig::Url { url, path, extract } => {
            let resolved_url = expand_source_template(url, version, platform);
            let resolve_context = ExtractResolveContext {
                version,
                platform,
                legacy_path: path.as_deref(),
            };
            let resolved_extract = resolve_extract_templates(extract, &resolve_context);
            (
                "url".to_string(),
                ToolSource::Url {
                    url: resolved_url.clone(),
                    extract: resolved_extract.clone(),
                },
                serde_json::json!({
                    "type": "url",
                    "url": resolved_url,
                    "extract": resolved_extract,
                }),
            )
        }
    }
}

struct ExtractResolveContext<'a> {
    version: &'a str,
    platform: &'a ToolPlatform,
    legacy_path: Option<&'a str>,
}

fn resolve_extract_templates(
    extract: &[GitHubExtract],
    context: &ExtractResolveContext<'_>,
) -> Vec<ToolExtract> {
    let mut resolved: Vec<ToolExtract> = extract
        .iter()
        .map(|item| match item {
            GitHubExtract::Bin { path, as_name } => ToolExtract::Bin {
                path: expand_source_template(path, context.version, context.platform),
                as_name: as_name.clone(),
            },
            GitHubExtract::Lib { path, env } => ToolExtract::Lib {
                path: expand_source_template(path, context.version, context.platform),
                env: env.clone(),
            },
            GitHubExtract::Include { path } => ToolExtract::Include {
                path: expand_source_template(path, context.version, context.platform),
            },
            GitHubExtract::PkgConfig { path } => ToolExtract::PkgConfig {
                path: expand_source_template(path, context.version, context.platform),
            },
            GitHubExtract::File { path, env } => ToolExtract::File {
                path: expand_source_template(path, context.version, context.platform),
                env: env.clone(),
            },
        })
        .collect();

    if resolved.is_empty()
        && let Some(path) = context.legacy_path
    {
        let path = expand_source_template(path, context.version, context.platform);
        if path_looks_like_library(&path) {
            resolved.push(ToolExtract::Lib { path, env: None });
        } else {
            resolved.push(ToolExtract::Bin {
                path,
                as_name: None,
            });
        }
    }

    resolved
}

fn path_looks_like_library(path: &str) -> bool {
    std::path::Path::new(path).extension().is_some_and(|ext| {
        ext.eq_ignore_ascii_case("dylib")
            || ext.eq_ignore_ascii_case("so")
            || ext.eq_ignore_ascii_case("dll")
    }) || path.to_ascii_lowercase().contains(".so.")
}

fn expand_source_template(value: &str, version: &str, platform: &ToolPlatform) -> String {
    let os_str = match platform.os {
        cuenv_core::tools::Os::Darwin => "darwin",
        cuenv_core::tools::Os::Linux => "linux",
    };
    let arch_str = match platform.arch {
        cuenv_core::tools::Arch::Arm64 => "aarch64",
        cuenv_core::tools::Arch::X86_64 => "x86_64",
    };

    value
        .replace(VERSION_TEMPLATE, version)
        .replace(OS_TEMPLATE, os_str)
        .replace(ARCH_TEMPLATE, arch_str)
}

/// Compute a SHA256 digest for a resolved tool (for lockfile).
///
/// This creates a deterministic content hash based on the tool's
/// identifying information (name, version, platform, source).
fn compute_tool_digest(resolved: &ResolvedTool) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(resolved.name.as_bytes());
    hasher.update(b":");
    hasher.update(resolved.version.as_bytes());
    hasher.update(b":");
    hasher.update(resolved.platform.to_string().as_bytes());
    hasher.update(b":");
    hasher.update(format!("{:?}", resolved.source).as_bytes());
    format!("{:x}", hasher.finalize())
}
