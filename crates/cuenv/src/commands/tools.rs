//! Tools command implementations for multi-source tool management.
//!
//! This module provides commands for downloading, activating, and listing tools
//! from multiple sources (GitHub releases, Nix packages, OCI images).

use crate::cli::CliError;
use cuenv_core::lockfile::{LOCKFILE_NAME, Lockfile};
use cuenv_core::tools::{
    FetchedTool, Platform, ResolvedTool, ResolvedToolActivationStep, ToolActivationResolveOptions,
    ToolExtract, ToolOptions, ToolRegistry, ToolSource, apply_resolved_tool_activation,
    resolve_tool_activation, validate_tool_activation,
};
use cuenv_events::{eprintln_redacted, println_redacted};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Display;
use std::path::{Path, PathBuf};

mod list;
use list::render_tools_list;

/// Create a tool registry with available providers.
fn create_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    // Register Nix provider
    registry.register(cuenv_tools_nix::NixToolProvider::new());

    // Register GitHub provider
    registry.register(cuenv_tools_github::GitHubToolProvider::new());

    // Register Rustup provider
    registry.register(cuenv_tools_rustup::RustupToolProvider::new());

    // Register URL provider
    registry.register(cuenv_tools_url::UrlToolProvider::new());

    // Register OCI provider
    registry.register(cuenv_tools_oci::OciToolProvider::new());

    registry
}

/// Execute the `tools download` command.
///
/// Downloads tools for the current platform from the lockfile.
///
/// # Errors
///
/// Returns an error if the lockfile is not found or if any tool download fails.
pub async fn execute_tools_download() -> Result<(), CliError> {
    let lockfile_path = find_lockfile(None).ok_or_else(|| {
        CliError::config_with_help(
            "No cuenv.lock found",
            "Run 'cuenv sync lock' to create the lockfile",
        )
    })?;

    let lockfile = Lockfile::load(&lockfile_path)
        .map_err(|e| CliError::other(format!("Failed to load lockfile: {e}")))?
        .ok_or_else(|| {
            CliError::config_with_help(
                "Lockfile is empty",
                "Run 'cuenv sync lock' to populate the lockfile",
            )
        })?;

    let session = ToolDownloadSession::new(&lockfile);
    session
        .check_prerequisites(PrerequisitePolicy::StrictCli)
        .await?;

    let mut reporter = CliDownloadReporter;
    let summary = session.download_missing(&mut reporter).await;
    reporter.finished(&summary);
    summary.ensure_success()
}

/// Ensure all tools from the lockfile are downloaded for the current platform.
///
/// This is called automatically before tool activation in exec/task commands.
/// If no lockfile exists or tools are already cached, this is a no-op.
///
/// If `project_path` is provided, the lockfile search starts from that directory.
/// Otherwise, it starts from the current working directory.
///
/// # Errors
///
/// Returns an error if tools cannot be downloaded due to provider issues.
pub async fn ensure_tools_downloaded(project_path: Option<&Path>) -> Result<(), CliError> {
    let Some(lockfile_path) = find_runtime_lockfile(project_path) else {
        tracing::debug!("No lockfile found - skipping tool download");
        return Ok(());
    };

    let Some(lockfile) = Lockfile::load(&lockfile_path)
        .map_err(|e| CliError::other(format!("Failed to load lockfile: {e}")))?
    else {
        tracing::debug!("Empty lockfile - skipping tool download");
        return Ok(());
    };

    if lockfile.tools.is_empty() {
        tracing::debug!("No tools in lockfile - skipping download");
        return Ok(());
    }

    let activation_options = ToolActivationResolveOptions::new(&lockfile, &lockfile_path);
    validate_tool_activation(&activation_options).map_err(|e| {
        CliError::config_with_help(
            format!("Invalid tool activation configuration: {e}"),
            "Run 'cuenv sync lock' to refresh cuenv.lock",
        )
    })?;

    let session = ToolDownloadSession::new(&lockfile);
    session
        .check_prerequisites(PrerequisitePolicy::BestEffortRuntime)
        .await?;

    let mut reporter = RuntimeDownloadReporter;
    let summary = session.download_missing(&mut reporter).await;
    reporter.finished(&summary);
    summary.ensure_success()
}

