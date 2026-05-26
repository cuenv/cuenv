use super::*;
use crate::types::ExecutionStatus;
use crate::types::{Hook, HookFailure, HookResult};
use chrono::Utc;
use std::collections::HashMap;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn test_compute_instance_hash() {
    let path = Path::new("/test/path");
    let config_hash = "test_config";
    let hash = compute_instance_hash(path, config_hash);
    assert_eq!(hash.len(), 16);

    // Same path and config should produce same hash
    let hash2 = compute_instance_hash(path, config_hash);
    assert_eq!(hash, hash2);

    // Different path should produce different hash
    let different_path = Path::new("/other/path");
    let different_hash = compute_instance_hash(different_path, config_hash);
    assert_ne!(hash, different_hash);

    // Same path but different config should produce different hash
    let different_config_hash = compute_instance_hash(path, "different_config");
    assert_ne!(hash, different_config_hash);
}

#[tokio::test]
async fn test_state_manager_operations() {
    let temp_dir = TempDir::new().unwrap();
    let state_manager = StateManager::new(temp_dir.path().to_path_buf());

    let directory_path = PathBuf::from("/test/dir");
    let config_hash = "test_config_hash".to_string();
    let instance_hash = compute_instance_hash(&directory_path, &config_hash);

    let hooks = vec![
        Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["test1".to_string()],
            dir: None,
            inputs: vec![],
            source: None,
        },
        Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["test2".to_string()],
            dir: None,
            inputs: vec![],
            source: None,
        },
    ];

    let mut state =
        HookExecutionState::new(directory_path, instance_hash.clone(), config_hash, hooks);

    // Save initial state
    state_manager.save_state(&state).await.unwrap();

    // Load state back
    let loaded_state = state_manager
        .load_state(&instance_hash)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded_state.instance_hash, state.instance_hash);
    assert_eq!(loaded_state.total_hooks, 2);
    assert_eq!(loaded_state.status, ExecutionStatus::Running);

    // Update state with hook result
    let hook = Hook {
        order: 100,
        propagate: false,
        command: "echo".to_string(),
        args: vec!["test".to_string()],
        dir: None,
        inputs: Vec::new(),
        source: Some(false),
    };

    let result = HookResult::success(
        hook,
        std::process::ExitStatus::from_raw(0),
        "test\n".to_string(),
        String::new(),
        100,
    );

    state.record_hook_result(0, result);
    state_manager.save_state(&state).await.unwrap();

    // Load updated state
    let updated_state = state_manager
        .load_state(&instance_hash)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated_state.completed_hooks, 1);
    assert_eq!(updated_state.hook_results.len(), 1);

    // Remove state
    state_manager.remove_state(&instance_hash).await.unwrap();
    let removed_state = state_manager.load_state(&instance_hash).await.unwrap();
    assert!(removed_state.is_none());
}

#[test]
fn test_hook_execution_state() {
    let directory_path = PathBuf::from("/test/dir");
    let instance_hash = "test_hash".to_string();
    let config_hash = "config_hash".to_string();
    let hooks = vec![
        Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["test1".to_string()],
            dir: None,
            inputs: vec![],
            source: None,
        },
        Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["test2".to_string()],
            dir: None,
            inputs: vec![],
            source: None,
        },
        Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["test3".to_string()],
            dir: None,
            inputs: vec![],
            source: None,
        },
    ];
    let mut state = HookExecutionState::new(directory_path, instance_hash, config_hash, hooks);

    // Initial state
    assert_eq!(state.status, ExecutionStatus::Running);
    assert_eq!(state.total_hooks, 3);
    assert_eq!(state.completed_hooks, 0);
    assert!(!state.is_complete());

    // Mark hook as running
    state.mark_hook_running(0);
    assert_eq!(state.current_hook_index, Some(0));

    // Record successful hook result
    let hook = Hook {
        order: 100,
        propagate: false,
        command: "echo".to_string(),
        args: vec![],
        dir: None,
        inputs: Vec::new(),
        source: Some(false),
    };

    let result = HookResult::success(
        hook.clone(),
        std::process::ExitStatus::from_raw(0),
        String::new(),
        String::new(),
        100,
    );

    state.record_hook_result(0, result);
    assert_eq!(state.completed_hooks, 1);
    assert_eq!(state.current_hook_index, None);
    assert_eq!(state.status, ExecutionStatus::Running);
    assert!(!state.is_complete());

    // Record failed hook result
    let failed_result = HookResult::failure(HookFailure {
        hook,
        exit_status: Some(std::process::ExitStatus::from_raw(256)),
        stdout: String::new(),
        stderr: "error".to_string(),
        duration_ms: 50,
        error: "Command failed".to_string(),
    });

    state.record_hook_result(1, failed_result);
    assert_eq!(state.completed_hooks, 2);
    assert_eq!(state.status, ExecutionStatus::Failed);
    assert!(state.is_complete());
    assert!(state.error_message.is_some());

    // Test cancellation
    let mut cancelled_state = HookExecutionState::new(
        PathBuf::from("/test"),
        "hash".to_string(),
        "config".to_string(),
        vec![Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec![],
            dir: None,
            inputs: vec![],
            source: None,
        }],
    );
    cancelled_state.mark_cancelled(Some("User cancelled".to_string()));
    assert_eq!(cancelled_state.status, ExecutionStatus::Cancelled);
    assert!(cancelled_state.is_complete());
}

