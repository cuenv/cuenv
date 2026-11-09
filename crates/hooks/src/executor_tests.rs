use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use super::{HookExecutor, source_environment};
use crate::state::{HookExecutionState, StateManager};
use crate::types::{ExecutionStatus, Hook, HookExecutionConfig};
use tempfile::TempDir;

fn session_context<'context>(
    directory: &'context std::path::Path,
    config_hash: &'context str,
) -> super::HookSessionContext<'context> {
    super::HookSessionContext {
        directory,
        config_hash,
        session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
    }
}

#[cfg(unix)]
fn private_temp_dir() -> TempDir {
    use std::os::unix::fs::PermissionsExt;

    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .expect("create private temporary directory")
}

#[cfg(not(unix))]
fn private_temp_dir() -> TempDir {
    TempDir::new().expect("create temporary directory")
}

#[test]
fn duration_millis_saturates_at_u64_max() {
    assert_eq!(super::duration_millis(Duration::from_millis(42)), 42);
    assert_eq!(
        super::duration_millis(
            Duration::from_millis(u64::MAX).saturating_add(Duration::from_millis(1))
        ),
        u64::MAX
    );
}

/// The supervisor tests require the current cuenv CLI binary.
fn cuenv_executable() -> PathBuf {
    let path = std::env::var_os("CUENV_EXECUTABLE").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/cuenv"),
        PathBuf::from,
    );
    assert!(
        path.is_file(),
        "hook supervisor tests require a built cuenv binary; set CUENV_EXECUTABLE (tried {})",
        path.display()
    );
    path
}

