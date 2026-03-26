//! Integration tests for NixFlake hook PATH propagation.
//!
//! Verifies that tools provided by the Nix flake dev shell appear on PATH
//! when using `cuenv exec` and `cuenv task`. Also documents that hook approval
//! is required — without it, Nix tools are NOT available.

// Integration tests can use unwrap/expect for cleaner assertions
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

/// Create a test directory with a CUE module and a minimal Nix flake
/// that puts `git` on PATH via the dev shell.
fn create_nix_flake_test_dir() -> TempDir {
    let temp_dir = tempfile::Builder::new()
        .prefix("cuenv_nix_path_")
        .tempdir()
        .expect("Failed to create temp directory");
    let path = temp_dir.path();

    // CUE module
    fs::create_dir_all(path.join("cue.mod")).unwrap();
    fs::write(
        path.join("cue.mod/module.cue"),
        "module: \"test.example/nix-path\"\nlanguage: version: \"v0.9.0\"\n",
    )
    .unwrap();

    // Minimal flake that provides git
    let flake = r#"
{
  description = "cuenv nix path test";
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

    // env.cue with NixFlake hook and a simple task
    let cue_content = r#"
package cuenv

name: "test"

hooks: onEnter: nix: {
    order:     10
    propagate: false
    command:   "nix"
    args: ["--extra-experimental-features", "nix-command flakes", "print-dev-env"]
    source:    true
    inputs: ["flake.nix", "flake.lock"]
}

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
    for (k, v) in common_env(path) {
        cmd.env(k, v);
    }
    for arg in args {
        cmd.arg(arg);
    }
    cmd.output().unwrap()
}

/// With hooks approved, `cuenv exec` should have Nix-provided git on PATH.
#[test]
fn test_exec_has_nix_tools_when_approved() {
    if !nix_available() {
        eprintln!("Skipping: nix not available");
        return;
    }
    if std::env::var_os("NEXTEST").is_some() {
        eprintln!("Skipping under nextest");
        return;
    }

    let temp_dir = create_nix_flake_test_dir();
    let path = temp_dir.path();

    // Approve hooks
    let allow_output = run_cuenv(path, &["allow", "--yes"]);
    if allow_output.status.code() == Some(3) {
        eprintln!("FFI not available in sandbox, skipping");
        return;
    }
    assert!(
        allow_output.status.success(),
        "cuenv allow failed: {}",
        String::from_utf8_lossy(&allow_output.stderr)
    );

    // Exec: check git is on PATH
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

/// With hooks approved, `cuenv task` should have Nix-provided git on PATH.
#[test]
fn test_task_has_nix_tools_when_approved() {
    if !nix_available() {
        eprintln!("Skipping: nix not available");
        return;
    }
    if std::env::var_os("NEXTEST").is_some() {
        eprintln!("Skipping under nextest");
        return;
    }

    let temp_dir = create_nix_flake_test_dir();
    let path = temp_dir.path();

    // Approve hooks
    let allow_output = run_cuenv(path, &["allow", "--yes"]);
    if allow_output.status.code() == Some(3) {
        eprintln!("FFI not available in sandbox, skipping");
        return;
    }
    assert!(
        allow_output.status.success(),
        "cuenv allow failed: {}",
        String::from_utf8_lossy(&allow_output.stderr)
    );

    // Task: check-git should find git
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

/// Without hook approval, Nix tools are NOT on PATH.
/// This documents the current limitation that is causing CI failures.
#[test]
fn test_no_nix_tools_without_approval() {
    if !nix_available() {
        eprintln!("Skipping: nix not available");
        return;
    }
    if std::env::var_os("NEXTEST").is_some() {
        eprintln!("Skipping under nextest");
        return;
    }

    let temp_dir = create_nix_flake_test_dir();
    let path = temp_dir.path();

    // Do NOT approve hooks

    // Exec: try to run git — should fail or show warning about hooks
    let output = run_cuenv(path, &["exec", "--", "git", "--version"]);
    if output.status.code() == Some(3) {
        eprintln!("FFI not available in sandbox, skipping");
        return;
    }

    let stderr = String::from_utf8_lossy(&output.stderr);

    // The command should either fail (git not found) or show the hooks warning
    // Either way, the Nix-provided git should NOT be available
    let hooks_not_run = stderr.contains("Hooks not run");
    let command_failed = !output.status.success();

    assert!(
        hooks_not_run || command_failed,
        "Expected hooks warning or command failure without approval.\nstdout: {}\nstderr: {stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
}
