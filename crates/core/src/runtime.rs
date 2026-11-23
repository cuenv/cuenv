//! Environment variables contributed by a project's configured runtime.
//!
//! Nix and devenv runtimes supply their development shell environment by
//! running the runtime's own `print-dev-env` command.

use crate::Result;
use crate::manifest::{DevenvRuntime, NixRuntime, Runtime};
use cuenv_hooks::{Hook, SourceEnvironmentOptions};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use tokio::process::Command;

const RUNTIME_ENV_TIMEOUT_SECONDS: u64 = 600;

/// Resolve environment variables provided by the configured runtime.
///
/// Supports `Runtime::Nix` (runs `nix print-dev-env`) and `Runtime::Devenv`
/// (runs `devenv print-dev-env`). Other runtime types return an empty map.
///
/// # Errors
///
/// Returns an error if the configured runtime environment cannot be acquired.
pub async fn resolve_runtime_environment(
    project_root: &Path,
    runtime: Option<&Runtime>,
) -> Result<HashMap<String, String>> {
    resolve_runtime_environment_withheld(project_root, runtime, &BTreeSet::new()).await
}

/// Resolve runtime variables with explicit subprocess environment exclusions.
///
/// # Errors
/// Returns an error if the configured runtime environment cannot be acquired.
pub async fn resolve_runtime_environment_withheld(
    project_root: &Path,
    runtime: Option<&Runtime>,
    withheld_env: &BTreeSet<String>,
) -> Result<HashMap<String, String>> {
    match runtime {
        Some(Runtime::Nix(nix_runtime)) => {
            resolve_nix_runtime_environment(project_root, nix_runtime, withheld_env).await
        }
        Some(Runtime::Devenv(devenv_runtime)) => {
            resolve_devenv_runtime_environment(project_root, devenv_runtime, withheld_env).await
        }
        _ => Ok(HashMap::new()),
    }
}

async fn resolve_nix_runtime_environment(
    project_root: &Path,
    runtime: &NixRuntime,
    withheld_env: &BTreeSet<String>,
) -> Result<HashMap<String, String>> {
    let hook = Hook {
        order: 10,
        propagate: false,
        command: "nix".to_string(),
        args: nix_print_dev_env_args(runtime),
        dir: Some(project_root.to_string_lossy().to_string()),
        inputs: vec!["flake.nix".to_string(), "flake.lock".to_string()],
        source: Some(true),
    };

    capture_runtime_environment(hook, withheld_env)
        .await
        .map_err(|e| {
            crate::Error::configuration(format!("Failed to acquire Nix runtime environment: {e}"))
        })
}

async fn resolve_devenv_runtime_environment(
    project_root: &Path,
    runtime: &DevenvRuntime,
    withheld_env: &BTreeSet<String>,
) -> Result<HashMap<String, String>> {
    let devenv_dir = if runtime.path.is_empty() || runtime.path == "." {
        project_root.to_path_buf()
    } else {
        project_root.join(&runtime.path)
    };

    let devenv_cmd = resolve_devenv_command(withheld_env).await?;

    let hook = Hook {
        order: 10,
        propagate: false,
        command: devenv_cmd,
        args: vec!["print-dev-env".to_string()],
        dir: Some(devenv_dir.to_string_lossy().to_string()),
        inputs: vec!["devenv.nix".to_string(), "devenv.lock".to_string()],
        source: Some(true),
    };

    capture_runtime_environment(hook, withheld_env)
        .await
        .map_err(|e| {
            crate::Error::configuration(format!(
                "Failed to acquire devenv runtime environment: {e}"
            ))
        })
}

async fn capture_runtime_environment(
    hook: Hook,
    withheld_env: &BTreeSet<String>,
) -> cuenv_hooks::Result<HashMap<String, String>> {
    cuenv_hooks::capture_source_environment_with_options(
        hook,
        SourceEnvironmentOptions {
            prior_env: &HashMap::new(),
            timeout_seconds: RUNTIME_ENV_TIMEOUT_SECONDS,
            withheld_env,
        },
    )
    .await
}

