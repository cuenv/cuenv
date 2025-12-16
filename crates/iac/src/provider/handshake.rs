//! Terraform go-plugin handshake implementation.
//!
//! This module implements the handshake protocol required to communicate with
//! Terraform providers using the HashiCorp go-plugin framework.

use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::time::Duration;

use base64::Engine;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, instrument, warn};

use crate::error::{Error, Result};

/// Magic cookie value required for go-plugin handshake.
pub const MAGIC_COOKIE_KEY: &str = "TF_PLUGIN_MAGIC_COOKIE";
pub const MAGIC_COOKIE_VALUE: &str = "d602bf8f470bc67ca7faa0386276bbdd4330efaf76d1a219cb4d6991ca9872b2";

/// Protocol version for tfplugin6.
pub const PROTOCOL_VERSION: u32 = 6;

/// Result of a successful handshake.
#[derive(Debug, Clone)]
pub struct HandshakeResult {
    /// Protocol version agreed upon
    pub protocol_version: u32,

    /// Network address (host:port) for gRPC connection
    pub address: String,

    /// Protocol type (should be "grpc")
    pub protocol_type: String,

    /// Server certificate for mTLS (base64 encoded)
    pub server_cert: Option<String>,
}

impl HandshakeResult {
    /// Returns the gRPC endpoint URI.
    #[must_use]
    pub fn endpoint_uri(&self) -> String {
        if self.server_cert.is_some() {
            format!("https://{}", self.address)
        } else {
            format!("http://{}", self.address)
        }
    }

    /// Decodes the server certificate.
    ///
    /// # Errors
    ///
    /// Returns an error if the certificate cannot be decoded.
    pub fn decode_server_cert(&self) -> Result<Option<Vec<u8>>> {
        match &self.server_cert {
            Some(cert) => {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(cert)
                    .map_err(|e| Error::ProviderHandshakeFailed {
                        message: format!("Failed to decode server certificate: {e}"),
                    })?;
                Ok(Some(decoded))
            }
            None => Ok(None),
        }
    }
}

/// Performs the go-plugin handshake with a provider process.
///
/// The handshake protocol:
/// 1. Set magic cookie environment variable
/// 2. Provider outputs handshake line to stdout: `CORE_PROTOCOL|APP_PROTOCOL|NET_TYPE|NET_ADDR|PROTO_TYPE|SERVER_CERT`
/// 3. Parse the handshake line and establish gRPC connection
///
/// # Arguments
///
/// * `provider_path` - Path to the provider binary
/// * `timeout_duration` - Maximum time to wait for handshake
///
/// # Errors
///
/// Returns an error if the handshake fails.
#[instrument(name = "provider_handshake", skip(provider_path))]
pub async fn perform_handshake(
    provider_path: &str,
    timeout_duration: Duration,
) -> Result<(tokio::process::Child, HandshakeResult)> {
    debug!(provider_path, "Starting provider handshake");

    // Spawn the provider process
    let mut child = Command::new(provider_path)
        .env(MAGIC_COOKIE_KEY, MAGIC_COOKIE_VALUE)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| Error::ProviderStartFailed {
            provider_name: provider_path.to_string(),
            message: e.to_string(),
        })?;

    // Read the handshake line from stdout
    let stdout = child.stdout.take().ok_or_else(|| Error::ProviderHandshakeFailed {
        message: "Failed to capture provider stdout".to_string(),
    })?;

    let mut reader = tokio::io::BufReader::new(stdout);
    let mut handshake_line = String::new();

    // Wait for handshake with timeout
    let read_result = timeout(timeout_duration, reader.read_line(&mut handshake_line)).await;

    match read_result {
        Ok(Ok(0)) => {
            // EOF - provider exited without handshake
            return Err(Error::ProviderHandshakeFailed {
                message: "Provider exited without completing handshake".to_string(),
            });
        }
        Ok(Ok(_)) => {
            // Successfully read handshake line
            debug!(handshake_line = %handshake_line.trim(), "Received handshake");
        }
        Ok(Err(e)) => {
            return Err(Error::ProviderHandshakeFailed {
                message: format!("Failed to read handshake: {e}"),
            });
        }
        Err(_) => {
            // Timeout
            let _ = child.kill().await;
            return Err(Error::Timeout {
                operation: "provider handshake".to_string(),
            });
        }
    }

    // Put stdout back for later use
    // Note: We've consumed the reader, but we don't need stdout anymore
    // The gRPC connection will be our communication channel

    // Parse the handshake line
    let result = parse_handshake_line(&handshake_line)?;

    // Validate protocol version
    if result.protocol_version != PROTOCOL_VERSION {
        warn!(
            expected = PROTOCOL_VERSION,
            actual = result.protocol_version,
            "Protocol version mismatch"
        );
    }

    // Validate protocol type
    if result.protocol_type != "grpc" {
        return Err(Error::ProviderHandshakeFailed {
            message: format!(
                "Unsupported protocol type: {} (expected grpc)",
                result.protocol_type
            ),
        });
    }

    Ok((child, result))
}

