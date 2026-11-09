use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::{
    HookChildStart, HookChildUpdate, HookRecoveryOutcome, HookReservation, HookReservationPhase,
    HookRunIdentity, HookRunUpdate, LockedState, StateManager,
};
use crate::state::{HookExecutionIdentity, HookExecutionState};
use crate::types::ExecutionStatus;
use tempfile::TempDir;
use uuid::Uuid;

#[cfg(unix)]
fn private_temp_dir() -> TempDir {
    use std::os::unix::fs::PermissionsExt;
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

#[cfg(not(unix))]
fn private_temp_dir() -> TempDir {
    TempDir::new().unwrap()
}

fn new_state(directory: &Path, config: &str) -> HookExecutionState {
    HookExecutionState::new(
        HookExecutionIdentity {
            session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
            directory_path: directory.to_path_buf(),
            instance_hash: "initial".to_string(),
            config_hash: config.to_string(),
        },
        Vec::new(),
    )
}

async fn begin_and_join(
    manager: &StateManager,
    directory: &Path,
    config: &str,
) -> (HookRunIdentity, HookExecutionState) {
    let mut state = new_state(directory, config);
    let identity = manager.begin_run(&mut state).await.unwrap();
    assert_eq!(
        manager.join_run(&identity).await.unwrap(),
        HookRunUpdate::Applied
    );
    (identity, state)
}

#[tokio::test]
async fn supervisor_payload_streams_large_frames_and_rejects_partial_input() {
    use tokio::io::AsyncWriteExt;

    let payload = super::SupervisorPayload {
        hooks: vec![crate::Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["x".repeat(256 * 1024)],
            dir: None,
            inputs: Vec::new(),
            source: Some(false),
        }],
        config: crate::HookExecutionConfig::default(),
    };
    let bytes = serde_json::to_vec(&payload).unwrap();
    let (mut writer, mut reader) = tokio::io::duplex(1024);
    let sender = tokio::spawn(async move {
        writer
            .write_u32(u32::try_from(bytes.len()).unwrap())
            .await
            .unwrap();
        writer.write_all(&bytes).await.unwrap();
    });
    let received = super::read_supervisor_payload(&mut reader).await.unwrap();
    sender.await.unwrap();
    assert_eq!(received.hooks[0].args[0].len(), 256 * 1024);
    assert!(received.config.state_dir.is_none());

    let (mut writer, mut reader) = tokio::io::duplex(1024);
    writer.write_u32(8).await.unwrap();
    writer.write_all(b"short").await.unwrap();
    drop(writer);
    assert!(super::read_supervisor_payload(&mut reader).await.is_err());
}

#[tokio::test]
async fn hook_arguments_output_environment_and_errors_are_not_persisted() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let sentinel = "cuenv-secret-state-regression";
    let hook = crate::Hook {
        order: 100,
        propagate: false,
        command: "printf".to_string(),
        args: vec![sentinel.to_string()],
        dir: None,
        inputs: Vec::new(),
        source: Some(true),
    };
    let mut state = new_state(directory.path(), "config");
    state.hooks = vec![hook.clone()];
    state
        .environment_vars
        .insert("VALUE".to_string(), sentinel.to_string());
    state.error_message = Some(sentinel.to_string());
    state.hook_results.insert(
        0,
        crate::HookResult {
            hook,
            success: false,
            exit_status: Some(1),
            stdout: sentinel.to_string(),
            stderr: sentinel.to_string(),
            duration_ms: 1,
            error: Some(sentinel.to_string()),
        },
    );
    assert!(!format!("{state:?}").contains(sentinel));
    assert!(!format!("{:?}", state.hook_results[&0]).contains(sentinel));
    let identity = manager.begin_run(&mut state).await.unwrap();
    assert_eq!(
        manager.join_run(&identity).await.unwrap(),
        HookRunUpdate::Applied
    );
    assert_eq!(
        manager.update_run(&identity, &state).await.unwrap(),
        HookRunUpdate::Applied
    );
    state.status = ExecutionStatus::Failed;
    assert_eq!(
        manager.update_run(&identity, &state).await.unwrap(),
        HookRunUpdate::Applied
    );
    for entry in std::fs::read_dir(manager.state_dir()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            let bytes = std::fs::read(entry.path()).unwrap();
            assert!(
                !bytes
                    .windows(sentinel.len())
                    .any(|part| part == sentinel.as_bytes())
            );
        }
    }
}

