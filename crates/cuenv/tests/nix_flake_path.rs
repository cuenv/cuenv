//! Integration tests for Nix runtime PATH propagation.
//!
//! Verifies that tools provided by the Nix flake dev shell appear on PATH when
//! using `cuenv exec` and `cuenv task` via `runtime: schema.#NixRuntime`.
//! Run these tests with a host Nix daemon and `CUENV_TEST_NIXPKGS` set to a
//! locally available nixpkgs flake source.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const PROBE: &str = "cuenv-nix-runtime-probe-6c2d42";
const PROBE_OUTPUT: &str = "cuenv-nix-runtime-probe-ok";

fn copy_schema(source: &Path, destination: &Path) -> TestResult {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source = entry.path();
        let destination = destination.join(entry.file_name());
        if source.is_dir() {
            copy_schema(&source, &destination)?;
        } else if source
            .extension()
            .is_some_and(|extension| extension == "cue")
        {
            fs::copy(source, destination)?;
        }
    }
    Ok(())
}

/// Create a project whose dev shell supplies an executable absent from the host PATH.
fn create_nix_runtime_test_dir() -> TestResult<TempDir> {
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            assert!(
                !directory.join(PROBE).exists(),
                "the test probe must not be available on the inherited PATH"
            );
        }
    }
    let temp_dir = tempfile::Builder::new()
        .prefix("cuenv_nix_runtime_")
        .tempdir()?;
    let path = temp_dir.path();

    fs::create_dir_all(path.join("cue.mod"))?;
    fs::write(
        path.join("cue.mod/module.cue"),
        "module: \"github.com/cuenv/cuenv\"\nlanguage: version: \"v0.14.1\"\n",
    )?;
    let schema = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema");
    copy_schema(&schema, &path.join("schema"))?;

    let nixpkgs = std::env::var("CUENV_TEST_NIXPKGS")?;
    let flake = r#"
{
  description = "cuenv nix runtime test";
  inputs.nixpkgs.url = "path:NIXPKGS_SOURCE";
  outputs = { self, nixpkgs }: {
    devShells = builtins.listToAttrs (map (system: let
      pkgs = import nixpkgs { inherit system; };
      probe = pkgs.writeShellScriptBin "cuenv-nix-runtime-probe-6c2d42" ''
        printf '%s\n' 'cuenv-nix-runtime-probe-ok'
      '';
    in {
      name = system;
      value.default = pkgs.mkShell { packages = [ probe ]; };
    }) [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ]);
  };
}
"#;
    fs::write(
        path.join("flake.nix"),
        flake.replace("NIXPKGS_SOURCE", &nixpkgs),
    )?;

    let cue_content = r#"
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "test"
    runtime: schema.#NixRuntime

    tasks: {
        "check-probe": {
            command: "cuenv-nix-runtime-probe-6c2d42"
            hermetic: false
        }
    }
}
"#;
    fs::write(path.join("env.cue"), cue_content)?;

    Ok(temp_dir)
}

fn common_env(path: &Path) -> TestResult<Vec<(&'static str, std::ffi::OsString)>> {
    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");
    let state_dir = path.join(".cuenv-state");
    let cache_dir = path.join(".cuenv-cache");
    let runtime_dir = path.join(".cuenv-runtime");
    fs::create_dir_all(&state_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700))?;
    }
    fs::create_dir_all(&cache_dir)?;
    fs::create_dir_all(&runtime_dir)?;

    Ok(vec![
        ("HOME", path.as_os_str().to_owned()),
        ("XDG_STATE_HOME", state_dir.as_os_str().to_owned()),
        ("CUENV_EXECUTABLE", cuenv_bin.into()),
        (
            "CUENV_HOOK_SESSION",
            uuid::Uuid::new_v4().to_string().into(),
        ),
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
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cuenv"));
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
#[ignore = "requires a host Nix daemon and CUENV_TEST_NIXPKGS"]
fn test_exec_has_nix_tools_from_runtime() -> TestResult {
    let temp_dir = create_nix_runtime_test_dir()?;
    let path = temp_dir.path();

    let output = run_cuenv(path, &["exec", "--", PROBE])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.trim() == PROBE_OUTPUT,
        "Expected the Nix-provided probe in exec output.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
#[ignore = "requires a host Nix daemon and CUENV_TEST_NIXPKGS"]
fn test_task_has_nix_tools_from_runtime() -> TestResult {
    let temp_dir = create_nix_runtime_test_dir()?;
    let path = temp_dir.path();

    let output = run_cuenv(path, &["task", "check-probe"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(PROBE_OUTPUT),
        "Expected the Nix-provided probe in task output.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
#[ignore = "requires a host Nix daemon and CUENV_TEST_NIXPKGS"]
fn test_nix_runtime_does_not_require_hook_approval() -> TestResult {
    let temp_dir = create_nix_runtime_test_dir()?;
    let path = temp_dir.path();

    let output = run_cuenv(
        path,
        &[
            "exec",
            "--",
            "sh",
            "-c",
            "command -v cuenv-nix-runtime-probe-6c2d42 && cuenv-nix-runtime-probe-6c2d42",
        ],
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("/nix/store/") && stdout.contains(PROBE_OUTPUT),
        "Expected the Nix-provided probe on PATH without hook approval.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("Hooks not run"),
        "Runtime-backed tool acquisition should not depend on hook approval: {stderr}"
    );
    Ok(())
}
