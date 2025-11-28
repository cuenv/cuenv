//! `EventCoordinator` server implementation.
//!
//! The coordinator accepts connections from CLI producers and UI consumers,
//! broadcasting events from producers to all connected consumers.

// Server is a work-in-progress for multi-UI support
#![allow(dead_code, clippy::too_many_lines)]

use super::protocol::{ClientType, MessageType, RegisterPayload, WireMessage};
use super::{pid_path, socket_path};
use cuenv_events::CuenvEvent;
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{RwLock, broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Coordinator configuration.
#[derive(Debug, Clone)]
pub struct CoordinatorConfig {
    /// Idle timeout before auto-exit (`Duration::ZERO` = never exit).
    pub idle_timeout: Duration,
    /// Maximum connected clients.
    pub max_clients: usize,
    /// Heartbeat interval.
    pub heartbeat_interval: Duration,
    /// Event buffer size for slow clients.
    pub event_buffer_size: usize,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(300), // 5 minutes
            max_clients: 64,
            heartbeat_interval: Duration::from_secs(30),
            event_buffer_size: 1000,
        }
    }
}

/// Connected client information.
#[derive(Debug)]
struct ConnectedClient {
    id: Uuid,
    client_type: ClientType,
    pid: u32,
    connected_at: Instant,
    tx: mpsc::UnboundedSender<WireMessage>,
}

/// `EventCoordinator` server.
pub struct EventCoordinator {
    config: CoordinatorConfig,
    clients: Arc<RwLock<HashMap<Uuid, ConnectedClient>>>,
    broadcast_tx: broadcast::Sender<CuenvEvent>,
    shutdown: CancellationToken,
}