/// Resolve devenv command path, installing via `nix profile install` if needed.
///
/// Returns the command string to invoke devenv — either `"devenv"` if already
/// on PATH, or the absolute path to the nix profile binary after installation.
async fn resolve_devenv_command(withheld_env: &BTreeSet<String>) -> Result<String> {
    if runtime_command("devenv", withheld_env)
        .arg("version")
        .output()
        .await
        .is_ok_and(|o| o.status.success())
    {
        return Ok("devenv".to_string());
    }

    tracing::info!("devenv not found, installing via nix profile install");
    let output = runtime_command("nix", withheld_env)
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "profile",
            "install",
            "nixpkgs#devenv",
        ])
        .output()
        .await
        .map_err(|e| crate::Error::configuration(format!("Failed to install devenv: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(crate::Error::configuration(format!(
            "Failed to install devenv: {stderr}"
        )));
    }

    // Return absolute path so we don't need to mutate the process PATH
    if let Ok(home) = std::env::var("HOME") {
        let devenv_path = format!("{home}/.nix-profile/bin/devenv");
        if std::path::Path::new(&devenv_path).exists() {
            return Ok(devenv_path);
        }
    }

    Ok("devenv".to_string())
}

fn runtime_command(program: &str, withheld_env: &BTreeSet<String>) -> Command {
    let mut command = Command::new(program);
    for name in withheld_env {
        command.env_remove(name);
    }
    command
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn devenv_runtime_from_cue_defaults_to_current_dir() {
        // When deserialized from CUE/JSON via the Runtime enum, serde default gives "."
        let runtime: Runtime = serde_json::from_str(r#"{"type":"devenv"}"#).unwrap();
        match runtime {
            Runtime::Devenv(devenv) => assert_eq!(devenv.path, "."),
            _ => panic!("Expected Devenv runtime"),
        }
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

    #[cfg(unix)]
    #[tokio::test]
    async fn runtimes_withhold_cache_credentials()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        const CREDENTIAL: &str = "CUENV_RUNTIME_TEST_CREDENTIAL";
        const SENTINEL: &str = "private-runtime-test-sentinel";
        if std::env::var(CREDENTIAL).as_deref() != Ok(SENTINEL) {
            let programs = tempfile::tempdir()?;
            let stub = r#"#!/bin/sh
test -z "${CUENV_RUNTIME_TEST_CREDENTIAL+x}" || exit 11
case "$*" in
    version) exit 0 ;;
    *print-dev-env*) ;;
    *) exit 12 ;;
esac
cat <<'SCRIPT'
test -z "${CUENV_RUNTIME_TEST_CREDENTIAL+x}" || exit 13
export CUENV_RUNTIME_TEST_VALUE=ready
SCRIPT
"#;
            for name in ["nix", "devenv"] {
                let path = programs.path().join(name);
                std::fs::write(&path, stub)?;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
            }
            let path = std::env::var_os("PATH").unwrap_or_default();
            let path = std::env::join_paths(
                std::iter::once(programs.path().to_path_buf()).chain(std::env::split_paths(&path)),
            )?;
            let child = std::process::Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "runtime::tests::runtimes_withhold_cache_credentials",
                ])
                .env("PATH", path)
                .env(CREDENTIAL, SENTINEL)
                .output()?;
            let output = String::from_utf8_lossy(&child.stdout);
            assert!(
                child.status.success() && output.contains("1 passed"),
                "{output}"
            );
            return Ok(());
        }

        let project = tempfile::tempdir()?;
        let withheld_env = BTreeSet::from([CREDENTIAL.to_owned()]);
        for runtime in [
            Runtime::Nix(NixRuntime::default()),
            Runtime::Devenv(DevenvRuntime::default()),
        ] {
            let environment =
                resolve_runtime_environment_withheld(project.path(), Some(&runtime), &withheld_env)
                    .await?;
            assert_eq!(environment["CUENV_RUNTIME_TEST_VALUE"], "ready");
            assert!(!environment.contains_key(CREDENTIAL));
        }
        assert_eq!(std::env::var(CREDENTIAL)?, SENTINEL);
        Ok(())
    }
}
