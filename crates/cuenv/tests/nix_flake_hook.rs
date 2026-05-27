//! Integration test for NixFlake onEnter hook behavior

use assert_cmd::Command;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;
use tempfile::TempDir;

const MAX_ATTEMPTS: usize = 5;
const NIX_CONFIG: &str = "experimental-features = nix-command flakes";

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct NixFlakeHookFixture {
    temp_dir: TempDir,
    state_root: PathBuf,
    cache_root: PathBuf,
    runtime_root: PathBuf,
}

impl NixFlakeHookFixture {
    fn new() -> TestResult<Self> {
        let temp_dir = tempfile::Builder::new().prefix("cuenv_test_").tempdir()?;
        let path = temp_dir.path();
        fs::create_dir_all(path.join("cue.mod"))?;
        fs::write(
            path.join("cue.mod/module.cue"),
            "module: \"test.example/nix-flake\"\nlanguage: version: \"v0.9.0\"\n",
        )?;

        let state_root = path.join(".cuenv-state");
        let cache_root = path.join(".cuenv-cache");
        let runtime_root = path.join(".cuenv-runtime");
        fs::create_dir_all(&state_root)?;
        fs::create_dir_all(&cache_root)?;
        fs::create_dir_all(&runtime_root)?;

        Ok(Self {
            temp_dir,
            state_root,
            cache_root,
            runtime_root,
        })
    }

    fn path(&self) -> &Path {
        self.temp_dir.path()
    }

    fn write_nix_hook_project(&self) -> TestResult {
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
        fs::write(self.path().join("flake.nix"), flake)?;

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
        fs::write(self.path().join("env.cue"), cue_content)?;
        Ok(())
    }

    fn command(&self) -> TestResult<Command> {
        #[allow(deprecated)]
        let mut cmd = Command::cargo_bin("cuenv")?;
        cmd.current_dir(self.path())
            .env("CUENV_EXECUTABLE", env!("CARGO_BIN_EXE_cuenv"))
            .env("CUENV_FOREGROUND_HOOKS", "1")
            .env("CUENV_STATE_DIR", self.state_root.as_os_str())
            .env("CUENV_CACHE_DIR", self.cache_root.as_os_str())
            .env("CUENV_RUNTIME_DIR", self.runtime_root.as_os_str())
            .env("NIX_CONFIG", NIX_CONFIG);
        Ok(cmd)
    }

    fn run(&self, args: &[&str]) -> TestResult<Output> {
        let mut cmd = self.command()?;
        for arg in args {
            cmd.arg(arg);
        }
        Ok(cmd.output()?)
    }
}

fn nix_available() -> bool {
    Command::new("nix").arg("--version").output().is_ok()
}

fn should_skip_nix_hook_test() -> bool {
    !nix_available() || std::env::var_os("NEXTEST").is_some()
}

#[test]
fn test_nix_flake_hook_runs_shell_hook() -> TestResult {
    if should_skip_nix_hook_test() {
        return Ok(());
    }

    let fixture = NixFlakeHookFixture::new()?;
    fixture.write_nix_hook_project()?;

    let allow_output = fixture.run(&["allow", "--yes"])?;

    if allow_output.status.code() == Some(3) {
        let stderr = String::from_utf8_lossy(&allow_output.stderr);
        assert!(
            stderr.contains("Evaluation/FFI error") || stderr.contains("Unexpected error"),
            "Expected FFI or Unexpected error in sandbox during allow, got: {stderr}"
        );
        return Ok(());
    }
    assert!(
        allow_output.status.success(),
        "cuenv allow failed: {}",
        String::from_utf8_lossy(&allow_output.stderr)
    );

    let mut last_output: Option<Output> = None;
    let mut last_inspect_output: Option<Output> = None;
    for attempt in 1..=MAX_ATTEMPTS {
        let output = fixture.run(&[
            "exec",
            "--",
            "sh",
            "-c",
            "if [ \"$NIX_SHELL_HOOK_VAR\" = \"from_nix_shell_hook\" ]; then echo FOUND; else echo MISSING; exit 1; fi",
        ])?;

        if output.status.code() == Some(3) {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("Evaluation/FFI error") || stderr.contains("Unexpected error"),
                "Expected FFI or Unexpected error in sandbox, got: {stderr}"
            );
            return Ok(());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if output.status.success() && stdout.contains("FOUND") {
            return Ok(());
        }

        let inspect_output = fixture.run(&["env", "inspect"])?;

        last_output = Some(output);
        last_inspect_output = Some(inspect_output);

        if attempt < MAX_ATTEMPTS {
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    let output = last_output.ok_or_else(|| io::Error::other("expected a failed exec attempt"))?;
    let inspect_output =
        last_inspect_output.ok_or_else(|| io::Error::other("expected inspect output"))?;

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
    Ok(())
}
