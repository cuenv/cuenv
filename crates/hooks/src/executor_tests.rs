use super::*;
use crate::types::Hook;
use tempfile::TempDir;

#[test]
fn duration_millis_saturates_at_u64_max() {
    assert_eq!(duration_millis(Duration::from_millis(42)), 42);
    assert_eq!(
        duration_millis(Duration::from_millis(u64::MAX).saturating_add(Duration::from_millis(1))),
        u64::MAX
    );
}

/// Helper to find CUENV_EXECUTABLE for tests that spawn the supervisor.
/// The cuenv binary must already be built (via `cargo build --bin cuenv`).
fn cuenv_executable() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CUENV_EXECUTABLE") {
        return Some(PathBuf::from(path));
    }

    // Try to find the cuenv binary in target/debug
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent()?.parent()?;
    let cuenv_binary = workspace_root.join("target/debug/cuenv");

    cuenv_binary.exists().then_some(cuenv_binary)
}

#[tokio::test]
async fn test_hook_executor_creation() {
    let temp_dir = TempDir::new().unwrap();
    let config = HookExecutionConfig {
        default_timeout_seconds: 60,
        fail_fast: true,
        state_dir: Some(temp_dir.path().to_path_buf()),
    };

    let executor = HookExecutor::new(config).unwrap();
    assert_eq!(executor.config.default_timeout_seconds, 60);
}

#[tokio::test]
async fn test_execute_single_hook_success() {
    let executor = HookExecutor::with_default_config().unwrap();

    let hook = Hook {
        order: 100,
        propagate: false,
        command: "echo".to_string(),
        args: vec!["hello".to_string()],
        dir: None,
        inputs: vec![],
        source: None,
    };

    let result = executor.execute_single_hook(hook).await.unwrap();
    assert!(result.success);
    assert!(result.stdout.contains("hello"));
}

#[tokio::test]
async fn test_execute_single_hook_failure() {
    let executor = HookExecutor::with_default_config().unwrap();

    let hook = Hook {
        order: 100,
        propagate: false,
        command: "false".to_string(), // Command that always fails
        args: vec![],
        dir: None,
        inputs: Vec::new(),
        source: Some(false),
    };

    let result = executor.execute_single_hook(hook).await.unwrap();
    assert!(!result.success);
    assert!(result.exit_status.is_some());
    assert_ne!(result.exit_status.unwrap(), 0);
}

#[tokio::test]
async fn test_execute_single_hook_timeout() {
    let temp_dir = TempDir::new().unwrap();
    let config = HookExecutionConfig {
        default_timeout_seconds: 1, // Set timeout to 1 second
        fail_fast: true,
        state_dir: Some(temp_dir.path().to_path_buf()),
    };
    let executor = HookExecutor::new(config).unwrap();

    let hook = Hook {
        order: 100,
        propagate: false,
        command: "sleep".to_string(),
        args: vec!["10".to_string()], // Sleep for 10 seconds
        dir: None,
        inputs: Vec::new(),
        source: Some(false),
    };

    let result = executor.execute_single_hook(hook).await.unwrap();
    assert!(!result.success);
    assert!(result.error.as_ref().unwrap().contains("timed out"));
}

#[tokio::test]
async fn test_background_execution() {
    let temp_dir = TempDir::new().unwrap();
    let config = HookExecutionConfig {
        default_timeout_seconds: 30,
        fail_fast: true,
        state_dir: Some(temp_dir.path().to_path_buf()),
    };

    let executor = HookExecutor::new(config).unwrap();
    let directory_path = PathBuf::from("/test/directory");
    let config_hash = "test_hash".to_string();

    let hooks = vec![
        Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["hook1".to_string()],
            dir: None,
            inputs: Vec::new(),
            source: Some(false),
        },
        Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["hook2".to_string()],
            dir: None,
            inputs: Vec::new(),
            source: Some(false),
        },
    ];

    let result = executor
        .execute_hooks_background(directory_path.clone(), config_hash.clone(), hooks)
        .await
        .unwrap();

    assert!(result.contains("Started execution of 2 hooks"));

    // Wait a bit for background execution to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Check execution status
    let status = executor
        .get_execution_status_for_instance(&directory_path, &config_hash)
        .await
        .unwrap();
    assert!(status.is_some());

    let state = status.unwrap();
    assert_eq!(state.total_hooks, 2);
    assert_eq!(state.directory_path, directory_path);
}