enum PrerequisitePolicy {
    StrictCli,
    BestEffortRuntime,
}

struct ToolDownloadSession<'a> {
    lockfile: &'a Lockfile,
    platform: Platform,
    platform_key: String,
    options: ToolOptions,
    registry: ToolRegistry,
}

impl<'a> ToolDownloadSession<'a> {
    fn new(lockfile: &'a Lockfile) -> Self {
        let platform = Platform::current();
        let platform_key = platform.to_string();

        Self {
            lockfile,
            platform,
            platform_key,
            options: ToolOptions::default(),
            registry: create_registry(),
        }
    }

    async fn check_prerequisites(&self, policy: PrerequisitePolicy) -> Result<(), CliError> {
        for provider_name in self.providers_used() {
            let Some(provider) = self.registry.get(&provider_name) else {
                continue;
            };

            if let Err(error) = provider.check_prerequisites().await {
                match policy {
                    PrerequisitePolicy::StrictCli => {
                        return Err(CliError::config_with_help(
                            format!("Provider '{provider_name}' not available: {error}"),
                            "Check that the required tools are installed",
                        ));
                    }
                    PrerequisitePolicy::BestEffortRuntime => {
                        tracing::warn!(
                            "Provider '{}' prerequisites check failed: {} - continuing with best-effort tool download",
                            provider_name,
                            error
                        );
                    }
                }
            }
        }

        Ok(())
    }

    async fn download_missing(
        &self,
        reporter: &mut impl ToolDownloadReporter,
    ) -> ToolDownloadSummary {
        let mut summary = ToolDownloadSummary::default();

        for (name, tool) in &self.lockfile.tools {
            let Some(locked) = tool.platforms.get(&self.platform_key) else {
                continue;
            };

            let Some(source) = lockfile_entry_to_source(name, &tool.version, locked) else {
                reporter.unknown_provider(&locked.provider, name);
                continue;
            };

            let Some(provider) = self.registry.find_for_source(&source) else {
                reporter.missing_provider(name);
                continue;
            };

            let resolved = ResolvedTool {
                name: name.clone(),
                version: tool.version.clone(),
                platform: self.platform.clone(),
                source,
            };

            if provider.is_cached(&resolved, &self.options) {
                summary.skipped += 1;
                reporter.cached(name);
                continue;
            }

            reporter.download_started(name, &tool.version);
            match provider.fetch(&resolved, &self.options).await {
                Ok(fetched) => {
                    summary.downloaded += 1;
                    reporter.download_finished(name, &fetched);
                }
                Err(error) => {
                    reporter.download_failed(name, &error);
                    summary.errors.push(format!("{name}: {error}"));
                }
            }
        }

        summary
    }

    fn providers_used(&self) -> BTreeSet<String> {
        self.lockfile
            .tools
            .values()
            .filter_map(|tool| tool.platforms.get(&self.platform_key))
            .map(|locked| locked.provider.clone())
            .collect()
    }
}

#[derive(Default)]
struct ToolDownloadSummary {
    downloaded: usize,
    skipped: usize,
    errors: Vec<String>,
}

impl ToolDownloadSummary {
    fn ensure_success(&self) -> Result<(), CliError> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(CliError::other(format!(
                "Failed to download tools: {}",
                self.errors.join(", ")
            )))
        }
    }
}

trait ToolDownloadReporter {
    fn unknown_provider(&mut self, provider_name: &str, tool_name: &str);
    fn missing_provider(&mut self, tool_name: &str);
    fn cached(&mut self, _tool_name: &str) {}
    fn download_started(&mut self, tool_name: &str, version: &str);
    fn download_finished(&mut self, tool_name: &str, fetched: &FetchedTool);
    fn download_failed(&mut self, tool_name: &str, error: &dyn Display);
    fn finished(&mut self, _summary: &ToolDownloadSummary) {}
}

struct CliDownloadReporter;

