use std::error::Error;
use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn identity() -> crate::state::HookRunIdentity {
    crate::state::HookRunIdentity {
        directory_key: "a".repeat(64),
        instance_hash: "b".repeat(64),
        run_id: uuid::Uuid::new_v4(),
    }
}

#[tokio::test]
async fn same_user_can_read_multiple_connections() -> TestResult {
    let identity = identity();
    let mut listener = super::bind(Path::new("."), &identity)?;
    let worker = tokio::spawn(async move {
        for _ in 0..2 {
            let mut server = listener.accept().await?;
            super::authenticate_server(&mut server).await?;
            let mut value = [0_u8; 1];
            server
                .read_exact(&mut value)
                .await
                .map_err(|_| crate::Error::configuration("Test pipe read failed"))?;
            server
                .write_all(&value)
                .await
                .map_err(|_| crate::Error::configuration("Test pipe write failed"))?;
        }
        Ok::<(), crate::Error>(())
    });
    for value in [41_u8, 42] {
        let mut client = super::connect(Path::new("."), &identity)
            .await?
            .ok_or("expected a live broker")?;
        client.write_all(&[value]).await?;
        let mut echoed = [0_u8; 1];
        client.read_exact(&mut echoed).await?;
        assert_eq!(echoed, [value]);
    }
    tokio::time::timeout(Duration::from_secs(15), worker).await???;
    Ok(())
}

#[tokio::test]
async fn preexisting_pipe_cannot_be_replaced() -> TestResult {
    let identity = identity();
    let name = super::pipe_name(&identity)?;
    let occupied = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&name)?;
    assert!(super::bind(Path::new("."), &identity).is_err());
    drop(occupied);
    assert!(super::bind(Path::new("."), &identity).is_ok());
    Ok(())
}

#[tokio::test]
async fn stale_cleanup_requires_a_missing_pipe() -> TestResult {
    let identity = identity();
    let path = Path::new(".");
    assert!(super::remove_stale(path, &identity).await.is_ok());

    let listener = super::bind(path, &identity)?;
    assert!(super::remove_stale(path, &identity).await.is_err());
    drop(listener);

    let listener = super::bind(path, &identity)?;
    let client = ClientOptions::new().open(super::pipe_name(&identity)?)?;
    assert!(super::remove_stale(path, &identity).await.is_err());

    drop(client);
    drop(listener);
    assert!(super::remove_stale(path, &identity).await.is_ok());
    Ok(())
}

#[tokio::test]
async fn malformed_handshake_is_rejected_and_listener_stays_available() -> TestResult {
    let identity = identity();
    let mut listener = super::bind(Path::new("."), &identity)?;
    let name = super::pipe_name(&identity)?;
    let mut client = ClientOptions::new().open(&name)?;
    let mut server = listener.accept().await?;
    client.write_all(&[super::GREETING ^ 1]).await?;
    assert!(super::authenticate_server(&mut server).await.is_err());
    drop(server);
    drop(client);

    let worker = tokio::spawn(async move {
        let mut server = listener.accept().await?;
        super::authenticate_server(&mut server).await
    });
    let _client = super::connect(Path::new("."), &identity)
        .await?
        .ok_or("expected a live broker")?;
    tokio::time::timeout(Duration::from_secs(15), worker).await???;
    Ok(())
}

#[test]
fn nil_and_nonrandom_run_ids_do_not_name_pipes() -> TestResult {
    let mut identity = identity();
    identity.run_id = uuid::Uuid::nil();
    assert!(super::pipe_name(&identity).is_err());
    assert!(super::bind(Path::new("."), &identity).is_err());
    Ok(())
}
