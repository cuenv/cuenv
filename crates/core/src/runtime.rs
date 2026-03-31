//! Runtime environment resolution trait and implementations.
//!
//! Provides the [`RuntimeResolver`] trait for resolving environment variables
//! from configured runtimes (Nix devShells, devenv, etc.) and a factory
//! function to dispatch on the [`Runtime`] enum.

use crate::Result;
use crate::manifest::{DevenvRuntime, NixRuntime, Runtime};
use async_trait::async_trait;
use cuenv_hooks::{Hook, capture_source_environment};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Default timeout (in seconds) for runtime environment resolution.
///
/// Covers long-running operations like `nix print-dev-env` and
/// `devenv print-dev-env`, which can take several minutes on first run.
pub const DEFAULT_RUNTIME_TIMEOUT_SECONDS: u64 = 600;

/// The result of resolving a runtime's environment contribution.
///
/// Nix resolvers capture PATH inside `vars` (from `nix print-dev-env` output).
/// Devenv resolvers may additionally populate `path_prepend` when devenv is
/// installed on-the-fly and its bin directory needs to be on PATH.
#[derive(Debug, Clone, Default)]
pub struct RuntimeEnvironment {
    /// Environment variables to set (e.g., captured from `nix print-dev-env`).
    pub vars: HashMap<String, String>,
    /// Directories to prepend to PATH. Applied in order, so the first entry
    /// has highest priority.
    pub path_prepend: Vec<PathBuf>,
}

impl RuntimeEnvironment {
    /// Compute the merged PATH value by prepending [`Self::path_prepend`] dirs
    /// to `current_path`. Returns `None` when there is nothing to prepend.
    #[must_use]
    pub fn merged_path(&self, current_path: Option<&str>) -> Option<String> {
        if self.path_prepend.is_empty() {
            return None;
        }
        let prepend = self
            .path_prepend
            .iter()
            .map(|p| p.to_string_lossy())
            .collect::<Vec<_>>()
            .join(":");
        let current = current_path.unwrap_or_default();
        if current.is_empty() {
            Some(prepend)
        } else {
            Some(format!("{prepend}:{current}"))
        }
    }

    /// Apply vars and PATH prepend into a [`BTreeMap`] environment.
    ///
    /// Used by the CI orchestrator which builds its env as a `BTreeMap`.
    pub fn apply_to_btree_map(&self, env: &mut BTreeMap<String, String>) {
        for (key, value) in &self.vars {
            env.insert(key.clone(), value.clone());
        }
        if let Some(path) = self.merged_path(env.get("PATH").map(String::as_str)) {
            env.insert("PATH".to_string(), path);
        }
    }
}

/// Context needed by runtime resolvers.
pub struct RuntimeResolveContext<'a> {
    /// Root directory of the project (where env.cue lives).
    pub project_root: &'a Path,
    /// Timeout in seconds for long-running operations like `nix print-dev-env`.
    pub timeout_seconds: u64,
}

/// Resolve the runtime environment for a project, if a runtime is configured.
///
/// This is the main entry point for callers. It dispatches to the appropriate
/// [`RuntimeResolver`] based on the [`Runtime`] variant and returns the
/// resolved environment, or an empty default if no resolver applies.
pub async fn resolve_runtime(
    project_root: &Path,
    runtime: Option<&Runtime>,
) -> Result<RuntimeEnvironment> {
    if let Some(resolver) = runtime.and_then(resolver_for_runtime) {
        let ctx = RuntimeResolveContext {
            project_root,
            timeout_seconds: DEFAULT_RUNTIME_TIMEOUT_SECONDS,
        };
        resolver.resolve(&ctx).await
    } else {
        Ok(RuntimeEnvironment::default())
    }
}

/// Trait for resolving environment variables from a configured runtime.
///
/// Implementations capture the shell environment exported by tools like
/// `nix print-dev-env` or `devenv print-dev-env` and return it as a
/// [`RuntimeEnvironment`] that callers compose into the task execution
/// environment without mutating global process state.
#[async_trait]
pub trait RuntimeResolver: Send + Sync {
    /// Human-readable name for logging and error messages.
    fn name(&self) -> &'static str;