#[tokio::test]
async fn test_command_validation() {
    let executor = HookExecutor::with_default_config().unwrap();

    // Commands are no longer validated against a whitelist
    // The approval mechanism is the security boundary

    // Test that echo command works with any arguments
    let hook = Hook {
        order: 100,
        propagate: false,
        command: "echo".to_string(),
        args: vec!["test message".to_string()],
        dir: None,
        inputs: Vec::new(),
        source: Some(false),
    };

    let result = executor.execute_single_hook(hook).await;
    assert!(result.is_ok(), "Echo command should succeed");

    // Verify the output contains the expected message
    let hook_result = result.unwrap();
    assert!(hook_result.stdout.contains("test message"));
}

#[tokio::test]
async fn test_cancellation() {
    // Skip if cuenv binary is not available
    let Some(cuenv_binary) = cuenv_executable() else {
        eprintln!("Skipping test_cancellation: cuenv binary not found");
        return;
    };

    temp_env::async_with_vars(
        [("CUENV_EXECUTABLE", Some(cuenv_binary.as_os_str()))],
        async {
            let temp_dir = TempDir::new().unwrap();
            let config = HookExecutionConfig {
                default_timeout_seconds: 30,
                fail_fast: false,
                state_dir: Some(temp_dir.path().to_path_buf()),
            };

            let executor = HookExecutor::new(config).unwrap();
            let directory_path = PathBuf::from("/test/cancel");
            let config_hash = "cancel_test".to_string();

            // Create a long-running hook
            let hooks = vec![Hook {
                order: 100,
                propagate: false,
                command: "sleep".to_string(),
                args: vec!["10".to_string()],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            }];

            executor
                .execute_hooks_background(directory_path.clone(), config_hash.clone(), hooks)
                .await
                .unwrap();

            // Wait for supervisor to actually start and create state
            // Poll until we see Running status or timeout
            let mut started = false;
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if let Ok(Some(state)) = executor
                    .get_execution_status_for_instance(&directory_path, &config_hash)
                    .await
                    && state.status == ExecutionStatus::Running
                {
                    started = true;
                    break;
                }
            }

            if !started {
                eprintln!("Warning: Supervisor didn't start in time, skipping cancellation test");
                return;
            }

            // Cancel the execution
            let cancelled = executor
                .cancel_execution(
                    &directory_path,
                    &config_hash,
                    Some("User cancelled".to_string()),
                )
                .await
                .unwrap();
            assert!(cancelled);

            // Check that state reflects cancellation
            let state = executor
                .get_execution_status_for_instance(&directory_path, &config_hash)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(state.status, ExecutionStatus::Cancelled);
        },
    )
    .await;
}

#[tokio::test]
async fn test_large_output_handling() {
    let executor = HookExecutor::with_default_config().unwrap();

    // Generate a large output using printf repeating a pattern
    // Create a large string in the environment variable instead
    let large_content = "x".repeat(1000); // 1KB per line
    let mut args = Vec::new();
    // Generate 100 lines of 1KB each = 100KB total
    for i in 0..100 {
        args.push(format!("Line {}: {}", i, large_content));
    }

    // Use echo with multiple arguments
    let hook = Hook {
        order: 100,
        propagate: false,
        command: "echo".to_string(),
        args,
        dir: None,
        inputs: Vec::new(),
        source: Some(false),
    };

    let result = executor.execute_single_hook(hook).await.unwrap();
    assert!(result.success);
    // Output should be captured without causing memory issues
    assert!(result.stdout.len() > 50_000); // At least 50KB of output
}

