//! Coordinator client for CLI and UI processes.
//!
//! Provides client-side connectivity to the cuenv coordinator server,
//! which manages event routing between producers (CLI commands) and
//! consumers (UI renderers like TUI or JSON output).

// Client has some unused methods reserved for future multi-UI support
#![allow(dead_code)]

use super::discovery::ensure_coordinator_running;
use super::protocol::{ClientType, MessageType, RegisterAckPayload, UiType, WireMessage};
use cuenv_events::CuenvEvent;
use std::io;
use tokio::net::UnixStream;
use uuid::Uuid;

/// Client handle for connecting to the coordinator.
///
/// Manages a Unix socket connection to the coordinator server and handles
/// message serialization/deserialization.
#[derive(Debug)]
pub struct CoordinatorClient {
    /// The Unix socket stream to the coordinator.
    stream: UnixStream,
    /// Unique identifier for this client session.
    client_id: Uuid,
}

impl CoordinatorClient {
    /// Connect to the coordinator as a producer (CLI command).
    ///
    /// Returns `Ok(None)` if the coordinator is unavailable but connection is optional.
    ///
    /// # Errors
    ///
    /// Returns an error if registration fails after connection.
    pub async fn connect_as_producer(command: &str) -> io::Result<Option<Self>> {
        Self::connect(ClientType::Producer {
            command: command.to_string(),
        })
        .await
    }

    /// Connect to the coordinator as a consumer (UI).
    ///
    /// # Errors
    ///
    /// Returns an error if the coordinator is unavailable or registration fails.
    pub async fn connect_as_consumer(ui_type: UiType) -> io::Result<Self> {
        Self::connect(ClientType::Consumer { ui_type })
            .await?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "coordinator not available"))
    }

    /// Connect to the coordinator with the given client type.
    async fn connect(client_type: ClientType) -> io::Result<Option<Self>> {
        // Ensure coordinator is running
        let handle = match ensure_coordinator_running().await {
            Ok(h) => h,
            Err(e) => {
                tracing::debug!(error = %e, "Coordinator not available");
                return Ok(None);
            }
        };

        // Connect to socket
        let mut stream = match UnixStream::connect(&handle.socket).await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = %e, "Failed to connect to coordinator");
                return Ok(None);
            }
        };

        let client_id = Uuid::new_v4();
        let pid = std::process::id();

        // Send registration
        let reg_msg = WireMessage::register(client_id, client_type, pid);
        reg_msg.write_to(&mut stream).await?;

        // Wait for ack
        let ack_msg = WireMessage::read_from(&mut stream).await?;

        if ack_msg.msg_type != MessageType::RegisterAck {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected registration ack",
            ));
        }

        let ack: RegisterAckPayload = serde_json::from_value(ack_msg.payload)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        if !ack.success {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                ack.error
                    .unwrap_or_else(|| "registration failed".to_string()),
            ));
        }

        tracing::debug!(client_id = %client_id, "Connected to coordinator");

        Ok(Some(Self { stream, client_id }))
    }

    /// Send an event to the coordinator.
    ///
    /// # Errors
    ///
    /// Returns an error if the message cannot be written to the socket.
    pub async fn send_event(&mut self, event: &CuenvEvent) -> io::Result<()> {
        let msg = WireMessage::event(event);
        msg.write_to(&mut self.stream).await
    }

    /// Receive an event from the coordinator (for consumers).
    ///
    /// Returns `None` for non-event messages (e.g., ping responses).
    ///
    /// # Errors
    ///
    /// Returns an error if reading from the socket fails.
    pub async fn recv_event(&mut self) -> io::Result<Option<CuenvEvent>> {
        let msg = WireMessage::read_from(&mut self.stream).await?;

        match msg.msg_type {
            MessageType::Event => Ok(msg.into_event()),
            MessageType::Ping => {
                // Respond with pong
                let pong = WireMessage::pong(msg.correlation_id);
                pong.write_to(&mut self.stream).await?;
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// Send a ping and wait for pong.
    ///
    /// # Errors
    ///
    /// Returns an error if sending/receiving fails or the response is not a pong.
    pub async fn ping(&mut self) -> io::Result<()> {
        let ping = WireMessage::ping();
        ping.write_to(&mut self.stream).await?;

        let msg = WireMessage::read_from(&mut self.stream).await?;

        if msg.msg_type == MessageType::Pong {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected pong response",
            ))
        }
    }

    /// Get the client ID.
    #[must_use]
    pub const fn client_id(&self) -> Uuid {
        self.client_id
    }

    /// Check if the connection is still alive.
    pub async fn is_alive(&mut self) -> bool {
        self.ping().await.is_ok()
    }
}

