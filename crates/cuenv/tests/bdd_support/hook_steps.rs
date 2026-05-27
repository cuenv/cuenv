#![allow(clippy::branches_sharing_code, clippy::needless_pass_by_value)]

use super::{StepResult, TestWorld};
use cucumber::codegen::anyhow::Context;
use cucumber::{given, then, when};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tokio::fs;
use tokio::process::Command;
use tokio::time::sleep;

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
        .await
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
        .await
}

async fn write_state_log(world: &TestWorld, name: &str, content: impl AsRef<[u8]>) {
    let _ = fs::write(world.state_file(name), content).await;
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
        .await
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

    write_state_log(world, "cuenv_hook_spawn_output.log", &world.last_output).await;

    // Hooks are running or completed (status shows something other than "No hook execution")
    let hooks_active = !world.last_output.contains("No hook execution in progress")
        && !world.last_output.is_empty();

    assert!(
        hooks_active,
        "Hooks were not started in background. Status output: {}",
        world.last_output
    );
    Ok(())
}

#[when(expr = "I wait for hooks to complete")]
async fn wait_for_hooks(world: &mut TestWorld) -> StepResult {
    // Wait up to 5 seconds for hooks to complete
    for i in 0..10 {
        write_state_log(
            world,
            "cuenv_wait_iteration.log",
            format!("Iteration {i}: Checking for hook completion"),
        )
        .await;
        if world.check_hooks_complete().await {
            write_state_log(
                world,
                "cuenv_hooks_complete.log",
                format!("Hooks complete at iteration {i}"),
            )
            .await;
            world.hooks_running = false;

            // Run the new env check command to get environment variables
            let dir_path = world.current_dir_arg()?;
            let package = package_for_location(&dir_path);

            // Debug: Log what we're about to run
            write_state_log(
                world,
                "cuenv_before_check.log",
                format!(
                    "Running env check:\nPath: {}\nPackage: {}\nState dir: {}",
                    dir_path,
                    package,
                    world.state_dir.display()
                ),
            )
            .await;

            // Use 'export' command which outputs shell eval statements
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

            // Parse the output and load environment variables
            if world.last_exit_code == 0 {
                let mut vars = HashMap::new();
                // Debug to file
                write_state_log(world, "cuenv_env_check_output.log", &world.last_output).await;
                for line in world.last_output.lines() {
                    if let Some(export) = line.strip_prefix("export ")
                        && let Some((key, value)) = export.split_once('=')
                    {
                        let clean_value = value.trim_matches('"');
                        vars.insert(key.to_string(), clean_value.to_string());
                    }
                }
                write_state_log(
                    world,
                    "cuenv_env_vars.log",
                    format!("Loading {} env vars", vars.len()),
                )
                .await;
                world.load_env_vars(vars);
            } else {
                write_state_log(
                    world,
                    "cuenv_env_check_failed.log",
                    format!("env check failed with exit code: {}", world.last_exit_code),
                )
                .await;
            }
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }
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
        .await
}

#[when(expr = "I wait for hooks to complete or fail")]
async fn wait_for_hooks_or_failure(world: &mut TestWorld) -> StepResult {
    // Wait for hooks to finish (successfully or with failure)
    for _ in 0..10 {
        run_status_wait(world).await?;

        if world.last_output.contains("Completed") || world.last_output.contains("Failed") {
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }

    // Try to load environment (should fail or return empty)
    let dir_path = world.current_dir_arg()?;
    world
        .run_cuenv(["env", "check", "--path", dir_path.as_str()])
        .await?;
    Ok(())
}

#[then(expr = "the environment variables should not be loaded")]
fn env_vars_not_loaded(world: &mut TestWorld) {
    assert!(
        !world.shell_env.contains_key("SHOULD_NOT_LOAD"),
        "Failed hook should not load environment variables"
    );
}

#[then(expr = "I should see an error message about hook failure")]
async fn see_hook_failure_message(world: &mut TestWorld) -> StepResult {
    run_status_wait(world).await?;

    // Also write the status to a debug file
    write_state_log(world, "cuenv_hook_failure_status.log", &world.last_output).await;

    assert!(
        world.last_output.contains("Failed")
            || world.last_output.contains("failed")
            || world.last_output.contains("error"),
        "No failure message found: {}",
        world.last_output
    );
    Ok(())
}

// Step definitions for "Changing Away From Directory Preserves State" scenario

#[then(expr = "the environment variables from hooks should still be set")]
fn env_vars_still_set(world: &mut TestWorld) {
    assert!(
        world.shell_env.contains_key("CUENV_TEST"),
        "CUENV_TEST should still be set after changing directories. Current env: {:?}",
        world.shell_env
    );
    assert_eq!(
        world.shell_env.get("CUENV_TEST").map(String::as_str),
        Some("loaded_successfully"),
        "CUENV_TEST should retain its value"
    );
}

#[when(expr = "I change back to {string}")]
fn change_back_to_directory(world: &mut TestWorld, dir: String) {
    // Simply update the current directory without triggering hook execution
    // This simulates going back to a directory where hooks already completed
    // Use just the last component of the path to avoid doubling "examples"
    let target = std::path::Path::new(&dir)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(dir.as_str());
    world.current_dir = world.current_dir.join(target);
}

#[then(expr = "hooks should not re-execute since configuration hasn't changed")]
async fn hooks_should_not_reexecute(world: &mut TestWorld) -> StepResult {
    // Check that no new hook execution is triggered
    // The hook state should still show the previous completed execution
    run_status(world).await?;

    // Status should show completed (from before), not running
    // Since hooks already ran, no new execution should be in progress
    assert!(
        !world.last_output.contains("Running") && !world.last_output.contains("in progress")
            || world.last_output.contains("Completed")
            || world.last_output.contains("completed"),
        "Hooks should not be re-executing. Status: {}",
        world.last_output
    );
    Ok(())
}