impl EventCoordinator {
    /// Create a new coordinator with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(CoordinatorConfig::default())
    }

    /// Create a new coordinator with the given configuration.
    #[must_use]
    pub fn with_config(config: CoordinatorConfig) -> Self {
        let (broadcast_tx, _) = broadcast::channel(config.event_buffer_size);
        Self {
            config,
            clients: Arc::new(RwLock::new(HashMap::new())),
            broadcast_tx,
            shutdown: CancellationToken::new(),
        }
    }

    /// Run the coordinator, listening for connections.
    pub async fn run(&self) -> io::Result<()> {
        let socket = socket_path();

        // Ensure parent directory exists
        if let Some(parent) = socket.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Remove stale socket if it exists
        let _ = tokio::fs::remove_file(&socket).await;

        // Create the listener
        let listener = UnixListener::bind(&socket)?;

        tracing::info!(socket = %socket.display(), "EventCoordinator listening");

        // Write PID file
        let pid = std::process::id();
        tokio::fs::write(pid_path(), pid.to_string()).await?;

        let mut last_activity = Instant::now();
        let mut idle_check = tokio::time::interval(Duration::from_secs(10));

        loop {
            tokio::select! {
                // Accept new connections
                result = listener.accept() => {
                    match result {
                        Ok((stream, _)) => {
                            last_activity = Instant::now();
                            self.handle_connection(stream);
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to accept connection");
                        }
                    }
                }

                // Check idle timeout
                _ = idle_check.tick() => {
                    if self.config.idle_timeout > Duration::ZERO {
                        let client_count = self.clients.read().await.len();
                        if client_count == 0 && last_activity.elapsed() > self.config.idle_timeout {
                            tracing::info!("Idle timeout reached, shutting down");
                            break;
                        }
                    }
                }

                // Handle shutdown signal
                () = self.shutdown.cancelled() => {
                    tracing::info!("Shutdown signal received");
                    break;
                }
            }
        }

        self.cleanup().await;
        Ok(())
    }

    /// Handle a new client connection.
    fn handle_connection(&self, stream: UnixStream) {
        let clients = Arc::clone(&self.clients);
        let broadcast_tx = self.broadcast_tx.clone();
        let max_clients = self.config.max_clients;

        tokio::spawn(async move {
            if let Err(e) = Self::handle_client(stream, clients, broadcast_tx, max_clients).await {
                tracing::debug!(error = %e, "Client connection error");
            }
        });
    }

    /// Handle a single client connection.
    async fn handle_client(
        mut stream: UnixStream,
        clients: Arc<RwLock<HashMap<Uuid, ConnectedClient>>>,
        broadcast_tx: broadcast::Sender<CuenvEvent>,
        max_clients: usize,
    ) -> io::Result<()> {
        // Read registration message
        let reg_msg = WireMessage::read_from(&mut stream).await?;

        if reg_msg.msg_type != MessageType::Register {
            let error = WireMessage {
                msg_type: MessageType::Error,
                correlation_id: reg_msg.correlation_id,
                payload: serde_json::json!({"error": "expected registration message"}),
            };
            error.write_to(&mut stream).await?;
            return Ok(());
        }

        let registration: RegisterPayload = serde_json::from_value(reg_msg.payload)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Check max clients
        {
            let current_count = clients.read().await.len();
            if current_count >= max_clients {
                let ack = WireMessage::register_ack(
                    registration.client_id,
                    false,
                    Some("max clients reached".to_string()),
                );
                ack.write_to(&mut stream).await?;
                return Ok(());
            }
        }

        // Create channel for this client
        let (tx, mut rx) = mpsc::unbounded_channel::<WireMessage>();

        // Register client
        let client = ConnectedClient {
            id: registration.client_id,
            client_type: registration.client_type.clone(),
            pid: registration.pid,
            connected_at: Instant::now(),
            tx,
        };

        tracing::debug!(
            client_id = %registration.client_id,
            client_type = ?registration.client_type,
            "Client registered"
        );

        clients.write().await.insert(registration.client_id, client);

        // Send ack
        let ack = WireMessage::register_ack(registration.client_id, true, None);
        ack.write_to(&mut stream).await?;

        let client_id = registration.client_id;
        let is_consumer = matches!(registration.client_type, ClientType::Consumer { .. });

        // Subscribe to broadcast if consumer
        let mut broadcast_rx = if is_consumer {
            Some(broadcast_tx.subscribe())
        } else {
            None
        };

        let (mut read_half, mut write_half) = stream.into_split();

        // Spawn writer task
        let write_task = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(e) = msg.write_to(&mut write_half).await {
                    tracing::debug!(error = %e, "Write error");
                    break;
                }
            }
        });

        // Main read loop
        loop {
            tokio::select! {
                // Read from client
                result = WireMessage::read_from(&mut read_half) => {
                    match result {
                        Ok(msg) => {
                            match msg.msg_type {
                                MessageType::Event => {
                                    // Broadcast event to all consumers
                                    if let Some(event) = msg.into_event() {
                                        let _ = broadcast_tx.send(event);
                                    }
                                }
                                MessageType::Ping => {
                                    // Respond with pong
                                    let clients = clients.read().await;
                                    if let Some(client) = clients.get(&client_id) {
                                        let _ = client.tx.send(WireMessage::pong(msg.correlation_id));
                                    }
                                }
                                _ => {}
                            }
                        }
                        Err(e) => {
                            if e.kind() != io::ErrorKind::UnexpectedEof {
                                tracing::debug!(error = %e, "Read error");
                            }
                            break;
                        }
                    }
                }

                // Forward broadcast events to consumer
                event = async {
                    if let Some(ref mut rx) = broadcast_rx {
                        rx.recv().await.ok()
                    } else {
                        std::future::pending().await
                    }
                } => {
                    if let Some(event) = event {
                        let clients = clients.read().await;
                        if let Some(client) = clients.get(&client_id) {
                            let _ = client.tx.send(WireMessage::event(&event));
                        }
                    }
                }
            }
        }

        // Cleanup
        clients.write().await.remove(&client_id);
        write_task.abort();

        tracing::debug!(client_id = %client_id, "Client disconnected");

        Ok(())
    }

    /// Clean up resources on shutdown.
    async fn cleanup(&self) {
        // Remove socket file
        let _ = tokio::fs::remove_file(socket_path()).await;
        // Remove PID file
        let _ = tokio::fs::remove_file(pid_path()).await;

        tracing::info!("EventCoordinator shutdown complete");
    }

    /// Signal the coordinator to shut down.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Get the number of connected clients.
    pub async fn client_count(&self) -> usize {
        self.clients.read().await.len()
    }
}

impl Default for EventCoordinator {
    fn default() -> Self {
        Self::new()
    }
}