impl ToolDownloadReporter for CliDownloadReporter {
    fn unknown_provider(&mut self, provider_name: &str, tool_name: &str) {
        eprintln_redacted(&format!(
            "Warning: Unknown provider '{provider_name}' for tool '{tool_name}'"
        ));
    }

    fn missing_provider(&mut self, tool_name: &str) {
        eprintln_redacted(&format!(
            "Warning: No provider found for source type of tool '{tool_name}'"
        ));
    }

    fn download_started(&mut self, tool_name: &str, version: &str) {
        println_redacted(&format!("Downloading {tool_name} v{version}..."));
    }

    fn download_finished(&mut self, _tool_name: &str, fetched: &FetchedTool) {
        println_redacted(&format!(
            "  -> {} ({})",
            fetched.binary_path.display(),
            fetched.sha256
        ));
    }

    fn download_failed(&mut self, tool_name: &str, error: &dyn Display) {
        eprintln_redacted(&format!("  Error downloading '{tool_name}': {error}"));
    }

    fn finished(&mut self, summary: &ToolDownloadSummary) {
        println_redacted("");
        println_redacted(&format!(
            "Downloaded {} tools, {} already cached",
            summary.downloaded, summary.skipped
        ));
    }
}

struct RuntimeDownloadReporter;

impl ToolDownloadReporter for RuntimeDownloadReporter {
    fn unknown_provider(&mut self, provider_name: &str, tool_name: &str) {
        tracing::debug!(
            "Unknown provider '{}' for tool '{}' - skipping",
            provider_name,
            tool_name
        );
    }

    fn missing_provider(&mut self, tool_name: &str) {
        tracing::debug!("No provider found for tool '{}' - skipping", tool_name);
    }

    fn download_started(&mut self, tool_name: &str, version: &str) {
        tracing::info!("Downloading {tool_name} v{version}...");
    }

    fn download_finished(&mut self, tool_name: &str, fetched: &FetchedTool) {
        tracing::info!(
            "Downloaded {} -> {} ({})",
            tool_name,
            fetched.binary_path.display(),
            fetched.sha256
        );
    }

    fn download_failed(&mut self, tool_name: &str, error: &dyn Display) {
        tracing::warn!("Failed to download '{}': {}", tool_name, error);
    }

    fn finished(&mut self, summary: &ToolDownloadSummary) {
        if summary.downloaded > 0 {
            tracing::info!("Downloaded {} tools", summary.downloaded);
        }
    }
}

/// Convert a lockfile entry to a ToolSource.
fn lockfile_entry_to_source(
    _name: &str,
    _version: &str,
    locked: &cuenv_core::lockfile::LockedToolPlatform,
) -> Option<ToolSource> {
    match locked.provider.as_str() {
        "oci" => {
            let image = locked
                .source
                .get("image")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let path = locked
                .source
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            Some(ToolSource::Oci {
                image: image.to_string(),
                path: path.to_string(),
            })
        }
        "github" => {
            let repo = locked
                .source
                .get("repo")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let tag = locked
                .source
                .get("tag")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let asset = locked
                .source
                .get("asset")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let extract = parse_github_extract_list(&locked.source);
            Some(ToolSource::GitHub {
                repo: repo.to_string(),
                tag: tag.to_string(),
                asset: asset.to_string(),
                extract,
            })
        }
        "nix" => {
            let flake = locked
                .source
                .get("flake")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let package = locked
                .source
                .get("package")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let output = locked
                .source
                .get("output")
                .and_then(|v| v.as_str())
                .map(String::from);
            Some(ToolSource::Nix {
                flake: flake.to_string(),
                package: package.to_string(),
                output,
            })
        }
        "rustup" => {
            let toolchain = locked
                .source
                .get("toolchain")
                .and_then(|v| v.as_str())
                .unwrap_or("stable");
            let profile = locked
                .source
                .get("profile")
                .and_then(|v| v.as_str())
                .map(String::from);
            let components = locked
                .source
                .get("components")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let targets = locked
                .source
                .get("targets")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            Some(ToolSource::Rustup {
                toolchain: toolchain.to_string(),
                profile,
                components,
                targets,
            })
        }
        "url" => {
            let url = locked
                .source
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let extract = parse_github_extract_list(&locked.source);
            Some(ToolSource::Url {
                url: url.to_string(),
                extract,
            })
        }
        _ => None,
    }
}

