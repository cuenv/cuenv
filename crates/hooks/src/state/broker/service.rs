//! Bounded request and response framing for hook-result brokers.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::task::JoinSet;

use super::super::{HookExecutionState, HookRunIdentity};
use crate::{Error, Result};

#[cfg(unix)]
use super::unix as transport;
#[cfg(windows)]
use super::windows as transport;

const MAX_REQUEST_BYTES: usize = 4 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CLIENTS: usize = 32;
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Identifies the Linux network namespace containing an abstract socket.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerNamespace {
    pub device: u64,
    pub inode: u64,
}

#[cfg(target_os = "linux")]
fn namespace_at(path: &Path) -> Result<BrokerNamespace> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(path)
        .map_err(|_| Error::configuration("Cannot inspect hook broker network namespace"))?;
    Ok(BrokerNamespace {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(target_os = "linux")]
pub fn current_namespace() -> Result<BrokerNamespace> {
    namespace_at(Path::new("/proc/thread-self/ns/net"))
}

#[cfg(target_os = "linux")]
pub fn verify_namespace(expected: BrokerNamespace) -> Result<()> {
    if current_namespace()? != expected {
        return Err(Error::configuration(
            "Hook broker belongs to a different network namespace",
        ));
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    identity: HookRunIdentity,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    identity: HookRunIdentity,
    state: HookExecutionState,
}

#[derive(Serialize)]
struct ResponseRef<'response> {
    identity: &'response HookRunIdentity,
    state: &'response HookExecutionState,
}

fn encode_response(identity: &HookRunIdentity, state: &HookExecutionState) -> Result<Vec<u8>> {
    super::super::serialize_bounded(&ResponseRef { identity, state }, MAX_RESPONSE_BYTES)
        .map_err(|_| Error::configuration("Hook broker response exceeds the 16 MiB limit"))
}

pub fn validate_snapshot(identity: &HookRunIdentity, state: &HookExecutionState) -> Result<()> {
    encode_response(identity, state).map(drop)
}

/// A bound listener and immutable response are prepared before publication.
pub struct Broker {
    listener: transport::Listener,
    identity: HookRunIdentity,
    response: Arc<Vec<u8>>,
    #[cfg(target_os = "linux")]
    namespace: BrokerNamespace,
}

pub struct BrokerLifecycle<F, G, H> {
    pub is_current: F,
    pub before_stop: G,
    pub after_stop: H,
}

impl Broker {
    pub fn prepare(
        state_dir: &Path,
        identity: &HookRunIdentity,
        state: &HookExecutionState,
    ) -> Result<Self> {
        if state.run_id != identity.run_id || state.instance_hash != identity.instance_hash {
            return Err(Error::configuration(
                "Hook broker state does not match its run",
            ));
        }
        let response = encode_response(identity, state)?;
        #[cfg(target_os = "linux")]
        let namespace = current_namespace()?;
        let listener = transport::bind(state_dir, identity)?;
        #[cfg(target_os = "linux")]
        verify_namespace(namespace)?;
        Ok(Self {
            listener,
            identity: identity.clone(),
            response: Arc::new(response),
            #[cfg(target_os = "linux")]
            namespace,
        })
    }

    #[cfg(target_os = "linux")]
    pub fn namespace(&self) -> BrokerNamespace {
        self.namespace
    }

    /// Serves readers until idle or until this generation is invalidated.
    pub async fn serve<F, Fut, G, Gut, H, Hut>(
        self,
        lifecycle: BrokerLifecycle<F, G, H>,
    ) -> Result<()>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<bool>>,
        G: FnOnce() -> Gut,
        Gut: Future<Output = Result<()>>,
        H: FnOnce() -> Hut,
        Hut: Future<Output = Result<()>>,
    {
        self.serve_until_idle(lifecycle, IDLE_TIMEOUT).await
    }

    async fn serve_until_idle<F, Fut, G, Gut, H, Hut>(
        mut self,
        lifecycle: BrokerLifecycle<F, G, H>,
        timeout: Duration,
    ) -> Result<()>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<bool>>,
        G: FnOnce() -> Gut,
        Gut: Future<Output = Result<()>>,
        H: FnOnce() -> Hut,
        Hut: Future<Output = Result<()>>,
    {
        let BrokerLifecycle {
            is_current,
            before_stop,
            after_stop,
        } = lifecycle;
        let mut clients = JoinSet::new();
        let idle = tokio::time::sleep(timeout);
        tokio::pin!(idle);
        let mut check = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                () = &mut idle => break,
                _ = check.tick() => {
                    if !is_current().await? { break; }
                }
                Some(result) = clients.join_next(), if !clients.is_empty() => {
                    if matches!(result, Ok(true)) {
                        idle.as_mut().reset(tokio::time::Instant::now() + timeout);
                    }
                }
                accepted = self.listener.accept(), if clients.len() < MAX_CLIENTS => {
                    let stream = accepted?;
                    let identity = self.identity.clone();
                    let response = self.response.clone();
                    clients.spawn(async move {
                        matches!(
                            tokio::time::timeout(CLIENT_TIMEOUT, serve_client(stream, identity, response)).await,
                            Ok(Ok(()))
                        )
                    });
                }
            }
        }
        before_stop().await?;
        clients.abort_all();
        while clients.join_next().await.is_some() {}
        drop(self.listener);
        after_stop().await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;

async fn serve_client(
    mut stream: transport::ServerStream,
    identity: HookRunIdentity,
    response: Arc<Vec<u8>>,
) -> Result<()> {
    transport::authenticate_server(&mut stream).await?;
    let request: Request = read_frame(&mut stream, MAX_REQUEST_BYTES).await?;
    if request.identity != identity {
        return Err(Error::configuration(
            "Hook broker request has a different run identity",
        ));
    }
    write_frame(&mut stream, &response).await
}

/// Reads one exact broker generation. The caller must recheck durable metadata.
pub async fn read_snapshot(
    state_dir: &Path,
    identity: &HookRunIdentity,
    #[cfg(target_os = "linux")] namespace: BrokerNamespace,
) -> Result<Option<HookExecutionState>> {
    tokio::time::timeout(CLIENT_TIMEOUT, async {
        #[cfg(target_os = "linux")]
        verify_namespace(namespace)?;
        let Some(mut stream) = transport::connect(state_dir, identity).await? else {
            return Ok(None);
        };
        let request = serde_json::to_vec(&Request {
            identity: identity.clone(),
        })
        .map_err(|_| Error::serialization("Cannot encode hook broker request"))?;
        write_frame(&mut stream, &request).await?;
        let response: Response = read_frame(&mut stream, MAX_RESPONSE_BYTES).await?;
        if response.identity != *identity
            || response.state.run_id != identity.run_id
            || response.state.instance_hash != identity.instance_hash
        {
            return Err(Error::configuration(
                "Hook broker returned a different run identity",
            ));
        }
        Ok(Some(response.state))
    })
    .await
    .map_err(|_| Error::configuration("Hook broker response timed out"))?
}

pub async fn is_available(
    state_dir: &Path,
    identity: &HookRunIdentity,
    #[cfg(target_os = "linux")] namespace: BrokerNamespace,
) -> Result<bool> {
    #[cfg(target_os = "linux")]
    verify_namespace(namespace)?;
    tokio::time::timeout(CLIENT_TIMEOUT, transport::connect(state_dir, identity))
        .await
        .map_err(|_| Error::configuration("Hook broker connection timed out"))?
        .map(|stream| stream.is_some())
}

pub async fn remove_stale(
    state_dir: &Path,
    identity: &HookRunIdentity,
    #[cfg(target_os = "linux")] namespace: BrokerNamespace,
) -> Result<()> {
    #[cfg(target_os = "linux")]
    verify_namespace(namespace)?;
    tokio::time::timeout(CLIENT_TIMEOUT, transport::remove_stale(state_dir, identity))
        .await
        .map_err(|_| Error::configuration("Hook broker cleanup timed out"))?
}

async fn read_frame<T: DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
    max: usize,
) -> Result<T> {
    let length = stream
        .read_u32()
        .await
        .map_err(|_| Error::configuration("Cannot read hook broker frame"))?
        as usize;
    if length > max {
        return Err(Error::configuration("Hook broker frame is too large"));
    }
    let mut bytes = vec![0; length];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|_| Error::configuration("Cannot read hook broker frame"))?;
    serde_json::from_slice(&bytes).map_err(|_| Error::configuration("Invalid hook broker frame"))
}

async fn write_frame(stream: &mut (impl AsyncWrite + Unpin), bytes: &[u8]) -> Result<()> {
    let length = u32::try_from(bytes.len())
        .map_err(|_| Error::configuration("Hook broker frame is too large"))?;
    stream
        .write_u32(length)
        .await
        .map_err(|_| Error::configuration("Cannot write hook broker frame"))?;
    stream
        .write_all(bytes)
        .await
        .map_err(|_| Error::configuration("Cannot write hook broker frame"))?;
    stream
        .flush()
        .await
        .map_err(|_| Error::configuration("Cannot flush hook broker frame"))
}