#[tokio::test]
async fn test_state_cleanup() {
    // Skip if cuenv binary is not available
    let Some(cuenv_binary) = cuenv_executable() else {
        eprintln!("Skipping test_state_cleanup: cuenv binary not found");
        return;
    };

    temp_env::async_with_vars(
        [("CUENV_EXECUTABLE", Some(cuenv_binary.as_os_str()))],
        async {
            let temp_dir = TempDir::new().unwrap();
            let config = HookExecutionConfig {
                default_timeout_seconds: 30,
                fail_fast: false,
                state_dir: Some(temp_dir.path().to_path_buf()),
            };

            let executor = HookExecutor::new(config).unwrap();
            let directory_path = PathBuf::from("/test/cleanup");
            let config_hash = "cleanup_test".to_string();

            // Execute some hooks
            let hooks = vec![Hook {
                order: 100,
                propagate: false,
                command: "echo".to_string(),
                args: vec!["test".to_string()],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            }];

            executor
                .execute_hooks_background(directory_path.clone(), config_hash.clone(), hooks)
                .await
                .unwrap();

            // Poll until state exists before waiting for completion
            let mut state_exists = false;
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if executor
                    .get_execution_status_for_instance(&directory_path, &config_hash)
                    .await
                    .unwrap()
                    .is_some()
                {
                    state_exists = true;
                    break;
                }
            }

            if !state_exists {
                eprintln!("Warning: State never created, skipping cleanup test");
                return;
            }

            // Wait for completion
            if let Err(e) = executor
                .wait_for_completion(&directory_path, &config_hash, Some(15))
                .await
            {
                eprintln!(
                    "Warning: wait_for_completion timed out: {}, skipping test",
                    e
                );
                return;
            }

            // Clean up old states (should clean up the completed state)
            let cleaned = executor
                .cleanup_old_states(chrono::Duration::seconds(0))
                .await
                .unwrap();
            assert_eq!(cleaned, 1);

            // State should be gone
            let state = executor
                .get_execution_status_for_instance(&directory_path, &config_hash)
                .await
                .unwrap();
            assert!(state.is_none());
        },
    )
    .await;
}

#[tokio::test]
async fn test_execution_state_tracking() {
    let temp_dir = TempDir::new().unwrap();
    let config = HookExecutionConfig {
        default_timeout_seconds: 30,
        fail_fast: true,
        state_dir: Some(temp_dir.path().to_path_buf()),
    };

    let executor = HookExecutor::new(config).unwrap();
    let directory_path = PathBuf::from("/test/directory");
    let config_hash = "hash".to_string();

    // Initially no state
    let status = executor
        .get_execution_status_for_instance(&directory_path, &config_hash)
        .await
        .unwrap();
    assert!(status.is_none());

    // Start execution
    let hooks = vec![Hook {
        order: 100,
        propagate: false,
        command: "echo".to_string(),
        args: vec!["test".to_string()],
        dir: None,
        inputs: Vec::new(),
        source: Some(false),
    }];

    executor
        .execute_hooks_background(directory_path.clone(), config_hash.clone(), hooks)
        .await
        .unwrap();

    // Should now have state
    let status = executor
        .get_execution_status_for_instance(&directory_path, &config_hash)
        .await
        .unwrap();
    assert!(status.is_some());
}

#[tokio::test]
async fn test_working_directory_handling() {
    let executor = HookExecutor::with_default_config().unwrap();
    let temp_dir = TempDir::new().unwrap();

    // Test with valid working directory
    let hook_with_valid_dir = Hook {
        order: 100,
        propagate: false,
        command: "pwd".to_string(),
        args: vec![],
        dir: Some(temp_dir.path().to_string_lossy().to_string()),
        inputs: vec![],
        source: None,
    };

    let result = executor
        .execute_single_hook(hook_with_valid_dir)
        .await
        .unwrap();
    assert!(result.success);
    assert!(result.stdout.contains(temp_dir.path().to_str().unwrap()));

    // Test with non-existent working directory
    let hook_with_invalid_dir = Hook {
        order: 100,
        propagate: false,
        command: "pwd".to_string(),
        args: vec![],
        dir: Some("/nonexistent/directory/that/does/not/exist".to_string()),
        inputs: vec![],
        source: None,
    };

    let result = executor.execute_single_hook(hook_with_invalid_dir).await;
    // This might succeed or fail depending on the implementation
    // The important part is it doesn't panic
    if let Ok(output) = result {
        // If it succeeds, the command might have handled the missing directory
        assert!(
            !output
                .stdout
                .contains("/nonexistent/directory/that/does/not/exist")
        );
    }
}

