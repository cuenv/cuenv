//! Integration test for NixFlake onEnter hook behavior.
//!
//! Run with a host Nix daemon and `CUENV_TEST_NIXPKGS` set to a locally
//! available nixpkgs flake source.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

const NIX_CONFIG: &str = "experimental-features = nix-command flakes";

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct NixFlakeHookFixture {
    temp_dir: TempDir,
    state_root: PathBuf,
    cache_root: PathBuf,
    runtime_root: PathBuf,
    session: uuid::Uuid,
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700))?;
        }
        fs::create_dir_all(&cache_root)?;
        fs::create_dir_all(&runtime_root)?;

        Ok(Self {
            temp_dir,
            state_root,
            cache_root,
            runtime_root,
            session: uuid::Uuid::new_v4(),
        })
    }

    fn path(&self) -> &Path {
        self.temp_dir.path()
    }

    fn write_nix_hook_project(&self) -> TestResult {
        let nixpkgs = std::env::var("CUENV_TEST_NIXPKGS")?;
        let flake = r#"
{
  description = "cuenv nix flake hook test";

  inputs.nixpkgs.url = "path:NIXPKGS_SOURCE";

  outputs = { self, nixpkgs }: {
    devShells = builtins.listToAttrs (map (system: {
      name = system;
      value.default = (import nixpkgs { inherit system; }).mkShell {
        shellHook = ''
          export NIX_SHELL_HOOK_VAR=from_nix_shell_hook
        '';
      };
    }) [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ]);
  };
}
"#;
        fs::write(
            self.path().join("flake.nix"),
            flake.replace("NIXPKGS_SOURCE", &nixpkgs),
        )?;
        let lock = Command::new("nix")
            .current_dir(self.path())
            .env("HOME", self.path())
            .env("NIX_CONFIG", NIX_CONFIG)
            .args(["flake", "lock", "--offline"])
            .output()?;
        assert_success(&lock, "nix flake lock --offline");

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

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cuenv"));
        cmd.current_dir(self.path())
            .env("HOME", self.path())
            .env("XDG_STATE_HOME", self.state_root.as_os_str())
            .env("CUENV_EXECUTABLE", env!("CARGO_BIN_EXE_cuenv"))
            .env("CUENV_HOOK_SESSION", self.session.to_string())
            .env("CUENV_STATE_DIR", self.state_root.as_os_str())
            .env("CUENV_CACHE_DIR", self.cache_root.as_os_str())
            .env("CUENV_RUNTIME_DIR", self.runtime_root.as_os_str())
            .env("NIX_CONFIG", NIX_CONFIG);
        cmd
    }

    fn run(&self, args: &[&str]) -> TestResult<Output> {
        let mut cmd = self.command();
        for arg in args {
            cmd.arg(arg);
        }
        Ok(cmd.output()?)
    }
}

#[test]
#[ignore = "requires a host Nix daemon and CUENV_TEST_NIXPKGS"]
fn test_nix_flake_hook_runs_shell_hook() -> TestResult {
    let fixture = NixFlakeHookFixture::new()?;
    fixture.write_nix_hook_project()?;

    let allow_output = fixture.run(&["allow", "--yes"])?;

    assert!(
        allow_output.status.success(),
        "cuenv allow failed: {}",
        String::from_utf8_lossy(&allow_output.stderr)
    );

    let load = fixture.run(&["env", "load"])?;
    assert_success(&load, "cuenv env load");
    let status = fixture.run(&[
        "env",
        "status",
        "--wait",
        "--timeout",
        "120",
        "--output",
        "short",
    ])?;
    assert_success(&status, "cuenv env status --wait");
    assert_eq!(
        String::from_utf8_lossy(&status.stdout).trim(),
        "[OK]",
        "hook execution did not complete successfully; stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    let exports = fixture.run(&["export", "--shell", "bash"])?;
    assert_success(&exports, "cuenv export");
    let script = format!(
        "{}\nprintf '%s' \"${{NIX_SHELL_HOOK_VAR-}}\"",
        String::from_utf8(exports.stdout)?
    );
    let output = Command::new("bash")
        .current_dir(fixture.path())
        .env_remove("NIX_SHELL_HOOK_VAR")
        .arg("-c")
        .arg(script)
        .output()?;
    assert_success(&output, "applying cuenv's shell exports");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "from_nix_shell_hook",
        "Nix shellHook was not applied to the exported environment"
    );
    Ok(())
}

fn assert_success(output: &Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation} failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