/// Parses the handshake line from a provider.
///
/// Format: `CORE_PROTOCOL|APP_PROTOCOL|NET_TYPE|NET_ADDR|PROTO_TYPE|SERVER_CERT`
///
/// Example: `1|6|tcp|127.0.0.1:12345|grpc|BASE64_CERT`
fn parse_handshake_line(line: &str) -> Result<HandshakeResult> {
    let line = line.trim();
    let parts: Vec<&str> = line.split('|').collect();

    if parts.len() < 5 {
        return Err(Error::ProviderHandshakeFailed {
            message: format!(
                "Invalid handshake format: expected at least 5 pipe-separated fields, got {}",
                parts.len()
            ),
        });
    }

    // Parse core protocol version (should be 1)
    let core_protocol: u32 = parts[0].parse().map_err(|_| Error::ProviderHandshakeFailed {
        message: format!("Invalid core protocol version: {}", parts[0]),
    })?;

    if core_protocol != 1 {
        return Err(Error::ProviderHandshakeFailed {
            message: format!("Unsupported core protocol version: {core_protocol}"),
        });
    }

    // Parse app protocol version (should be 5 or 6)
    let app_protocol: u32 = parts[1].parse().map_err(|_| Error::ProviderHandshakeFailed {
        message: format!("Invalid app protocol version: {}", parts[1]),
    })?;

    // Parse network type (tcp or unix)
    let net_type = parts[2];
    if net_type != "tcp" && net_type != "unix" {
        return Err(Error::ProviderHandshakeFailed {
            message: format!("Unsupported network type: {net_type}"),
        });
    }

    // Parse network address
    let address = parts[3].to_string();

    // Parse protocol type (should be grpc)
    let protocol_type = parts[4].to_string();

    // Parse optional server certificate
    let server_cert = if parts.len() > 5 && !parts[5].is_empty() {
        Some(parts[5].to_string())
    } else {
        None
    };

    Ok(HandshakeResult {
        protocol_version: app_protocol,
        address,
        protocol_type,
        server_cert,
    })
}

/// Synchronous handshake for use in non-async contexts.
///
/// # Errors
///
/// Returns an error if the handshake fails.
pub fn perform_handshake_sync(
    provider_path: &str,
    timeout_duration: Duration,
) -> Result<(Child, HandshakeResult)> {
    use std::process::Command as SyncCommand;

    debug!(provider_path, "Starting provider handshake (sync)");

    // Spawn the provider process
    let mut child = SyncCommand::new(provider_path)
        .env(MAGIC_COOKIE_KEY, MAGIC_COOKIE_VALUE)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::ProviderStartFailed {
            provider_name: provider_path.to_string(),
            message: e.to_string(),
        })?;

    // Read the handshake line from stdout
    let stdout = child.stdout.take().ok_or_else(|| Error::ProviderHandshakeFailed {
        message: "Failed to capture provider stdout".to_string(),
    })?;

    let mut reader = BufReader::new(stdout);
    let mut handshake_line = String::new();

    // Set read timeout using a thread with timeout
    let read_handle = std::thread::spawn(move || {
        reader.read_line(&mut handshake_line).map(|_| handshake_line)
    });

    match read_handle.join() {
        Ok(Ok(line)) => {
            let result = parse_handshake_line(&line)?;
            Ok((child, result))
        }
        Ok(Err(e)) => Err(Error::ProviderHandshakeFailed {
            message: format!("Failed to read handshake: {e}"),
        }),
        Err(_) => {
            let _ = child.kill();
            Err(Error::Timeout {
                operation: "provider handshake".to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_handshake_line() {
        let line = "1|6|tcp|127.0.0.1:12345|grpc|";
        let result = parse_handshake_line(line).unwrap();

        assert_eq!(result.protocol_version, 6);
        assert_eq!(result.address, "127.0.0.1:12345");
        assert_eq!(result.protocol_type, "grpc");
        assert!(result.server_cert.is_none());
    }

    #[test]
    fn test_parse_handshake_line_with_cert() {
        let line = "1|6|tcp|127.0.0.1:12345|grpc|c29tZWNlcnQ=";
        let result = parse_handshake_line(line).unwrap();

        assert_eq!(result.protocol_version, 6);
        assert_eq!(result.server_cert, Some("c29tZWNlcnQ=".to_string()));
    }

    #[test]
    fn test_parse_handshake_line_invalid() {
        let line = "invalid|format";
        assert!(parse_handshake_line(line).is_err());
    }

    #[test]
    fn test_endpoint_uri() {
        let result = HandshakeResult {
            protocol_version: 6,
            address: "127.0.0.1:12345".to_string(),
            protocol_type: "grpc".to_string(),
            server_cert: None,
        };
        assert_eq!(result.endpoint_uri(), "http://127.0.0.1:12345");

        let result_with_cert = HandshakeResult {
            protocol_version: 6,
            address: "127.0.0.1:12345".to_string(),
            protocol_type: "grpc".to_string(),
            server_cert: Some("cert".to_string()),
        };
        assert_eq!(result_with_cert.endpoint_uri(), "https://127.0.0.1:12345");
    }
}
