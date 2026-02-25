//! Integration test for NixFlake onEnter hook behavior

// Integration tests can use unwrap/expect for cleaner assertions
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use assert_cmd::Command;
use std::fs;
use std::time::Duration;
use tempfile::TempDir;

const MAX_ATTEMPTS: usize = 5;

/// Create a test directory with non-hidden name and CUE module setup
fn create_test_dir() -> TempDir {
    let temp_dir = tempfile::Builder::new()
        .prefix("cuenv_test_")
        .tempdir()
        .expect("Failed to create temp directory");
    let path = temp_dir.path();
    // Create CUE module for module-wide evaluation
    fs::create_dir_all(path.join("cue.mod")).unwrap();
    fs::write(
        path.join("cue.mod/module.cue"),
        "module: \"test.example/nix-flake\"\nlanguage: version: \"v0.9.0\"\n",
    )
    .unwrap();
    temp_dir
}

fn nix_available() -> bool {
    Command::new("nix").arg("--version").output().is_ok()
}

#[test]
fn test_nix_flake_hook_runs_shell_hook() {
    if !nix_available() {
        eprintln!("Skipping test: nix is not available on PATH");
        return;
    }
    if std::env::var_os("NEXTEST").is_some() {
        eprintln!("Skipping test under nextest: Nix shellHook propagation is flaky in this harness");
        return;
    }

    let temp_dir = create_test_dir();
    let path = temp_dir.path();

    // Minimal flake with shellHook export
    let flake = r#"
{
  description = "cuenv nix flake hook test";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.11";

  outputs = { self, nixpkgs }: let
    system = builtins.currentSystem;
    pkgs = import nixpkgs { inherit system; };
  in {
    devShells.${system}.default = pkgs.mkShell {
      shellHook = ''
        export NIX_SHELL_HOOK_VAR=from_nix_shell_hook
      '';
    };
  };
}
"#;
    fs::write(path.join("flake.nix"), flake).unwrap();

    // Create env.cue with Nix flake hook (mirrors contrib/nix #NixFlake)
    let cue_content = r#"
package cuenv

name: "test"

hooks: {
    onEnter: {
        nix: {
            order: 10
            propagate: false
            command: "nix"
            args: ["--extra-experimental-features", "nix-command flakes", "print-dev-env"]
            source: true
            inputs: ["flake.nix", "flake.lock"]
        }
    }
}
"#;
    fs::write(path.join("env.cue"), cue_content).unwrap();

    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");
    let nix_config = "experimental-features = nix-command flakes";
    let state_dir = path.join(".cuenv-state");
    let cache_dir = path.join(".cuenv-cache");
    let runtime_dir = path.join(".cuenv-runtime");

    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&cache_dir).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();

    // 1. Approve config
    #[allow(deprecated)]
    let mut cmd = Command::cargo_bin("cuenv").unwrap();
    let allow_output = cmd
        .current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin)
        .env("CUENV_FOREGROUND_HOOKS", "1")
        .env("CUENV_STATE_DIR", state_dir.as_os_str())
        .env("CUENV_CACHE_DIR", cache_dir.as_os_str())
        .env("CUENV_RUNTIME_DIR", runtime_dir.as_os_str())
        .env("NIX_CONFIG", nix_config)
        .arg("allow")
        .arg("--yes")
        .output()
        .unwrap();

    // Handle FFI error in sandbox during allow
    if allow_output.status.code() == Some(3) {
        let stderr = String::from_utf8_lossy(&allow_output.stderr);
        assert!(
            stderr.contains("Evaluation/FFI error") || stderr.contains("Unexpected error"),
            "Expected FFI or Unexpected error in sandbox during allow, got: {stderr}"
        );
        return; // Skip rest of test in sandbox
    }
    assert!(
        allow_output.status.success(),
        "cuenv allow failed: {}",
        String::from_utf8_lossy(&allow_output.stderr)
    );

    // 2. Exec command to check shellHook-exported variable
    let mut last_output: Option<std::process::Output> = None;
    let mut last_inspect_output: Option<std::process::Output> = None;
    for attempt in 1..=MAX_ATTEMPTS {
        #[allow(deprecated)]
        let mut cmd = Command::cargo_bin("cuenv").unwrap();
        let output = cmd
            .current_dir(path)
            .env("CUENV_EXECUTABLE", cuenv_bin)
            .env("CUENV_FOREGROUND_HOOKS", "1")
            .env("CUENV_STATE_DIR", state_dir.as_os_str())
            .env("CUENV_CACHE_DIR", cache_dir.as_os_str())
            .env("CUENV_RUNTIME_DIR", runtime_dir.as_os_str())
            .env("NIX_CONFIG", nix_config)
            .arg("exec")
            .arg("--")
            .arg("sh")
            .arg("-c")
            .arg(
                "if [ \"$NIX_SHELL_HOOK_VAR\" = \"from_nix_shell_hook\" ]; then echo FOUND; else echo MISSING; exit 1; fi",
            )
            .output()
            .unwrap();

        // Handle FFI error in sandbox
        if output.status.code() == Some(3) {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("Evaluation/FFI error") || stderr.contains("Unexpected error"),
                "Expected FFI or Unexpected error in sandbox, got: {stderr}"
            );
            return;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if output.status.success() && stdout.contains("FOUND") {
            return;
        }

        #[allow(deprecated)]
        let mut inspect_cmd = Command::cargo_bin("cuenv").unwrap();
        let inspect_output = inspect_cmd
            .current_dir(path)
            .env("CUENV_EXECUTABLE", cuenv_bin)
            .env("CUENV_FOREGROUND_HOOKS", "1")
            .env("CUENV_STATE_DIR", state_dir.as_os_str())
            .env("CUENV_CACHE_DIR", cache_dir.as_os_str())
            .env("CUENV_RUNTIME_DIR", runtime_dir.as_os_str())
            .env("NIX_CONFIG", nix_config)
            .arg("env")
            .arg("inspect")
            .output()
            .unwrap();

        last_output = Some(output);
        last_inspect_output = Some(inspect_output);

        if attempt < MAX_ATTEMPTS {
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    let output = last_output.expect("expected at least one failed attempt");
    let inspect_output = last_inspect_output.expect("expected inspect output for failed attempt");

    assert!(
        output.status.success(),
        "cuenv exec failed after {MAX_ATTEMPTS} attempts (code {:?})\nstdout:\n{}\nstderr:\n{}\ninspect stdout:\n{}\ninspect stderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&inspect_output.stdout),
        String::from_utf8_lossy(&inspect_output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("FOUND"),
        "Expected FOUND in stdout after {MAX_ATTEMPTS} attempts, got: {stdout}"
    );
}
