//! Outbound relay connection: WS dial, device auth, E2E frame crypto,
//! heartbeat, and reconnect scaffolding.
//!
//! The transport carries raw text frames (JSON). `RelayConnection` owns an
//! optional `SessionKey`: when absent (pre-pairing) it sends/receives plain
//! `Envelope` JSON (used for the `DeviceAuth` handshake); when present
//! (post-pairing) it encrypts each `Envelope` into an `EncryptedEnvelope`
//! and vice versa, so the relay routes ciphertext only. This lets auth and
//! E2E share one transport and makes the driver end-to-end testable.

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use relay_protocol::{EncryptedEnvelope, Envelope, Message};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::WebSocketStream;

use crate::crypto::{SessionKey, decrypt, encrypt};

/// Abstract duplex text-frame transport. The real client uses a TLS
/// WebSocket; tests use a plain-WS mock. Carries raw JSON text so the
/// connection layer can choose plain `Envelope` or `EncryptedEnvelope`
/// framing.
pub trait RelayTransport: Send {
    fn send_text(&mut self, frame: String) -> impl std::future::Future<Output = Result<()>> + Send;
    fn recv_text(&mut self) -> impl std::future::Future<Output = Result<Option<String>>> + Send;
    fn close(&mut self) -> impl std::future::Future<Output = ()> + Send;
}

/// A `tokio-tungstenite` WebSocket transport carrying raw text frames.
pub struct WsTransport<S> {
    stream: WebSocketStream<S>,
}

impl<S> WsTransport<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    pub fn new(stream: WebSocketStream<S>) -> Self {
        Self { stream }
    }
}

impl<S> RelayTransport for WsTransport<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    async fn send_text(&mut self, frame: String) -> Result<()> {
        self.stream
            .send(WsMessage::text(frame))
            .await
            .map_err(|e| anyhow::anyhow!("ws send: {e}"))
    }

    async fn recv_text(&mut self) -> Result<Option<String>> {
        loop {
            match self.stream.next().await {
                None => return Ok(None),
                Some(Ok(WsMessage::Text(text))) => return Ok(Some(text.to_string())),
                Some(Ok(WsMessage::Ping(_))) => {
                    let _ = self.stream.send(WsMessage::Pong(vec![0u8; 0].into())).await;
                    continue;
                }
                Some(Ok(WsMessage::Close(_))) => return Ok(None),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(anyhow::anyhow!("ws recv: {e}")),
            }
        }
    }

    async fn close(&mut self) {
        let _ = self.stream.close(None).await;
    }
}

/// A relay connection with optional E2E encryption.
pub struct RelayConnection<T: RelayTransport> {
    transport: T,
    heartbeat: Duration,
    session_key: Option<SessionKey>,
    peer_device_id: Option<String>,
}

impl<T: RelayTransport> RelayConnection<T> {
    pub fn new(transport: T, heartbeat: Duration) -> Self {
        Self {
            transport,
            heartbeat,
            session_key: None,
            peer_device_id: None,
        }
    }

    /// Install the E2E session key (post-pairing). Subsequent
    /// `send_envelope`/`recv_envelope` calls encrypt/decrypt with it.
    pub fn install_session_key(&mut self, key: SessionKey, peer_device_id: String) {
        self.session_key = Some(key);
        self.peer_device_id = Some(peer_device_id);
    }

    pub fn has_session_key(&self) -> bool {
        self.session_key.is_some()
    }

    /// Send an envelope: encrypt to `EncryptedEnvelope` when a session key
    /// is installed, otherwise send plain `Envelope` JSON (auth phase).
    pub async fn send_envelope(&mut self, envelope: &Envelope) -> Result<()> {
        let frame = match (&self.session_key, &self.peer_device_id) {
            (Some(key), Some(peer)) => {
                let enc = encrypt(key, peer, envelope)?;
                serde_json::to_string(&enc)?
            }
            _ => serde_json::to_string(envelope)?,
        };
        self.transport.send_text(frame).await
    }

    /// Receive the next envelope: decrypt an `EncryptedEnvelope` when a
    /// session key is installed, otherwise parse a plain `Envelope`.
    pub async fn recv_envelope(&mut self) -> Result<Option<Envelope>> {
        let Some(frame) = self.transport.recv_text().await? else {
            return Ok(None);
        };
        let envelope = match &self.session_key {
            Some(key) => {
                let enc: EncryptedEnvelope = serde_json::from_str(&frame)
                    .context("decode encrypted envelope")?;
                decrypt(key, &enc)?
            }
            None => serde_json::from_str(&frame).context("decode plain envelope")?,
        };
        Ok(Some(envelope))
    }

    /// Pre-pairing auth: send a `DeviceAuth` envelope (plain) and await an
    /// ack. Must be called before `install_session_key`.
    pub async fn authenticate(
        &mut self,
        device_id: &str,
        signature: &str,
        timestamp_ms: u64,
    ) -> Result<()> {
        let auth = Message::DeviceAuth(relay_protocol::DeviceAuth {
            device_id: device_id.to_string(),
            signature: signature.to_string(),
            timestamp_ms,
        });
        let envelope = Envelope::from_message(None, &auth)?;
        self.send_envelope(&envelope).await?;
        let ack = self
            .recv_envelope()
            .await?
            .context("relay closed during auth handshake")?;
        match ack.into_message()? {
            Message::DeviceAuth(_) | Message::SubscriptionStatus(_) => Ok(()),
            other => Err(anyhow::anyhow!("unexpected auth response: {other:?}")),
        }
    }

    pub fn heartbeat(&self) -> Duration {
        self.heartbeat
    }