#[tokio::test]
async fn sessions_keep_distinct_completed_generations_while_directory_runs_are_serialized() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let (first, mut first_state) = begin_and_join(&manager, directory.path(), "same").await;
    let first_session = first_state.session_id;
    first_state.status = ExecutionStatus::Completed;
    first_state.finished_at = Some(chrono::Utc::now());
    first_state
        .environment_vars
        .insert("VALUE".to_string(), "first".to_string());
    let first_manager = manager.clone();
    let first_identity = first.clone();
    let first_broker = tokio::spawn(async move {
        first_manager
            .publish_and_serve(&first_identity, &first_state)
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if manager
                .state_for_instance(directory.path(), "same", first_session)
                .await
                .ok()
                .flatten()
                .is_some_and(|state| state.status == ExecutionStatus::Completed)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let mut second_state = new_state(directory.path(), "same");
    second_state.session_id = Uuid::new_v4();
    let second_session = second_state.session_id;
    let second = manager.begin_run(&mut second_state).await.unwrap();
    assert_eq!(
        manager.join_run(&second).await.unwrap(),
        HookRunUpdate::Applied
    );
    let first_snapshot = manager
        .state_for_instance(directory.path(), "same", first_session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        first_snapshot
            .environment_vars
            .get("VALUE")
            .map(String::as_str),
        Some("first")
    );
    let second_running = manager
        .state_for_instance(directory.path(), "same", second_session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second_running.status, ExecutionStatus::Running);
    assert!(second_running.environment_vars.is_empty());
    let mut third_state = new_state(directory.path(), "same");
    third_state.session_id = Uuid::new_v4();
    assert!(matches!(
        manager.begin_run(&mut third_state).await,
        Err(crate::Error::Busy)
    ));

    second_state.status = ExecutionStatus::Completed;
    second_state.finished_at = Some(chrono::Utc::now());
    second_state
        .environment_vars
        .insert("VALUE".to_string(), "second".to_string());
    let second_manager = manager.clone();
    let second_identity = second.clone();
    let second_broker = tokio::spawn(async move {
        second_manager
            .publish_and_serve(&second_identity, &second_state)
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if manager
                .state_for_instance(directory.path(), "same", second_session)
                .await
                .ok()
                .flatten()
                .is_some_and(|state| state.status == ExecutionStatus::Completed)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let first_snapshot = manager
        .state_for_instance(directory.path(), "same", first_session)
        .await
        .unwrap()
        .unwrap();
    let second_snapshot = manager
        .state_for_instance(directory.path(), "same", second_session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        first_snapshot
            .environment_vars
            .get("VALUE")
            .map(String::as_str),
        Some("first")
    );
    assert_eq!(
        second_snapshot
            .environment_vars
            .get("VALUE")
            .map(String::as_str),
        Some("second")
    );
    first_broker.abort();
    second_broker.abort();
    let _ = first_broker.await;
    let _ = second_broker.await;
}

#[tokio::test]
async fn automatic_start_does_not_replace_a_new_generation_after_a_stale_miss() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let (old_identity, mut old_state) = begin_and_join(&manager, directory.path(), "same").await;
    let session_id = old_state.session_id;
    old_state.status = ExecutionStatus::Completed;
    let old_manager = manager.clone();
    let old_broker = tokio::spawn(async move {
        old_manager
            .publish_and_serve(&old_identity, &old_state)
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if manager
                .state_for_instance(directory.path(), "same", session_id)
                .await
                .ok()
                .flatten()
                .is_some_and(|state| state.status == ExecutionStatus::Completed)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    old_broker.abort();
    let _ = old_broker.await;

    let (observed, observed_rx) = tokio::sync::oneshot::channel();
    let (resume, resume_rx) = tokio::sync::oneshot::channel();
    let paused_manager = manager.clone();
    let project = directory.path().to_path_buf();
    let delayed_start = tokio::spawn(async move {
        assert!(
            paused_manager
                .state_for_instance(&project, "same", session_id)
                .await
                .unwrap()
                .is_none()
        );
        observed.send(()).unwrap();
        resume_rx.await.unwrap();
        let mut candidate = new_state(&project, "same");
        paused_manager
            .begin_run_with_mode(&mut candidate, super::HookStartMode::IfMissing)
            .await
            .unwrap()
    });
    observed_rx.await.unwrap();

    let (new_identity, mut fresh_state) = begin_and_join(&manager, directory.path(), "same").await;
    fresh_state.status = ExecutionStatus::Completed;
    fresh_state
        .environment_vars
        .insert("VALUE".to_string(), "new".to_string());
    let new_manager = manager.clone();
    let new_broker = tokio::spawn(async move {
        new_manager
            .publish_and_serve(&new_identity, &fresh_state)
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if manager
                .state_for_instance(directory.path(), "same", session_id)
                .await
                .ok()
                .flatten()
                .is_some_and(|state| state.status == ExecutionStatus::Completed)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    resume.send(()).unwrap();
    assert_eq!(
        delayed_start.await.unwrap(),
        super::HookRunStart::AlreadyPresent
    );
    let snapshot = manager
        .state_for_instance(directory.path(), "same", session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot.environment_vars.get("VALUE").map(String::as_str),
        Some("new")
    );
    assert!(manager.reservations().await.unwrap().is_empty());

    let mut changed = new_state(directory.path(), "changed");
    let started = manager
        .begin_run_with_mode(&mut changed, super::HookStartMode::IfMissing)
        .await
        .unwrap();
    let super::HookRunStart::Started(changed_identity) = started else {
        panic!("changed inputs must start a new run")
    };
    assert_eq!(
        manager.recover_drained(&changed_identity).await.unwrap(),
        HookRecoveryOutcome::Recovered
    );
    new_broker.abort();
    let _ = new_broker.await;
}

#[tokio::test]
async fn removing_a_terminal_run_invalidates_its_broker_and_allows_restart() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let (identity, mut state) = begin_and_join(&manager, directory.path(), "cleanup").await;
    let session = state.session_id;
    state.status = ExecutionStatus::Completed;
    state.finished_at = Some(chrono::Utc::now());
    state
        .environment_vars
        .insert("VALUE".to_string(), "private".to_string());
    let owner = manager.clone();
    let broker =
        tokio::spawn(async move { owner.publish_and_serve(&identity, &state).await.unwrap() });
    let snapshot = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(Some(state)) = manager
                .state_for_instance(directory.path(), "cleanup", session)
                .await
                && state.status == ExecutionStatus::Completed
            {
                break state;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(manager.remove_terminal_state(&snapshot).await.unwrap());
    assert!(
        manager
            .state_for_instance(directory.path(), "cleanup", session)
            .await
            .unwrap()
            .is_none()
    );
    let mut replacement = new_state(directory.path(), "cleanup");
    let started = manager
        .begin_run_with_mode(&mut replacement, super::HookStartMode::IfMissing)
        .await
        .unwrap();
    let super::HookRunStart::Started(replacement_identity) = started else {
        panic!("cleaned generation must not block a new run")
    };
    assert_eq!(
        manager
            .recover_drained(&replacement_identity)
            .await
            .unwrap(),
        HookRecoveryOutcome::Recovered
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), broker)
            .await
            .unwrap()
            .unwrap(),
        HookRunUpdate::Applied
    );
    assert!(
        manager
            .with_state_lock(|locked| Ok(locked.broker_leases()?.is_empty()))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn oversized_broker_result_is_refused_before_publication() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let (identity, mut state) = begin_and_join(&manager, directory.path(), "oversized").await;
    state.status = ExecutionStatus::Completed;
    state
        .environment_vars
        .insert("LARGE".to_string(), "x".repeat(17 * 1024 * 1024));
    assert!(manager.publish_and_serve(&identity, &state).await.is_err());
    let reservations = manager.reservations().await.unwrap();
    assert!(
        reservations
            .iter()
            .any(|reservation| reservation.identity == identity)
    );
    let persisted = manager
        .load_state(&identity.instance_hash)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.status, ExecutionStatus::Running);
    assert!(persisted.environment_vars.is_empty());
    assert_eq!(
        manager.recover_drained(&identity).await.unwrap(),
        HookRecoveryOutcome::Recovered
    );
}

#[tokio::test]
async fn escaped_broker_values_respect_the_encoded_limit() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let (identity, mut state) = begin_and_join(&manager, directory.path(), "escaped").await;
    state.status = ExecutionStatus::Completed;
    state
        .environment_vars
        .insert("VALUE".to_string(), "\0".repeat(3 * 1024 * 1024));
    assert!(manager.publish_and_serve(&identity, &state).await.is_err());
    assert!(
        manager
            .reservations()
            .await
            .unwrap()
            .iter()
            .any(|reservation| reservation.identity == identity)
    );
    assert_eq!(
        manager.recover_drained(&identity).await.unwrap(),
        HookRecoveryOutcome::Recovered
    );
}

#[tokio::test]
async fn live_broker_leases_enforce_the_global_capacity() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    manager.ensure_state_dir().await.unwrap();
    let mut live_brokers = Vec::new();
    for _ in 0..super::MAX_BROKER_LEASES {
        let mut state = new_state(directory.path(), "occupied");
        state.instance_hash = Uuid::new_v4().simple().to_string();
        let identity = HookRunIdentity {
            directory_key: "a".repeat(64),
            instance_hash: state.instance_hash.clone(),
            run_id: state.run_id,
        };
        let broker =
            super::super::broker::Broker::prepare(manager.state_dir(), &identity, &state).unwrap();
        let lease = super::BrokerLease {
            identity,
            session_id: state.session_id,
            #[cfg(target_os = "linux")]
            namespace: broker.namespace(),
        };
        live_brokers.push(broker);
        manager
            .with_state_lock(move |locked| {
                super::write_atomic(&locked.broker_lease_path(&lease.identity)?, &lease)
            })
            .await
            .unwrap();
    }
    let (identity, mut state) = begin_and_join(&manager, directory.path(), "new").await;
    state.status = ExecutionStatus::Completed;
    let error = manager
        .publish_and_serve(&identity, &state)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("capacity"));
    assert!(
        manager
            .reservations()
            .await
            .unwrap()
            .iter()
            .any(|reservation| reservation.identity == identity)
    );
    drop(live_brokers);
    assert_eq!(
        manager.recover_drained(&identity).await.unwrap(),
        HookRecoveryOutcome::Recovered
    );
}

#[tokio::test]
async fn concurrent_reclaimers_remove_only_an_absent_exact_broker() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    manager.ensure_state_dir().await.unwrap();
    let lease = super::BrokerLease {
        identity: HookRunIdentity {
            directory_key: "a".repeat(64),
            instance_hash: "b".repeat(64),
            run_id: Uuid::new_v4(),
        },
        session_id: Uuid::new_v4(),
        #[cfg(target_os = "linux")]
        namespace: super::super::broker::current_namespace().unwrap(),
    };
    let inserted = lease.clone();
    manager
        .with_state_lock(move |locked| {
            super::write_atomic(&locked.broker_lease_path(&inserted.identity)?, &inserted)?;
            super::write_atomic(
                &locked.generation_path(&inserted.identity.directory_key, inserted.session_id)?,
                &inserted.identity,
            )
        })
        .await
        .unwrap();
    let first = manager.clone();
    let second = manager.clone();
    let (first, second) = tokio::join!(
        first.reclaim_absent_brokers(),
        second.reclaim_absent_brokers()
    );
    first.unwrap();
    second.unwrap();
    manager
        .with_state_lock(move |locked| {
            assert!(locked.broker_leases()?.is_empty());
            let path = locked.generation_path(&lease.identity.directory_key, lease.session_id)?;
            assert!(!path.exists());
            Ok(())
        })
        .await
        .unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn foreign_or_missing_namespace_evidence_cannot_invalidate_a_generation() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let (identity, mut state) = begin_and_join(&manager, directory.path(), "namespace").await;
    state.status = ExecutionStatus::Completed;
    state.finished_at = Some(chrono::Utc::now());
    assert_eq!(
        manager.update_run(&identity, &state).await.unwrap(),
        HookRunUpdate::Applied
    );
    let current = super::super::broker::current_namespace().unwrap();
    let lease = super::BrokerLease {
        identity: identity.clone(),
        session_id: state.session_id,
        namespace: super::super::broker::BrokerNamespace {
            device: current.device,
            inode: current.inode.wrapping_add(1),
        },
    };
    let inserted = lease.clone();
    manager
        .with_state_lock(move |locked| {
            super::write_atomic(&locked.broker_lease_path(&inserted.identity)?, &inserted)?;
            super::write_atomic(
                &locked.generation_path(&inserted.identity.directory_key, inserted.session_id)?,
                &inserted.identity,
            )
        })
        .await
        .unwrap();
    assert!(
        manager
            .state_for_instance(directory.path(), "namespace", state.session_id)
            .await
            .is_err()
    );
    manager.reclaim_absent_brokers().await.unwrap();
    let retained = lease.clone();
    manager
        .with_state_lock(move |locked| {
            assert!(locked.broker_generation_matches(&retained)?);
            let incomplete = serde_json::json!({
                "identity": &retained.identity,
                "session_id": retained.session_id,
            });
            super::write_atomic(&locked.broker_lease_path(&retained.identity)?, &incomplete)
        })
        .await
        .unwrap();
    assert!(
        manager
            .state_for_instance(directory.path(), "namespace", state.session_id)
            .await
            .is_err()
    );
    assert!(manager.reclaim_absent_brokers().await.is_err());
    let generation = manager
        .with_state_lock(move |locked| {
            super::read_optional::<HookRunIdentity>(
                &locked.generation_path(&identity.directory_key, state.session_id)?,
            )
        })
        .await
        .unwrap();
    assert!(generation.is_some());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn drained_recovery_can_remove_an_exact_foreign_namespace_lease() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let (identity, state) = begin_and_join(&manager, directory.path(), "recover-namespace").await;
    let current = super::super::broker::current_namespace().unwrap();
    let lease = super::BrokerLease {
        identity: identity.clone(),
        session_id: state.session_id,
        namespace: super::super::broker::BrokerNamespace {
            device: current.device,
            inode: current.inode.wrapping_add(1),
        },
    };
    let inserted = lease.clone();
    manager
        .with_state_lock(move |locked| {
            super::write_atomic(&locked.broker_lease_path(&inserted.identity)?, &inserted)
        })
        .await
        .unwrap();
    assert_eq!(
        manager.recover_drained(&identity).await.unwrap(),
        HookRecoveryOutcome::Recovered
    );
    manager
        .with_state_lock(move |locked| {
            assert!(locked.broker_lease(&lease.identity)?.is_none());
            assert!(locked.reservation(&lease.identity.directory_key)?.is_none());
            Ok(())
        })
        .await
        .unwrap();
}