fn parse_github_extract_list(source: &serde_json::Value) -> Vec<ToolExtract> {
    let mut extract = source
        .get("extract")
        .cloned()
        .and_then(|value| serde_json::from_value::<Vec<ToolExtract>>(value).ok())
        .unwrap_or_default();

    if extract.is_empty()
        && let Some(path) = source.get("path").and_then(|v| v.as_str())
    {
        if path_looks_like_library(path) {
            extract.push(ToolExtract::Lib {
                path: path.to_string(),
                env: None,
            });
        } else {
            extract.push(ToolExtract::Bin {
                path: path.to_string(),
                as_name: None,
            });
        }
    }

    extract
}

fn path_looks_like_library(path: &str) -> bool {
    let ext_is = |target: &str| {
        std::path::Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case(target))
    };
    ext_is("dylib") || ext_is("so") || path.to_ascii_lowercase().contains(".so.") || ext_is("dll")
}

/// Resolve inferred activation steps from the lockfile for the current platform.
///
/// Returns `Ok(None)` when no lockfile exists or no activation steps resolve.
///
/// # Errors
///
/// Returns an error when lockfile parsing fails or activation hints are invalid.
pub fn resolve_tool_activation_steps(
    project_path: Option<&Path>,
) -> Result<Option<Vec<ResolvedToolActivationStep>>, CliError> {
    let Some(lockfile_path) = find_runtime_lockfile(project_path) else {
        return Ok(None);
    };

    let Some(lockfile) = Lockfile::load(&lockfile_path)
        .map_err(|e| CliError::other(format!("Failed to load lockfile: {e}")))?
    else {
        return Ok(None);
    };

    let options = ToolActivationResolveOptions::new(&lockfile, &lockfile_path);
    let activation = resolve_tool_activation(&options).map_err(|e| {
        CliError::config_with_help(
            format!("Invalid tool activation configuration: {e}"),
            "Run 'cuenv sync lock' to refresh cuenv.lock",
        )
    })?;

    if activation.is_empty() {
        return Ok(None);
    }

    Ok(Some(activation))
}

/// Execute the `tools activate` command.
///
/// Outputs shell export statements inferred from lockfile tool metadata.
///
/// # Errors
///
/// Returns an error if the lockfile is not found.
pub fn execute_tools_activate() -> Result<(), CliError> {
    let activation_steps = resolve_tool_activation_steps(None)?.ok_or_else(|| {
        CliError::config_with_help(
            "No cuenv.lock found or no tools configured",
            "Run 'cuenv sync lock' to create the lockfile",
        )
    })?;

    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    let mut touched_vars: Vec<String> = Vec::new();
    let mut touched_set: HashSet<String> = HashSet::new();

    for step in activation_steps {
        let current = env.get(&step.var).map(String::as_str);
        if let Some(new_value) = apply_resolved_tool_activation(current, &step) {
            if touched_set.insert(step.var.clone()) {
                touched_vars.push(step.var.clone());
            }
            env.insert(step.var, new_value);
        }
    }

    for var in touched_vars {
        if let Some(value) = env.get(&var) {
            println_redacted(&format!("export {var}={}", shell_quote(value)));
        }
    }

    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Execute the `tools list` command.
///
/// Lists all tools configured in the lockfile.
///
/// # Errors
///
/// Returns an error if the lockfile is not found.
pub fn execute_tools_list() -> Result<(), CliError> {
    // Find the lockfile
    let lockfile_path = find_lockfile(None).ok_or_else(|| {
        CliError::config_with_help(
            "No cuenv.lock found",
            "Run 'cuenv sync lock' to create the lockfile",
        )
    })?;

    // Load the lockfile
    let lockfile = Lockfile::load(&lockfile_path)
        .map_err(|e| CliError::other(format!("Failed to load lockfile: {e}")))?
        .ok_or_else(|| {
            CliError::config_with_help(
                "Lockfile is empty",
                "Run 'cuenv sync lock' to populate the lockfile",
            )
        })?;

    for line in render_tools_list(
        &lockfile,
        &lockfile_path,
        &cuenv_core::lockfile::current_platform(),
    ) {
        println_redacted(&line);
    }

    Ok(())
}

/// Find the lockfile by walking up from the given directory (or current directory).
fn find_lockfile(start_path: Option<&Path>) -> Option<PathBuf> {
    let mut current = start_path
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::current_dir().ok())?;
    loop {
        let lockfile_path = current.join(LOCKFILE_NAME);
        if lockfile_path.exists() {
            return Some(lockfile_path);
        }

        // Also check in cue.mod directory
        let cue_mod_lockfile = current.join("cue.mod").join(LOCKFILE_NAME);
        if cue_mod_lockfile.exists() {
            return Some(cue_mod_lockfile);
        }

        if !current.pop() {
            return None;
        }
    }
}

