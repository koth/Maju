//! Minimal HTTP `/health` endpoint over raw TCP.
//!
//! Implemented without an HTTP framework to avoid pulling new dependencies
//! into the server crate. Only `GET /health` is served; anything else returns
//! a 404. Intended for load-balancer / container liveness probes.

use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::errors::Result;

/// Run the health-check HTTP listener until the task is cancelled.
pub async fn run(addr: SocketAddr) -> Result<()> {
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "health endpoint listening");
    loop {
        match listener.accept().await {
            Ok((mut stream, peer)) => {
                tokio::spawn(async move {
                    if let Err(e) = serve(&mut stream).await {
                        tracing::debug!(%peer, error = %e, "health connection error");
                    }
                });
            }
            Err(e) => tracing::warn!(error = %e, "health accept failed"),
        }
    }
}

async fn serve(stream: &mut tokio::net::TcpStream) -> Result<()> {
    // Read the request line (and discard the rest of the headers); we only
    // need enough to identify the method and path.
    let mut buf = [0u8; 1024];
    let read = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf))
        .await;
    if read.is_err() {
        return Ok(()); // client stalled; drop the connection
    }
    let req = String::from_utf8_lossy(&buf);
    let request_line = req.lines().next().unwrap_or("");
    let body = if request_line.starts_with("GET /health") {
        "ok"
    } else {
        stream
            .write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await?;
        return Ok(());
    };
    let body_bytes = body.as_bytes();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body_bytes.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.write_all(body_bytes).await?;
    Ok(())
}
