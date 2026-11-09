use super::{HookExecutionState, StateManager};
use crate::types::{ExecutionStatus, Hook, HookFailure, HookResult};
use chrono::Utc;
use std::collections::HashMap;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;

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
fn test_compute_instance_hash() {
    let path = Path::new("/test/path");
    let config_hash = "test_config";
    let hash = super::compute_instance_hash(
        path,
        config_hash,
        uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
    );
    assert_eq!(hash.len(), 64);

    // Same path and config should produce same hash
    let hash2 = super::compute_instance_hash(
        path,
        config_hash,
        uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
    );
    assert_eq!(hash, hash2);

    // Different path should produce different hash
    let different_path = Path::new("/other/path");
    let different_hash = super::compute_instance_hash(
        different_path,
        config_hash,
        uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
    );
    assert_ne!(hash, different_hash);

    // Same path but different config should produce different hash
    let different_config_hash = super::compute_instance_hash(
        path,
        "different_config",
        uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
    );
    assert_ne!(hash, different_config_hash);
    assert_ne!(
        hash,
        super::compute_instance_hash(path, config_hash, uuid::Uuid::new_v4())
    );
}

#[test]
fn test_hook_duration_formatting() {
    assert_eq!(
        HookExecutionState::format_duration(chrono::Duration::milliseconds(2340)),
        "2.3s"
    );
    assert_eq!(
        HookExecutionState::format_duration(chrono::Duration::milliseconds(59_950)),
        "60.0s"
    );
    assert_eq!(
        HookExecutionState::format_duration(chrono::Duration::seconds(75)),
        "1m 15s"
    );
    assert_eq!(
        HookExecutionState::format_duration(chrono::Duration::hours(2)),
        "2h"
    );
}