    /// Resolve the runtime's environment contribution.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime tool is unavailable, installation
    /// fails, or environment capture times out.
    async fn resolve(&self, ctx: &RuntimeResolveContext<'_>) -> Result<RuntimeEnvironment>;
}

/// Create a [`RuntimeResolver`] for the given [`Runtime`], if applicable.
///
/// Returns `None` for runtime types that do not contribute environment
/// variables (Container, Dagger, OCI, Tools) — those are handled by
/// execution backends or tool activation.
pub fn resolver_for_runtime(runtime: &Runtime) -> Option<Box<dyn RuntimeResolver>> {
    match runtime {
        Runtime::Nix(nix) => Some(Box::new(NixRuntimeResolver::new(nix.clone()))),
        Runtime::Devenv(devenv) => Some(Box::new(DevenvRuntimeResolver::new(devenv.clone()))),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Nix
// ---------------------------------------------------------------------------

/// Resolves environment by running `nix print-dev-env` and capturing the
/// resulting shell variables.
struct NixRuntimeResolver {
    runtime: NixRuntime,
}

impl NixRuntimeResolver {
    fn new(runtime: NixRuntime) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl RuntimeResolver for NixRuntimeResolver {
    fn name(&self) -> &'static str {
        "nix"
    }

    async fn resolve(&self, ctx: &RuntimeResolveContext<'_>) -> Result<RuntimeEnvironment> {
        let hook = Hook {
            order: 10,
            propagate: false,
            command: "nix".to_string(),
            args: nix_print_dev_env_args(&self.runtime),
            dir: Some(ctx.project_root.to_string_lossy().to_string()),
            inputs: vec!["flake.nix".to_string(), "flake.lock".to_string()],
            source: Some(true),
        };

        let vars =
            capture_source_environment(hook, &HashMap::new(), ctx.timeout_seconds)
                .await
                .map_err(|e| {
                    crate::Error::configuration(format!(
                        "Failed to acquire Nix runtime environment: {e}"
                    ))
                })?;

        Ok(RuntimeEnvironment {
            // PATH from `nix print-dev-env` is included in `vars` — no
            // additional `path_prepend` needed for Nix runtimes.
            vars,
            path_prepend: vec![],
        })
    }
}

fn nix_print_dev_env_args(runtime: &NixRuntime) -> Vec<String> {
    let mut args = vec![
        "--extra-experimental-features".to_string(),
        "nix-command flakes".to_string(),
        "print-dev-env".to_string(),
    ];
    args.push(nix_runtime_target(runtime));
    args
}

fn nix_runtime_target(runtime: &NixRuntime) -> String {
    match &runtime.output {
        Some(output) => format!("{}#{}", runtime.flake, output),
        None => runtime.flake.clone(),
    }
}

// ---------------------------------------------------------------------------
// Devenv
// ---------------------------------------------------------------------------

/// Resolves environment by running `devenv print-dev-env`, installing devenv
/// via `nix profile install` if it is not already on PATH.
struct DevenvRuntimeResolver {
    runtime: DevenvRuntime,
}

impl DevenvRuntimeResolver {
    fn new(runtime: DevenvRuntime) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl RuntimeResolver for DevenvRuntimeResolver {
    fn name(&self) -> &'static str {
        "devenv"
    }

    async fn resolve(&self, ctx: &RuntimeResolveContext<'_>) -> Result<RuntimeEnvironment> {
        let devenv_dir = if self.runtime.path.is_empty() || self.runtime.path == "." {
            ctx.project_root.to_path_buf()
        } else {
            ctx.project_root.join(&self.runtime.path)
        };

        let (devenv_command, path_prepend) = resolve_devenv_command().await?;

        let hook = Hook {
            order: 10,
            propagate: false,
            command: devenv_command,
            args: vec!["print-dev-env".to_string()],
            dir: Some(devenv_dir.to_string_lossy().to_string()),
            inputs: vec!["devenv.nix".to_string(), "devenv.lock".to_string()],
            source: Some(true),
        };

        let vars =
            capture_source_environment(hook, &HashMap::new(), ctx.timeout_seconds)
                .await
                .map_err(|e| {
                    crate::Error::configuration(format!(
                        "Failed to acquire devenv runtime environment: {e}"
                    ))
                })?;

        Ok(RuntimeEnvironment { vars, path_prepend })
    }
}

/// Resolve the devenv command, installing via `nix profile install` if needed.
///
/// Returns the command string (either `"devenv"` or absolute path) and any
/// directories to prepend to PATH.
async fn resolve_devenv_command() -> Result<(String, Vec<PathBuf>)> {
    if tokio::process::Command::new("devenv")
        .arg("version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Ok(("devenv".to_string(), vec![]));
    }

    tracing::info!("devenv not found, installing via nix profile install");
    let output = tokio::process::Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "profile",
            "install",
            "nixpkgs#devenv",
        ])
        .output()
        .await
        .map_err(|e| {
            crate::Error::configuration(format!("Failed to install devenv: {e}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(crate::Error::configuration(format!(
            "Failed to install devenv: {stderr}"
        )));
    }

    // Return the absolute path to the newly installed devenv binary and the
    // profile bin directory for PATH prepending — avoids mutating global
    // process environment.
    if let Ok(home) = std::env::var("HOME") {
        let profile_bin = PathBuf::from(format!("{home}/.nix-profile/bin"));
        let devenv_bin = profile_bin.join("devenv");
        if devenv_bin.exists() {
            return Ok((
                devenv_bin.to_string_lossy().to_string(),
                vec![profile_bin],
            ));
        }
    }

    // Fallback: assume nix profile install put it somewhere on PATH.
    Ok(("devenv".to_string(), vec![]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{ContainerRuntime, DaggerRuntime, OciRuntime};

    #[test]
    fn nix_runtime_defaults_to_local_flake() {
        let runtime = NixRuntime::default();

        assert_eq!(
            nix_print_dev_env_args(&runtime),
            vec![
                "--extra-experimental-features",
                "nix-command flakes",
                "print-dev-env",
                ".",
            ]
        );
    }

    #[test]
    fn nix_runtime_uses_explicit_output_target() {
        let runtime = NixRuntime {
            flake: "github:example/project".to_string(),
            output: Some("devShells.x86_64-linux.ci".to_string()),
        };

        assert_eq!(
            nix_print_dev_env_args(&runtime),
            vec![
                "--extra-experimental-features",
                "nix-command flakes",
                "print-dev-env",
                "github:example/project#devShells.x86_64-linux.ci",
            ]
        );
    }

    #[test]
    fn devenv_runtime_from_cue_defaults_to_current_dir() {
        let runtime: Runtime =
            serde_json::from_str(r#"{"type":"devenv"}"#).unwrap();
        match runtime {
            Runtime::Devenv(devenv) => assert_eq!(devenv.path, "."),
            _ => panic!("Expected Devenv runtime"),
        }
    }

    #[test]
    fn resolver_for_nix_returns_some() {
        let runtime = Runtime::Nix(NixRuntime::default());
        let resolver = resolver_for_runtime(&runtime);
        assert!(resolver.is_some());
        assert_eq!(resolver.unwrap().name(), "nix");
    }

    #[test]
    fn resolver_for_devenv_returns_some() {
        let runtime = Runtime::Devenv(DevenvRuntime::default());
        let resolver = resolver_for_runtime(&runtime);
        assert!(resolver.is_some());
        assert_eq!(resolver.unwrap().name(), "devenv");
    }

    #[test]
    fn resolver_for_container_returns_none() {
        let runtime = Runtime::Container(ContainerRuntime {
            image: "ubuntu:latest".to_string(),
        });
        assert!(resolver_for_runtime(&runtime).is_none());
    }

    #[test]
    fn resolver_for_dagger_returns_none() {
        let runtime = Runtime::Dagger(DaggerRuntime::default());
        assert!(resolver_for_runtime(&runtime).is_none());
    }

    #[test]
    fn resolver_for_oci_returns_none() {
        let runtime = Runtime::Oci(OciRuntime::default());
        assert!(resolver_for_runtime(&runtime).is_none());
    }

    #[test]
    fn resolver_for_tools_returns_none() {
        let runtime = Runtime::Tools(Box::default());
        assert!(resolver_for_runtime(&runtime).is_none());
    }

    #[test]
    fn runtime_environment_default_is_empty() {
        let env = RuntimeEnvironment::default();
        assert!(env.vars.is_empty());
        assert!(env.path_prepend.is_empty());
    }

    #[test]
    fn merged_path_returns_none_when_empty() {
        let env = RuntimeEnvironment::default();
        assert!(env.merged_path(Some("/usr/bin")).is_none());
    }

    #[test]
    fn merged_path_prepends_to_existing() {
        let env = RuntimeEnvironment {
            vars: HashMap::new(),
            path_prepend: vec![PathBuf::from("/nix/bin")],
        };
        assert_eq!(
            env.merged_path(Some("/usr/bin")),
            Some("/nix/bin:/usr/bin".to_string())
        );
    }

    #[test]
    fn merged_path_sets_when_no_current() {
        let env = RuntimeEnvironment {
            vars: HashMap::new(),
            path_prepend: vec![PathBuf::from("/nix/bin")],
        };
        assert_eq!(
            env.merged_path(None),
            Some("/nix/bin".to_string())
        );
    }

    #[test]
    fn merged_path_preserves_dir_order() {
        let env = RuntimeEnvironment {
            vars: HashMap::new(),
            path_prepend: vec![
                PathBuf::from("/first"),
                PathBuf::from("/second"),
            ],
        };
        assert_eq!(
            env.merged_path(Some("/usr/bin")),
            Some("/first:/second:/usr/bin".to_string())
        );
    }

    #[test]
    fn apply_to_btree_map_sets_vars_and_path() {
        let runtime_env = RuntimeEnvironment {
            vars: HashMap::from([("FOO".to_string(), "bar".to_string())]),
            path_prepend: vec![PathBuf::from("/nix/bin")],
        };
        let mut env = BTreeMap::from([("PATH".to_string(), "/usr/bin".to_string())]);
        runtime_env.apply_to_btree_map(&mut env);
        assert_eq!(env.get("FOO").unwrap(), "bar");
        assert_eq!(env.get("PATH").unwrap(), "/nix/bin:/usr/bin");
    }
}