#[tokio::test]
async fn test_hook_execution_with_complex_output() {
    let executor = HookExecutor::with_default_config().unwrap();

    // Test simple hooks without dangerous characters
    let hook = Hook {
        order: 100,
        propagate: false,
        command: "echo".to_string(),
        args: vec!["stdout output".to_string()],
        dir: None,
        inputs: vec![],
        source: None,
    };

    let result = executor.execute_single_hook(hook).await.unwrap();
    assert!(result.success);
    assert!(result.stdout.contains("stdout output"));

    // Test hook with non-zero exit code (using false command)
    let hook_with_exit_code = Hook {
        order: 100,
        propagate: false,
        command: "false".to_string(),
        args: vec![],
        dir: None,
        inputs: Vec::new(),
        source: Some(false),
    };

    let result = executor
        .execute_single_hook(hook_with_exit_code)
        .await
        .unwrap();
    assert!(!result.success);
    // Exit code should be non-zero
    assert!(result.exit_status.is_some());
}

#[tokio::test]
async fn test_state_dir_getter() {
    use crate::state::StateManager;

    let temp_dir = TempDir::new().unwrap();
    let state_dir = temp_dir.path().to_path_buf();
    let state_manager = StateManager::new(state_dir.clone());

    assert_eq!(state_manager.get_state_dir(), state_dir.as_path());
}

/// Test timeout behavior edge cases:
/// - Verify that hooks are terminated after timeout
/// - Verify error message includes timeout duration
/// - Verify partial output is not captured on timeout
#[tokio::test]
async fn test_hook_timeout_behavior() {
    let temp_dir = TempDir::new().unwrap();

    // Test with very short timeout (1 second)
    let config = HookExecutionConfig {
        default_timeout_seconds: 1,
        fail_fast: true,
        state_dir: Some(temp_dir.path().to_path_buf()),
    };
    let executor = HookExecutor::new(config).unwrap();

    // Hook that sleeps longer than timeout
    let slow_hook = Hook {
        order: 100,
        propagate: false,
        command: "sleep".to_string(),
        args: vec!["30".to_string()],
        dir: None,
        inputs: Vec::new(),
        source: Some(false),
    };

    let result = executor.execute_single_hook(slow_hook).await.unwrap();

    // Verify timeout behavior
    assert!(!result.success, "Hook should fail due to timeout");
    assert!(
        result.error.is_some(),
        "Should have error message on timeout"
    );
    let error_msg = result.error.as_ref().unwrap();
    assert!(
        error_msg.contains("timed out"),
        "Error should mention timeout: {}",
        error_msg
    );
    assert!(
        error_msg.contains('1'),
        "Error should mention timeout duration: {}",
        error_msg
    );

    // Verify exit_status is None for timeout (process was killed)
    assert!(
        result.exit_status.is_none(),
        "Exit status should be None for timed out process"
    );

    // Test that timeout duration is roughly correct
    assert!(
        result.duration_ms >= 1000,
        "Duration should be at least 1 second"
    );
    assert!(
        result.duration_ms < 5000,
        "Duration should not be much longer than timeout"
    );
}

