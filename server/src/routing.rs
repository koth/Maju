use relay_protocol::EncryptedEnvelope;

use crate::errors::{RelayError, Result};
use crate::state::AppState;

/// Route an `EncryptedEnvelope` text frame from `from_device_id` to the
/// paired target. Only `to_device_id` is inspected (for routing and pairing
/// isolation); `ciphertext` and `nonce` are forwarded untouched.
pub async fn route_encrypted(
    state: &AppState,
    from_device_id: &str,
    text: &str,
) -> Result<()> {
    let env: EncryptedEnvelope = serde_json::from_str(text)?;
    let paired = state
        .db
        .pairing_for(from_device_id.to_string(), env.to_device_id.clone())
        .await?;
    if paired.is_none() {
        return Err(RelayError::NotPaired);
    }
    match state.connections.get(&env.to_device_id) {
        Some(tx) => {
            let _ = tx.send(text.to_string()).await;
        }
        None => {
            tracing::debug!(
                to_device_id = %env.to_device_id,
                "target offline; dropping encrypted frame (MVP)"
            );
        }
    }
    Ok(())
}
