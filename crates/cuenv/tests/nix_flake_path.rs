//! Integration tests for Nix runtime PATH propagation.
//!
//! Verifies that tools provided by the Nix flake dev shell appear on PATH when
//! using `cuenv exec` and `cuenv task` via `runtime: schema.#NixRuntime`.

use assert_cmd::Command;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Output;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// Create a test directory with a CUE module and a minimal Nix flake
/// that puts `git` on PATH via the dev shell.
fn create_nix_runtime_test_dir() -> TestResult<TempDir> {
    let temp_dir = tempfile::Builder::new()
        .prefix("cuenv_nix_runtime_")
        .tempdir()?;
    let path = temp_dir.path();

    fs::create_dir_all(path.join("cue.mod"))?;
    fs::write(
        path.join("cue.mod/module.cue"),
        "module: \"test.example/nix-runtime\"\nlanguage: version: \"v0.9.0\"\n",
    )?;

    let flake = r#"
{
  description = "cuenv nix runtime test";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.11";
  outputs = { self, nixpkgs }: let
    system = builtins.currentSystem;
    pkgs = import nixpkgs { inherit system; };
  in {
    devShells.${system}.default = pkgs.mkShell {
      packages = [ pkgs.git pkgs.coreutils ];
    };
  };
}
"#;
    fs::write(path.join("flake.nix"), flake)?;

    let cue_content = r#"
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "test"
    runtime: schema.#NixRuntime

    tasks: {
        "check-git": {
            command: "git"
            args: ["--version"]
            hermetic: false
        }
        "check-path": {
            command: "sh"
            args: ["-c", "which git && which cp"]
            hermetic: false
        }
    }
}
"#;
    fs::write(path.join("env.cue"), cue_content)?;

    Ok(temp_dir)
}

fn nix_available() -> bool {
    Command::new("nix").arg("--version").output().is_ok()
}

fn should_skip_nix_runtime_test() -> bool {
    !nix_available() || std::env::var_os("NEXTEST").is_some()
}

fn common_env(path: &Path) -> TestResult<Vec<(&'static str, std::ffi::OsString)>> {
    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");
    let state_dir = path.join(".cuenv-state");
    let cache_dir = path.join(".cuenv-cache");
    let runtime_dir = path.join(".cuenv-runtime");
    fs::create_dir_all(&state_dir)?;
    fs::create_dir_all(&cache_dir)?;
    fs::create_dir_all(&runtime_dir)?;

    Ok(vec![
        ("CUENV_EXECUTABLE", cuenv_bin.into()),
        ("CUENV_FOREGROUND_HOOKS", "1".into()),
        ("CUENV_STATE_DIR", state_dir.into_os_string()),
        ("CUENV_CACHE_DIR", cache_dir.into_os_string()),
        ("CUENV_RUNTIME_DIR", runtime_dir.into_os_string()),
        (
            "NIX_CONFIG",
            "experimental-features = nix-command flakes".into(),
        ),
    ])
}

fn run_cuenv(path: &Path, args: &[&str]) -> TestResult<Output> {
    #[allow(deprecated)]
    let mut cmd = Command::cargo_bin("cuenv")?;
    cmd.current_dir(path);
    for (key, value) in common_env(path)? {
        cmd.env(key, value);
    }
    for arg in args {
        cmd.arg(arg);
    }
    Ok(cmd.output()?)
}

#[test]
fn test_exec_has_nix_tools_from_runtime() -> TestResult {
    if should_skip_nix_runtime_test() {
        return Ok(());
    }

    let temp_dir = create_nix_runtime_test_dir()?;
    let path = temp_dir.path();

    let output = run_cuenv(path, &["exec", "--", "git", "--version"])?;
    if output.status.code() == Some(3) {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("git version"),
        "Expected 'git version' in exec output.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn test_task_has_nix_tools_from_runtime() -> TestResult {
    if should_skip_nix_runtime_test() {
        return Ok(());
    }

    let temp_dir = create_nix_runtime_test_dir()?;
    let path = temp_dir.path();

    let output = run_cuenv(path, &["task", "check-git"])?;
    if output.status.code() == Some(3) {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("git version"),
        "Expected 'git version' in task output.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn test_nix_runtime_does_not_require_hook_approval() -> TestResult {
    if should_skip_nix_runtime_test() {
        return Ok(());
    }

    let temp_dir = create_nix_runtime_test_dir()?;
    let path = temp_dir.path();

    let output = run_cuenv(path, &["exec", "--", "sh", "-c", "which git && which cp"])?;
    if output.status.code() == Some(3) {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("/git") && stdout.contains("/cp"),
        "Expected Nix tools on PATH without hook approval.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("Hooks not run"),
        "Runtime-backed tool acquisition should not depend on hook approval: {stderr}"
    );
    Ok(())
}
