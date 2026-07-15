use relay_protocol::{Envelope, Message, SubscriptionStatus};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::connections::Connections;
use crate::errors::Result;

/// Serialize a typed message into an `Envelope` text frame and send it.
pub async fn send_message(
    tx: &mpsc::Sender<String>,
    id: Option<Uuid>,
    msg: &Message,
) -> Result<()> {
    let env = Envelope::from_message(id, msg)?;
    let text = serde_json::to_string(&env)?;
    let _ = tx.send(text).await;
    Ok(())
}

/// Push a `SubscriptionStatus` to both devices of a pairing (best-effort;
/// offline devices are silently skipped).
pub async fn push_subscription_status(
    connections: &Connections,
    device_a: &str,
    device_b: &str,
    status: &SubscriptionStatus,
) -> Result<()> {
    let env = Envelope::from_message(None, &Message::SubscriptionStatus(status.clone()))?;
    let text = serde_json::to_string(&env)?;
    for dev in [device_a, device_b] {
        if let Some(tx) = connections.get(dev) {
            let _ = tx.send(text.clone()).await;
        }
    }
    Ok(())
}
