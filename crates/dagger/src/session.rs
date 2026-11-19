//! Dagger Engine session lifecycle management.
//!
//! A single [`DaggerSession`] can be shared across an entire task DAG or CI
//! pipeline run via `Arc<DaggerSession>`, avoiding repeated engine startup and
//! maximizing Dagger's in-session cache reuse.

use std::fmt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::client::DaggerClient;
use crate::config::EngineTarget;
use crate::error::{DaggerError, Result};

const SESSION_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);
const SESSION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

/// Connection details for a running Dagger Engine GraphQL session.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionEndpoint {
    /// Local TCP port exposed by `dagger session`.
    pub port: u16,
    /// Session bearer/basic-auth token.
    pub session_token: String,
}

impl fmt::Debug for SessionEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionEndpoint")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl SessionEndpoint {
    /// GraphQL query endpoint URL (`http://127.0.0.1:<port>/query`).
    #[must_use]
    pub fn query_url(&self) -> String {
        format!("http://127.0.0.1:{}/query", self.port)
    }

    /// Read an existing session from `DAGGER_SESSION_PORT` and `DAGGER_SESSION_TOKEN`.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let port_str = std::env::var("DAGGER_SESSION_PORT").ok()?;
        let session_token = std::env::var("DAGGER_SESSION_TOKEN").ok()?;
        if session_token.is_empty() {
            return None;
        }
        let port = port_str.trim().parse::<u16>().ok()?;
        if port == 0 {
            return None;
        }
        Some(Self {
            port,
            session_token,
        })
    }
}

/// Configuration for connecting to or starting a Dagger Engine session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Target engine (`local`, `cloud`, or remote URI).
    pub engine: EngineTarget,
    /// Working directory for `dagger session` (defaults to current directory).
    pub working_dir: Option<PathBuf>,
    /// Path or binary name for the Dagger CLI (defaults to `"dagger"`).
    pub dagger_bin: PathBuf,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            engine: EngineTarget::Local,
            working_dir: None,
            dagger_bin: PathBuf::from("dagger"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct SessionHandshake {
    port: u16,
    session_token: String,
}

/// A live connection to a Dagger Engine GraphQL session.
///
/// If the session was started by cuenv (`dagger session`), the child process is
/// terminated cleanly on [`Self::shutdown`] or [`Drop`]. If cuenv attached to
/// an outer session via `DAGGER_SESSION_PORT`/`DAGGER_SESSION_TOKEN`, shutdown
/// is a no-op so the parent session remains intact.
#[derive(Debug)]
pub struct DaggerSession {
    endpoint: SessionEndpoint,
    child: Mutex<Option<Child>>,
}

impl DaggerSession {
    /// Attach directly to a known [`SessionEndpoint`] without spawning `dagger session`.
    #[must_use]
    pub fn from_endpoint(endpoint: SessionEndpoint) -> Self {
        Self {
            endpoint,
            child: Mutex::new(None),
        }
    }

    /// Connect to an existing Dagger session from environment variables if
    /// present, otherwise spawn `dagger session`.
    ///
    /// # Errors
    ///
    /// Returns [`DaggerError::Session`] if `dagger` cannot be spawned, times
    /// out before printing its handshake JSON, or prints malformed handshake
    /// output.
    pub async fn connect(config: SessionConfig) -> Result<Self> {
        if let Some(endpoint) = SessionEndpoint::from_env() {
            debug!(
                port = endpoint.port,
                "attaching to existing Dagger session from environment"
            );
            return Ok(Self::from_endpoint(endpoint));
        }

        Self::spawn(session_command(&config), &config).await
    }

    /// Start a fresh session on an explicitly selected engine.
    ///
    /// An inherited session cannot be retargeted. For an explicit local
    /// engine, remove inherited engine selectors and use the CLI's default.
    ///
    /// # Errors
    ///
    /// Returns [`DaggerError::Session`] on spawn or handshake failure.
    pub async fn connect_with_engine(config: SessionConfig) -> Result<Self> {
        let mut cmd = session_command(&config);
        cmd.env_remove("DAGGER_SESSION_PORT")
            .env_remove("DAGGER_SESSION_TOKEN")
            .env_remove("DAGGER_ENGINE")
            .env_remove("DAGGER_CLOUD_ENGINE")
            .env_remove("_EXPERIMENTAL_DAGGER_RUNNER_HOST");
        Self::spawn(cmd, &config).await
    }

    async fn spawn(mut cmd: Command, config: &SessionConfig) -> Result<Self> {
        if let Some(engine) = config.engine.env_value() {
            cmd.arg("--engine").arg(engine);
        }
        if let Some(dir) = &config.working_dir {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().map_err(|err| {
            DaggerError::Session(format!(
                "failed to spawn `{}` session (is Dagger 1.0 installed and on PATH?): {err}",
                config.dagger_bin.display()
            ))
        })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            DaggerError::Session("dagger session did not provide a stdout pipe".to_string())
        })?;
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();

        let bytes_read =
            tokio::time::timeout(SESSION_HANDSHAKE_TIMEOUT, reader.read_line(&mut line))
                .await
                .map_err(|_| {
                    DaggerError::Session(format!(
                        "timed out after {}s waiting for `dagger session` handshake",
                        SESSION_HANDSHAKE_TIMEOUT.as_secs()
                    ))
                })?
                .map_err(|err| {
                    DaggerError::Session(format!(
                        "failed reading `dagger session` handshake: {err}"
                    ))
                })?;

        if bytes_read == 0 {
            let status = child.try_wait().ok().flatten();
            return Err(DaggerError::Session(format!(
                "`dagger session` exited before printing connection info (status: {status:?})"
            )));
        }

        let handshake: SessionHandshake = serde_json::from_str(line.trim()).map_err(|err| {
            DaggerError::Session(format!("invalid dagger session handshake JSON: {err}"))
        })?;
        if handshake.port == 0 || handshake.session_token.is_empty() {
            return Err(DaggerError::Session(
                "dagger session returned an invalid port or empty token".to_string(),
            ));
        }

        debug!(port = handshake.port, "connected to Dagger session");

        Ok(Self {
            endpoint: SessionEndpoint {
                port: handshake.port,
                session_token: handshake.session_token,
            },
            child: Mutex::new(Some(child)),
        })
    }

    /// Return the underlying [`SessionEndpoint`].
    #[must_use]
    pub const fn endpoint(&self) -> &SessionEndpoint {
        &self.endpoint
    }

    /// Construct a [`DaggerClient`] bound to this session.
    ///
    /// # Errors
    ///
    /// Returns [`DaggerError::Transport`] if the HTTP client cannot be built.
    pub fn client(&self) -> Result<DaggerClient> {
        DaggerClient::new(self.endpoint.clone())
    }

    /// Gracefully terminate the spawned `dagger session` child process, if any.
    pub async fn shutdown(&self) {
        let mut guard = self.child.lock().await;
        if let Some(mut child) = guard.take() {
            // Closing stdin tells Dagger to close its listener and clean up.
            drop(child.stdin.take());
            match tokio::time::timeout(SESSION_SHUTDOWN_TIMEOUT, child.wait()).await {
                Ok(Err(error)) => warn!(%error, "failed waiting for Dagger session"),
                Ok(Ok(_)) => {}
                Err(_) => {
                    if let Err(error) = child.kill().await {
                        warn!(%error, "failed terminating Dagger session");
                    }
                }
            }
        }
    }
}

