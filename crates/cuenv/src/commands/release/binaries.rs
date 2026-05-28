//! Binary release command orchestration.

use cuenv_release::{
    CargoManifest, GitHubBackendConfig, HomebrewBackendConfig, OrchestratorConfig, ReleaseBackend,
    ReleaseConfig, ReleaseOrchestrator, ReleasePhase, ReleaseReport, Target,
};
use std::fmt::Write;
use std::path::Path;

/// Release phase to execute.
#[derive(Debug, Clone, Copy, Default)]
pub enum ReleaseBinariesPhase {
    /// Build binaries only.
    Build,
    /// Package binaries only.
    Package,
    /// Publish only (requires existing artifacts).
    Publish,
    /// Full pipeline: build, package, publish.
    #[default]
    Full,
}

/// Options for the `release binaries` command.
#[derive(Debug, Clone, Default)]
pub struct ReleaseBinariesOptions {
    /// Project root path.
    pub path: String,
    /// Dry run mode (no actual publishing).
    pub dry_run: cuenv_core::DryRun,
    /// Filter to specific backends.
    pub backends: Option<Vec<String>>,
    /// Release phase to execute.
    pub phase: ReleaseBinariesPhase,
    /// Target platforms to build for.
    pub targets: Option<Vec<String>>,
    /// Version to release (auto-detected from Cargo.toml if not provided).
    pub version: Option<String>,
}

impl ReleaseBinariesOptions {
    /// Creates new options with the given path.
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            ..Default::default()
        }
    }

    /// Sets dry run mode.
    #[must_use]
    pub const fn with_dry_run(mut self, dry_run: cuenv_core::DryRun) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Sets the backends filter.
    #[must_use]
    pub fn with_backends(mut self, backends: Option<Vec<String>>) -> Self {
        self.backends = backends;
        self
    }

    /// Sets the release phase.
    #[must_use]
    pub const fn with_phase(mut self, phase: ReleaseBinariesPhase) -> Self {
        self.phase = phase;
        self
    }

    /// Sets the target platforms.
    #[must_use]
    pub fn with_targets(mut self, targets: Option<Vec<String>>) -> Self {
        self.targets = targets;
        self
    }

    /// Sets the version.
    #[must_use]
    pub fn with_version(mut self, version: Option<String>) -> Self {
        self.version = version;
        self
    }
}

/// Execute the `release binaries` command.
///
/// Builds, packages, and publishes binary releases to configured backends.
///
/// # Errors
///
/// Returns an error if the release process fails.
pub async fn execute_release_binaries(opts: ReleaseBinariesOptions) -> cuenv_core::Result<String> {
    let ReleaseBinariesOptions {
        path,
        dry_run,
        backends,
        phase,
        targets,
        version,
    } = opts;

    let root = Path::new(&path);
    let manifest = CargoManifest::new(root);
    let release_config = super::load_release_config(root)?;
    let release_version = release_binary_version(version, &manifest)?;
    let binary_name = release_binary_name(&manifest, release_config.binary.as_deref())?;
    let release_targets = release_binary_targets(targets.as_deref(), &release_config.targets)?;
    let config = OrchestratorConfig::new(&binary_name, &release_version)
        .with_targets(release_targets)
        .with_output_dir("target/release-artifacts")
        .with_dry_run(dry_run);
    let phase = release_binary_phase(phase);
    let backends =
        release_binary_backends(root, &binary_name, backends.as_deref(), &release_config);
    let report = run_release_binaries(config, backends, phase).await?;

    Ok(format_release_binaries_output(&ReleaseBinariesOutput {
        dry_run,
        binary_name: &binary_name,
        release_version: &release_version,
        report: &report,
    }))
}

fn release_binary_version(
    requested: Option<String>,
    manifest: &CargoManifest,
) -> cuenv_core::Result<String> {
    if let Some(version) = requested {
        return Ok(version);
    }

    manifest
        .read_workspace_version()
        .map(|version| version.to_string())
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to read version: {e}")))
}