#[cfg(target_os = "linux")]
async fn completed_foreign_broker(
    manager: &StateManager,
    directory: &Path,
) -> (HookRunIdentity, HookExecutionState) {
    let (identity, mut state) = begin_and_join(manager, directory, "foreign").await;
    state.status = ExecutionStatus::Completed;
    state.finished_at = Some(chrono::Utc::now());
    assert_eq!(
        manager.update_run(&identity, &state).await.unwrap(),
        HookRunUpdate::Applied
    );
    let current = super::super::broker::current_namespace().unwrap();
    let lease = super::BrokerLease {
        identity: identity.clone(),
        session_id: state.session_id,
        namespace: super::super::broker::BrokerNamespace {
            device: current.device,
            inode: current.inode.wrapping_add(1),
        },
    };
    manager
        .with_state_lock(move |locked| {
            super::write_atomic(&locked.broker_lease_path(&lease.identity)?, &lease)?;
            super::write_atomic(
                &locked.generation_path(&lease.identity.directory_key, lease.session_id)?,
                &lease.identity,
            )
        })
        .await
        .unwrap();
    (identity, state)
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn completed_foreign_broker_can_be_listed_and_recovered_after_drain() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let (identity, state) = completed_foreign_broker(&manager, directory.path()).await;
    let entries = manager.reservations().await.unwrap();
    assert!(entries.iter().any(|entry| {
        entry.identity == identity && entry.phase == HookReservationPhase::ForeignBroker
    }));
    assert!(
        manager
            .state_for_instance(directory.path(), "foreign", state.session_id)
            .await
            .is_err()
    );
    assert_eq!(
        manager.recover_drained(&identity).await.unwrap(),
        HookRecoveryOutcome::Recovered
    );
    assert!(manager.reservations().await.unwrap().is_empty());
    assert!(
        manager
            .state_for_instance(directory.path(), "foreign", state.session_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn recovering_a_foreign_broker_does_not_touch_a_newer_run() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let (old, _) = completed_foreign_broker(&manager, directory.path()).await;
    let mut fresh = new_state(directory.path(), "foreign");
    let new = manager.begin_run(&mut fresh).await.unwrap();
    assert_ne!(old.run_id, new.run_id);
    assert_eq!(
        manager.recover_drained(&old).await.unwrap(),
        HookRecoveryOutcome::Recovered
    );
    let reservation = manager
        .current_reservation(directory.path())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reservation.identity, new);
    let state = manager
        .load_state(&new.instance_hash)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.run_id, new.run_id);
    assert_eq!(state.status, ExecutionStatus::Running);
    assert_eq!(
        manager.recover_drained(&new).await.unwrap(),
        HookRecoveryOutcome::Recovered
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn completed_live_broker_cannot_be_recovered_until_the_listener_stops() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let (identity, mut state) = begin_and_join(&manager, directory.path(), "live").await;
    state.status = ExecutionStatus::Completed;
    state.finished_at = Some(chrono::Utc::now());
    assert_eq!(
        manager.update_run(&identity, &state).await.unwrap(),
        HookRunUpdate::Applied
    );
    let broker =
        super::super::broker::Broker::prepare(manager.state_dir(), &identity, &state).unwrap();
    let lease = super::BrokerLease {
        identity: identity.clone(),
        session_id: state.session_id,
        namespace: broker.namespace(),
    };
    manager
        .with_state_lock(move |locked| {
            super::write_atomic(&locked.broker_lease_path(&lease.identity)?, &lease)?;
            super::write_atomic(
                &locked.generation_path(&lease.identity.directory_key, lease.session_id)?,
                &lease.identity,
            )
        })
        .await
        .unwrap();
    assert!(manager.recover_drained(&identity).await.is_err());
    drop(broker);
    assert_eq!(
        manager.recover_drained(&identity).await.unwrap(),
        HookRecoveryOutcome::Recovered
    );
}

#[tokio::test]
async fn cancellation_prevents_stale_updates_and_new_runs_until_acknowledged() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let (first, mut state) = begin_and_join(&manager, directory.path(), "first").await;
    assert_eq!(
        manager
            .request_cancellation(&first, Some("cancel".to_string()))
            .await
            .unwrap(),
        HookRunUpdate::CancelRequested
    );
    state.status = ExecutionStatus::Completed;
    assert_eq!(
        manager.update_run(&first, &state).await.unwrap(),
        HookRunUpdate::CancelRequested
    );
    let mut changed = new_state(directory.path(), "changed");
    assert!(manager.begin_run(&mut changed).await.is_err());
    assert_eq!(
        manager.acknowledge_cancellation(&first).await.unwrap(),
        HookRunUpdate::Applied
    );
    let (second, _) = begin_and_join(&manager, directory.path(), "changed").await;
    assert_ne!(first.run_id, second.run_id);
    assert_eq!(
        manager.update_run(&first, &state).await.unwrap(),
        HookRunUpdate::Superseded
    );
    assert_eq!(
        manager.request_cancellation(&first, None).await.unwrap(),
        HookRunUpdate::Superseded
    );
    assert_eq!(
        manager.recover_drained(&first).await.unwrap(),
        HookRecoveryOutcome::Stale
    );
    assert_eq!(
        manager.request_cancellation(&second, None).await.unwrap(),
        HookRunUpdate::CancelRequested
    );
    assert_eq!(
        manager.acknowledge_cancellation(&second).await.unwrap(),
        HookRunUpdate::Applied
    );
}

