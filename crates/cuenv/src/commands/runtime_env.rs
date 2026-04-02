use cuenv_core::Result;
use cuenv_core::manifest::{DevenvRuntime, NixRuntime, Runtime};
use cuenv_hooks::{Hook, capture_source_environment};
use std::collections::HashMap;
use std::path::Path;

const RUNTIME_ENV_TIMEOUT_SECONDS: u64 = 600;

/// Resolve environment variables provided by the configured runtime.
///
/// # Errors
///
/// Returns an error if the configured runtime environment cannot be acquired.
pub async fn resolve_runtime_environment(
    project_root: &Path,
    runtime: Option<&Runtime>,
) -> Result<HashMap<String, String>> {
    match runtime {
        Some(Runtime::Nix(nix_runtime)) => {
            resolve_nix_runtime_environment(project_root, nix_runtime).await
        }
        Some(Runtime::Devenv(devenv_runtime)) => {
            resolve_devenv_runtime_environment(project_root, devenv_runtime).await
        }
        _ => Ok(HashMap::new()),
    }
}

async fn resolve_nix_runtime_environment(
    project_root: &Path,
    runtime: &NixRuntime,
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

    capture_source_environment(hook, &HashMap::new(), RUNTIME_ENV_TIMEOUT_SECONDS)
        .await
        .map_err(|e| {
            cuenv_core::Error::configuration(format!(
                "Failed to acquire Nix runtime environment: {e}"
            ))
        })
}

async fn resolve_devenv_runtime_environment(
    project_root: &Path,
    runtime: &DevenvRuntime,
) -> Result<HashMap<String, String>> {
    let devenv_dir = if runtime.path.is_empty() || runtime.path == "." {
        project_root.to_path_buf()
    } else {
        project_root.join(&runtime.path)
    };

    let devenv_cmd = resolve_devenv_command().await?;

    let hook = Hook {
        order: 10,
        propagate: false,
        command: devenv_cmd,
        args: vec!["print-dev-env".to_string()],
        dir: Some(devenv_dir.to_string_lossy().to_string()),
        inputs: vec!["devenv.nix".to_string(), "devenv.lock".to_string()],
        source: Some(true),
    };

    capture_source_environment(hook, &HashMap::new(), RUNTIME_ENV_TIMEOUT_SECONDS)
        .await
        .map_err(|e| {
            cuenv_core::Error::configuration(format!(
                "Failed to acquire devenv runtime environment: {e}"
            ))
        })
}

/// Resolve devenv command path, installing via `nix profile install` if needed.
///
/// Returns the command string to invoke devenv — either `"devenv"` if already
/// on PATH, or the absolute path to the nix profile binary after installation.
async fn resolve_devenv_command() -> Result<String> {
    if tokio::process::Command::new("devenv")
        .arg("version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Ok("devenv".to_string());
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
            cuenv_core::Error::configuration(format!("Failed to install devenv: {e}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(cuenv_core::Error::configuration(format!(
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
        let runtime: Runtime =
            serde_json::from_str(r#"{"type":"devenv"}"#).unwrap();
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
}
