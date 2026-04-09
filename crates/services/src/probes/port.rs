//! TCP port readiness probe.

use std::time::Duration;

use async_trait::async_trait;
use tokio::net::TcpStream;

use super::{ProbeOutcome, ProbeRunner};

/// Probes readiness by attempting a TCP connection.
pub struct PortProbe {
    host: String,
    port: u16,
}

impl PortProbe {
    /// Create a new port probe.
    #[must_use]
    pub fn new(host: String, port: u16) -> Self {
        Self { host, port }
    }
}

#[async_trait]
impl ProbeRunner for PortProbe {
    async fn check(&self) -> ProbeOutcome {
        let addr = format!("{}:{}", self.host, self.port);
        match tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(&addr)).await {
            Ok(Ok(_)) => ProbeOutcome::Ready,
            Ok(Err(_)) | Err(_) => ProbeOutcome::NotReady,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_port_probe_not_ready() {
        // Port 1 is unlikely to be listening
        let probe = PortProbe::new("127.0.0.1".to_string(), 1);
        let result = probe.check().await;
        assert!(matches!(result, ProbeOutcome::NotReady));
    }

    #[tokio::test]
    async fn test_port_probe_ready() {
        // Bind a listener, then probe it
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let probe = PortProbe::new("127.0.0.1".to_string(), port);
        let result = probe.check().await;
        assert!(matches!(result, ProbeOutcome::Ready));
    }
}
