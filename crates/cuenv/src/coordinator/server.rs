//! `EventCoordinator` server implementation.
//!
//! The coordinator accepts connections from CLI producers and UI consumers,
//! broadcasting events from producers to all connected consumers.

use super::protocol::{ClientType, MessageType, RegisterPayload, WireMessage};
use super::{pid_path, socket_path};
use cuenv_events::CuenvEvent;
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{RwLock, broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

type ClientRegistry = Arc<RwLock<HashMap<Uuid, ConnectedClient>>>;

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
            idle_timeout: Duration::new(300, 0),
            max_clients: 64,
            heartbeat_interval: Duration::from_secs(30),
            event_buffer_size: 1000,
        }
    }
}

/// Connected client information.
#[derive(Debug)]
struct ConnectedClient {
    tx: mpsc::UnboundedSender<WireMessage>,
}

/// `EventCoordinator` server.
pub struct EventCoordinator {
    config: CoordinatorConfig,
    clients: ClientRegistry,
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
    ///
    /// # Errors
    ///
    /// Returns an error if the socket cannot be created or client handling fails.
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
        clients: ClientRegistry,
        broadcast_tx: broadcast::Sender<CuenvEvent>,
        max_clients: usize,
    ) -> io::Result<()> {
        let Some(registration) = read_registration(&mut stream).await? else {
            return Ok(());
        };

        if !client_capacity_available(&clients, max_clients).await {
            send_capacity_rejection(&mut stream, &registration).await?;
            return Ok(());
        }

        let (tx, rx) = mpsc::unbounded_channel::<WireMessage>();
        register_connected_client(&clients, &registration, tx).await;
        WireMessage::register_ack(registration.client_id, true, None)
            .write_to(&mut stream)
            .await?;

        let runtime = ClientRuntime::new(clients, broadcast_tx, &registration);
        let (read_half, write_half) = stream.into_split();
        let write_task = spawn_client_writer(rx, write_half);

        let runtime = runtime.run(read_half).await;
        runtime.disconnect(write_task).await;

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

async fn read_registration(stream: &mut UnixStream) -> io::Result<Option<RegisterPayload>> {
    let reg_msg = WireMessage::read_from(stream).await?;

    if reg_msg.msg_type != MessageType::Register {
        let error = WireMessage {
            msg_type: MessageType::Error,
            correlation_id: reg_msg.correlation_id,
            payload: serde_json::json!({"error": "expected registration message"}),
        };
        error.write_to(stream).await?;
        return Ok(None);
    }

    serde_json::from_value(reg_msg.payload)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

async fn client_capacity_available(clients: &ClientRegistry, max_clients: usize) -> bool {
    clients.read().await.len() < max_clients
}

async fn send_capacity_rejection(
    stream: &mut UnixStream,
    registration: &RegisterPayload,
) -> io::Result<()> {
    let ack = WireMessage::register_ack(
        registration.client_id,
        false,
        Some("max clients reached".to_string()),
    );
    ack.write_to(stream).await
}

async fn register_connected_client(
    clients: &ClientRegistry,
    registration: &RegisterPayload,
    tx: mpsc::UnboundedSender<WireMessage>,
) {
    let client = ConnectedClient { tx };

    tracing::debug!(
        client_id = %registration.client_id,
        client_type = ?registration.client_type,
        "Client registered"
    );

    clients.write().await.insert(registration.client_id, client);
}

fn spawn_client_writer(
    mut rx: mpsc::UnboundedReceiver<WireMessage>,
    mut write_half: OwnedWriteHalf,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = msg.write_to(&mut write_half).await {
                tracing::debug!(error = %e, "Write error");
                break;
            }
        }
    })
}

struct ClientRuntime {
    client_id: Uuid,
    clients: ClientRegistry,
    broadcast_tx: broadcast::Sender<CuenvEvent>,
    broadcast_rx: Option<broadcast::Receiver<CuenvEvent>>,
}

impl ClientRuntime {
    fn new(
        clients: ClientRegistry,
        broadcast_tx: broadcast::Sender<CuenvEvent>,
        registration: &RegisterPayload,
    ) -> Self {
        let broadcast_rx = if matches!(registration.client_type, ClientType::Consumer { .. }) {
            Some(broadcast_tx.subscribe())
        } else {
            None
        };

        Self {
            client_id: registration.client_id,
            clients,
            broadcast_tx,
            broadcast_rx,
        }
    }

    async fn run(mut self, mut read_half: OwnedReadHalf) -> Self {
        loop {
            tokio::select! {
                result = WireMessage::read_from(&mut read_half) => {
                    match result {
                        Ok(msg) => self.handle_client_message(msg).await,
                        Err(e) => {
                            if e.kind() != io::ErrorKind::UnexpectedEof {
                                tracing::debug!(error = %e, "Read error");
                            }
                            break;
                        }
                    }
                }

                event = self.next_broadcast_event() => {
                    if let Some(event) = event {
                        self.forward_broadcast_event(event).await;
                    }
                }
            }
        }

        self
    }

    async fn handle_client_message(&self, msg: WireMessage) {
        match msg.msg_type {
            MessageType::Event => {
                if let Some(event) = msg.into_event() {
                    let _ = self.broadcast_tx.send(event);
                }
            }
            MessageType::Ping => {
                let clients = self.clients.read().await;
                if let Some(client) = clients.get(&self.client_id) {
                    let _ = client.tx.send(WireMessage::pong(msg.correlation_id));
                }
            }
            _ => {}
        }
    }

    async fn next_broadcast_event(&mut self) -> Option<CuenvEvent> {
        if let Some(rx) = self.broadcast_rx.as_mut() {
            rx.recv().await.ok()
        } else {
            std::future::pending().await
        }
    }

    async fn forward_broadcast_event(&self, event: CuenvEvent) {
        let clients = self.clients.read().await;
        if let Some(client) = clients.get(&self.client_id) {
            let _ = client.tx.send(WireMessage::event(&event));
        }
    }

    async fn disconnect(self, write_task: tokio::task::JoinHandle<()>) {
        self.clients.write().await.remove(&self.client_id);
        write_task.abort();

        tracing::debug!(client_id = %self.client_id, "Client disconnected");
    }
}

impl Default for EventCoordinator {
    fn default() -> Self {
        Self::new()
    }
}