#[tokio::test]
async fn test_hook_executor_creation() {
    let temp_dir = private_temp_dir();
    let config = HookExecutionConfig {
        default_timeout_seconds: 60,
        fail_fast: true,
        state_dir: Some(temp_dir.path().to_path_buf()),
    };

    let executor = HookExecutor::try_new(config).unwrap();
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
    let temp_dir = private_temp_dir();
    let config = HookExecutionConfig {
        default_timeout_seconds: 1, // Set timeout to 1 second
        fail_fast: true,
        state_dir: Some(temp_dir.path().to_path_buf()),
    };
    let executor = HookExecutor::try_new(config).unwrap();

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
    let cuenv_binary = cuenv_executable();
    temp_env::async_with_vars(
        [("CUENV_EXECUTABLE", Some(cuenv_binary.as_os_str()))],
        async {
            let temp_dir = private_temp_dir();
            let config = HookExecutionConfig {
                default_timeout_seconds: 30,
                fail_fast: true,
                state_dir: Some(temp_dir.path().to_path_buf()),
            };

            let executor = HookExecutor::try_new(config).unwrap();
            let directory_path = temp_dir.path().to_path_buf();

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
            let config_hash = crate::compute_execution_hash(&hooks, &directory_path).unwrap();

            let result = executor
                .execute_hooks_background(
                    session_context(&directory_path, &config_hash),
                    hooks,
                    crate::HookStartMode::Force,
                )
                .await
                .unwrap();

            assert!(result.contains("Started execution of 2 hooks"));

            let state = executor
                .wait_for_completion(session_context(&directory_path, &config_hash), Some(15))
                .await
                .expect("supervisor must finish both hooks within 15 seconds");
            assert_eq!(state.status, ExecutionStatus::Completed, "{state:?}");
            assert_eq!(state.total_hooks, 2);
            assert_eq!(state.completed_hooks, 2);
            assert_eq!(state.directory_path, directory_path);
            assert_eq!(state.hook_results[&0].stdout.trim(), "hook1");
            assert_eq!(state.hook_results[&1].stdout.trim(), "hook2");
        },
    )
    .await;
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
    let cuenv_binary = cuenv_executable();
    temp_env::async_with_vars(
        [("CUENV_EXECUTABLE", Some(cuenv_binary.as_os_str()))],
        async {
            let temp_dir = private_temp_dir();
            let config = HookExecutionConfig {
                default_timeout_seconds: 30,
                fail_fast: false,
                state_dir: Some(temp_dir.path().to_path_buf()),
            };

            let executor = HookExecutor::try_new(config).unwrap();
            let directory_path = temp_dir.path().to_path_buf();
            let config_hash = "cancel_test".to_string();
            let ready_file = temp_dir.path().join("hook-ready");

            // Create a long-running hook
            let hooks = vec![Hook {
                order: 100,
                propagate: false,
                command: "sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "printf ready > \"$1\"; exec sleep 3".to_string(),
                    "cuenv-hook-test".to_string(),
                    ready_file.to_string_lossy().into_owned(),
                ],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            }];

            executor
                .execute_hooks_background(
                    session_context(&directory_path, &config_hash),
                    hooks,
                    crate::HookStartMode::Force,
                )
                .await
                .unwrap();

            tokio::time::timeout(Duration::from_secs(15), async {
                while !ready_file.is_file() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("supervisor must start the hook within 15 seconds");

            // Cancel the execution
            let cancelled = executor
                .cancel_execution(
                    session_context(&directory_path, &config_hash),
                    Some("User cancelled".to_string()),
                )
                .await
                .unwrap();
            assert!(cancelled);

            // Check that state reflects cancellation
            let state = tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    let state = executor
                        .execution_status_for_instance(session_context(
                            &directory_path,
                            &config_hash,
                        ))
                        .await
                        .unwrap()
                        .unwrap();
                    if state.status == ExecutionStatus::Cancelled {
                        break state;
                    }
                    assert_eq!(state.status, ExecutionStatus::Cancelling);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("the supervisor must acknowledge cancellation");
            assert_eq!(state.status, ExecutionStatus::Cancelled);
        },
    )
    .await;
}

#[tokio::test]
async fn observed_cancellation_is_not_saved_as_completed() {
    let temp_dir = private_temp_dir();
    let marker = temp_dir.path().join("unexpected-hook");
    let config = HookExecutionConfig {
        default_timeout_seconds: 10,
        fail_fast: true,
        state_dir: Some(temp_dir.path().to_path_buf()),
    };
    let state_manager = StateManager::new(temp_dir.path().to_path_buf());
    let hook = Hook {
        order: 100,
        propagate: false,
        command: "sh".to_string(),
        args: vec![
            "-c".to_string(),
            "printf ran > \"$1\"".to_string(),
            "cuenv-hook-test".to_string(),
            marker.to_string_lossy().into_owned(),
        ],
        dir: None,
        inputs: Vec::new(),
        source: Some(false),
    };
    let mut state = HookExecutionState::new(
        crate::HookExecutionIdentity {
            session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
            directory_path: temp_dir.path().to_path_buf(),
            instance_hash: "cancelled".to_string(),
            config_hash: "config".to_string(),
        },
        vec![hook.clone()],
    );
    let identity = state_manager.begin_run(&mut state).await.unwrap();
    assert_eq!(
        state_manager.join_run(&identity).await.unwrap(),
        crate::HookRunUpdate::Applied
    );
    assert_eq!(
        state_manager
            .request_cancellation(&identity, Some("private-cancellation-sentinel".to_string()))
            .await
            .unwrap(),
        crate::HookRunUpdate::CancelRequested
    );

    super::execute_hooks(
        vec![hook],
        &config,
        super::HookRunState {
            manager: &state_manager,
            state: &mut state,
            identity: &identity,
        },
    )
    .await
    .unwrap();

    assert_eq!(state.status, ExecutionStatus::Cancelled);
    assert_eq!(
        state.error_message.as_deref(),
        Some("Hook execution was cancelled")
    );
    let stored = std::fs::read_to_string(
        state_manager
            .try_state_file_path(&identity.instance_hash)
            .unwrap(),
    )
    .unwrap();
    assert!(!stored.contains("private-cancellation-sentinel"));
    assert_eq!(
        state_manager
            .load_state(&identity.instance_hash)
            .await
            .unwrap(),
        Some(state)
    );
    assert!(!marker.exists(), "the cancelled hook must not start");
}

#[tokio::test]
async fn cancellation_stops_a_blocking_source_evaluation_before_release() {
    let temp_dir = private_temp_dir();
    let ready = temp_dir.path().join("source-ready");
    let script = format!(
        "printf ready > {}; exec sleep 30",
        source_environment::shell_quote(&ready.to_string_lossy())
    );
    let hook = Hook {
        order: 100,
        propagate: false,
        command: "printf".to_string(),
        args: vec!["%s".to_string(), script],
        dir: None,
        inputs: Vec::new(),
        source: Some(true),
    };
    let config = HookExecutionConfig {
        default_timeout_seconds: 60,
        fail_fast: true,
        state_dir: Some(temp_dir.path().to_path_buf()),
    };
    let manager = StateManager::new(temp_dir.path().to_path_buf());
    let mut state = HookExecutionState::new(
        crate::HookExecutionIdentity {
            session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
            directory_path: temp_dir.path().to_path_buf(),
            instance_hash: "source".to_string(),
            config_hash: "source".to_string(),
        },
        vec![hook.clone()],
    );
    let identity = manager.begin_run(&mut state).await.unwrap();
    assert_eq!(
        manager.join_run(&identity).await.unwrap(),
        crate::HookRunUpdate::Applied
    );
    let worker_manager = manager.clone();
    let worker_identity = identity.clone();
    let worker = tokio::spawn(async move {
        super::execute_hooks(
            vec![hook],
            &config,
            super::HookRunState {
                manager: &worker_manager,
                state: &mut state,
                identity: &worker_identity,
            },
        )
        .await
        .unwrap();
        state
    });

    tokio::time::timeout(Duration::from_secs(15), async {
        while !ready.is_file() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("source evaluation must start");
    let reservation = manager
        .current_reservation(temp_dir.path())
        .await
        .unwrap()
        .unwrap();
    assert!(
        reservation.child_pid.is_some(),
        "source shell must be registered"
    );
    assert_eq!(
        manager
            .request_cancellation(&identity, Some("stop".to_string()))
            .await
            .unwrap(),
        crate::HookRunUpdate::CancelRequested
    );
    let state = tokio::time::timeout(Duration::from_secs(15), worker)
        .await
        .expect("source shell must stop")
        .expect("worker must finish");
    assert_eq!(state.status, ExecutionStatus::Cancelled);
    assert!(state.environment_vars.is_empty());
    assert!(manager.reservations().await.unwrap().is_empty());
}

#[derive(Clone, Copy)]
enum InputMutation {
    ChangeContents,
    ReplaceWithDirectory,
}

async fn run_with_changed_input(mutation: InputMutation) -> HookExecutionState {
    let temp_dir = private_temp_dir();
    let input = temp_dir.path().join("input");
    let ready = temp_dir.path().join("ready");
    let release = temp_dir.path().join("release");
    std::fs::write(&input, "before").unwrap();
    let hook = Hook {
        order: 100,
        propagate: false,
        command: "sh".to_string(),
        args: vec![
            "-c".to_string(),
            "printf ready > \"$1\"; while [ ! -e \"$2\" ]; do sleep 0.05; done; printf '%s\\n' 'export CUENV_CHANGED_INPUT=value'".to_string(),
            "hook-input-test".to_string(),
            ready.to_string_lossy().into_owned(),
            release.to_string_lossy().into_owned(),
        ],
        dir: None,
        inputs: vec!["input".to_string()],
        source: Some(true),
    };
    let config = HookExecutionConfig {
        default_timeout_seconds: 30,
        fail_fast: true,
        state_dir: Some(temp_dir.path().to_path_buf()),
    };
    let config_hash =
        crate::compute_execution_hash(std::slice::from_ref(&hook), temp_dir.path()).unwrap();
    let manager = StateManager::new(temp_dir.path().to_path_buf());
    let mut state = HookExecutionState::new(
        crate::HookExecutionIdentity {
            session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
            directory_path: temp_dir.path().to_path_buf(),
            instance_hash: "initial".to_string(),
            config_hash,
        },
        vec![hook.clone()],
    );
    let identity = manager.begin_run(&mut state).await.unwrap();
    assert_eq!(
        manager.join_run(&identity).await.unwrap(),
        crate::HookRunUpdate::Applied
    );
    let worker_manager = manager.clone();
    let worker_identity = identity.clone();
    let worker = tokio::spawn(async move {
        super::execute_hooks(
            vec![hook],
            &config,
            super::HookRunState {
                manager: &worker_manager,
                state: &mut state,
                identity: &worker_identity,
            },
        )
        .await
        .unwrap();
        state
    });
    tokio::time::timeout(Duration::from_secs(15), async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("hook must start");
    match mutation {
        InputMutation::ChangeContents => std::fs::write(&input, "after").unwrap(),
        InputMutation::ReplaceWithDirectory => {
            std::fs::remove_file(&input).unwrap();
            std::fs::create_dir(&input).unwrap();
        }
    }
    std::fs::write(release, "continue").unwrap();
    let state = tokio::time::timeout(Duration::from_secs(15), worker)
        .await
        .unwrap()
        .unwrap();
    assert!(manager.reservations().await.unwrap().is_empty());
    state
}

#[tokio::test]
async fn changed_or_unreadable_inputs_prevent_hook_environment_publication() {
    for (mutation, expected) in [
        (InputMutation::ChangeContents, "Hook inputs changed"),
        (
            InputMutation::ReplaceWithDirectory,
            "Hook inputs could not be validated",
        ),
    ] {
        let state = run_with_changed_input(mutation).await;
        assert_eq!(state.status, ExecutionStatus::Failed);
        assert!(
            state
                .error_message
                .as_deref()
                .unwrap_or_default()
                .contains(expected)
        );
        assert!(state.environment_vars.is_empty());
    }
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
    let cuenv_binary = cuenv_executable();
    temp_env::async_with_vars(
        [("CUENV_EXECUTABLE", Some(cuenv_binary.as_os_str()))],
        async {
            let temp_dir = private_temp_dir();
            let config = HookExecutionConfig {
                default_timeout_seconds: 30,
                fail_fast: false,
                state_dir: Some(temp_dir.path().to_path_buf()),
            };

            let executor = HookExecutor::try_new(config).unwrap();
            let directory_path = temp_dir.path().to_path_buf();

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
            let config_hash = crate::compute_execution_hash(&hooks, &directory_path).unwrap();

            executor
                .execute_hooks_background(
                    session_context(&directory_path, &config_hash),
                    hooks,
                    crate::HookStartMode::Force,
                )
                .await
                .unwrap();

            let state = executor
                .wait_for_completion(session_context(&directory_path, &config_hash), Some(15))
                .await
                .expect("supervisor must complete the cleanup hook within 15 seconds");
            assert_eq!(state.status, ExecutionStatus::Completed, "{state:?}");
            assert_eq!(state.hook_results[&0].stdout.trim(), "test");

            // Clean up old states (should clean up the completed state)
            let cleaned = executor
                .cleanup_old_states(chrono::Duration::seconds(0))
                .await
                .unwrap();
            assert_eq!(cleaned, 1);

            // State should be gone
            let state = executor
                .execution_status_for_instance(session_context(&directory_path, &config_hash))
                .await
                .unwrap();
            assert!(state.is_none());
        },
    )
    .await;
}

#[tokio::test]
async fn test_execution_state_tracking() {
    let cuenv_binary = cuenv_executable();
    temp_env::async_with_vars(
        [("CUENV_EXECUTABLE", Some(cuenv_binary.as_os_str()))],
        async {
            let temp_dir = private_temp_dir();
            let config = HookExecutionConfig {
                default_timeout_seconds: 30,
                fail_fast: true,
                state_dir: Some(temp_dir.path().to_path_buf()),
            };

            let executor = HookExecutor::try_new(config).unwrap();
            let directory_path = temp_dir.path().to_path_buf();
            let config_hash = "hash".to_string();

            // Initially no state
            let status = executor
                .execution_status_for_instance(session_context(&directory_path, &config_hash))
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
            let config_hash = crate::compute_execution_hash(&hooks, &directory_path).unwrap();

            executor
                .execute_hooks_background(
                    session_context(&directory_path, &config_hash),
                    hooks,
                    crate::HookStartMode::Force,
                )
                .await
                .unwrap();

            // Should now have state
            let status = executor
                .execution_status_for_instance(session_context(&directory_path, &config_hash))
                .await
                .unwrap();
            assert!(status.is_some());
            let state = executor
                .wait_for_completion(session_context(&directory_path, &config_hash), Some(15))
                .await
                .expect("supervisor must complete the tracking hook within 15 seconds");
            assert_eq!(state.status, ExecutionStatus::Completed, "{state:?}");
            assert_eq!(state.hook_results[&0].stdout.trim(), "test");
        },
    )
    .await;
}

#[tokio::test]
async fn test_working_directory_handling() {
    let executor = HookExecutor::with_default_config().unwrap();
    let temp_dir = private_temp_dir();

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
        dir: Some(
            temp_dir
                .path()
                .join("missing")
                .to_string_lossy()
                .into_owned(),
        ),
        inputs: vec![],
        source: None,
    };

    let result = executor
        .execute_single_hook(hook_with_invalid_dir)
        .await
        .unwrap();
    assert!(!result.success);
    assert!(result.error.unwrap().contains("Failed to execute command"));
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

    let temp_dir = private_temp_dir();
    let state_dir = temp_dir.path().to_path_buf();
    let state_manager = StateManager::new(state_dir.clone());

    assert_eq!(state_manager.state_dir(), state_dir.as_path());
}

/// Test timeout behavior edge cases:
/// - Verify that hooks are terminated after timeout
/// - Verify error message includes timeout duration
/// - Verify partial output is not captured on timeout
#[tokio::test]
async fn test_hook_timeout_behavior() {
    let temp_dir = private_temp_dir();

    // Test with very short timeout (1 second)
    let config = HookExecutionConfig {
        default_timeout_seconds: 1,
        fail_fast: true,
        state_dir: Some(temp_dir.path().to_path_buf()),
    };
    let executor = HookExecutor::try_new(config).unwrap();

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
    let temp_dir = private_temp_dir();

    let config = HookExecutionConfig {
        default_timeout_seconds: 1,
        fail_fast: true,
        state_dir: Some(temp_dir.path().to_path_buf()),
    };
    let executor = HookExecutor::try_new(config).unwrap();

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

    let temp_dir = private_temp_dir();
    let config = HookExecutionConfig {
        default_timeout_seconds: 30,
        fail_fast: false,
        state_dir: Some(temp_dir.path().to_path_buf()),
    };
    let executor = Arc::new(HookExecutor::try_new(config).unwrap());

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

    let prior_env = HashMap::from([("MULTILINE_VAR".to_string(), "before".to_string())]);
    let result =
        source_environment::evaluate_shell_environment(multiline_script, &prior_env, 10).await;
    assert!(result.is_ok(), "Should parse multiline env vars");

    let (env_vars, _removed) = result.unwrap();
    assert_eq!(
        env_vars.get("MULTILINE_VAR").map(String::as_str),
        Some("line1\nline2\nline3")
    );

    // Test Unicode characters
    let unicode_script = r"
export UNICODE_VAR='Hello 世界 🌍 émoji'
export CHINESE_VAR='中文测试'
export JAPANESE_VAR='日本語テスト'
";

    let prior_env = HashMap::from([
        ("UNICODE_VAR".to_string(), "before".to_string()),
        ("CHINESE_VAR".to_string(), "before".to_string()),
        ("JAPANESE_VAR".to_string(), "before".to_string()),
    ]);
    let result =
        source_environment::evaluate_shell_environment(unicode_script, &prior_env, 10).await;
    assert!(result.is_ok(), "Should parse unicode env vars");

    let (env_vars, _removed) = result.unwrap();
    assert_eq!(
        env_vars.get("UNICODE_VAR").map(String::as_str),
        Some("Hello 世界 🌍 émoji")
    );
    assert_eq!(
        env_vars.get("CHINESE_VAR").map(String::as_str),
        Some("中文测试")
    );
    assert_eq!(
        env_vars.get("JAPANESE_VAR").map(String::as_str),
        Some("日本語テスト")
    );

    // Test special shell characters
    let special_chars_script = r#"
export QUOTED_VAR="value with 'single' and \"double\" quotes"
export PATH_VAR="/usr/local/bin:/usr/bin:/bin"
export EQUALS_VAR="key=value=another"
"#;

    let prior_env = HashMap::from([
        ("QUOTED_VAR".to_string(), "before".to_string()),
        ("PATH_VAR".to_string(), "before".to_string()),
        ("EQUALS_VAR".to_string(), "before".to_string()),
    ]);
    let result =
        source_environment::evaluate_shell_environment(special_chars_script, &prior_env, 10).await;
    assert!(result.is_ok(), "Should parse special chars");

    let (env_vars, _removed) = result.unwrap();
    assert_eq!(
        env_vars.get("QUOTED_VAR").map(String::as_str),
        Some("value with 'single' and \"double\" quotes")
    );
    assert_eq!(
        env_vars.get("PATH_VAR").map(String::as_str),
        Some("/usr/local/bin:/usr/bin:/bin")
    );
    assert_eq!(
        env_vars.get("EQUALS_VAR").map(String::as_str),
        Some("key=value=another")
    );
}

/// Test environment capture with empty and whitespace-only values
#[tokio::test]
async fn test_environment_capture_edge_cases() {
    // Test empty value
    let empty_script = r"
export EMPTY_VAR=''
export SPACE_VAR='   '
";

    let prior_env = HashMap::from([
        ("EMPTY_VAR".to_string(), "before".to_string()),
        ("SPACE_VAR".to_string(), "before".to_string()),
    ]);
    let result = source_environment::evaluate_shell_environment(empty_script, &prior_env, 10).await;
    assert!(result.is_ok(), "Should handle empty/whitespace values");
    let (env_vars, _removed) = result.unwrap();
    assert_eq!(env_vars.get("EMPTY_VAR").map(String::as_str), Some(""));
    assert_eq!(env_vars.get("SPACE_VAR").map(String::as_str), Some("   "));

    // Test very long value
    let long_value = "x".repeat(10000);
    let long_script = format!("export LONG_VAR='{}'", long_value);

    let prior_env = HashMap::from([("LONG_VAR".to_string(), "before".to_string())]);
    let result = source_environment::evaluate_shell_environment(&long_script, &prior_env, 10).await;
    assert!(result.is_ok(), "Should handle very long values");

    let (env_vars, _removed) = result.unwrap();
    assert_eq!(env_vars.get("LONG_VAR"), Some(&long_value));
}

/// Test that prior_env is passed through to child shells and that unset propagation works
#[tokio::test]
async fn test_environment_prior_env_chaining() {
    // Test 1: prior_env variables are visible and can be extended
    let mut prior_env = HashMap::new();
    prior_env.insert("CUENV_TEST_PRIOR".to_string(), "original_value".to_string());

    let script = r#"export CUENV_TEST_PRIOR="extended_${CUENV_TEST_PRIOR}""#;
    let result = source_environment::evaluate_shell_environment(script, &prior_env, 10).await;
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
    let result = source_environment::evaluate_shell_environment(script, &prior_env, 10).await;
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
    let temp_dir1 = private_temp_dir();
    let temp_dir2 = private_temp_dir();

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

    let environment = source_environment::capture_source_environment(hook, &HashMap::new(), 5)
        .await
        .unwrap();

    assert_eq!(
        environment.get("CUENV_RUNTIME_TEST"),
        Some(&"from_runtime".to_string())
    );
}

#[tokio::test]
async fn failed_source_capture_does_not_evaluate_printed_exports() {
    let temp_dir = private_temp_dir();
    let marker = temp_dir.path().join("unexpected-evaluation");
    let output = format!(
        "touch {}; export CUENV_FAILED_SOURCE=value",
        source_environment::shell_quote(&marker.to_string_lossy())
    );
    let hook = source_hook(&format!(
        "printf '%s\\n' {}; exit 3",
        source_environment::shell_quote(&output)
    ));
    let error = source_environment::capture_source_environment(hook, &HashMap::new(), 5)
        .await
        .expect_err("a failing source command must not produce an environment");
    assert!(error.to_string().contains("status"));
    assert!(
        !marker.exists(),
        "failed command output must not be evaluated"
    );
}

#[tokio::test]
async fn hook_children_do_not_inherit_the_shell_session() {
    temp_env::async_with_vars(
        [("CUENV_HOOK_SESSION", Some("private-shell-session"))],
        async {
            let hook = Hook {
                order: 100,
                propagate: false,
                command: "sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "test -z \"${CUENV_HOOK_SESSION+x}\"".to_string(),
                ],
                dir: None,
                inputs: Vec::new(),
                source: Some(false),
            };
            let executor = HookExecutor::try_new(HookExecutionConfig::default()).unwrap();
            assert!(
                executor
                    .execute_single_hook(hook.clone())
                    .await
                    .unwrap()
                    .success
            );
            assert_eq!(
                run_source_hook_to_completion(hook).await.status,
                ExecutionStatus::Completed
            );
        },
    )
    .await;
}

#[tokio::test]
async fn changed_configuration_can_still_cancel_its_session_run() {
    let directory = private_temp_dir();
    let executor = HookExecutor::try_new(HookExecutionConfig {
        state_dir: Some(directory.path().join("state")),
        ..HookExecutionConfig::default()
    })
    .unwrap();
    let mut state = HookExecutionState::new(
        crate::HookExecutionIdentity {
            directory_path: directory.path().to_path_buf(),
            instance_hash: "initial".to_string(),
            session_id: session_context(directory.path(), "old").session_id,
            config_hash: "old".to_string(),
        },
        Vec::new(),
    );
    let manager = executor.state_manager();
    let identity = manager.begin_run(&mut state).await.unwrap();
    assert_eq!(
        manager.join_run(&identity).await.unwrap(),
        crate::HookRunUpdate::Applied
    );
    assert!(
        executor
            .cancel_execution(session_context(directory.path(), "new"), None)
            .await
            .unwrap()
    );
    assert_eq!(
        manager.acknowledge_cancellation(&identity).await.unwrap(),
        crate::HookRunUpdate::Applied
    );
}

#[cfg(unix)]
#[tokio::test]
async fn standalone_timeout_stops_and_reaps_the_direct_child() {
    let directory = private_temp_dir();
    let pid_file = directory.path().join("child-pid");
    let hook = Hook {
        order: 100,
        propagate: false,
        command: "sh".to_string(),
        args: vec![
            "-c".to_string(),
            format!(
                "printf '%s' \"$$\" > {}; exec sleep 30",
                source_environment::shell_quote(&pid_file.to_string_lossy())
            ),
        ],
        dir: None,
        inputs: Vec::new(),
        source: Some(false),
    };
    let executor = HookExecutor::try_new(HookExecutionConfig {
        default_timeout_seconds: 1,
        ..HookExecutionConfig::default()
    })
    .unwrap();
    let result = executor.execute_single_hook(hook).await.unwrap();
    assert!(!result.success);
    let pid: i32 = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
    let pid = rustix::process::Pid::from_raw(pid).unwrap();
    assert!(rustix::process::test_kill_process(pid).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn hook_output_budget_combines_stdout_and_stderr() {
    let hook = Hook {
        order: 100,
        propagate: false,
        command: "sh".to_string(),
        args: vec![
            "-c".to_string(),
            "head -c 9000000 /dev/zero; head -c 9000000 /dev/zero >&2".to_string(),
        ],
        dir: None,
        inputs: Vec::new(),
        source: Some(false),
    };
    let executor = HookExecutor::try_new(HookExecutionConfig::default()).unwrap();
    let error = executor.execute_single_hook(hook).await.unwrap_err();
    assert!(error.to_string().contains("capture"));
}

#[tokio::test]
async fn oversized_supervisor_payload_is_refused_before_reservation() {
    let directory = private_temp_dir();
    let state_dir = directory.path().join("state");
    let executor = HookExecutor::try_new(HookExecutionConfig {
        state_dir: Some(state_dir.clone()),
        ..HookExecutionConfig::default()
    })
    .unwrap();
    let hook = Hook {
        order: 100,
        propagate: false,
        command: "printf".to_string(),
        args: vec!["x".repeat(17 * 1024 * 1024)],
        dir: None,
        inputs: Vec::new(),
        source: Some(false),
    };
    let error = executor
        .execute_hooks_background(
            session_context(directory.path(), "config"),
            vec![hook],
            crate::HookStartMode::Force,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("limit"));
    assert!(!state_dir.exists());
}

#[tokio::test]
async fn incomplete_and_failing_source_scripts_never_capture_an_environment() {
    let prior = HashMap::from([("CUENV_PRIOR_TEST".to_string(), "existing".to_string())]);
    for script in [
        "export CUENV_TEST_NEW=value; exit 0",
        "export CUENV_TEST_NEW=value; exit 9",
        "false; export CUENV_TEST_NEW=value",
    ] {
        let error = source_environment::evaluate_shell_environment(script, &prior, 10)
            .await
            .expect_err("incomplete or failed source evaluation must fail");
        assert!(!error.to_string().is_empty());
    }
}

#[cfg(target_os = "linux")]
enum SourceEvaluationMode {
    Standalone,
    Reserved,
}

#[cfg(target_os = "linux")]
async fn source_script_is_not_visible_in_evaluator_arguments(mode: SourceEvaluationMode) {
    let temp_dir = private_temp_dir();
    let started = temp_dir.path().join("evaluator-pid");
    let finish = temp_dir.path().join("finish");
    let sentinel = "cuenv-source-secret-argv-regression";
    let script = format!(
        "export CUENV_TEST_SOURCE_VALUE='{}'\nprintf '%s' \"$$\" > {}\nwhile [ ! -e {} ]; do sleep 0.05; done",
        sentinel,
        source_environment::shell_quote(&started.to_string_lossy()),
        source_environment::shell_quote(&finish.to_string_lossy()),
    );
    let evaluator = match mode {
        SourceEvaluationMode::Standalone => tokio::spawn(async move {
            let (environment, _) =
                source_environment::evaluate_shell_environment(&script, &HashMap::new(), 10)
                    .await
                    .unwrap();
            environment
        }),
        SourceEvaluationMode::Reserved => {
            let hook = source_hook(&format!(
                "printf '%s' {}",
                source_environment::shell_quote(&script)
            ));
            tokio::spawn(async move { run_source_hook_to_completion(hook).await.environment_vars })
        }
    };

    let pid = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(pid) = tokio::fs::read_to_string(&started).await {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("source evaluator did not start");
    let arguments = tokio::fs::read(format!("/proc/{pid}/cmdline"))
        .await
        .expect("source evaluator must be running");
    assert!(
        !arguments
            .windows(sentinel.len())
            .any(|part| part == sentinel.as_bytes()),
        "source script appeared in the evaluator's process arguments"
    );
    tokio::fs::write(&finish, "done").await.unwrap();
    let environment = tokio::time::timeout(Duration::from_secs(10), evaluator)
        .await
        .expect("source evaluator did not finish")
        .expect("source evaluator task failed");
    assert_eq!(
        environment
            .get("CUENV_TEST_SOURCE_VALUE")
            .map(String::as_str),
        Some(sentinel)
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn standalone_source_script_is_not_passed_in_process_arguments() {
    source_script_is_not_visible_in_evaluator_arguments(SourceEvaluationMode::Standalone).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn reserved_source_script_is_not_passed_in_process_arguments() {
    source_script_is_not_visible_in_evaluator_arguments(SourceEvaluationMode::Reserved).await;
}

#[test]
fn source_capture_rejects_ambiguous_frames_and_invalid_names() {
    assert!(source_environment::environment_output_bytes(b"missing", "receipt").is_none());
    assert!(
        source_environment::environment_output_bytes(b"\0receipt\0a=1\0receipt\0", "receipt")
            .is_none()
    );
    assert!(
        source_environment::parse_environment_delta(
            b"X; touch /tmp/cuenv-injected; #=v\0",
            &HashMap::new(),
            &HashMap::new(),
        )
        .is_err()
    );
}

#[tokio::test]
async fn source_capture_diagnostics_do_not_include_script_or_output() {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    struct Writer(Arc<Mutex<Vec<u8>>>);
    impl Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for Capture {
        type Writer = Writer;
        fn make_writer(&'writer self) -> Self::Writer {
            Writer(self.0.clone())
        }
    }

    let output = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(Capture(output.clone()))
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let sentinel = "CUENV_SOURCE_PRIVATE_SENTINEL";
    let script = format!("printf '%s' '{sentinel}'; exit 0");
    let error = source_environment::evaluate_shell_environment(&script, &HashMap::new(), 10)
        .await
        .expect_err("early exit must fail");
    assert!(!error.to_string().contains(sentinel));
    let hook = source_hook(&format!("printf '%s' '{sentinel}'; exit 3"));
    let error = source_environment::capture_source_environment(hook, &HashMap::new(), 5)
        .await
        .expect_err("failing source command must fail");
    assert!(!error.to_string().contains(sentinel));
    let mut state = HookExecutionState::new(
        crate::HookExecutionIdentity {
            directory_path: std::env::temp_dir(),
            instance_hash: "trace-test".to_string(),
            session_id: uuid::Uuid::new_v4(),
            config_hash: "trace-test".to_string(),
        },
        Vec::new(),
    );
    state.record_hook_result(
        0,
        &crate::HookResult {
            hook: source_hook("true"),
            success: false,
            exit_status: None,
            stdout: String::new(),
            stderr: String::new(),
            duration_ms: 0,
            error: Some(sentinel.to_string()),
        },
    );
    let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();
    assert!(
        !logs.contains(sentinel),
        "source diagnostics must not contain script or output"
    );
}

fn source_hook(script: &str) -> Hook {
    Hook {
        order: 100,
        propagate: false,
        command: "sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        dir: None,
        inputs: vec![],
        source: Some(true),
    }
}

async fn run_source_hook_to_completion(hook: Hook) -> HookExecutionState {
    let temp_dir = private_temp_dir();
    let config = HookExecutionConfig {
        default_timeout_seconds: 10,
        fail_fast: true,
        state_dir: Some(temp_dir.path().to_path_buf()),
    };
    let state_manager = StateManager::new(temp_dir.path().to_path_buf());
    let config_hash =
        crate::compute_execution_hash(std::slice::from_ref(&hook), temp_dir.path()).unwrap();
    let mut state = HookExecutionState::new(
        crate::HookExecutionIdentity {
            session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
            directory_path: temp_dir.path().to_path_buf(),
            instance_hash: "instance".to_string(),
            config_hash,
        },
        vec![hook.clone()],
    );

    let identity = state_manager.begin_run(&mut state).await.unwrap();
    assert_eq!(
        state_manager.join_run(&identity).await.unwrap(),
        crate::HookRunUpdate::Applied
    );

    super::execute_hooks(
        vec![hook],
        &config,
        super::HookRunState {
            manager: &state_manager,
            state: &mut state,
            identity: &identity,
        },
    )
    .await
    .unwrap();
    state
}

#[tokio::test]
async fn a_source_hook_whose_output_cannot_be_evaluated_is_a_failed_hook() {
    // The hook process itself exits 0. What it printed is not shell, so
    // nothing can be captured from it. Reporting that as success would tell
    // the user their environment is loaded when it is empty.
    let hook = source_hook("echo 'export BAD=\"unclosed'; echo 'export GOOD=success'");

    let state = run_source_hook_to_completion(hook).await;

    let result = state.hook_results.get(&0).expect("hook result recorded");
    assert!(!result.success, "unevaluable output must fail the hook");
    assert!(
        result
            .error
            .as_deref()
            .is_some_and(|error| error.contains("evaluate source hook output")),
        "unexpected error: {:?}",
        result.error
    );
    assert_eq!(state.status, ExecutionStatus::Failed);
    assert!(
        !state.environment_vars.contains_key("GOOD") && !state.environment_vars.contains_key("BAD"),
        "a failed evaluation must not leak a partial environment"
    );
}

#[tokio::test]
async fn a_source_hook_that_exits_nonzero_cannot_capture_exports() {
    let hook = source_hook("echo 'export CUENV_PARTIAL=kept'; exit 3");

    let state = run_source_hook_to_completion(hook).await;

    let result = state.hook_results.get(&0).expect("hook result recorded");
    assert!(!result.success);
    assert_eq!(result.exit_status, Some(3));
    assert!(!state.environment_vars.contains_key("CUENV_PARTIAL"));
}

#[tokio::test]
async fn a_failed_source_hook_keeps_its_exit_status_in_the_error() {
    // Output of a failed command is not evaluated, even when it is invalid.
    let hook = source_hook("echo 'export BAD=\"unclosed'; exit 3");

    let state = run_source_hook_to_completion(hook).await;

    let result = state.hook_results.get(&0).expect("hook result recorded");
    assert!(!result.success);
    let error = result.error.as_deref().unwrap_or_default();
    assert!(
        error.starts_with("Command exited with status"),
        "the exit-status error must come first: {error}"
    );
}
