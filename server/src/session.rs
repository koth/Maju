use std::time::Duration;

use futures::StreamExt;
use relay_protocol::{Envelope, Message};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite;

use crate::connections::ConnectionId;
use crate::errors::{RelayError, Result};
use crate::state::AppState;

/// Per-connection driver implementing the `Connected -> Authenticated ->
/// Paired -> Disconnected` state machine. Owns the outbound frame sender so
/// handlers can reply, and registers itself in the connections table once
/// authenticated so peers can route `EncryptedEnvelope` frames to it.
pub struct Session {
    state: AppState,
    tx: mpsc::Sender<String>,
    connection_id: ConnectionId,
    device_id: Option<String>,
}

impl Session {
    pub fn new(
        state: AppState,
        tx: mpsc::Sender<String>,
        connection_id: ConnectionId,
    ) -> Self {
        Self {
            state,
            tx,
            connection_id,
            device_id: None,
        }
    }

    pub async fn run<S>(&mut self, mut ws_rx: S)
    where
        S: futures::Stream<
                Item = std::result::Result<tungstenite::Message, tungstenite::Error>,
            > + Unpin,
    {
        let timeout = Duration::from_secs(self.state.config.heartbeat_timeout_secs);
        loop {
            match tokio::time::timeout(timeout, ws_rx.next()).await {
                Err(_) => {
                    tracing::info!("heartbeat timeout; closing connection");
                    break;
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    tracing::warn!(error = %e, "ws read error; closing");
                    break;
                }
                Ok(Some(Ok(msg))) => {
                    if matches!(msg, tungstenite::Message::Close(_)) {
                        break;
                    }
                    if let tungstenite::Message::Text(t) = &msg {
                        if let Err(e) = self.handle_text(t.as_str()).await {
                            // Always name the sender: an unattributed "frame
                            // handling error" made a bad-client report
                            // impossible to trace during the pairing outage.
                            tracing::warn!(
                                error = %e,
                                device_id = ?self.device_id,
                                "frame handling error"
                            );
                        }
                    }
                }
            }
        }
        if let Some(did) = &self.device_id {
            self.state
                .connections
                .remove_if(did, self.connection_id);
            tracing::info!(
                device_id = %did,
                "device disconnected; connection state cleared (pairing/device records persist)"
            );
        }
    }

    async fn handle_text(&mut self, text: &str) -> Result<()> {
        let value: serde_json::Value = serde_json::from_str(text)?;
        if value.get("to_device_id").is_some() {
            let did = self.require_device()?;
            crate::routing::route_encrypted(&self.state, did, text).await?;
            return Ok(());
        }
        let env: Envelope = serde_json::from_value(value)?;
        let msg = env.into_message()?;
        tracing::info!(
            device_id = ?self.device_id,
            message_type = ?std::mem::discriminant(&msg),
            "inbound message"
        );
        match msg {
            Message::DeviceAuth(auth) => {
                if self.device_id.is_some() {
                    return Ok(());
                }
                let did = auth.device_id.clone();
                crate::auth::handle_device_auth(&self.state, auth, &self.tx).await?;
                self.state
                    .connections
                    .insert(&did, self.connection_id, self.tx.clone());
                self.device_id = Some(did);
            }
            Message::BindDeviceRequest(req) => {
                let did = self.require_device()?;
                crate::binding::handle_bind_device(&self.state, req, did, &self.tx).await?;
            }
            Message::PairingRegister(req) => {
                let did = self.require_device()?;
                crate::pairing::handle_pairing_register(&self.state, req, did, &self.tx).await?;
            }
            Message::PairingInitiate(pi) => {
                let did = self.require_device()?;
                crate::pairing::handle_pairing_initiate(&self.state, pi, did, &self.tx).await?;
            }
            Message::PairingResume(pr) => {
                let did = self.require_device()?;
                crate::pairing::handle_pairing_resume(&self.state, pr, did, &self.tx).await?;
            }
            Message::PeerSessionReset(_) => {
                let did = self.require_device()?;
                crate::routing::route_peer_session_reset(&self.state, did).await?;
            }
            other => {
                tracing::debug!(message = ?other, "ignoring envelope");
            }
        }
        Ok(())
    }

    fn require_device(&self) -> Result<&str> {
        self.device_id
            .as_deref()
            .ok_or_else(|| RelayError::Other("not authenticated".into()))
    }
}
