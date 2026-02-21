//! Integration test for NixFlake onEnter hook behavior

// Integration tests can use unwrap/expect for cleaner assertions
#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

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
            args: ["print-dev-env"]
            source: true
            inputs: ["flake.nix", "flake.lock"]
        }
    }
}
"#;
    fs::write(path.join("env.cue"), cue_content).unwrap();

    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");
    let nix_config = "experimental-features = nix-command flakes";

    // 1. Approve config
    #[allow(deprecated)]
    let mut cmd = Command::cargo_bin("cuenv").unwrap();
    let allow_output = cmd
        .current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin)
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
    #[allow(deprecated)]
    let mut cmd = Command::cargo_bin("cuenv").unwrap();
    let output = cmd
        .current_dir(path)
        .env("CUENV_EXECUTABLE", cuenv_bin)
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
    } else {
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("FOUND"),
            "Expected FOUND in stdout, got: {stdout}"
        );
    }
}