#[tokio::test]
async fn test_state_manager_operations() {
    let temp_dir = private_temp_dir();
    let state_manager = StateManager::new(temp_dir.path().to_path_buf());

    let directory_path = PathBuf::from("/test/dir");
    let config_hash = "test_config_hash".to_string();
    let instance_hash = super::compute_instance_hash(
        &directory_path,
        &config_hash,
        uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
    );

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

    let mut state = HookExecutionState::new(
        crate::HookExecutionIdentity {
            session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
            directory_path,
            instance_hash: instance_hash.clone(),
            config_hash,
        },
        hooks,
    );

    // Save initial state
    state_manager.seed_test_state(&state).await.unwrap();

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

    let result = HookResult::success(crate::HookSuccess {
        hook,
        exit_status: std::process::ExitStatus::from_raw(0),
        stdout: "test\n".to_string(),
        stderr: String::new(),
        duration_ms: 100,
    });

    state.record_hook_result(0, &result);
    state_manager.seed_test_state(&state).await.unwrap();

    // Load updated state
    let updated_state = state_manager
        .load_state(&instance_hash)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated_state.completed_hooks, 1);
    assert!(updated_state.hook_results.is_empty());

    // Active state cannot be removed without a drain acknowledgement.
    assert!(state_manager.remove_state(&instance_hash).await.is_err());
    state.mark_cancelled(None);
    state_manager.seed_test_state(&state).await.unwrap();

    // Terminal state can be removed.
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
    let mut state = HookExecutionState::new(
        crate::HookExecutionIdentity {
            session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
            directory_path,
            instance_hash,
            config_hash,
        },
        hooks,
    );

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

    let result = HookResult::success(crate::HookSuccess {
        hook: hook.clone(),
        exit_status: std::process::ExitStatus::from_raw(0),
        stdout: String::new(),
        stderr: String::new(),
        duration_ms: 100,
    });

    state.record_hook_result(0, &result);
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

    state.record_hook_result(1, &failed_result);
    assert_eq!(state.completed_hooks, 2);
    assert_eq!(state.status, ExecutionStatus::Failed);
    assert!(state.is_complete());
    assert!(state.error_message.is_some());

    // Test cancellation
    let mut cancelled_state = HookExecutionState::new(
        crate::HookExecutionIdentity {
            session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
            directory_path: PathBuf::from("/test"),
            instance_hash: "hash".to_string(),
            config_hash: "config".to_string(),
        },
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
    let mut state = HookExecutionState::new(
        crate::HookExecutionIdentity {
            session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
            directory_path,
            instance_hash,
            config_hash,
        },
        hooks,
    );

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
    let temp_dir = private_temp_dir();
    let state_manager = StateManager::new(temp_dir.path().to_path_buf());

    // Create multiple states with different statuses
    let completed_state = HookExecutionState {
        session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
        instance_hash: "completed_hash".to_string(),
        run_id: uuid::Uuid::new_v4(),
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
    };

    let running_state = HookExecutionState {
        session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
        instance_hash: "running_hash".to_string(),
        run_id: uuid::Uuid::new_v4(),
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
    };

    let failed_state = HookExecutionState {
        session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
        instance_hash: "failed_hash".to_string(),
        run_id: uuid::Uuid::new_v4(),
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
    };

    // Save all states
    state_manager
        .seed_test_state(&completed_state)
        .await
        .unwrap();
    state_manager.seed_test_state(&running_state).await.unwrap();
    state_manager.seed_test_state(&failed_state).await.unwrap();

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
async fn test_active_orphan_requires_explicit_drain_recovery() {
    let temp_dir = private_temp_dir();
    let state_manager = StateManager::new(temp_dir.path().to_path_buf());

    // Create an old running state (orphaned)
    let orphaned_state = HookExecutionState {
        session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
        instance_hash: "a".repeat(16),
        run_id: uuid::Uuid::new_v4(),
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
    };

    // Create a recent running state (not orphaned)
    let recent_state = HookExecutionState {
        session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
        instance_hash: "b".repeat(16),
        run_id: uuid::Uuid::new_v4(),
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
    };

    // Save both states
    state_manager
        .seed_test_state(&orphaned_state)
        .await
        .unwrap();
    state_manager.seed_test_state(&recent_state).await.unwrap();

    assert_eq!(state_manager.cleanup_state_directory().await.unwrap(), 0);
    let reservations = state_manager.reservations().await.unwrap();
    let orphan = reservations
        .iter()
        .find(|reservation| reservation.identity.instance_hash == orphaned_state.instance_hash)
        .expect("old active state must be recoverable");
    assert_eq!(orphan.phase, super::HookReservationPhase::Orphaned);
    assert_eq!(
        state_manager
            .recover_drained(&orphan.identity)
            .await
            .unwrap(),
        super::HookRecoveryOutcome::Recovered
    );
    assert_eq!(state_manager.cleanup_state_directory().await.unwrap(), 1);

    let remaining_states = state_manager.list_active_states().await.unwrap();
    assert_eq!(remaining_states.len(), 1);
    assert_eq!(
        remaining_states[0].instance_hash,
        recent_state.instance_hash
    );
}

#[tokio::test]
async fn test_corrupted_state_file_handling() {
    let temp_dir = private_temp_dir();
    let state_dir = temp_dir.path().join("state");
    let state_manager = StateManager::new(state_dir.clone());

    // Ensure state directory exists
    state_manager.ensure_state_dir().await.unwrap();

    // Write corrupted JSON to a state file
    let corrupted_file = state_dir.join("corrupted.json");
    tokio::fs::write(&corrupted_file, "{invalid json}")
        .await
        .unwrap();

    // Malformed state must not be hidden or removed automatically.
    assert!(state_manager.list_active_states().await.is_err());
    assert!(state_manager.cleanup_state_directory().await.is_err());
    assert_eq!(
        std::fs::read_to_string(corrupted_file).unwrap(),
        "{invalid json}"
    );
}

#[tokio::test]
async fn test_concurrent_state_modifications() {
    use tokio::sync::Barrier;

    let temp_dir = private_temp_dir();
    let state_manager = Arc::new(StateManager::new(temp_dir.path().to_path_buf()));

    // Create initial state
    let initial_state = HookExecutionState {
        session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
        instance_hash: "concurrent_hash".to_string(),
        run_id: uuid::Uuid::new_v4(),
        directory_path: PathBuf::from("/concurrent"),
        config_hash: format!("0:{}", "x".repeat(8192)),
        status: ExecutionStatus::Running,
        total_hooks: 10,
        completed_hooks: 0,
        current_hook_index: Some(1000),
        hooks: vec![],
        hook_results: HashMap::new(),
        environment_vars: HashMap::from([(
            "PAYLOAD".to_string(),
            format!("0:{}", "x".repeat(8192)),
        )]),
        started_at: Utc::now(),
        finished_at: None,
        current_hook_started_at: None,
        completed_display_until: None,
        error_message: None,
    };

    state_manager.seed_test_state(&initial_state).await.unwrap();

    // Independent writers may replace one another, but no reader may see a
    // mixture of their snapshots.
    let writers = 8;
    let barrier = Arc::new(Barrier::new(writers + 1));
    let mut handles = Vec::new();
    for i in 1..=writers {
        let sm = state_manager.clone();
        let barrier = barrier.clone();
        let mut state = initial_state.clone();
        state.completed_hooks = i;
        state.current_hook_index = Some(1000 + i);
        state.config_hash = format!("{i}:{}", "x".repeat(8192));
        state
            .environment_vars
            .insert("PAYLOAD".to_string(), format!("{i}:{}", "x".repeat(8192)));

        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            for _ in 0..8 {
                sm.seed_test_state(&state)
                    .await
                    .expect("concurrent save must succeed");
            }
        }));
    }

    barrier.wait().await;
    for _ in 0..100 {
        let state = state_manager
            .load_state(&initial_state.instance_hash)
            .await
            .expect("concurrent read must succeed")
            .expect("saved state must exist");
        assert!(state.completed_hooks <= writers);
        assert_eq!(state.current_hook_index, Some(1000 + state.completed_hooks));
        assert_eq!(
            state.config_hash,
            format!("{}:{}", state.completed_hooks, "x".repeat(8192))
        );
        assert!(state.environment_vars.is_empty());
        tokio::task::yield_now().await;
    }
    for handle in handles {
        handle.await.expect("writer task must finish");
    }
    let state = state_manager
        .load_state("concurrent_hash")
        .await
        .unwrap()
        .unwrap();
    assert!((1..=writers).contains(&state.completed_hooks));
    assert_eq!(state.current_hook_index, Some(1000 + state.completed_hooks));
}