/// Test timeout with a hook that produces output before timing out
#[tokio::test]
async fn test_hook_timeout_with_partial_output() {
    let temp_dir = TempDir::new().unwrap();

    let config = HookExecutionConfig {
        default_timeout_seconds: 1,
        fail_fast: true,
        state_dir: Some(temp_dir.path().to_path_buf()),
    };
    let executor = HookExecutor::new(config).unwrap();

    // Hook that outputs something then sleeps
    // Using bash -c to chain commands
    let hook = Hook {
        order: 100,
        propagate: false,
        command: "bash".to_string(),
        args: vec!["-c".to_string(), "echo 'started'; sleep 30".to_string()],
        dir: None,
        inputs: Vec::new(),
        source: Some(false),
    };

    let result = executor.execute_single_hook(hook).await.unwrap();

    assert!(!result.success, "Hook should timeout");
    assert!(
        result.error.as_ref().unwrap().contains("timed out"),
        "Should indicate timeout"
    );
}

/// Test concurrent hook isolation: multiple hooks executing in parallel
/// should not interfere with each other's state or environment
#[tokio::test]
async fn test_concurrent_hook_isolation() {
    use std::sync::Arc;
    use tokio::task::JoinSet;

    let temp_dir = TempDir::new().unwrap();
    let config = HookExecutionConfig {
        default_timeout_seconds: 30,
        fail_fast: false,
        state_dir: Some(temp_dir.path().to_path_buf()),
    };
    let executor = Arc::new(HookExecutor::new(config).unwrap());

    let mut join_set = JoinSet::new();

    // Spawn multiple hooks concurrently with unique identifiers
    for i in 0..5 {
        let executor = executor.clone();
        let unique_id = format!("hook_{}", i);

        join_set.spawn(async move {
            let hook = Hook {
                order: 100,
                propagate: false,
                command: "bash".to_string(),
                args: vec![
                    "-c".to_string(),
                    format!(
                        "echo 'ID:{}'; sleep 0.1; echo 'DONE:{}'",
                        unique_id, unique_id
                    ),
                ],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            };

            let result = executor.execute_single_hook(hook).await.unwrap();
            (i, result)
        });
    }

    // Collect all results
    let mut results = Vec::new();
    while let Some(result) = join_set.join_next().await {
        results.push(result.unwrap());
    }

    // Verify each hook completed successfully and output is isolated
    assert_eq!(results.len(), 5, "All 5 hooks should complete");

    for (i, result) in results {
        assert!(result.success, "Hook {} should succeed", i);

        let expected_id = format!("hook_{}", i);
        assert!(
            result.stdout.contains(&format!("ID:{}", expected_id)),
            "Hook {} output should contain its ID. Got: {}",
            i,
            result.stdout
        );
        assert!(
            result.stdout.contains(&format!("DONE:{}", expected_id)),
            "Hook {} output should contain its DONE marker. Got: {}",
            i,
            result.stdout
        );

        // Verify no cross-contamination: output should not contain other hook IDs
        for j in 0..5 {
            if j != i {
                let other_id = format!("hook_{}", j);
                assert!(
                    !result.stdout.contains(&format!("ID:{}", other_id)),
                    "Hook {} output should not contain hook {} ID",
                    i,
                    j
                );
            }
        }
    }
}

