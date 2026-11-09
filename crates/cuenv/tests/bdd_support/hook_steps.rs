#![allow(clippy::branches_sharing_code, clippy::needless_pass_by_value)]

use super::{StepResult, TestWorld};
use cucumber::codegen::anyhow::{Context, anyhow};
use cucumber::{given, then, when};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const TEST_CUE_MODULE: &str = "module: \"test.example\"\nlanguage: version: \"v0.14.1\"\n";

fn package_for_location(location: &str) -> &'static str {
    if location.contains("examples") {
        "examples"
    } else {
        "cuenv"
    }
}

fn unique_test_id() -> StepResult<String> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system time is before UNIX_EPOCH")?
        .as_millis();
    let suffix = uuid::Uuid::new_v4()
        .to_string()
        .chars()
        .take(8)
        .collect::<String>();
    Ok(format!("{millis}_{suffix}"))
}

async fn create_cue_module(dir: &Path) -> StepResult {
    let cue_mod_dir = dir.join("cue.mod");
    fs::create_dir_all(&cue_mod_dir)
        .await
        .with_context(|| format!("failed to create {}", cue_mod_dir.display()))?;
    fs::write(cue_mod_dir.join("module.cue"), TEST_CUE_MODULE)
        .await
        .with_context(|| format!("failed to write module.cue in {}", cue_mod_dir.display()))?;
    Ok(())
}

async fn write_hook_project(path: &Path, cue_content: &str) -> StepResult {
    fs::create_dir_all(path)
        .await
        .with_context(|| format!("failed to create {}", path.display()))?;
    let cue_file = path.join("env.cue");
    fs::write(&cue_file, cue_content)
        .await
        .with_context(|| format!("failed to write {}", cue_file.display()))?;
    Ok(())
}

async fn run_status(world: &mut TestWorld) -> StepResult {
    let dir_path = world.current_dir_arg()?;
    let package = package_for_location(&dir_path);
    world
        .run_cuenv([
            "env",
            "status",
            "--path",
            dir_path.as_str(),
            "--package",
            package,
        ])
        .await?;
    require_success(world, "env status")
}

async fn run_status_wait(world: &mut TestWorld) -> StepResult {
    let dir_path = world.current_dir_arg()?;
    let package = package_for_location(&dir_path);
    world
        .run_cuenv([
            "env",
            "status",
            "--path",
            dir_path.as_str(),
            "--package",
            package,
            "--wait",
            "--timeout",
            "5",
        ])
        .await?;
    require_success(world, "env status --wait")
}

fn require_success(world: &TestWorld, command: &str) -> StepResult {
    if world.last_exit_code != 0 {
        return Err(anyhow!(
            "{command} failed ({}): {}",
            world.last_exit_code,
            world.last_output
        ));
    }
    Ok(())
}

