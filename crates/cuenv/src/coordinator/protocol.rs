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

    #[test]
    fn test_pong_message() {
        let correlation_id = Uuid::new_v4();
        let msg = WireMessage::pong(correlation_id);

        assert_eq!(msg.msg_type, MessageType::Pong);
        assert_eq!(msg.correlation_id, correlation_id);
        assert_eq!(msg.payload, serde_json::Value::Null);
    }

    #[test]
    fn test_register_ack_success() {
        let correlation_id = Uuid::new_v4();
        let msg = WireMessage::register_ack(correlation_id, true, None);

        assert_eq!(msg.msg_type, MessageType::RegisterAck);
        assert_eq!(msg.correlation_id, correlation_id);

        // Verify payload structure
        let payload: RegisterAckPayload =
            serde_json::from_value(msg.payload).expect("should deserialize");
        assert!(payload.success);
        assert!(payload.error.is_none());
    }

    #[test]
    fn test_register_ack_failure() {
        let correlation_id = Uuid::new_v4();
        let error_msg = "registration failed".to_string();
        let msg = WireMessage::register_ack(correlation_id, false, Some(error_msg.clone()));

        assert_eq!(msg.msg_type, MessageType::RegisterAck);

        let payload: RegisterAckPayload =
            serde_json::from_value(msg.payload).expect("should deserialize");
        assert!(!payload.success);
        assert_eq!(payload.error, Some(error_msg));
    }

    #[test]
    fn test_register_consumer() {
        let client_id = Uuid::new_v4();
        let msg = WireMessage::register(
            client_id,
            ClientType::Consumer {
                ui_type: UiType::Tui,
            },
            5678,
        );

        assert_eq!(msg.msg_type, MessageType::Register);
        assert_eq!(msg.correlation_id, client_id);

        let payload: RegisterPayload =
            serde_json::from_value(msg.payload).expect("should deserialize");
        assert_eq!(payload.client_id, client_id);
        assert_eq!(payload.pid, 5678);
        assert!(matches!(
            payload.client_type,
            ClientType::Consumer {
                ui_type: UiType::Tui
            }
        ));
    }

    #[test]
    fn test_register_consumer_web() {
        let client_id = Uuid::new_v4();
        let msg = WireMessage::register(
            client_id,
            ClientType::Consumer {
                ui_type: UiType::Web,
            },
            9999,
        );

        let payload: RegisterPayload =
            serde_json::from_value(msg.payload).expect("should deserialize");
        assert!(matches!(
            payload.client_type,
            ClientType::Consumer {
                ui_type: UiType::Web
            }
        ));
    }

    #[test]
    fn test_register_consumer_external() {
        let client_id = Uuid::new_v4();
        let msg = WireMessage::register(
            client_id,
            ClientType::Consumer {
                ui_type: UiType::External,
            },
            1111,
        );

        let payload: RegisterPayload =
            serde_json::from_value(msg.payload).expect("should deserialize");
        assert!(matches!(
            payload.client_type,
            ClientType::Consumer {
                ui_type: UiType::External
            }
        ));
    }

    #[test]
    fn test_into_event_wrong_message_type() {
        let msg = WireMessage::ping();
        assert!(msg.into_event().is_none());
    }

    #[test]
    fn test_into_event_register_message() {
        let msg = WireMessage::register(
            Uuid::new_v4(),
            ClientType::Producer {
                command: "test".to_string(),
            },
            1234,
        );
        assert!(msg.into_event().is_none());
    }

    #[test]
    fn test_message_type_equality() {
        assert_eq!(MessageType::Event, MessageType::Event);
        assert_eq!(MessageType::Register, MessageType::Register);
        assert_eq!(MessageType::RegisterAck, MessageType::RegisterAck);
        assert_eq!(MessageType::Ping, MessageType::Ping);
        assert_eq!(MessageType::Pong, MessageType::Pong);
        assert_eq!(MessageType::Error, MessageType::Error);
        assert_ne!(MessageType::Ping, MessageType::Pong);
    }

    #[tokio::test]
    async fn test_read_message_too_large() {
        // Create a buffer with a length header indicating a message larger than MAX_MESSAGE_SIZE
        let mut buf = Vec::new();
        let large_size: u32 = MAX_MESSAGE_SIZE + 1;
        buf.extend_from_slice(&large_size.to_be_bytes());
        buf.extend_from_slice(&[0u8; 100]); // some dummy data

        let mut reader = BufReader::new(Cursor::new(buf));
        let result = WireMessage::read_from(&mut reader).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("message too large"));
    }

    #[tokio::test]
    async fn test_read_invalid_json() {
        // Create a buffer with valid length but invalid JSON
        let invalid_json = b"not valid json";
        let len = invalid_json.len() as u32;

        let mut buf = Vec::new();
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(invalid_json);

        let mut reader = BufReader::new(Cursor::new(buf));
        let result = WireMessage::read_from(&mut reader).await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn test_roundtrip_all_message_types() {
        // Test ping
        let ping_wire = WireMessage::ping();
        let mut buf = Vec::new();
        ping_wire.write_to(&mut buf).await.unwrap();
        let mut reader = BufReader::new(Cursor::new(buf));
        let read = WireMessage::read_from(&mut reader).await.unwrap();
        assert_eq!(read.msg_type, MessageType::Ping);

        // Test pong
        let pong_response = WireMessage::pong(Uuid::new_v4());
        let mut buf = Vec::new();
        pong_response.write_to(&mut buf).await.unwrap();
        let mut reader = BufReader::new(Cursor::new(buf));
        let read = WireMessage::read_from(&mut reader).await.unwrap();
        assert_eq!(read.msg_type, MessageType::Pong);

        // Test register
        let register = WireMessage::register(
            Uuid::new_v4(),
            ClientType::Producer {
                command: "test".to_string(),
            },
            1234,
        );
        let mut buf = Vec::new();
        register.write_to(&mut buf).await.unwrap();
        let mut reader = BufReader::new(Cursor::new(buf));
        let read = WireMessage::read_from(&mut reader).await.unwrap();
        assert_eq!(read.msg_type, MessageType::Register);

        // Test register_ack
        let ack = WireMessage::register_ack(Uuid::new_v4(), true, None);
        let mut buf = Vec::new();
        ack.write_to(&mut buf).await.unwrap();
        let mut reader = BufReader::new(Cursor::new(buf));
        let read = WireMessage::read_from(&mut reader).await.unwrap();
        assert_eq!(read.msg_type, MessageType::RegisterAck);
    }

    #[test]
    fn test_wire_message_debug() {
        let msg = WireMessage::ping();
        let debug_str = format!("{msg:?}");
        assert!(debug_str.contains("WireMessage"));
        assert!(debug_str.contains("Ping"));
    }

    #[test]
    fn test_message_type_serde() {
        let msg_type = MessageType::Event;
        let json = serde_json::to_string(&msg_type).unwrap();
        let deserialized: MessageType = serde_json::from_str(&json).unwrap();
        assert_eq!(msg_type, deserialized);
    }

    #[test]
    fn test_client_type_serde() {
        let client_type = ClientType::Producer {
            command: "build".to_string(),
        };
        let json = serde_json::to_string(&client_type).unwrap();
        let deserialized: ClientType = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            deserialized,
            ClientType::Producer { command } if command == "build"
        ));
    }

    #[test]
    fn test_ui_type_serde() {
        for ui_type in [UiType::Tui, UiType::Web, UiType::External] {
            let json = serde_json::to_string(&ui_type).unwrap();
            let deserialized: UiType = serde_json::from_str(&json).unwrap();
            assert_eq!(
                std::mem::discriminant(&ui_type),
                std::mem::discriminant(&deserialized)
            );
        }
    }
}