#[tokio::test]
async fn recovery_preserves_prior_terminal_state_after_partial_begin() {
    for marker_written in [false, true] {
        let directory = private_temp_dir();
        let manager = StateManager::new(directory.path().join("state"));
        let (old, mut state) = begin_and_join(&manager, directory.path(), "same").await;
        state.status = ExecutionStatus::Completed;
        state.finished_at = Some(chrono::Utc::now());
        assert_eq!(
            manager.update_run(&old, &state).await.unwrap(),
            HookRunUpdate::Applied
        );
        let fresh = HookRunIdentity {
            run_id: Uuid::new_v4(),
            ..old.clone()
        };
        let reservation = HookReservation {
            directory: std::fs::canonicalize(directory.path()).unwrap(),
            identity: fresh.clone(),
            session_id: state.session_id,
            phase: HookReservationPhase::Starting,
            worker_pid: None,
            child_pid: None,
        };
        let new_identity = fresh.clone();
        manager
            .with_state_lock(move |locked| {
                locked.write_reservation(&reservation)?;
                if marker_written {
                    super::write_atomic(
                        &locked.marker_path(&new_identity.directory_key, reservation.session_id)?,
                        &new_identity,
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();
        assert!(
            manager
                .state_for_instance(directory.path(), "same", state.session_id)
                .await
                .is_err()
        );
        assert_eq!(
            manager.recover_drained(&fresh).await.unwrap(),
            HookRecoveryOutcome::Recovered
        );
        let retained = manager
            .load_state(&old.instance_hash)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retained.run_id, old.run_id);
        assert_eq!(retained.status, ExecutionStatus::Completed);
        assert!(manager.reservations().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn a_registered_child_prevents_cancellation_acknowledgement_until_reaped() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let (identity, _) = begin_and_join(&manager, directory.path(), "child").await;
    let mut command = tokio::process::Command::new("sleep");
    command.arg("30").kill_on_drop(true);
    let HookChildStart::Started(mut child) =
        manager.spawn_run_child(&identity, command).await.unwrap()
    else {
        panic!("child must start");
    };
    let pid = child.id().unwrap();
    assert_eq!(
        manager.request_cancellation(&identity, None).await.unwrap(),
        HookRunUpdate::CancelRequested
    );
    assert!(manager.acknowledge_cancellation(&identity).await.is_err());
    assert_eq!(
        manager.reservations().await.unwrap()[0].child_pid,
        Some(pid)
    );
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    assert_eq!(
        manager
            .record_child(&identity, HookChildUpdate::Reaped(pid))
            .await
            .unwrap(),
        HookRunUpdate::CancelRequested
    );
    assert_eq!(
        manager.acknowledge_cancellation(&identity).await.unwrap(),
        HookRunUpdate::Applied
    );
    assert!(manager.reservations().await.unwrap().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_and_cancellation_are_ordered_by_the_same_lock() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let (first, _) = begin_and_join(&manager, directory.path(), "cancel-first").await;
    let marker = directory.path().join("must-not-start");
    let (release, wait) = tokio::sync::oneshot::channel::<()>();
    let task_manager = manager.clone();
    let task_identity = first.clone();
    let task_marker = marker.clone();
    let spawn = tokio::spawn(async move {
        wait.await.unwrap();
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg("printf started > \"$1\"")
            .arg("hook-test")
            .arg(task_marker)
            .kill_on_drop(true);
        task_manager
            .spawn_run_child(&task_identity, command)
            .await
            .unwrap()
    });
    assert_eq!(
        manager.request_cancellation(&first, None).await.unwrap(),
        HookRunUpdate::CancelRequested
    );
    release.send(()).unwrap();
    assert!(matches!(
        spawn.await.unwrap(),
        HookChildStart::CancelRequested
    ));
    assert!(!marker.exists());
    assert_eq!(
        manager.acknowledge_cancellation(&first).await.unwrap(),
        HookRunUpdate::Applied
    );

    let (second, _) = begin_and_join(&manager, directory.path(), "spawn-first").await;
    let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let worker_gate = gate.clone();
    let (entered, observed) = std::sync::mpsc::channel();
    let task_manager = manager.clone();
    let task_identity = second.clone();
    let spawn = tokio::spawn(async move {
        let mut command = tokio::process::Command::new("sleep");
        command.arg("30").kill_on_drop(true);
        task_manager
            .spawn_run_child_with(&task_identity, command, move |event| {
                if matches!(event, super::HookSpawnEvent::Authorized) {
                    entered.send(()).unwrap();
                    let (lock, condition) = &*worker_gate;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = condition.wait(released).unwrap();
                    }
                }
            })
            .await
            .unwrap()
    });
    tokio::task::spawn_blocking(move || observed.recv_timeout(Duration::from_secs(10)).unwrap())
        .await
        .unwrap();
    let cancel_manager = manager.clone();
    let cancel_identity = second.clone();
    let (cancel_started, cancellation_entered) = tokio::sync::oneshot::channel();
    let mut cancel = tokio::spawn(async move {
        cancel_started.send(()).unwrap();
        cancel_manager
            .request_cancellation(&cancel_identity, None)
            .await
            .unwrap()
    });
    cancellation_entered.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut cancel)
            .await
            .is_err(),
        "cancellation must wait for child registration"
    );
    {
        let (lock, condition) = &*gate;
        *lock.lock().unwrap() = true;
        condition.notify_one();
    }
    let HookChildStart::Started(mut child) = spawn.await.unwrap() else {
        panic!("child must start");
    };
    let pid = child.id().unwrap();
    assert_eq!(cancel.await.unwrap(), HookRunUpdate::CancelRequested);
    assert_eq!(
        manager.reservations().await.unwrap()[0].child_pid,
        Some(pid)
    );
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    assert_eq!(
        manager
            .record_child(&second, HookChildUpdate::Reaped(pid))
            .await
            .unwrap(),
        HookRunUpdate::CancelRequested
    );
    assert_eq!(
        manager.acknowledge_cancellation(&second).await.unwrap(),
        HookRunUpdate::Applied
    );
}

#[tokio::test]
async fn exact_recovery_works_when_the_project_directory_disappears() {
    let state_root = private_temp_dir();
    let project = private_temp_dir();
    let manager = StateManager::new(state_root.path().to_path_buf());
    let mut state = new_state(project.path(), "gone");
    let identity = manager.begin_run(&mut state).await.unwrap();
    drop(project);
    let listed = manager.reservations().await.unwrap();
    assert_eq!(listed[0].identity, identity);
    let outdated_identity = HookRunIdentity {
        run_id: Uuid::new_v4(),
        ..identity.clone()
    };
    assert_eq!(
        manager.recover_drained(&outdated_identity).await.unwrap(),
        HookRecoveryOutcome::Stale
    );
    assert_eq!(
        manager.recover_drained(&identity).await.unwrap(),
        HookRecoveryOutcome::Recovered
    );
    assert!(manager.reservations().await.unwrap().is_empty());
}

#[test]
fn cross_process_lock_probe() {
    if let Some(directory) = std::env::var_os("CUENV_HOOK_LOCK_PROBE") {
        let manager = StateManager::new(PathBuf::from(directory));
        let _locked = LockedState::acquire(&manager).unwrap();
        std::fs::write(manager.state_dir().join("lock-held"), "ready").unwrap();
        std::thread::sleep(Duration::from_secs(30));
    } else {
        let directory = private_temp_dir();
        let manager = StateManager::new(directory.path().to_path_buf());
        let _locked = LockedState::acquire(&manager).unwrap();
        assert!(manager.state_dir().join(".hook-state.lock").is_file());
    }
}

#[test]
fn process_crash_releases_the_persistent_state_lock() {
    let directory = private_temp_dir();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("state::coordinator::tests::cross_process_lock_probe")
        .env("CUENV_HOOK_LOCK_PROBE", directory.path())
        .spawn()
        .unwrap();
    let ready = directory.path().join("lock-held");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !ready.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if !ready.exists() {
        child.kill().unwrap();
        child.wait().unwrap();
        panic!("child never acquired the state lock");
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let manager = StateManager::new(directory.path().to_path_buf());
    let _locked = LockedState::acquire(&manager).unwrap();
    assert!(manager.state_dir().join(".hook-state.lock").is_file());
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn failed_child_registration_reaps_the_child_and_keeps_recovery_blocked() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let (identity, _) = begin_and_join(&manager, directory.path(), "spawn-failure").await;
    let reservation_path = manager
        .state_dir()
        .join(format!("{}.reservation", identity.directory_key));
    let backup_path = directory.path().join("backup");
    let (sent, received) = std::sync::mpsc::channel();
    let path = reservation_path.clone();
    let backup = backup_path.clone();
    let mut command = tokio::process::Command::new("sleep");
    command.arg("30").kill_on_drop(true);
    let result = manager
        .spawn_run_child_with(&identity, command, move |event| {
            if let super::HookSpawnEvent::Spawned(pid) = event {
                sent.send(pid).unwrap();
                std::fs::copy(&path, &backup).unwrap();
                std::fs::remove_file(&path).unwrap();
                std::fs::create_dir(&path).unwrap();
            }
        })
        .await;
    assert!(result.is_err());
    let pid = received.recv_timeout(Duration::from_secs(5)).unwrap();
    let pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    assert!(
        rustix::process::test_kill_process(pid).is_err(),
        "child must be reaped"
    );
    assert!(manager.reservations().await.is_err());
    std::fs::remove_dir(&reservation_path).unwrap();
    std::fs::copy(&backup_path, &reservation_path).unwrap();
    assert_eq!(
        manager.reservations().await.unwrap()[0].phase,
        HookReservationPhase::ChildStarting
    );
    assert!(manager.request_cancellation(&identity, None).await.is_err());
    let mut replacement = new_state(directory.path(), "different");
    assert!(manager.begin_run(&mut replacement).await.is_err());
    assert_eq!(
        manager.recover_drained(&identity).await.unwrap(),
        HookRecoveryOutcome::Recovered
    );
}

#[cfg(unix)]
#[tokio::test]
async fn recovery_does_not_follow_legacy_log_symlinks() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let mut state = new_state(directory.path(), "log");
    let identity = manager.begin_run(&mut state).await.unwrap();
    let target = directory.path().join("victim");
    std::fs::write(&target, "untouched").unwrap();
    let log = manager.state_dir().join(format!(
        "{}.{}.supervisor.log",
        identity.instance_hash, identity.run_id
    ));
    std::os::unix::fs::symlink(&target, &log).unwrap();
    assert_eq!(
        manager.recover_drained(&identity).await.unwrap(),
        HookRecoveryOutcome::Recovered
    );
    assert!(
        std::fs::symlink_metadata(&log)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(target).unwrap(), "untouched");
}

#[cfg(unix)]
#[tokio::test]
async fn drained_legacy_recovery_removes_only_exact_regular_payloads() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    manager.ensure_state_dir().await.unwrap();
    let mut state = new_state(directory.path(), "legacy");
    state.instance_hash = "a".repeat(16);
    state.run_id = Uuid::nil();
    state.status = ExecutionStatus::Completed;
    let state_path = manager.try_state_file_path(&state.instance_hash).unwrap();
    std::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    let hooks = manager
        .state_dir()
        .join(format!("{}_hooks.json", state.instance_hash));
    let config = manager
        .state_dir()
        .join(format!("{}_config.json", state.instance_hash));
    let target = directory.path().join("untouched");
    std::fs::write(&target, "secret").unwrap();
    std::os::unix::fs::symlink(&target, &hooks).unwrap();
    std::fs::write(&config, "secret").unwrap();
    let listed = manager.reservations().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].phase, HookReservationPhase::LegacyState);
    let identity = &listed[0].identity;
    assert!(manager.recover_drained(identity).await.is_err());
    assert!(state_path.is_file());
    assert!(target.is_file());
    std::fs::remove_file(&hooks).unwrap();
    std::fs::write(&hooks, "secret").unwrap();
    assert_eq!(
        manager.recover_drained(identity).await.unwrap(),
        HookRecoveryOutcome::Recovered
    );
    assert!(!state_path.exists());
    assert!(!hooks.exists());
    assert!(!config.exists());
    assert_eq!(std::fs::read_to_string(target).unwrap(), "secret");
}