async fn evaluate_bash_export(world: &TestWorld) -> StepResult<HashMap<String, String>> {
    let mut shell = Command::new("bash")
        .args(["-s", "-e"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(&world.shell_env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start Bash for the exported environment")?;
    let mut input = shell
        .stdin
        .take()
        .ok_or_else(|| anyhow!("missing Bash stdin"))?;
    input.write_all(world.last_output.as_bytes()).await?;
    input.write_all(b"\nenv -0\n").await?;
    drop(input);
    let output = shell.wait_with_output().await?;
    if !output.status.success() {
        return Err(anyhow!(
            "exported shell code failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let mut variables = HashMap::new();
    for entry in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry = std::str::from_utf8(entry).context("shell environment is not UTF-8")?;
        let (name, value) = entry
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid shell environment entry"))?;
        variables.insert(name.to_string(), value.to_string());
    }
    Ok(variables)
}

#[given(expr = "cuenv is installed and available")]
fn cuenv_is_installed(world: &mut TestWorld) {
    // Verify the binary exists
    assert!(world.cuenv_binary.exists(), "cuenv binary not found");
}

#[given(expr = "the shell integration is configured")]
fn shell_integration_configured(world: &mut TestWorld) {
    // Set up environment to simulate shell integration
    world
        .env_vars
        .insert("CUENV_SHELL_INTEGRATION".to_string(), "true".to_string());
}

#[given(expr = "I am in the {string} directory")]
async fn in_directory(world: &mut TestWorld, dir: String) -> StepResult {
    // Create a unique test directory under .test to maintain schema access
    // Use timestamp and random suffix to ensure uniqueness
    let unique_id = unique_test_id()?;

    let path = if dir == "examples" {
        // Use a _tests/bdd directory in the repo root so CUE can find the module
        // NOTE: Must NOT start with '.' because CUE's loader ignores hidden directories
        let test_dir = TestWorld::repo_root()?
            .join("_tests/bdd")
            .join(format!("test_{unique_id}"));
        if !test_dir.exists() {
            fs::create_dir_all(&test_dir)
                .await
                .with_context(|| format!("failed to create {}", test_dir.display()))?;
            create_cue_module(&test_dir).await?;
        }
        // Store the unique test dir for cleanup later
        world.test_base_dir = Some(test_dir.clone());
        test_dir
    } else {
        // For subdirectories, create them under the unique test dir
        let test_dir = TestWorld::repo_root()?
            .join("_tests/bdd")
            .join(format!("test_{unique_id}"));
        test_dir.join(dir)
    };

    if !path.exists() {
        fs::create_dir_all(&path)
            .await
            .with_context(|| format!("failed to create {}", path.display()))?;
    }
    world.current_dir.clone_from(&path);
    Ok(())
}

#[given(expr = "I am in the {string} directory with completed hooks")]
async fn in_directory_with_completed_hooks(world: &mut TestWorld, dir: String) -> StepResult {
    // Extract the parent directory from the path (e.g., "examples/hook" -> "examples")
    let parts: Vec<&str> = dir.split('/').collect();
    let parent_dir = if parts.len() >= 2 {
        parts[0].to_string()
    } else {
        "examples".to_string()
    };

    // 1. Set up parent directory (creates test_base_dir and sets current_dir)
    in_directory(world, parent_dir).await?;

    // 2. Allow hooks in the directory (use FULL path to ensure "examples" is in path)
    // This ensures package detection works correctly (looks for "examples" in path)
    cuenv_allowed_in_dir(world, dir.clone()).await?;

    // 3. Change to directory (triggers hook execution) - use FULL path
    change_directory(world, dir).await?;

    // 4. Wait for hooks to complete
    wait_for_hooks(world).await
}

#[given(expr = "cuenv is allowed in {string} directory")]
async fn cuenv_allowed_in_dir(world: &mut TestWorld, dir: String) -> StepResult {
    // Create a valid CUE file for the hook test (schema-free for test isolation)
    let cue_content = r#"package examples

name: "hook-test"

// Environment variables to be loaded after hooks complete
env: {
    CUENV_TEST: "loaded_successfully"
    API_ENDPOINT: "http://localhost:8080/api"
    DEBUG_MODE: "true"
    PROJECT_NAME: "hook-example"
}

// Hooks to execute when entering this directory
hooks: {
    onEnter: {
        setup: {
            command: "sh"
            args: ["-c", "printf 'export CUENV_TEST=\"loaded_successfully\"\\nexport HOOK_VAR=\"from_hook\"\\nexport DYNAMIC_VALUE=\"computed\"\\n'"]
            source: true
        }
    }
}

// Task definitions for the environment
tasks: {
    verify_env: {
        command: "sh"
        args: ["-c", "echo CUENV_TEST=$CUENV_TEST API_ENDPOINT=$API_ENDPOINT"]
    }

    show_env: {
        command: "sh"
        args: ["-c", "env | grep CUENV"]
    }
}
"#;

    // Create the CUE file in the test's unique directory
    let test_path = world.scenario_path(&dir)?;
    write_hook_project(&test_path, cue_content).await?;

    // Pre-approve the configuration
    let package = package_for_location(&dir);
    let test_path_arg = TestWorld::path_arg(&test_path)?;
    world
        .run_cuenv([
            "allow",
            "--path",
            test_path_arg.as_str(),
            "--package",
            package,
            "--yes",
        ])
        .await?;
    require_success(world, "allow")
}

#[when(expr = "I change directory to {string}")]
async fn change_directory(world: &mut TestWorld, dir: String) -> StepResult {
    let new_path = world.current_dir.join(dir);
    world.current_dir.clone_from(&new_path);

    // Trigger cuenv env load (simulating shell integration)
    let new_path_arg = TestWorld::path_arg(&new_path)?;
    let package = package_for_location(&new_path_arg);
    world
        .run_cuenv([
            "env",
            "load",
            "--path",
            new_path_arg.as_str(),
            "--package",
            package,
        ])
        .await?;
    require_success(world, "env load")?;

    // Mark hooks as potentially running
    world.hooks_running = true;
    Ok(())
}

#[then(expr = "hooks should be spawned in the background")]
async fn hooks_spawned(world: &mut TestWorld) -> StepResult {
    // The env load command doesn't print to stdout (by design, to avoid terminal clutter).
    // Instead, we verify hooks were started by checking the hook execution status.

    // Give the supervisor a moment to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    run_status(world).await?;

    let manager = cuenv_hooks::StateManager::new(world.state_dir.clone());
    let session = uuid::Uuid::parse_str(&world.hook_session)?;
    let state = manager
        .fast_status(&world.current_dir, session)
        .await?
        .ok_or_else(|| anyhow!("No background hook state: {}", world.last_output))?;
    if !matches!(
        state.status,
        cuenv_hooks::ExecutionStatus::Running | cuenv_hooks::ExecutionStatus::Completed
    ) {
        return Err(anyhow!("Background hook entered {:?}", state.status));
    }
    Ok(())
}

#[when(expr = "I wait for hooks to complete")]
async fn wait_for_hooks(world: &mut TestWorld) -> StepResult {
    let dir_path = world.current_dir_arg()?;
    let package = package_for_location(&dir_path);
    world
        .run_cuenv([
            "env",
            "status",
            "--path",
            dir_path.as_str(),
            "--package",
            package,
            "--wait",
            "--timeout",
            "10",
            "--output",
            "short",
        ])
        .await?;
    require_success(world, "env status --wait")?;
    if world.last_output.trim() != "[OK]" {
        return Err(anyhow!(
            "Hook execution did not complete: {}",
            world.last_output
        ));
    }
    world.hooks_running = false;
    world
        .run_cuenv([
            "export",
            "--shell",
            "bash",
            "--path",
            dir_path.as_str(),
            "--package",
            package,
        ])
        .await?;
    require_success(world, "export")?;
    let vars = evaluate_bash_export(world).await?;
    world.load_env_vars(vars);
    Ok(())
}

#[then(expr = "the environment variables should be loaded in my shell")]
fn env_vars_loaded(world: &mut TestWorld) {
    assert!(
        world.shell_env.contains_key("CUENV_TEST"),
        "CUENV_TEST not found in environment"
    );
    assert_eq!(
        world.shell_env.get("CUENV_TEST").map(String::as_str),
        Some("loaded_successfully")
    );
}

#[when(expr = "I execute {string}")]
async fn execute_command(world: &mut TestWorld, command: String) -> StepResult {
    // Always execute the actual command with the test environment
    let output = Command::new("sh")
        .arg("-c")
        .arg(&command)
        .envs(&world.shell_env)
        .output()
        .await
        .with_context(|| format!("failed to execute shell command `{command}`"))?;

    world.last_output = String::from_utf8_lossy(&output.stdout).to_string();
    world.last_exit_code = output.status.code().unwrap_or(-1);
    Ok(())
}

#[then(expr = "I should see {string}")]
fn should_see_output(world: &mut TestWorld, expected: String) {
    assert!(
        world.last_output.contains(&expected),
        "Expected '{}' in output, got: '{}'",
        expected,
        world.last_output
    );
}

#[when(expr = "I check the hook execution status")]
async fn check_hook_status(world: &mut TestWorld) -> StepResult {
    run_status(world).await
}

#[then(expr = "I should see hooks are running")]
fn hooks_are_running(world: &mut TestWorld) {
    // Hooks may complete very quickly, so accept either running or completed status
    assert!(
        world.last_output.contains("Running")
            || world.last_output.contains("in progress")
            || world.last_output.contains("completed")
            || world.last_output.contains("Completed"),
        "Unexpected hook status: {}",
        world.last_output
    );
}

#[when(expr = "I check the hook execution status again")]
async fn check_hook_status_again(world: &mut TestWorld) -> StepResult {
    run_status(world).await
}

#[then(expr = "I should see hooks have completed successfully")]
fn hooks_completed_successfully(world: &mut TestWorld) {
    assert!(
        world.last_output.contains("Completed")
            || world.last_output.contains("Success")
            || world.last_output.contains("successfully"),
        "Hooks not reported as completed: {}",
        world.last_output
    );
}

#[then(expr = "the environment variable {string} should equal {string}")]
fn env_var_equals(world: &mut TestWorld, var: String, value: String) {
    assert_eq!(
        world.shell_env.get(&var).map(String::as_str),
        Some(value.as_str()),
        "Environment variable {var} does not equal expected value"
    );
}

#[when(expr = "I execute a command that uses these variables")]
async fn execute_with_vars(world: &mut TestWorld) -> StepResult {
    execute_command(world, "echo $CUENV_TEST:$API_ENDPOINT".to_string()).await
}

#[then(expr = "the command should have access to the loaded environment")]
fn command_has_env_access(world: &mut TestWorld) {
    assert!(
        world
            .last_output
            .contains("loaded_successfully:http://localhost:8080/api")
    );
}

// Failure scenario steps
#[given(expr = "cuenv is allowed in {string} directory with failing hooks")]
async fn cuenv_allowed_with_failing_hooks(world: &mut TestWorld, dir: String) -> StepResult {
    // Create a CUE file with hooks that will fail (schema-free for test isolation)
    let cue_content = r#"package cuenv

name: "failing-hook-test"

env: {
    SHOULD_NOT_LOAD: "this_should_not_be_set"
}

hooks: {
    onEnter: {
        failing_hook: {
            command: "sh"
            args: ["-c", "exit 1"]  // This command always fails with exit code 1
        }
    }
}

tasks: {}
"#;

    // Create the CUE file in the test's unique directory
    let test_path = world.scenario_path(&dir)?;
    write_hook_project(&test_path, cue_content).await?;

    // Pre-approve the configuration
    let package = package_for_location(&dir);
    let test_path_arg = TestWorld::path_arg(&test_path)?;
    world
        .run_cuenv([
            "allow",
            "--path",
            test_path_arg.as_str(),
            "--package",
            package,
            "--yes",
        ])
        .await?;
    require_success(world, "allow")
}

#[when(expr = "I wait for hooks to complete or fail")]
async fn wait_for_hooks_or_failure(world: &mut TestWorld) -> StepResult {
    // Wait for hooks to finish (successfully or with failure)
    let mut completed = false;
    for _ in 0..10 {
        run_status_wait(world).await?;

        let status = world.last_output.to_ascii_lowercase();
        if status.contains("completed") || status.contains("failed") {
            completed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if !completed {
        return Err(anyhow!(
            "Timed out waiting for hook completion or failure: {}",
            world.last_output
        ));
    }

    // Exercise the shell export path for the failed run.
    let dir_path = world.current_dir_arg()?;
    world
        .run_cuenv(["export", "--shell", "bash", "--path", dir_path.as_str()])
        .await?;
    require_success(world, "export")
}

#[then(expr = "the environment variables should not be loaded")]
fn env_vars_not_loaded(world: &mut TestWorld) {
    assert!(
        !world.shell_env.contains_key("SHOULD_NOT_LOAD")
            && !world.last_output.contains("SHOULD_NOT_LOAD="),
        "Failed hook should not load environment variables"
    );
}

#[then(expr = "I should see an error message about hook failure")]
async fn see_hook_failure_message(world: &mut TestWorld) -> StepResult {
    run_status_wait(world).await?;

    assert!(
        world.last_output.contains("Failed")
            || world.last_output.contains("failed")
            || world.last_output.contains("error"),
        "No failure message found: {}",
        world.last_output
    );
    Ok(())
}

#[then(
    expr = "leaving a project restores the shell environment and returning reuses its hook result"
)]
async fn leaving_restores_environment(world: &mut TestWorld) -> StepResult {
    let fixture = tempfile::tempdir()?;
    let project = fixture.path().join("project");
    create_cue_module(fixture.path()).await?;
    write_hook_project(
        &project,
        r#"package cuenv
name: "shell-test"
hooks: onEnter: setup: {
    command: "sh"
    args: ["-c", "printf 'ran\\n' >> \"$TEST_ROOT/runs\"; printf 'export CUENV_TEST=loaded_successfully\\n'"]
    source: true
}
"#,
    )
    .await?;

    let executable_dir = world
        .cuenv_binary
        .parent()
        .ok_or_else(|| anyhow!("cuenv binary has no parent directory"))?;
    let mut paths = vec![executable_dir.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths)?;
    let script = r#"
export CUENV_TEST=ambient
"$TEST_CUENV" allow --yes || exit 1
source <("$TEST_CUENV" shell init bash) || exit 2
cuenv shell refresh || exit 3
cuenv env status --wait --timeout 30 --output short || exit 4
cuenv shell refresh || exit 5
[ "$CUENV_TEST" = loaded_successfully ] || exit 6
cd "$TEST_ROOT" || exit 7
cuenv shell refresh || exit 8
[ "$CUENV_TEST" = ambient ] || exit 9
cd "$TEST_PROJECT" || exit 10
cuenv shell refresh || exit 11
[ "$CUENV_TEST" = loaded_successfully ] || exit 12
[ "$(cat "$TEST_ROOT/runs")" = ran ] || exit 13
"#;
    let output = Command::new("bash")
        .args(["-c", script])
        .current_dir(&project)
        .env_clear()
        .env("PATH", path)
        .env("HOME", fixture.path())
        .env("XDG_STATE_HOME", fixture.path())
        .env("XDG_CONFIG_HOME", fixture.path())
        .env("XDG_CACHE_HOME", fixture.path())
        .env("CUENV_STATE_DIR", &world.state_dir)
        .env("CUENV_APPROVAL_FILE", fixture.path().join("approvals.json"))
        .env("CUENV_EXECUTABLE", &world.cuenv_binary)
        .env("TEST_CUENV", &world.cuenv_binary)
        .env("TEST_ROOT", fixture.path())
        .env("TEST_PROJECT", &project)
        .output()
        .await?;
    if !output.status.success() {
        return Err(anyhow!(
            "shell environment lifecycle failed ({}): stdout={}, stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}
