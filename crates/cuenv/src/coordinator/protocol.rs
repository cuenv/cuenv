//! Wire protocol for coordinator communication.
//!
//! Messages are framed as length-prefixed JSON:
//! - 4 bytes: big-endian message length
//! - N bytes: JSON payload

// Protocol has some unused fields/methods reserved for future multi-UI support
#![allow(dead_code, clippy::cast_possible_truncation)]

use cuenv_events::CuenvEvent;
use serde::{Deserialize, Serialize};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

/// Maximum message size (1MB).
const MAX_MESSAGE_SIZE: u32 = 1024 * 1024;

/// Wire message envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireMessage {
    /// Message type for routing.
    pub msg_type: MessageType,
    /// Correlation ID for request-response matching.
    pub correlation_id: Uuid,
    /// The actual payload.
    pub payload: serde_json::Value,
}

/// Message types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageType {
    /// Event from producer to coordinator.
    Event,
    /// Client registration.
    Register,
    /// Registration acknowledgment.
    RegisterAck,
    /// Heartbeat request.
    Ping,
    /// Heartbeat response.
    Pong,
    /// Error response.
    Error,
}

/// Client type for registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientType {
    /// CLI process producing events.
    Producer {
        /// Command name being executed.
        command: String,
    },
    /// UI process consuming events.
    Consumer {
        /// UI type.
        ui_type: UiType,
    },
}

/// UI type for consumers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum UiType {
    /// Terminal UI.
    Tui,
    /// Web UI.
    Web,
    /// External/custom UI.
    External,
}

/// Client registration payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterPayload {
    /// Client identifier.
    pub client_id: Uuid,
    /// Client type.
    pub client_type: ClientType,
    /// Process ID.
    pub pid: u32,
}

/// Registration acknowledgment payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAckPayload {
    /// Whether registration was successful.
    pub success: bool,
    /// Error message if registration failed.
    pub error: Option<String>,
}

impl WireMessage {
    /// Create a new event message.
    #[must_use]
    pub fn event(event: &CuenvEvent) -> Self {
        Self {
            msg_type: MessageType::Event,
            correlation_id: event.correlation_id,
            payload: serde_json::to_value(event).unwrap_or(serde_json::Value::Null),
        }
    }

    /// Create a new registration message.
    #[must_use]
    pub fn register(client_id: Uuid, client_type: ClientType, pid: u32) -> Self {
        let payload = RegisterPayload {
            client_id,
            client_type,
            pid,
        };
        Self {
            msg_type: MessageType::Register,
            correlation_id: client_id,
            payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
        }
    }

    /// Create a registration acknowledgment.
    #[must_use]
    pub fn register_ack(correlation_id: Uuid, success: bool, error: Option<String>) -> Self {
        let payload = RegisterAckPayload { success, error };
        Self {
            msg_type: MessageType::RegisterAck,
            correlation_id,
            payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
        }
    }

    /// Create a ping message.
    #[must_use]
    pub fn ping() -> Self {
        Self {
            msg_type: MessageType::Ping,
            correlation_id: Uuid::new_v4(),
            payload: serde_json::Value::Null,
        }
    }

    /// Create a pong message.
    #[must_use]
    pub const fn pong(correlation_id: Uuid) -> Self {
        Self {
            msg_type: MessageType::Pong,
            correlation_id,
            payload: serde_json::Value::Null,
        }
    }

    /// Write this message to a stream.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or I/O fails.
    pub async fn write_to<W: AsyncWrite + Unpin>(&self, writer: &mut W) -> io::Result<()> {
        let json =
            serde_json::to_vec(self).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        let len = json.len() as u32;
        if len > MAX_MESSAGE_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "message too large",
            ));
        }

        writer.write_all(&len.to_be_bytes()).await?;
        writer.write_all(&json).await?;
        writer.flush().await?;

        Ok(())
    }

    /// Read a message from a stream.
    ///
    /// # Errors
    ///
    /// Returns an error if I/O or deserialization fails.
    pub async fn read_from<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Self> {
        let mut len_buf = [0u8; 4];
        reader.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf);

        if len > MAX_MESSAGE_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "message too large",
            ));
        }

        let mut buf = vec![0u8; len as usize];
        reader.read_exact(&mut buf).await?;

        serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Extract the event from this message if it's an event message.
    #[must_use]
    pub fn into_event(self) -> Option<CuenvEvent> {
        if self.msg_type == MessageType::Event {
            serde_json::from_value(self.payload).ok()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn test_write_read_roundtrip() {
        let msg = WireMessage::ping();

        let mut buf = Vec::new();
        msg.write_to(&mut buf).await.unwrap();

        let mut reader = BufReader::new(Cursor::new(buf));
        let read_msg = WireMessage::read_from(&mut reader).await.unwrap();

        assert_eq!(read_msg.msg_type, MessageType::Ping);
        assert_eq!(read_msg.correlation_id, msg.correlation_id);
    }

    #[test]
    fn test_register_message() {
        let msg = WireMessage::register(
            Uuid::new_v4(),
            ClientType::Producer {
                command: "build".to_string(),
            },
            1234,
        );

        assert_eq!(msg.msg_type, MessageType::Register);
    }
}
