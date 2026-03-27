use cuenv_core::Result;
use cuenv_core::manifest::{NixRuntime, Runtime};
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
