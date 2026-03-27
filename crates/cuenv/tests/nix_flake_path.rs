//! Integration tests for Nix runtime PATH propagation.
//!
//! Verifies that tools provided by the Nix flake dev shell appear on PATH when
//! using `cuenv exec` and `cuenv task` via `runtime: schema.#NixRuntime`.

// Integration tests can use unwrap/expect for cleaner assertions
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

/// Create a test directory with a CUE module and a minimal Nix flake
/// that puts `git` on PATH via the dev shell.
fn create_nix_runtime_test_dir() -> TempDir {
    let temp_dir = tempfile::Builder::new()
        .prefix("cuenv_nix_runtime_")
        .tempdir()
        .expect("Failed to create temp directory");
    let path = temp_dir.path();

    fs::create_dir_all(path.join("cue.mod")).unwrap();
    fs::write(
        path.join("cue.mod/module.cue"),
        "module: \"test.example/nix-runtime\"\nlanguage: version: \"v0.9.0\"\n",
    )
    .unwrap();

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
    fs::write(path.join("flake.nix"), flake).unwrap();

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
    fs::write(path.join("env.cue"), cue_content).unwrap();

    temp_dir
}

fn nix_available() -> bool {
    Command::new("nix").arg("--version").output().is_ok()
}

fn common_env(path: &std::path::Path) -> Vec<(&str, std::ffi::OsString)> {
    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");
    let state_dir = path.join(".cuenv-state");
    let cache_dir = path.join(".cuenv-cache");
    let runtime_dir = path.join(".cuenv-runtime");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&cache_dir).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();

    vec![
        ("CUENV_EXECUTABLE", cuenv_bin.into()),
        ("CUENV_FOREGROUND_HOOKS", "1".into()),
        ("CUENV_STATE_DIR", state_dir.into_os_string()),
        ("CUENV_CACHE_DIR", cache_dir.into_os_string()),
        ("CUENV_RUNTIME_DIR", runtime_dir.into_os_string()),
        (
            "NIX_CONFIG",
            "experimental-features = nix-command flakes".into(),
        ),
    ]
}

fn run_cuenv(path: &std::path::Path, args: &[&str]) -> std::process::Output {
    #[allow(deprecated)]
    let mut cmd = Command::cargo_bin("cuenv").unwrap();
    cmd.current_dir(path);
    for (key, value) in common_env(path) {
        cmd.env(key, value);
    }
    for arg in args {
        cmd.arg(arg);
    }
    cmd.output().unwrap()
}

#[test]
fn test_exec_has_nix_tools_from_runtime() {
    if !nix_available() {
        eprintln!("Skipping: nix not available");
        return;
    }
    if std::env::var_os("NEXTEST").is_some() {
        eprintln!("Skipping under nextest");
        return;
    }

    let temp_dir = create_nix_runtime_test_dir();
    let path = temp_dir.path();

    let output = run_cuenv(path, &["exec", "--", "git", "--version"]);
    if output.status.code() == Some(3) {
        eprintln!("FFI not available in sandbox, skipping");
        return;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("git version"),
        "Expected 'git version' in exec output.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_task_has_nix_tools_from_runtime() {
    if !nix_available() {
        eprintln!("Skipping: nix not available");
        return;
    }
    if std::env::var_os("NEXTEST").is_some() {
        eprintln!("Skipping under nextest");
        return;
    }

    let temp_dir = create_nix_runtime_test_dir();
    let path = temp_dir.path();

    let output = run_cuenv(path, &["task", "check-git"]);
    if output.status.code() == Some(3) {
        eprintln!("FFI not available in sandbox, skipping");
        return;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("git version"),
        "Expected 'git version' in task output.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_nix_runtime_does_not_require_hook_approval() {
    if !nix_available() {
        eprintln!("Skipping: nix not available");
        return;
    }
    if std::env::var_os("NEXTEST").is_some() {
        eprintln!("Skipping under nextest");
        return;
    }

    let temp_dir = create_nix_runtime_test_dir();
    let path = temp_dir.path();

    let output = run_cuenv(path, &["exec", "--", "sh", "-c", "which git && which cp"]);
    if output.status.code() == Some(3) {
        eprintln!("FFI not available in sandbox, skipping");
        return;
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
}
