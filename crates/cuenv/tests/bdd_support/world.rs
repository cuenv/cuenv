use super::StepResult;
use cucumber::World;
use cucumber::codegen::anyhow::{Context, anyhow};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;
use tokio::fs;
use tokio::process::Command;

const TEST_CUE_MODULE: &str = "module: \"test.example\"\nlanguage: version: \"v0.14.1\"\n";

/// The test world holds state across test steps
#[derive(Debug, World)]
#[world(init = Self::new)]
pub struct TestWorld {
    /// Current working directory for the test
    pub(super) current_dir: PathBuf,
    /// Environment variables set during test
    pub(super) env_vars: HashMap<String, String>,
    /// Last command output
    pub(super) last_output: String,
    /// Last command exit status
    pub(super) last_exit_code: i32,
    /// Path to cuenv binary
    pub(super) cuenv_binary: PathBuf,
    /// Simulated shell environment
    pub(super) shell_env: HashMap<String, String>,
    /// Whether hooks are currently running
    pub(super) hooks_running: bool,
    /// Hook execution state directory
    pub(super) state_dir: PathBuf,
    /// Unique test base directory for this scenario
    pub(super) test_base_dir: Option<PathBuf>,
}

impl TestWorld {
    async fn new() -> StepResult<Self> {
        // Resolve the cuenv binary path, preferring an already built binary
        let cuenv_binary = if let Ok(path) = std::env::var("CUENV_TEST_BIN") {
            PathBuf::from(path)
        } else if let Some(bin_path) = option_env!("CARGO_BIN_EXE_cuenv") {
            PathBuf::from(bin_path)
        } else {
            Self::repo_root()?.join("target/debug/cuenv")
        };

        // Build the cuenv binary only if it does not already exist
        if !cuenv_binary.exists() {
            let output = Command::new("cargo")
                .args(["build", "--bin", "cuenv"])
                .output()
                .await
                .context("failed to build cuenv binary")?;

            if !output.status.success() {
                return Err(anyhow!(
                    "failed to build cuenv binary: status={:?}, stdout={}, stderr={}",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }

        // Use a persistent directory in temp dir that won't be cleaned up during the test
        // This ensures the supervisor can write to it
        let state_base = std::env::temp_dir().join(format!("cuenv_test_{}", uuid::Uuid::new_v4()));
        let state_dir = state_base.join(".cuenv/state");
        std::fs::create_dir_all(&state_dir)
            .with_context(|| format!("failed to create state dir {}", state_dir.display()))?;

        Ok(Self {
            current_dir: std::env::current_dir().context("failed to read current directory")?,
            env_vars: HashMap::new(),
            last_output: String::new(),
            last_exit_code: 0,
            cuenv_binary,
            shell_env: HashMap::new(),
            hooks_running: false,
            state_dir,
            test_base_dir: None,
        })
    }

    pub(super) fn repo_root() -> StepResult<PathBuf> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let crates_dir = manifest_dir
            .parent()
            .ok_or_else(|| anyhow!("cuenv crate directory has no parent"))?;
        let repo_root = crates_dir
            .parent()
            .ok_or_else(|| anyhow!("crates directory has no parent"))?;
        Ok(repo_root.to_path_buf())
    }

    pub(super) fn path_arg(path: &Path) -> StepResult<String> {
        path.to_str()
            .map(ToOwned::to_owned)
            .ok_or_else(|| anyhow!("path is not valid UTF-8: {}", path.display()))
    }

    fn state_root(&self) -> &Path {
        self.state_dir.parent().unwrap_or(&self.state_dir)
    }

    pub(super) fn state_file(&self, name: &str) -> PathBuf {
        self.state_root().join(name)
    }

    pub(super) fn current_dir_arg(&self) -> StepResult<String> {
        Self::path_arg(&self.current_dir)
    }

    pub(super) fn scenario_path(&self, dir: &str) -> StepResult<PathBuf> {
        if let Some(base_dir) = &self.test_base_dir {
            Ok(base_dir.join(dir))
        } else {
            let parent = self
                .current_dir
                .parent()
                .ok_or_else(|| anyhow!("current directory has no parent"))?;
            Ok(parent.join(dir))
        }
    }

    /// Run cuenv command with arguments
    pub(super) async fn run_cuenv<I, S>(&mut self, args: I) -> StepResult
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let approval_file = self.state_root().join("approved.json");
        let mut cmd = Command::new(&self.cuenv_binary);
        // Clear inherited env vars to prevent CI environment from affecting tests
        cmd.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .env("USER", std::env::var("USER").unwrap_or_default())
            .args(args)
            .current_dir(&self.current_dir)
            .env("CUENV_STATE_DIR", &self.state_dir)
            .env("CUENV_APPROVAL_FILE", approval_file)
            .env("CUENV_EXECUTABLE", &self.cuenv_binary); // Set path for supervisor spawning

        // Add shell environment variables
        for (key, value) in &self.shell_env {
            cmd.env(key, value);
        }

        let output = cmd
            .output()
            .await
            .with_context(|| format!("failed to run {}", self.cuenv_binary.display()))?;

        self.last_output = String::from_utf8_lossy(&output.stdout).to_string()
            + &String::from_utf8_lossy(&output.stderr);
        self.last_exit_code = output.status.code().unwrap_or(-1);

        Ok(())
    }

    /// Simulate shell environment variable loading
    pub(super) fn load_env_vars(&mut self, vars: HashMap<String, String>) {
        self.shell_env.extend(vars);
    }

    pub(super) async fn write_test_project(
        &mut self,
        prefix: &str,
        cue_content: &str,
    ) -> StepResult {
        let test_dir = self.create_test_project(prefix).await?;
        fs::write(test_dir.join("env.cue"), cue_content)
            .await
            .with_context(|| format!("failed to write env.cue in {}", test_dir.display()))?;
        Ok(())
    }

    pub(super) async fn write_current_env_cue(&self, cue_content: &str) -> StepResult {
        let env_file = self.current_dir.join("env.cue");
        fs::write(&env_file, cue_content)
            .await
            .with_context(|| format!("failed to write {}", env_file.display()))?;
        Ok(())
    }

    async fn create_test_project(&mut self, prefix: &str) -> StepResult<PathBuf> {
        let unique_id = uuid::Uuid::new_v4()
            .to_string()
            .chars()
            .take(8)
            .collect::<String>();
        let test_dir = Self::repo_root()?
            .join("_tests/bdd")
            .join(format!("{prefix}_{unique_id}"));

        let cue_mod_dir = test_dir.join("cue.mod");
        fs::create_dir_all(&cue_mod_dir)
            .await
            .with_context(|| format!("failed to create {}", cue_mod_dir.display()))?;
        fs::write(test_dir.join("cue.mod/module.cue"), TEST_CUE_MODULE)
            .await
            .with_context(|| format!("failed to write CUE module in {}", cue_mod_dir.display()))?;

        self.test_base_dir = Some(test_dir.clone());
        self.current_dir.clone_from(&test_dir);

        Ok(test_dir)
    }

    /// Check if hooks are complete by examining state files
    pub(super) async fn check_hooks_complete(&self) -> bool {
        // List all files in the state directory to see what's there
        if let Ok(mut entries) = fs::read_dir(&self.state_dir).await {
            let mut files = Vec::new();
            while let Some(entry) = entries.next_entry().await.ok().flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                files.push(name.clone());

                // Check if any state file shows completion
                if std::path::Path::new(&name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
                    && let Ok(content) = fs::read_to_string(entry.path()).await
                {
                    // Log the content for debugging
                    let _ = fs::write(
                        format!(
                            "/tmp/cuenv_state_content_{}.json",
                            name.replace(".json", "")
                        ),
                        &content,
                    )
                    .await;

                    if content.contains("\"Completed\"") {
                        let _ = fs::write(
                            self.state_root().join("cuenv_found_completed_state.log"),
                            format!("Found completed state in: {name}"),
                        )
                        .await;
                        return true;
                    }
                }
            }
            let _ = fs::write(
                self.state_root().join("cuenv_state_dir_contents.log"),
                format!("Files in {}: {:?}", self.state_dir.display(), files),
            )
            .await;
        } else {
            let _ = fs::write(
                self.state_root().join("cuenv_state_dir_error.log"),
                format!("Failed to read state dir: {}", self.state_dir.display()),
            )
            .await;
        }
        false
    }
}