fn release_binary_name(
    manifest: &CargoManifest,
    configured: Option<&str>,
) -> cuenv_core::Result<String> {
    if let Some(name) = configured {
        return Ok(name.to_string());
    }

    manifest
        .get_package_names()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to read packages: {e}")))?
        .into_iter()
        .next()
        .ok_or_else(|| cuenv_core::Error::configuration("No packages found in workspace"))
}

fn release_binary_targets(
    requested: Option<&[String]>,
    configured: &[String],
) -> cuenv_core::Result<Vec<Target>> {
    let targets = requested.unwrap_or(configured);
    if targets.is_empty() {
        return Ok(vec![
            Target::LinuxX64,
            Target::LinuxArm64,
            Target::DarwinArm64,
        ]);
    }

    targets
        .iter()
        .map(|target| {
            target.parse::<Target>().map_err(|e| {
                cuenv_core::Error::configuration(format!("Invalid target '{target}': {e}"))
            })
        })
        .collect()
}

const fn release_binary_phase(phase: ReleaseBinariesPhase) -> ReleasePhase {
    match phase {
        ReleaseBinariesPhase::Build => ReleasePhase::Build,
        ReleaseBinariesPhase::Package => ReleasePhase::Package,
        ReleaseBinariesPhase::Publish => ReleasePhase::Publish,
        ReleaseBinariesPhase::Full => ReleasePhase::Full,
    }
}

fn release_binary_backends(
    root: &Path,
    binary_name: &str,
    filter: Option<&[String]>,
    config: &ReleaseConfig,
) -> Vec<Box<dyn ReleaseBackend>> {
    let mut backends: Vec<Box<dyn ReleaseBackend>> = Vec::new();
    let configured_backends = config.backends.is_some();

    #[cfg(feature = "github")]
    add_github_release_backend(
        &mut backends,
        root,
        configured_backends,
        config.backends.as_ref().and_then(|b| b.github.as_ref()),
        &config.git.tag_prefix,
    );
    #[cfg(not(feature = "github"))]
    let _ = root;

    #[cfg(feature = "homebrew")]
    add_homebrew_release_backend(
        &mut backends,
        binary_name,
        configured_backends,
        config.backends.as_ref().and_then(|b| b.homebrew.as_ref()),
        &config.git.tag_prefix,
    );
    #[cfg(not(feature = "homebrew"))]
    let _ = binary_name;

    if let Some(filter) = filter {
        let filter_lower: Vec<String> = filter.iter().map(|s| s.to_lowercase()).collect();
        backends.retain(|backend| filter_lower.contains(&backend.name().to_lowercase()));
    }

    backends
}

#[cfg(feature = "github")]
fn add_github_release_backend(
    backends: &mut Vec<Box<dyn ReleaseBackend>>,
    root: &Path,
    configured_backends: bool,
    config: Option<&GitHubBackendConfig>,
    tag_prefix: &str,
) {
    if configured_backends && config.is_none() {
        return;
    }
    if config.is_some_and(|cfg| !cfg.assets) {
        return;
    }

    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        let repo = config
            .and_then(|cfg| cfg.repo.as_deref())
            .and_then(parse_repo)
            .or_else(|| get_github_repo_from_remote(root));

        if let Some((owner, repo)) = repo {
            let draft = config.is_some_and(|cfg| cfg.draft);
            let config = cuenv_github::GitHubReleaseConfig::new(&owner, &repo, token)
                .with_draft(draft)
                .with_tag_prefix(tag_prefix);
            backends.push(Box::new(cuenv_github::GitHubReleaseBackend::new(config)));
        }
    }
}