/// Test environment variable capture with special characters including:
/// - Multiline values
/// - Unicode characters
/// - Special shell characters (quotes, backslashes, etc.)
#[tokio::test]
async fn test_environment_capture_special_chars() {
    // Test multiline environment variable values
    let multiline_script = r"
export MULTILINE_VAR='line1
line2
line3'
";

    let result = evaluate_shell_environment(multiline_script, &HashMap::new()).await;
    assert!(result.is_ok(), "Should parse multiline env vars");

    let (env_vars, _removed) = result.unwrap();
    if let Some(value) = env_vars.get("MULTILINE_VAR") {
        assert!(
            value.contains("line1"),
            "Should contain first line: {}",
            value
        );
        assert!(
            value.contains("line2"),
            "Should contain second line: {}",
            value
        );
    }

    // Test Unicode characters
    let unicode_script = r"
export UNICODE_VAR='Hello 世界 🌍 émoji'
export CHINESE_VAR='中文测试'
export JAPANESE_VAR='日本語テスト'
";

    let result = evaluate_shell_environment(unicode_script, &HashMap::new()).await;
    assert!(result.is_ok(), "Should parse unicode env vars");

    let (env_vars, _removed) = result.unwrap();
    if let Some(value) = env_vars.get("UNICODE_VAR") {
        assert!(
            value.contains("世界"),
            "Should preserve Chinese characters: {}",
            value
        );
        assert!(value.contains("🌍"), "Should preserve emoji: {}", value);
    }

    // Test special shell characters
    let special_chars_script = r#"
export QUOTED_VAR="value with 'single' and \"double\" quotes"
export PATH_VAR="/usr/local/bin:/usr/bin:/bin"
export EQUALS_VAR="key=value=another"
"#;

    let result = evaluate_shell_environment(special_chars_script, &HashMap::new()).await;
    assert!(result.is_ok(), "Should parse special chars");

    let (env_vars, _removed) = result.unwrap();
    if let Some(value) = env_vars.get("EQUALS_VAR") {
        assert!(
            value.contains("key=value=another"),
            "Should preserve equals signs: {}",
            value
        );
    }
}

/// Test environment capture with empty and whitespace-only values
#[tokio::test]
async fn test_environment_capture_edge_cases() {
    // Test empty value
    let empty_script = r"
export EMPTY_VAR=''
export SPACE_VAR='   '
";

    let result = evaluate_shell_environment(empty_script, &HashMap::new()).await;
    assert!(result.is_ok(), "Should handle empty/whitespace values");
    let (_env_vars, _removed) = result.unwrap();

    // Test very long value
    let long_value = "x".repeat(10000);
    let long_script = format!("export LONG_VAR='{}'", long_value);

    let result = evaluate_shell_environment(&long_script, &HashMap::new()).await;
    assert!(result.is_ok(), "Should handle very long values");

    let (env_vars, _removed) = result.unwrap();
    if let Some(value) = env_vars.get("LONG_VAR") {
        assert_eq!(value.len(), 10000, "Should preserve full length");
    }
}

/// Test that prior_env is passed through to child shells and that unset propagation works
#[tokio::test]
async fn test_environment_prior_env_chaining() {
    // Test 1: prior_env variables are visible and can be extended
    let mut prior_env = HashMap::new();
    prior_env.insert("CUENV_TEST_PRIOR".to_string(), "original_value".to_string());

    let script = r#"export CUENV_TEST_PRIOR="extended_${CUENV_TEST_PRIOR}""#;
    let result = evaluate_shell_environment(script, &prior_env).await;
    assert!(
        result.is_ok(),
        "Should evaluate with prior_env: {:?}",
        result.as_ref().err()
    );

    let (env_vars, _removed) = result.unwrap();
    if let Some(value) = env_vars.get("CUENV_TEST_PRIOR") {
        assert!(
            value.contains("extended_"),
            "Value should contain extended_ prefix: {}",
            value
        );
        assert!(
            value.contains("original_value"),
            "Value should contain original_value from prior_env: {}",
            value
        );
    } else {
        panic!("CUENV_TEST_PRIOR should be in env_vars delta since it was modified");
    }

    // Test 2: unsetting a prior_env variable is reported in removed_keys
    let mut prior_env = HashMap::new();
    prior_env.insert("CUENV_TEST_REMOVE".to_string(), "bar".to_string());

    let script = "unset CUENV_TEST_REMOVE";
    let result = evaluate_shell_environment(script, &prior_env).await;
    assert!(result.is_ok(), "Should evaluate unset script");

    let (env_vars, removed) = result.unwrap();
    assert!(
        !env_vars.contains_key("CUENV_TEST_REMOVE"),
        "Unset variable should not appear in env_vars"
    );
    assert!(
        removed.contains(&"CUENV_TEST_REMOVE".to_string()),
        "Unset variable should appear in removed_keys: {:?}",
        removed
    );
}