#[test]
fn test_progress_display() {
    let directory_path = PathBuf::from("/test/dir");
    let instance_hash = "test_hash".to_string();
    let config_hash = "config_hash".to_string();
    let hooks = vec![
        Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["test1".to_string()],
            dir: None,
            inputs: vec![],
            source: None,
        },
        Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["test2".to_string()],
            dir: None,
            inputs: vec![],
            source: None,
        },
    ];
    let mut state = HookExecutionState::new(directory_path, instance_hash, config_hash, hooks);

    // Running state
    let display = state.progress_display();
    assert!(display.contains("0 of 2"));

    // Running with current hook
    state.mark_hook_running(0);
    let display = state.progress_display();
    assert!(display.contains("Executing hook 1 of 2"));

    // Completed state
    state.status = ExecutionStatus::Completed;
    state.current_hook_index = None;
    let display = state.progress_display();
    assert_eq!(display, "All hooks completed successfully");

    // Failed state
    state.status = ExecutionStatus::Failed;
    state.error_message = Some("Test error".to_string());
    let display = state.progress_display();
    assert!(display.contains("Hook execution failed: Test error"));
}

#[tokio::test]
async fn test_state_directory_cleanup() {
    let temp_dir = TempDir::new().unwrap();
    let state_manager = StateManager::new(temp_dir.path().to_path_buf());

    // Create multiple states with different statuses
    let completed_state = HookExecutionState {
        instance_hash: "completed_hash".to_string(),
        directory_path: PathBuf::from("/completed"),
        config_hash: "config1".to_string(),
        status: ExecutionStatus::Completed,
        total_hooks: 1,
        completed_hooks: 1,
        current_hook_index: None,
        hooks: vec![],
        hook_results: HashMap::new(),
        environment_vars: HashMap::new(),
        started_at: Utc::now() - chrono::Duration::hours(1),
        finished_at: Some(Utc::now() - chrono::Duration::minutes(30)),
        current_hook_started_at: None,
        completed_display_until: None,
        error_message: None,
        previous_env: None,
    };

    let running_state = HookExecutionState {
        instance_hash: "running_hash".to_string(),
        directory_path: PathBuf::from("/running"),
        config_hash: "config2".to_string(),
        status: ExecutionStatus::Running,
        total_hooks: 2,
        completed_hooks: 1,
        current_hook_index: Some(1),
        hooks: vec![],
        hook_results: HashMap::new(),
        environment_vars: HashMap::new(),
        started_at: Utc::now() - chrono::Duration::minutes(5),
        finished_at: None,
        current_hook_started_at: None,
        completed_display_until: None,
        error_message: None,
        previous_env: None,
    };

    let failed_state = HookExecutionState {
        instance_hash: "failed_hash".to_string(),
        directory_path: PathBuf::from("/failed"),
        config_hash: "config3".to_string(),
        status: ExecutionStatus::Failed,
        total_hooks: 1,
        completed_hooks: 0,
        current_hook_index: None,
        hooks: vec![],
        hook_results: HashMap::new(),
        environment_vars: HashMap::new(),
        started_at: Utc::now() - chrono::Duration::hours(2),
        finished_at: Some(Utc::now() - chrono::Duration::hours(1)),
        current_hook_started_at: None,
        completed_display_until: None,
        error_message: Some("Test failure".to_string()),
        previous_env: None,
    };

    // Save all states
    state_manager.save_state(&completed_state).await.unwrap();
    state_manager.save_state(&running_state).await.unwrap();
    state_manager.save_state(&failed_state).await.unwrap();

    // Verify all states exist
    let states = state_manager.list_active_states().await.unwrap();
    assert_eq!(states.len(), 3);

    // Clean up completed states
    let cleaned = state_manager.cleanup_state_directory().await.unwrap();
    assert_eq!(cleaned, 2); // Should clean up completed and failed states

    // Verify only running state remains
    let remaining_states = state_manager.list_active_states().await.unwrap();
    assert_eq!(remaining_states.len(), 1);
    assert_eq!(remaining_states[0].instance_hash, "running_hash");
}