/// Find a lockfile scoped to the current project only.
///
/// Runtime commands like `task` and `exec` should not inherit an ancestor
/// workspace lockfile when the target project does not define one.
fn find_lockfile_in_project(project_path: &Path) -> Option<PathBuf> {
    let project_lockfile = project_path.join(LOCKFILE_NAME);
    if project_lockfile.exists() {
        return Some(project_lockfile);
    }

    let cue_mod_lockfile = project_path.join("cue.mod").join(LOCKFILE_NAME);
    if cue_mod_lockfile.exists() {
        return Some(cue_mod_lockfile);
    }

    None
}

fn find_runtime_lockfile(project_path: Option<&Path>) -> Option<PathBuf> {
    project_path.map_or_else(|| find_lockfile(None), find_lockfile_in_project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::lockfile::LockedToolPlatform;
    use std::fs;

    #[test]
    fn test_find_lockfile_not_found() {
        // Save and restore CWD to avoid breaking parallel tests
        let original_cwd = std::env::current_dir().unwrap();

        // Create temp dir without lockfile
        let temp = tempfile::tempdir().unwrap();
        std::env::set_current_dir(temp.path()).unwrap();

        // Should return None when searching from temp dir
        let result = find_lockfile(None);

        // Restore CWD before assertions (in case of panic)
        std::env::set_current_dir(&original_cwd).unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn test_find_lockfile_in_project_checks_project_root() {
        let temp = tempfile::tempdir().unwrap();
        let lockfile_path = temp.path().join(LOCKFILE_NAME);
        fs::write(&lockfile_path, "").unwrap();

        let result = find_lockfile_in_project(temp.path());

        assert_eq!(result, Some(lockfile_path));
    }

    #[test]
    fn test_find_lockfile_in_project_does_not_walk_to_parent() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(LOCKFILE_NAME), "").unwrap();

        let project_dir = temp.path().join("nested-project");
        fs::create_dir_all(&project_dir).unwrap();

        let result = find_lockfile_in_project(&project_dir);

        assert!(result.is_none());
    }

    #[test]
    fn test_lockfile_entry_to_source_parses_url_source() {
        let locked = LockedToolPlatform {
            provider: "url".to_string(),
            digest: "sha256:abc".to_string(),
            source: serde_json::json!({
                "type": "url",
                "url": "https://example.com/tool.tar.gz",
                "extract": [{"kind": "bin", "path": "tool"}],
            }),
            size: None,
            dependencies: vec![],
        };

        let source = lockfile_entry_to_source("tool", "1.0.0", &locked).expect("parsed source");
        match source {
            ToolSource::Url { url, extract } => {
                assert_eq!(url, "https://example.com/tool.tar.gz");
                assert_eq!(
                    extract,
                    vec![ToolExtract::Bin {
                        path: "tool".to_string(),
                        as_name: None,
                    }]
                );
            }
            _ => panic!("expected URL source"),
        }
    }
}
