//! HTTP readiness probe using raw TCP.
//!
//! Performs a minimal HTTP/1.1 request and checks the status code
//! without pulling in a full HTTP client library.

use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::{ProbeOutcome, ProbeRunner};

/// Probes readiness by making an HTTP request and checking the status code.
pub struct HttpProbe {
    url: String,
    expected_status: Vec<u16>,
    method: String,
}

impl HttpProbe {
    /// Create a new HTTP probe.
    #[must_use]
    pub fn new(url: String, expected_status: Vec<u16>, method: String) -> Self {
        Self {
            url,
            expected_status,
            method,
        }
    }

    /// Parse the URL into (host, port, path) components.
    fn parse_url(&self) -> Option<(String, u16, String)> {
        let url = &self.url;

        let (scheme, rest) = if let Some(rest) = url.strip_prefix("http://") {
            ("http", rest)
        } else if let Some(rest) = url.strip_prefix("https://") {
            ("https", rest)
        } else {
            ("http", url.as_str())
        };

        let (host_port, path) = rest
            .find('/')
            .map_or((rest, "/"), |i| (&rest[..i], &rest[i..]));

        let default_port: u16 = if scheme == "https" { 443 } else { 80 };

        let (host, port) = host_port.rfind(':').map_or((host_port, default_port), |i| {
            let port_str = &host_port[i + 1..];
            port_str
                .parse()
                .map_or((host_port, default_port), |p| (&host_port[..i], p))
        });

        Some((host.to_string(), port, path.to_string()))
    }
}

#[async_trait]
impl ProbeRunner for HttpProbe {
    async fn check(&self) -> ProbeOutcome {
        let Some((host, port, path)) = self.parse_url() else {
            return ProbeOutcome::Fatal(format!("invalid URL: {}", self.url));
        };

        let addr = format!("{host}:{port}");

        let connect_result =
            tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&addr)).await;

        let mut stream = match connect_result {
            Ok(Ok(s)) => s,
            Ok(Err(_)) | Err(_) => return ProbeOutcome::NotReady,
        };

        let request = format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.method, path, host,
        );

        if stream.write_all(request.as_bytes()).await.is_err() {
            return ProbeOutcome::NotReady;
        }

        let mut buf = vec![0u8; 1024];
        let read_result = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await;

        let n = match read_result {
            Ok(Ok(n)) if n > 0 => n,
            _ => return ProbeOutcome::NotReady,
        };

        let response = String::from_utf8_lossy(&buf[..n]);

        // Parse status line: "HTTP/1.1 200 OK"
        let status_code = response
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok());

        match status_code {
            Some(code) if self.expected_status.contains(&code) => ProbeOutcome::Ready,
            Some(_) | None => ProbeOutcome::NotReady,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_url_basic() {
        let probe = HttpProbe::new(
            "http://localhost:3000/health".to_string(),
            vec![200],
            "GET".to_string(),
        );
        let (host, port, path) = probe.parse_url().unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 3000);
        assert_eq!(path, "/health");
    }

    #[test]
    fn test_parse_url_default_port() {
        let probe = HttpProbe::new(
            "http://localhost/health".to_string(),
            vec![200],
            "GET".to_string(),
        );
        let (host, port, path) = probe.parse_url().unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 80);
        assert_eq!(path, "/health");
    }

    #[test]
    fn test_parse_url_no_path() {
        let probe = HttpProbe::new(
            "http://localhost:8080".to_string(),
            vec![200],
            "GET".to_string(),
        );
        let (host, port, path) = probe.parse_url().unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 8080);
        assert_eq!(path, "/");
    }

    #[tokio::test]
    async fn test_http_probe_not_ready() {
        let probe = HttpProbe::new(
            "http://127.0.0.1:1/health".to_string(),
            vec![200],
            "GET".to_string(),
        );
        let result = probe.check().await;
        assert!(matches!(result, ProbeOutcome::NotReady));
    }
}