fn session_command(config: &SessionConfig) -> Command {
    let mut command = Command::new(&config.dagger_bin);
    command
        .args(["--silent", "session"])
        // EOF on stdin immediately closes the Dagger session listener.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    command
}

impl Drop for DaggerSession {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.child.try_lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.start_kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_builds_query_url() {
        let endpoint = SessionEndpoint {
            port: 43210,
            session_token: "tok-123".to_string(),
        };
        assert_eq!(endpoint.query_url(), "http://127.0.0.1:43210/query");
    }

    #[test]
    fn handshake_deserializes_standard_payload() {
        let parsed: SessionHandshake =
            serde_json::from_str(r#"{"port":54321,"session_token":"secret-token"}"#)
                .expect("valid handshake");
        assert_eq!(parsed.port, 54321);
        assert_eq!(parsed.session_token, "secret-token");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn session_input_remains_open_until_shutdown() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir()?;
        let dagger_bin = directory.path().join("dagger");
        std::fs::write(
            &dagger_bin,
            r#"#!/bin/sh
printf '%s\n' '{"port":54321,"session_token":"test-token"}'
while IFS= read -r line; do :; done
printf 'closed\n' > closed
"#,
        )?;
        std::fs::set_permissions(&dagger_bin, std::fs::Permissions::from_mode(0o700))?;
        let session = DaggerSession::connect_with_engine(SessionConfig {
            working_dir: Some(directory.path().to_path_buf()),
            dagger_bin,
            ..SessionConfig::default()
        })
        .await?;

        assert_eq!(session.endpoint().port, 54321);
        assert!(!format!("{:?}", session.endpoint()).contains("test-token"));
        {
            let child = session.child.lock().await;
            assert!(
                child
                    .as_ref()
                    .is_some_and(|process| process.stdin.is_some())
            );
        }
        session.shutdown().await;
        assert_eq!(
            std::fs::read_to_string(directory.path().join("closed"))?,
            "closed\n"
        );
        Ok(())
    }
}
