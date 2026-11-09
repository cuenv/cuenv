use std::collections::HashMap;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use super::{Broker, BrokerLifecycle, MAX_REQUEST_BYTES, read_snapshot, transport};
use crate::state::{HookExecutionIdentity, HookExecutionState, HookRunIdentity};

#[tokio::test]
async fn broker_serves_exact_generation_to_multiple_readers_and_expires() {
    let directory = tempfile::tempdir().unwrap();
    let identity = HookRunIdentity {
        directory_key: "a".repeat(64),
        instance_hash: "b".repeat(64),
        run_id: Uuid::new_v4(),
    };
    let mut state = HookExecutionState::new(
        HookExecutionIdentity {
            session_id: uuid::Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
            directory_path: directory.path().to_path_buf(),
            instance_hash: identity.instance_hash.clone(),
            config_hash: "config".to_string(),
        },
        Vec::new(),
    );
    state.run_id = identity.run_id;
    state.environment_vars = HashMap::from([("VALUE".to_string(), "private-value".to_string())]);
    let broker = Broker::prepare(directory.path(), &identity, &state).unwrap();
    #[cfg(target_os = "linux")]
    let namespace = broker.namespace();
    let (expired, observed_expiry) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        broker
            .serve_until_idle(
                BrokerLifecycle {
                    is_current: || async { Ok(true) },
                    before_stop: || async { Ok(()) },
                    after_stop: || async move {
                        expired.send(()).unwrap();
                        Ok(())
                    },
                },
                Duration::from_millis(250),
            )
            .await
            .unwrap();
    });

    let mut wrong = identity.clone();
    wrong.instance_hash = "c".repeat(64);
    assert!(
        read_snapshot(
            directory.path(),
            &wrong,
            #[cfg(target_os = "linux")]
            namespace,
        )
        .await
        .is_err()
    );

    let mut malformed = transport::connect(directory.path(), &identity)
        .await
        .unwrap()
        .unwrap();
    malformed
        .write_u32(u32::try_from(MAX_REQUEST_BYTES + 1).unwrap())
        .await
        .unwrap();
    drop(malformed);

    let readers = (0..4)
        .map(|_| {
            let directory = directory.path().to_path_buf();
            let identity = identity.clone();
            tokio::spawn(async move {
                read_snapshot(
                    &directory,
                    &identity,
                    #[cfg(target_os = "linux")]
                    namespace,
                )
                .await
                .unwrap()
                .unwrap()
            })
        })
        .collect::<Vec<_>>();
    for reader in readers {
        assert_eq!(
            reader.await.unwrap().environment_vars,
            state.environment_vars
        );
    }
    assert!(
        std::fs::read_dir(directory.path())
            .unwrap()
            .next()
            .is_none()
    );

    tokio::time::timeout(Duration::from_secs(3), observed_expiry)
        .await
        .unwrap()
        .unwrap();
    server.await.unwrap();
    assert!(
        read_snapshot(
            directory.path(),
            &identity,
            #[cfg(target_os = "linux")]
            namespace,
        )
        .await
        .unwrap()
        .is_none()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn broker_namespace_evidence_must_be_available_and_match() {
    let current = super::current_namespace().unwrap();
    let different = super::BrokerNamespace {
        device: current.device,
        inode: current.inode.wrapping_add(1),
    };
    assert!(super::verify_namespace(different).is_err());
    let directory = tempfile::tempdir().unwrap();
    assert!(super::namespace_at(&directory.path().join("missing")).is_err());
}