#[tokio::test]
async fn stale_cleanup_cannot_remove_a_restarted_run() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let (first, mut old_state) = begin_and_join(&manager, directory.path(), "same").await;
    old_state.status = ExecutionStatus::Completed;
    old_state.finished_at = Some(chrono::Utc::now());
    assert_eq!(
        manager.update_run(&first, &old_state).await.unwrap(),
        HookRunUpdate::Applied
    );
    let (second, _) = begin_and_join(&manager, directory.path(), "same").await;
    assert!(!manager.remove_terminal_state(&old_state).await.unwrap());
    assert_eq!(
        manager
            .current_reservation(directory.path())
            .await
            .unwrap()
            .unwrap()
            .identity,
        second
    );
    assert_eq!(
        manager.request_cancellation(&second, None).await.unwrap(),
        HookRunUpdate::CancelRequested
    );
    assert_eq!(
        manager.acknowledge_cancellation(&second).await.unwrap(),
        HookRunUpdate::Applied
    );
}

#[tokio::test]
async fn legacy_runs_with_nil_ids_require_the_exact_instance() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    let mut first = new_state(directory.path(), "first");
    first.instance_hash = "a".repeat(16);
    first.run_id = Uuid::nil();
    let mut second = new_state(directory.path(), "second");
    second.instance_hash = "b".repeat(16);
    second.run_id = Uuid::nil();
    manager.seed_test_state(&first).await.unwrap();
    manager.seed_test_state(&second).await.unwrap();
    let listed = manager.reservations().await.unwrap();
    assert_eq!(listed.len(), 2);
    let first_id = listed
        .iter()
        .find(|entry| entry.identity.instance_hash == first.instance_hash)
        .unwrap()
        .identity
        .clone();
    assert_eq!(
        manager.recover_drained(&first_id).await.unwrap(),
        HookRecoveryOutcome::Recovered
    );
    let remaining = manager.reservations().await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].identity.instance_hash, second.instance_hash);
    let mut replacement = new_state(directory.path(), "replacement");
    assert!(manager.begin_run(&mut replacement).await.is_err());
    assert_eq!(
        manager
            .recover_drained(&remaining[0].identity)
            .await
            .unwrap(),
        HookRecoveryOutcome::Recovered
    );
}

#[tokio::test]
async fn malformed_reservations_fail_closed_without_removal() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    manager.ensure_state_dir().await.unwrap();
    let path = manager
        .state_dir()
        .join(format!("{}.reservation", "a".repeat(64)));
    std::fs::write(&path, "not-json").unwrap();
    assert!(manager.reservations().await.is_err());
    let mut state = new_state(directory.path(), "new");
    assert!(manager.begin_run(&mut state).await.is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "not-json");
}

#[tokio::test]
async fn oversized_state_metadata_fails_without_removal() {
    let directory = private_temp_dir();
    let manager = StateManager::new(directory.path().join("state"));
    manager.ensure_state_dir().await.unwrap();
    let path = manager.try_state_file_path(&"a".repeat(16)).unwrap();
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(super::MAX_METADATA_BYTES + 1).unwrap();
    let error = manager.load_state(&"a".repeat(16)).await.unwrap_err();
    assert!(error.to_string().contains("metadata limit"));
    assert_eq!(
        std::fs::metadata(path).unwrap().len(),
        super::MAX_METADATA_BYTES + 1
    );
}