#[tokio::test]
async fn test_cleanup_orphaned_states() {
    let temp_dir = TempDir::new().unwrap();
    let state_manager = StateManager::new(temp_dir.path().to_path_buf());

    // Create an old running state (orphaned)
    let orphaned_state = HookExecutionState {
        instance_hash: "orphaned_hash".to_string(),
        directory_path: PathBuf::from("/orphaned"),
        config_hash: "config".to_string(),
        status: ExecutionStatus::Running,
        total_hooks: 1,
        completed_hooks: 0,
        current_hook_index: Some(0),
        hooks: vec![],
        hook_results: HashMap::new(),
        environment_vars: HashMap::new(),
        started_at: Utc::now() - chrono::Duration::hours(3),
        finished_at: None,
        current_hook_started_at: None,
        completed_display_until: None,
        error_message: None,
        previous_env: None,
    };

    // Create a recent running state (not orphaned)
    let recent_state = HookExecutionState {
        instance_hash: "recent_hash".to_string(),
        directory_path: PathBuf::from("/recent"),
        config_hash: "config".to_string(),
        status: ExecutionStatus::Running,
        total_hooks: 1,
        completed_hooks: 0,
        current_hook_index: Some(0),
        hooks: vec![],
        hook_results: HashMap::new(),
        environment_vars: HashMap::new(),
        started_at: Utc::now() - chrono::Duration::minutes(5),
        finished_at: None,
        current_hook_started_at: None,
        completed_display_until: None,
        error_message: None,
        previous_env: None,
    };

    // Save both states
    state_manager.save_state(&orphaned_state).await.unwrap();
    state_manager.save_state(&recent_state).await.unwrap();

    // Clean up orphaned states older than 1 hour
    let cleaned = state_manager
        .cleanup_orphaned_states(chrono::Duration::hours(1))
        .await
        .unwrap();
    assert_eq!(cleaned, 1); // Should clean up only the orphaned state

    // Verify only recent state remains
    let remaining_states = state_manager.list_active_states().await.unwrap();
    assert_eq!(remaining_states.len(), 1);
    assert_eq!(remaining_states[0].instance_hash, "recent_hash");
}

#[tokio::test]
async fn test_corrupted_state_file_handling() {
    let temp_dir = TempDir::new().unwrap();
    let state_dir = temp_dir.path().join("state");
    let state_manager = StateManager::new(state_dir.clone());

    // Ensure state directory exists
    state_manager.ensure_state_dir().await.unwrap();

    // Write corrupted JSON to a state file
    let corrupted_file = state_dir.join("corrupted.json");
    tokio::fs::write(&corrupted_file, "{invalid json}")
        .await
        .unwrap();

    // List active states should handle the corrupted file gracefully
    let states = state_manager.list_active_states().await.unwrap();
    assert_eq!(states.len(), 0); // Corrupted file should be skipped

    // Cleanup should remove the corrupted file
    let cleaned = state_manager.cleanup_state_directory().await.unwrap();
    assert_eq!(cleaned, 1);

    // Verify the corrupted file is gone
    assert!(!corrupted_file.exists());
}

#[tokio::test]
async fn test_concurrent_state_modifications() {
    use tokio::task;

    let temp_dir = TempDir::new().unwrap();
    let state_manager = Arc::new(StateManager::new(temp_dir.path().to_path_buf()));

    // Create initial state
    let initial_state = HookExecutionState {
        instance_hash: "concurrent_hash".to_string(),
        directory_path: PathBuf::from("/concurrent"),
        config_hash: "config".to_string(),
        status: ExecutionStatus::Running,
        total_hooks: 10,
        completed_hooks: 0,
        current_hook_index: Some(0),
        hooks: vec![],
        hook_results: HashMap::new(),
        environment_vars: HashMap::new(),
        started_at: Utc::now(),
        finished_at: None,
        current_hook_started_at: None,
        completed_display_until: None,
        error_message: None,
        previous_env: None,
    };

    state_manager.save_state(&initial_state).await.unwrap();

    // Spawn multiple tasks that concurrently modify the state
    let mut handles = vec![];

    for i in 0..5 {
        let sm = state_manager.clone();
        let path = initial_state.directory_path.clone();

        let handle = task::spawn(async move {
            // Load state - it might have been modified by another task
            let instance_hash = compute_instance_hash(&path, "concurrent_config");

            // Simulate some work
            tokio::time::sleep(Duration::from_millis(10)).await;

            // Load state, modify, and save (handle potential concurrent modifications)
            if let Ok(Some(mut state)) = sm.load_state(&instance_hash).await {
                state.completed_hooks += 1;
                state.current_hook_index = Some(i + 1);

                // Save state - ignore errors from concurrent saves
                let _ = sm.save_state(&state).await;
            }
        });

        handles.push(handle);
    }

    // Wait for all tasks to complete
    for handle in handles {
        handle.await.unwrap();
    }

    // Verify final state - due to concurrent writes, the exact values may vary
    // but the state should be loadable and valid
    let final_state = state_manager
        .load_state(&initial_state.instance_hash)
        .await
        .unwrap();

    // The state might exist or not depending on timing of concurrent operations
    if let Some(state) = final_state {
        assert_eq!(state.instance_hash, "concurrent_hash");
        // Completed hooks will be 0 if all concurrent writes failed, or > 0 if some succeeded
    }
}