#[cfg(feature = "homebrew")]
fn add_homebrew_release_backend(
    backends: &mut Vec<Box<dyn ReleaseBackend>>,
    binary_name: &str,
    configured_backends: bool,
    config: Option<&HomebrewBackendConfig>,
    tag_prefix: &str,
) {
    if configured_backends && config.is_none() {
        return;
    }
    let token_env = config.map_or("HOMEBREW_TAP_TOKEN", |cfg| cfg.token_env.as_str());
    if std::env::var(token_env).is_err() {
        return;
    }

    let tap = config
        .map(|cfg| cfg.tap.as_str())
        .filter(|tap| !tap.is_empty())
        .map_or_else(
            || format!("{binary_name}/homebrew-tap"),
            ToString::to_string,
        );
    let formula = config
        .and_then(|cfg| cfg.formula.as_deref())
        .unwrap_or(binary_name);
    let config = cuenv_homebrew::HomebrewConfig::new(&tap, formula)
        .with_license("AGPL-3.0-or-later")
        .with_homepage(format!("https://github.com/{binary_name}"))
        .with_token_env(token_env)
        .with_tag_prefix(tag_prefix);
    backends.push(Box::new(cuenv_homebrew::HomebrewBackend::new(config)));
}

async fn run_release_binaries(
    config: OrchestratorConfig,
    backends: Vec<Box<dyn ReleaseBackend>>,
    phase: ReleasePhase,
) -> cuenv_core::Result<ReleaseReport> {
    ReleaseOrchestrator::new(config)
        .with_backends(backends)
        .run(phase)
        .await
        .map_err(|e| cuenv_core::Error::configuration(format!("Release failed: {e}")))
}

struct ReleaseBinariesOutput<'a> {
    dry_run: cuenv_core::DryRun,
    binary_name: &'a str,
    release_version: &'a str,
    report: &'a ReleaseReport,
}

fn format_release_binaries_output(summary: &ReleaseBinariesOutput<'_>) -> String {
    let mut output = String::new();

    if summary.dry_run.is_dry_run() {
        output.push_str("[dry-run] ");
    }

    let _ = writeln!(
        output,
        "Release {} {}",
        summary.binary_name, summary.release_version
    );
    let _ = writeln!(output, "Phase: {:?}", summary.report.phase);

    if !summary.report.artifacts.is_empty() {
        output.push_str("\nArtifacts:\n");
        for artifact in &summary.report.artifacts {
            let _ = writeln!(
                output,
                "  - {} ({})",
                artifact.archive_name, artifact.sha256
            );
        }
    }

    if !summary.report.backend_results.is_empty() {
        output.push_str("\nBackend results:\n");
        for result in &summary.report.backend_results {
            let status = if result.success { "✓" } else { "✗" };
            let _ = writeln!(
                output,
                "  {} {}: {}",
                status, result.backend, result.message
            );
            if let Some(url) = &result.url {
                let _ = writeln!(output, "      URL: {url}");
            }
        }
    }

    if summary.report.success {
        output.push_str("\nRelease completed successfully.\n");
    } else {
        output.push_str("\nRelease completed with errors.\n");
    }

    output
}

/// Gets the GitHub owner/repo from the git remote origin.
#[cfg(feature = "github")]
fn get_github_repo_from_remote(root: &Path) -> Option<(String, String)> {
    let repo = gix::discover(root).ok()?;
    let remote = repo.find_remote("origin").ok()?;
    let url = remote.url(gix::remote::Direction::Fetch)?;
    parse_github_url(&url.to_bstring().to_string())
}

/// Parses a GitHub URL into (owner, repo).
#[cfg(feature = "github")]
fn parse_repo(repo: &str) -> Option<(String, String)> {
    let (owner, repo) = repo.split_once('/')?;
    Some((owner.to_string(), repo.to_string()))
}

/// Parses a GitHub URL into (owner, repo).
#[cfg(feature = "github")]
fn parse_github_url(url: &str) -> Option<(String, String)> {
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let path = rest.strip_suffix(".git").unwrap_or(rest);
        let (owner, repo) = path.split_once('/')?;
        return Some((owner.to_string(), repo.to_string()));
    }

    if let Some(rest) = url.strip_prefix("https://github.com/") {
        let path = rest.strip_suffix(".git").unwrap_or(rest);
        let (owner, repo) = path.split_once('/')?;
        return Some((owner.to_string(), repo.to_string()));
    }

    None
}