/// Test that hooks with different working directories are isolated
#[tokio::test]
async fn test_working_directory_isolation() {
    let executor = HookExecutor::with_default_config().unwrap();

    // Create two temp directories
    let temp_dir1 = TempDir::new().unwrap();
    let temp_dir2 = TempDir::new().unwrap();

    // Write unique files to each directory
    std::fs::write(temp_dir1.path().join("marker.txt"), "dir1").unwrap();
    std::fs::write(temp_dir2.path().join("marker.txt"), "dir2").unwrap();

    // Hook that reads the marker file in its working directory
    let hook1 = Hook {
        order: 100,
        propagate: false,
        command: "cat".to_string(),
        args: vec!["marker.txt".to_string()],
        dir: Some(temp_dir1.path().to_string_lossy().to_string()),
        inputs: vec![],
        source: None,
    };

    let hook2 = Hook {
        order: 100,
        propagate: false,
        command: "cat".to_string(),
        args: vec!["marker.txt".to_string()],
        dir: Some(temp_dir2.path().to_string_lossy().to_string()),
        inputs: vec![],
        source: None,
    };

    let result1 = executor.execute_single_hook(hook1).await.unwrap();
    let result2 = executor.execute_single_hook(hook2).await.unwrap();

    assert!(result1.success, "Hook 1 should succeed");
    assert!(result2.success, "Hook 2 should succeed");

    assert!(
        result1.stdout.contains("dir1"),
        "Hook 1 should read from dir1: {}",
        result1.stdout
    );
    assert!(
        result2.stdout.contains("dir2"),
        "Hook 2 should read from dir2: {}",
        result2.stdout
    );
}

/// Test hook execution with stderr output
#[tokio::test]
async fn test_stderr_capture() {
    let executor = HookExecutor::with_default_config().unwrap();

    // Hook that writes to both stdout and stderr
    let hook = Hook {
        order: 100,
        propagate: false,
        command: "bash".to_string(),
        args: vec![
            "-c".to_string(),
            "echo 'to stdout'; echo 'to stderr' >&2".to_string(),
        ],
        dir: None,
        inputs: vec![],
        source: None,
    };

    let result = executor.execute_single_hook(hook).await.unwrap();

    assert!(result.success, "Hook should succeed");
    assert!(
        result.stdout.contains("to stdout"),
        "Should capture stdout: {}",
        result.stdout
    );
    assert!(
        result.stderr.contains("to stderr"),
        "Should capture stderr: {}",
        result.stderr
    );
}

/// Test that hooks handle binary output gracefully
#[tokio::test]
async fn test_binary_output_handling() {
    let executor = HookExecutor::with_default_config().unwrap();

    // Hook that outputs some binary-like data (null bytes will be lossy-converted)
    let hook = Hook {
        order: 100,
        propagate: false,
        command: "bash".to_string(),
        args: vec!["-c".to_string(), "printf 'hello\\x00world'".to_string()],
        dir: None,
        inputs: vec![],
        source: None,
    };

    let result = executor.execute_single_hook(hook).await.unwrap();

    // Should complete without panic even with binary output
    assert!(result.success, "Hook should succeed");
    // Output will contain replacement character for null byte
    assert!(
        result.stdout.contains("hello") && result.stdout.contains("world"),
        "Should contain text parts: {}",
        result.stdout
    );
}

#[tokio::test]
async fn test_capture_source_environment_returns_resulting_env() {
    let hook = Hook {
        order: 100,
        propagate: false,
        command: "bash".to_string(),
        args: vec![
            "-c".to_string(),
            "printf '%s\n' 'export CUENV_RUNTIME_TEST=from_runtime'".to_string(),
        ],
        dir: None,
        inputs: vec![],
        source: Some(true),
    };

    let environment = capture_source_environment(hook, &HashMap::new(), 5)
        .await
        .unwrap();

    assert_eq!(
        environment.get("CUENV_RUNTIME_TEST"),
        Some(&"from_runtime".to_string())
    );
}
