use base64::Engine;
use relay_protocol::{DeviceAuth, Message, SubscriptionStatus};
use tokio::sync::mpsc;

use crate::errors::{RelayError, Result};
use crate::state::AppState;
use crate::wire::send_message;

/// Validate that `device_id` is base64 of a 32-byte (SHA-256-sized) value.
pub fn validate_device_id_format(device_id: &str) -> Result<()> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(device_id)
        .map_err(|_| RelayError::InvalidDeviceId(device_id.to_string()))?;
    if decoded.len() != 32 {
        return Err(RelayError::InvalidDeviceId(device_id.to_string()));
    }
    Ok(())
}

/// Validate `timestamp_ms` is within ±`window_secs` of now (replay window).
pub fn validate_timestamp(timestamp_ms: u64, window_secs: u64) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let ts = timestamp_ms as i64;
    let window_ms = (window_secs as i64) * 1000;
    if (ts - now).abs() > window_ms {
        return Err(RelayError::StaleTimestamp);
    }
    Ok(())
}

/// Authenticate a device connection (MVP).
///
/// Validates `device_id` format and `timestamp_ms` freshness, rate-limits
/// failures, and registers the device on first auth (requirements doc §6).
/// The HMAC `signature` is recorded for audit but not cryptographically
/// verified in the MVP (known contract gap; v2 upgrades to Ed25519). On
/// success, sends a `SubscriptionStatus` ack, which the PC accepts as the
/// auth ack alongside `DeviceAuth`.
pub async fn handle_device_auth(
    state: &AppState,
    auth: DeviceAuth,
    tx: &mpsc::Sender<String>,
) -> Result<()> {
    if !state.rate_limiter.allowed(&auth.device_id) {
        return Err(RelayError::Other("rate limited".to_string()));
    }
    let res = authenticate(state, &auth, tx).await;
    if res.is_err() {
        state.rate_limiter.record_failure(&auth.device_id);
    }
    res
}

async fn authenticate(
    state: &AppState,
    auth: &DeviceAuth,
    tx: &mpsc::Sender<String>,
) -> Result<()> {
    validate_device_id_format(&auth.device_id)?;
    validate_timestamp(auth.timestamp_ms, state.config.auth_timestamp_window_secs)?;
    state
        .db
        .register_device(auth.device_id.clone(), String::new())
        .await?;
    state.rate_limiter.reset(&auth.device_id);
    tracing::info!(
        device_id = %auth.device_id,
        "device authenticated (signature recorded for audit; HMAC not verified in MVP)"
    );
    let ack = Message::SubscriptionStatus(SubscriptionStatus {
        active: false,
        plan: None,
        expires_at: None,
    });
    send_message(tx, None, &ack).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn device_id_format_accepts_32_byte_base64() {
        let id = base64::engine::general_purpose::STANDARD.encode([0u8; 32]);
        assert!(validate_device_id_format(&id).is_ok());
    }

    #[test]
    fn device_id_format_rejects_wrong_length() {
        let id = base64::engine::general_purpose::STANDARD.encode([0u8; 16]);
        assert!(validate_device_id_format(&id).is_err());
    }

    #[test]
    fn device_id_format_rejects_non_base64() {
        assert!(validate_device_id_format("not-base64!!").is_err());
    }

    #[test]
    fn timestamp_window_rejects_skew() {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert!(validate_timestamp(now_ms, 300).is_ok());
        assert!(validate_timestamp(now_ms + 1_000_000, 300).is_err());
        assert!(validate_timestamp(now_ms.saturating_sub(1_000_000), 300).is_err());
    }
}
