//! Coordinator client for CLI and UI processes.

// Client has some unused methods reserved for future multi-UI support
#![allow(dead_code)]

use super::discovery::ensure_coordinator_running;
use super::protocol::{ClientType, MessageType, RegisterAckPayload, UiType, WireMessage};
use cuenv_events::CuenvEvent;
use std::io;
use tokio::net::UnixStream;
use uuid::Uuid;

/// Client handle for connecting to the coordinator.
#[derive(Debug)]
pub struct CoordinatorClient {
    stream: UnixStream,
    client_id: Uuid,
}

impl CoordinatorClient {
    /// Connect to the coordinator as a producer (CLI command).
    ///
    /// Returns `Ok(None)` if the coordinator is unavailable but connection is optional.
    pub async fn connect_as_producer(command: &str) -> io::Result<Option<Self>> {
        Self::connect(ClientType::Producer {
            command: command.to_string(),
        })
        .await
    }

    /// Connect to the coordinator as a consumer (UI).
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
    pub async fn send_event(&mut self, event: &CuenvEvent) -> io::Result<()> {
        let msg = WireMessage::event(event);
        msg.write_to(&mut self.stream).await
    }

    /// Receive an event from the coordinator (for consumers).
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