#[tokio::test]
async fn state_identifiers_cannot_escape_the_state_directory() {
    let temp_dir = private_temp_dir();
    let state_manager = StateManager::new(temp_dir.path().join("state"));
    let mut state = HookExecutionState::new(
        super::HookExecutionIdentity {
            session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
            directory_path: temp_dir.path().to_path_buf(),
            instance_hash: "valid".to_string(),
            config_hash: "config".to_string(),
        },
        Vec::new(),
    );
    let outside = temp_dir.path().join("outside.json");
    std::fs::write(&outside, "untouched").unwrap();

    for invalid in [
        "",
        ".",
        "..",
        "../outside",
        "/outside",
        "nested/name",
        "CON",
        "prn",
        "AUX",
        "nul",
        "com1",
        "lPt9",
    ] {
        assert!(
            state_manager.try_state_file_path(invalid).is_err(),
            "{invalid:?}"
        );
        assert!(
            state_manager.load_state(invalid).await.is_err(),
            "{invalid:?}"
        );
        assert!(
            state_manager.load_state_sync(invalid).is_err(),
            "{invalid:?}"
        );
        assert!(
            state_manager.remove_state(invalid).await.is_err(),
            "{invalid:?}"
        );
        state.instance_hash = invalid.to_string();
        assert!(
            state_manager.seed_test_state(&state).await.is_err(),
            "{invalid:?}"
        );
    }
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "untouched");

    state.instance_hash = "valid".to_string();
    state_manager.seed_test_state(&state).await.unwrap();
    let mismatched = state_manager.try_state_file_path("other").unwrap();
    std::fs::write(&mismatched, serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(state_manager.load_state("other").await.is_err());
    assert!(state_manager.load_state_sync("other").is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn state_directory_and_files_are_private() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let temp_dir = private_temp_dir();
    let state_dir = temp_dir.path().join("state");
    let state_manager = StateManager::new(state_dir.clone());
    let state = HookExecutionState::new(
        super::HookExecutionIdentity {
            session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
            directory_path: temp_dir.path().to_path_buf(),
            instance_hash: "private".to_string(),
            config_hash: "config".to_string(),
        },
        Vec::new(),
    );
    state_manager.seed_test_state(&state).await.unwrap();
    let state_file = state_manager.try_state_file_path("private").unwrap();
    assert_eq!(std::fs::metadata(&state_dir).unwrap().mode() & 0o077, 0);
    assert_eq!(std::fs::metadata(&state_file).unwrap().mode() & 0o077, 0);

    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(state_manager.seed_test_state(&state).await.is_err());
    assert!(state_manager.load_state("private").await.is_err());
    assert_eq!(std::fs::metadata(&state_dir).unwrap().mode() & 0o777, 0o755);
}

#[cfg(unix)]
#[tokio::test]
async fn state_directory_rejects_symlinks() {
    let temp_dir = private_temp_dir();
    let linked = temp_dir.path().join("linked");
    std::os::unix::fs::symlink(temp_dir.path(), &linked).unwrap();
    let state_manager = StateManager::new(linked);
    assert!(state_manager.ensure_state_dir().await.is_err());
    assert!(state_manager.load_state("valid").await.is_err());
}

#[tokio::test]
async fn failed_state_rename_cleans_up_temporary_file() {
    let temp_dir = private_temp_dir();
    let state_manager = StateManager::new(temp_dir.path().to_path_buf());
    let state = HookExecutionState::new(
        super::HookExecutionIdentity {
            session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
            directory_path: temp_dir.path().to_path_buf(),
            instance_hash: "conflict".to_string(),
            config_hash: "config".to_string(),
        },
        Vec::new(),
    );
    let destination = state_manager.try_state_file_path("conflict").unwrap();
    std::fs::create_dir(&destination).unwrap();
    assert!(state_manager.seed_test_state(&state).await.is_err());
    assert!(destination.is_dir());
    let mut names = std::fs::read_dir(temp_dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, [".hook-state.lock", "conflict.json"]);
}

#[tokio::test]
async fn test_state_with_unicode_and_special_chars() {
    let temp_dir = private_temp_dir();
    let state_manager = StateManager::new(temp_dir.path().to_path_buf());

    // Create state with unicode and special characters
    let mut unicode_state = HookExecutionState {
        session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
        instance_hash: "unicode_hash".to_string(),
        run_id: uuid::Uuid::new_v4(),
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
    state_manager.seed_test_state(&unicode_state).await.unwrap();

    let loaded = state_manager
        .load_state(&unicode_state.instance_hash)
        .await
        .unwrap()
        .unwrap();

    // Metadata keeps Unicode while hook output and errors remain in memory.
    assert_eq!(loaded.config_hash, "config_ñ_é_ü");
    assert_eq!(
        loaded.error_message.as_deref(),
        Some("Hook execution failed")
    );
    assert!(loaded.hook_results.is_empty());
    let persisted = std::fs::read_to_string(
        state_manager
            .try_state_file_path(&unicode_state.instance_hash)
            .unwrap(),
    )
    .unwrap();
    assert!(!persisted.contains("輸出: Hello 世界! 🌍"));
    assert!(!persisted.contains("錯誤信息"));
}

#[tokio::test]
async fn test_state_directory_with_many_states() {
    let temp_dir = private_temp_dir();
    let state_manager = StateManager::new(temp_dir.path().to_path_buf());

    // Create many states to test scalability
    for i in 0..50 {
        let state = HookExecutionState {
            session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
            instance_hash: format!("hash_{}", i),
            run_id: uuid::Uuid::new_v4(),
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
        };
        state_manager.seed_test_state(&state).await.unwrap();
    }

    // List all states
    let listed = state_manager.list_active_states().await.unwrap();
    assert_eq!(listed.len(), 50);

    // Cleanup removes terminal states and retains every running state.
    assert_eq!(state_manager.cleanup_state_directory().await.unwrap(), 33);
    let remaining = state_manager.list_active_states().await.unwrap();
    assert_eq!(remaining.len(), 17);
    assert!(
        remaining
            .iter()
            .all(|state| state.status == ExecutionStatus::Running)
    );
}