    pub async fn close(&mut self) {
        self.transport.close().await;
    }
}

/// Spawn a mock relay that parses each inbound `Envelope` (plain) and calls
/// `on_envelope` to decide the reply. Used for auth + plaintext routing
/// tests. Returns the `ws://127.0.0.1:PORT` URL to dial.
pub async fn spawn_mock_relay<F>(on_envelope: F) -> Result<String>
where
    F: Fn(Envelope) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Envelope>> + Send>>
        + Send
        + Sync
        + 'static,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let url = format!("ws://127.0.0.1:{port}");
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            if let Ok(ws) = tokio_tungstenite::accept_async(stream).await {
                let mut server = WsTransport::new(ws);
                while let Ok(Some(frame)) = server.recv_text().await {
                    let Ok(envelope) = serde_json::from_str::<Envelope>(&frame) else {
                        break;
                    };
                    match on_envelope(envelope).await {
                        Some(reply) => {
                            let json = serde_json::to_string(&reply).unwrap();
                            if server.send_text(json).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                server.close().await;
            }
        }
    });
    Ok(url)
}

/// Spawn a passthrough mock relay that forwards every raw text frame back to
/// the client unchanged. Used for E2E driver tests where both endpoints
/// encrypt/decrypt and the relay must not inspect payloads. Returns the
/// `ws://127.0.0.1:PORT` URL to dial.
pub async fn spawn_passthrough_relay() -> Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let url = format!("ws://127.0.0.1:{port}");
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            if let Ok(ws) = tokio_tungstenite::accept_async(stream).await {
                let mut server = WsTransport::new(ws);
                while let Ok(Some(frame)) = server.recv_text().await {
                    if server.send_text(frame).await.is_err() {
                        break;
                    }
                }
                server.close().await;
            }
        }
    });
    Ok(url)
}

/// Dial a (plain ws://) endpoint. The real client uses `connect_async` with
/// TLS; tests use this against the mock relays.
pub async fn dial_plain(
    url: &str,
    heartbeat: Duration,
) -> Result<RelayConnection<WsTransport<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>>
{
    let (ws, _response) = tokio_tungstenite::connect_async(url)
        .await
        .context("dial relay")?;
    Ok(RelayConnection::new(WsTransport::new(ws), heartbeat))
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_protocol::{ControlRequest, Message};
    use uuid::Uuid;

    #[tokio::test]
    async fn mock_relay_echoes_envelope_roundtrip() {
        let url = spawn_mock_relay(|envelope| Box::pin(async move { Some(envelope) }))
            .await
            .unwrap();
        let mut conn = dial_plain(&url, Duration::from_secs(30)).await.unwrap();

        let request_id = Uuid::new_v4();
        let envelope = Envelope::from_message(
            Some(request_id),
            &Message::ControlRequest(ControlRequest::Cancel { request_id }),
        )
        .unwrap();
        conn.send_envelope(&envelope).await.unwrap();

        let received = conn
            .recv_envelope()
            .await
            .expect("recv ok")
            .expect("envelope echoed");
        assert_eq!(received, envelope);
        conn.close().await;
    }

    #[tokio::test]
    async fn authenticate_handshake_succeeds_against_mock_relay() {
        use relay_protocol::{DeviceAuth, Message};
        let url = spawn_mock_relay(|envelope| {
            Box::pin(async move {
                if matches!(envelope.into_message().ok(), Some(Message::DeviceAuth(_))) {
                    Some(
                        Envelope::from_message(
            None,
            &Message::DeviceAuth(DeviceAuth {
                device_id: "relay-ack".to_string(),
                signature: String::new(),
                timestamp_ms: 0,
            }),
        )
        .unwrap(),
                    )
                } else {
                    None
                }
            })
        })
        .await
        .unwrap();
        let mut conn = dial_plain(&url, Duration::from_secs(30)).await.unwrap();
        conn.authenticate("dev-pc", "sig-b64", 1_700_000_000_000)
            .await
            .expect("auth handshake succeeds");
        conn.close().await;
    }

    #[tokio::test]
    async fn recv_returns_none_on_clean_close() {
        let url = spawn_mock_relay(|_| Box::pin(async move { None })).await.unwrap();
        let mut conn = dial_plain(&url, Duration::from_secs(30)).await.unwrap();
        let request_id = Uuid::new_v4();
        let envelope = Envelope::from_message(
            Some(request_id),
            &Message::ControlRequest(ControlRequest::Cancel { request_id }),
        )
        .unwrap();
        conn.send_envelope(&envelope).await.unwrap();
        let received = conn.recv_envelope().await.unwrap();
        assert!(received.is_none(), "clean close yields None");
    }

    #[tokio::test]
    async fn e2e_envelope_roundtrips_through_passthrough_relay() {
        // Both endpoints share a session key; the relay forwards ciphertext
        // unchanged. Proves encrypt -> relay -> decrypt recovers the envelope.
        let url = spawn_passthrough_relay().await.unwrap();
        let mut conn = dial_plain(&url, Duration::from_secs(30)).await.unwrap();
        let key = SessionKey::derive(b"pairing-secret", b"kodex-relay-salt");
        conn.install_session_key(key.clone(), "dev-phone".to_string());

        let request_id = Uuid::new_v4();
        let envelope = Envelope::from_message(
            Some(request_id),
            &Message::ControlRequest(ControlRequest::Cancel { request_id }),
        )
        .unwrap();
        conn.send_envelope(&envelope).await.unwrap();
        let received = conn
            .recv_envelope()
            .await
            .expect("recv ok")
            .expect("envelope recovered");
        assert_eq!(received, envelope);
        conn.close().await;
    }
}
