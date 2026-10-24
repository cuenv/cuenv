//! A minimal HTTP server standing in for a remote database in tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// -----------------------------------------------------------------------

/// How the fake server answers one request: `None` drops the connection.
pub(super) type Responder = dyn Fn(usize, Value) -> Option<(u16, String)> + Send + Sync;

pub(super) struct FakeServer {
    pub(super) url: String,
    pub(super) requests: Arc<AtomicUsize>,
}

pub(super) async fn fake_server(responder: Arc<Responder>) -> FakeServer {
    fake_server_with_headers(responder, String::new()).await
}

/// A fake server whose every reply also carries `headers` (each line
/// ending in `\r\n`).
pub(super) async fn fake_server_with_headers(
    responder: Arc<Responder>,
    headers: String,
) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&requests);
    let replies = Arc::new(Replies { responder, headers });
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let index = counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(answer(stream, index, Arc::clone(&replies)));
        }
    });
    FakeServer { url, requests }
}

/// How a fake server replies.
struct Replies {
    responder: Arc<Responder>,
    headers: String,
}

async fn answer(mut stream: TcpStream, index: usize, replies: Arc<Replies>) {
    let Replies { responder, headers } = replies.as_ref();
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).await.unwrap();
        if read == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let request_headers = String::from_utf8_lossy(&buffer[..header_end]).to_ascii_lowercase();
    let length: usize = request_headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .map_or(0, |value| value.trim().parse().unwrap());
    while buffer.len() < header_end + length {
        let read = stream.read(&mut chunk).await.unwrap();
        if read == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body: Value = serde_json::from_slice(&buffer[header_end..header_end + length]).unwrap();
    let Some((status, response)) = responder(index, body) else {
        return;
    };
    let reply = format!(
        "HTTP/1.1 {status} Status\r\ncontent-type: application/json\r\n{headers}\
         content-length: {}\r\nconnection: close\r\n\r\n{response}",
        response.len()
    );
    stream.write_all(reply.as_bytes()).await.unwrap();
    stream.shutdown().await.unwrap();
}