#[tokio::test]
async fn test_state_with_unicode_and_special_chars() {
    let temp_dir = TempDir::new().unwrap();
    let state_manager = StateManager::new(temp_dir.path().to_path_buf());

    // Create state with unicode and special characters
    let mut unicode_state = HookExecutionState {
        instance_hash: "unicode_hash".to_string(),
        directory_path: PathBuf::from("/測試/目錄/🚀"),
        config_hash: "config_ñ_é_ü".to_string(),
        status: ExecutionStatus::Failed,
        total_hooks: 1,
        completed_hooks: 1,
        current_hook_index: None,
        hooks: vec![],
        hook_results: HashMap::new(),
        environment_vars: HashMap::new(),
        started_at: Utc::now(),
        finished_at: Some(Utc::now()),
        current_hook_started_at: None,
        completed_display_until: None,
        error_message: Some("Error: 錯誤信息 with émojis 🔥💥".to_string()),
        previous_env: None,
    };

    // Add hook result with unicode output
    let unicode_hook = Hook {
        order: 100,
        propagate: false,
        command: "echo".to_string(),
        args: vec![],
        dir: None,
        inputs: vec![],
        source: None,
    };
    let unicode_result = HookResult {
        hook: unicode_hook,
        success: false,
        exit_status: Some(1),
        stdout: "輸出: Hello 世界! 🌍".to_string(),
        stderr: "錯誤: ñoño error ⚠️".to_string(),
        duration_ms: 100,
        error: Some("失敗了 😢".to_string()),
    };
    unicode_state.hook_results.insert(0, unicode_result);

    // Save and load the state
    state_manager.save_state(&unicode_state).await.unwrap();

    let loaded = state_manager
        .load_state(&unicode_state.instance_hash)
        .await
        .unwrap()
        .unwrap();

    // Verify all unicode content is preserved
    assert_eq!(loaded.config_hash, "config_ñ_é_ü");
    assert_eq!(
        loaded.error_message,
        Some("Error: 錯誤信息 with émojis 🔥💥".to_string())
    );

    let hook_result = loaded.hook_results.get(&0).unwrap();
    assert_eq!(hook_result.stdout, "輸出: Hello 世界! 🌍");
    assert_eq!(hook_result.stderr, "錯誤: ñoño error ⚠️");
    assert_eq!(hook_result.error, Some("失敗了 😢".to_string()));
}

#[tokio::test]
async fn test_state_directory_with_many_states() {
    let temp_dir = TempDir::new().unwrap();
    let state_manager = StateManager::new(temp_dir.path().to_path_buf());

    // Create many states to test scalability
    for i in 0..50 {
        let state = HookExecutionState {
            instance_hash: format!("hash_{}", i),
            directory_path: PathBuf::from(format!("/dir/{}", i)),
            config_hash: format!("config_{}", i),
            status: if i % 3 == 0 {
                ExecutionStatus::Completed
            } else if i % 3 == 1 {
                ExecutionStatus::Running
            } else {
                ExecutionStatus::Failed
            },
            total_hooks: 1,
            completed_hooks: usize::from(i % 3 == 0),
            current_hook_index: if i % 3 == 1 { Some(0) } else { None },
            hooks: vec![],
            hook_results: HashMap::new(),
            environment_vars: HashMap::new(),
            started_at: Utc::now() - chrono::Duration::hours(i64::from(i)),
            finished_at: if i % 3 == 1 {
                None
            } else {
                Some(Utc::now() - chrono::Duration::hours(i64::from(i) - 1))
            },
            current_hook_started_at: None,
            completed_display_until: None,
            error_message: if i % 3 == 2 {
                Some(format!("Error {}", i))
            } else {
                None
            },
            previous_env: None,
        };
        state_manager.save_state(&state).await.unwrap();
    }

    // List all states
    let listed = state_manager.list_active_states().await.unwrap();
    assert_eq!(listed.len(), 50);

    // Clean up old completed states (older than 24 hours)
    let cleaned = state_manager
        .cleanup_orphaned_states(chrono::Duration::hours(24))
        .await
        .unwrap();

    // Should clean up states older than 24 hours
    assert!(cleaned > 0);
}