/// Handle returned when starting or connecting to a coordinator.
#[derive(Debug, Clone)]
pub struct CoordinatorHandle {
    /// Coordinator process ID (if we know it).
    pub pid: Option<u32>,
    /// Path to the socket.
    pub socket: std::path::PathBuf,
    /// Whether we started the coordinator.
    pub we_started_it: bool,
}

impl CoordinatorHandle {
    /// Create a handle for an existing coordinator.
    #[must_use]
    pub const fn existing(pid: u32, socket: std::path::PathBuf) -> Self {
        Self {
            pid: Some(pid),
            socket,
            we_started_it: false,
        }
    }

    /// Create a handle for a coordinator we started.
    #[must_use]
    pub const fn new(pid: u32, socket: std::path::PathBuf) -> Self {
        Self {
            pid: Some(pid),
            socket,
            we_started_it: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_coordinator_handle_existing() {
        let socket = PathBuf::from("/tmp/test.sock");
        let handle = CoordinatorHandle::existing(1234, socket.clone());

        assert_eq!(handle.pid, Some(1234));
        assert_eq!(handle.socket, socket);
        assert!(!handle.we_started_it);
    }

    #[test]
    fn test_coordinator_handle_new() {
        let socket = PathBuf::from("/tmp/new.sock");
        let handle = CoordinatorHandle::new(5678, socket.clone());

        assert_eq!(handle.pid, Some(5678));
        assert_eq!(handle.socket, socket);
        assert!(handle.we_started_it);
    }

    #[test]
    fn test_coordinator_handle_clone() {
        let socket = PathBuf::from("/tmp/clone.sock");
        let handle = CoordinatorHandle::new(1111, socket);
        let cloned = handle.clone();

        assert_eq!(handle.pid, cloned.pid);
        assert_eq!(handle.socket, cloned.socket);
        assert_eq!(handle.we_started_it, cloned.we_started_it);
    }

    #[test]
    fn test_coordinator_handle_debug() {
        let socket = PathBuf::from("/var/run/cuenv.sock");
        let handle = CoordinatorHandle::existing(2222, socket);

        let debug = format!("{handle:?}");
        assert!(debug.contains("CoordinatorHandle"));
        assert!(debug.contains("2222"));
        assert!(debug.contains("cuenv.sock"));
    }

    #[test]
    fn test_coordinator_handle_socket_with_spaces() {
        let socket = PathBuf::from("/tmp/path with spaces/test.sock");
        let handle = CoordinatorHandle::new(3333, socket.clone());

        assert_eq!(handle.socket, socket);
    }

    #[test]
    fn test_coordinator_handle_relative_socket() {
        let socket = PathBuf::from("./local.sock");
        let handle = CoordinatorHandle::existing(4444, socket.clone());

        assert_eq!(handle.socket, socket);
    }

    #[test]
    fn test_coordinator_handle_existing_vs_new_difference() {
        let socket = PathBuf::from("/tmp/test.sock");

        let existing = CoordinatorHandle::existing(100, socket.clone());
        let new = CoordinatorHandle::new(100, socket);

        // Both have same pid and socket, but different we_started_it
        assert_eq!(existing.pid, new.pid);
        assert_eq!(existing.socket, new.socket);
        assert!(!existing.we_started_it);
        assert!(new.we_started_it);
    }
}
