//! Verify captured hook values stay out of cuenv-owned files.

#![cfg(unix)]

use std::error::Error;
use std::fs::{self, DirBuilder};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

type TestResult = Result<(), Box<dyn Error>>;

struct Fixture {
    root: tempfile::TempDir,
    project: PathBuf,
    state: PathBuf,
    session: uuid::Uuid,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let project = root.path().join("project");
        let state = root.path().join("state");
        for path in [&project, &state, &root.path().join("home")] {
            DirBuilder::new().mode(0o700).create(path)?;
        }
        fs::create_dir(project.join("cue.mod"))?;
        fs::write(
            project.join("cue.mod/module.cue"),
            "module: \"test.example/hook-privacy\"\nlanguage: version: \"v0.9.0\"\n",
        )?;
        let script = "printf 'export CUENV_CAPTURED_SECRET=%s\\n' \"$PRIVACY_CREDENTIAL\"";
        fs::write(
            project.join("env.cue"),
            format!(
                "package cuenv\nname: \"privacy\"\nhooks: onEnter: private: {{command: \"sh\", args: [\"-c\", {}], source: true}}\n",
                serde_json::to_string(script)?
            ),
        )?;
        Ok(Self {
            root,
            project,
            state,
            session: uuid::Uuid::new_v4(),
        })
    }

    fn run(&self, args: &[&str], sentinel: &str) -> Result<Output, Box<dyn Error>> {
        let binary = env!("CARGO_BIN_EXE_cuenv");
        let mut command = Command::new(binary);
        command
            .current_dir(&self.project)
            .env("HOME", self.root.path().join("home"))
            .env("XDG_STATE_HOME", self.root.path().join("state-home"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env("CUENV_STATE_DIR", &self.state)
            .env(
                "CUENV_APPROVAL_FILE",
                self.root.path().join("approved.json"),
            )
            .env("CUENV_EXECUTABLE", binary)
            .env("CUENV_HOOK_SESSION", self.session.to_string())
            .env("PRIVACY_CREDENTIAL", sentinel)
            .args(args);
        for variable in [
            "CI",
            "GITHUB_ACTIONS",
            "GITLAB_CI",
            "BUILDKITE",
            "JENKINS_URL",
            "CIRCLECI",
            "TRAVIS",
            "BITBUCKET_PIPELINES",
            "AZURE_PIPELINES",
            "TF_BUILD",
            "DRONE",
            "TEAMCITY_VERSION",
            "CUENV_MANAGED_VARS",
            "CUENV_LOADED_DIR",
        ] {
            command.env_remove(variable);
        }
        Ok(command.output()?)
    }
}

fn assert_no_value_in_files(root: &Path, sentinel: &str) -> TestResult {
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let bytes = fs::read(entry.path())?;
            assert!(
                !bytes
                    .windows(sentinel.len())
                    .any(|part| part == sentinel.as_bytes()),
                "captured value appeared in {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn captured_secret_is_shared_in_memory_without_a_state_file_copy() -> TestResult {
    let fixture = Fixture::new()?;
    let sentinel = "cuenv-runtime-secret-sentinel-816f0c";
    assert!(fixture.run(&["allow", "--yes"], sentinel)?.status.success());
    assert!(fixture.run(&["env", "load"], sentinel)?.status.success());
    let status = fixture.run(
        &[
            "env",
            "status",
            "--wait",
            "--timeout",
            "15",
            "--output",
            "short",
        ],
        sentinel,
    )?;
    assert!(status.status.success());
    assert_eq!(String::from_utf8_lossy(&status.stdout).trim(), "[OK]");
    let export = fixture.run(&["export", "--shell", "bash"], sentinel)?;
    assert!(export.status.success());
    assert!(String::from_utf8_lossy(&export.stdout).contains(sentinel));
    assert_no_value_in_files(fixture.root.path(), sentinel)?;

    let manager = cuenv_hooks::StateManager::new(fixture.state.clone());
    assert!(manager.cleanup_state_directory().await? >= 1);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mut leases_remain = false;
            for entry in fs::read_dir(&fixture.state)? {
                if entry?.file_name().to_string_lossy().starts_with(".broker-") {
                    leases_remain = true;
                    break;
                }
            }
            if !leases_remain {
                return Ok::<(), std::io::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    Ok(())
}
