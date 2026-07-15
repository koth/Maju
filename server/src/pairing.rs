use relay_protocol::{
    Message, PairingConfirm, PairingInitiate, PairingRegister, SubscriptionStatus,
};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::errors::{RelayError, Result};
use crate::state::AppState;
use crate::wire::send_message;

/// PC -> relay: register a one-time pairing code bound to the sender's
/// connection so a scanning phone's `PairingInitiate` can be routed here.
/// Acknowledged with a `SubscriptionStatus` ack (the doc's relay->device
/// success-ack shape).
pub async fn handle_pairing_register(
    state: &AppState,
    req: PairingRegister,
    pc_device_id: &str,
    tx: &mpsc::Sender<String>,
) -> Result<()> {
    // §7: rate-limit pairing-code generation per PC device_id to deter
    // brute-force / flooding. Reuses the failure limiter as a request counter.
    if !state.rate_limiter.allowed(pc_device_id) {
        return Err(RelayError::Other("pairing code rate limited".into()));
    }
    state.rate_limiter.record_failure(pc_device_id);
    state
        .db
        .register_pairing_code(
            req.pairing_code.clone(),
            pc_device_id.to_string(),
            state.config.pairing_code_ttl_secs,
        )
        .await?;
    tracing::info!(
        pairing_code = %req.pairing_code,
        pc_device_id = %pc_device_id,
        "pairing code registered"
    );
    let ack = Message::SubscriptionStatus(SubscriptionStatus {
        active: false,
        plan: None,
        expires_at: None,
    });
    send_message(tx, None, &ack).await?;
    Ok(())
}

/// Phone -> relay: validate the pairing code, bind pc<->phone, mark the code
/// used, and send `PairingConfirm` to both peers. The relay forwards the
/// phone's ephemeral public key to the PC (via `session_key_material`) and
/// the PC's static public key to the phone; it never derives the E2E session
/// key.
pub async fn handle_pairing_initiate(
    state: &AppState,
    pi: PairingInitiate,
    phone_device_id: &str,
    tx: &mpsc::Sender<String>,
) -> Result<()> {
    let pc_device_id = state
        .db
        .take_pairing_code(pi.pairing_code.clone())
        .await?
        .ok_or(RelayError::InvalidPairingCode)?;
    let pairing_token = Uuid::new_v4().to_string();
    state
        .db
        .create_pairing(
            pairing_token.clone(),
            pc_device_id.clone(),
            phone_device_id.to_string(),
        )
        .await?;
    state
        .db
        .mark_pairing_code_used(pi.pairing_code.clone())
        .await?;

    let phone_ephemeral = pi.phone_ephemeral_pubkey.clone().unwrap_or_default();
    let phone_confirm = Message::PairingConfirm(PairingConfirm {
        pairing_token: pairing_token.clone(),
        session_key_material: pi.pc_device_pubkey.clone(),
        pc_device_id: pc_device_id.clone(),
        phone_device_id: phone_device_id.to_string(),
    });
    let pc_confirm = Message::PairingConfirm(PairingConfirm {
        pairing_token: pairing_token.clone(),
        session_key_material: phone_ephemeral,
        pc_device_id: pc_device_id.clone(),
        phone_device_id: phone_device_id.to_string(),
    });

    send_message(tx, None, &phone_confirm).await?;
    if let Some(pc_tx) = state.connections.get(&pc_device_id) {
        send_message(&pc_tx, None, &pc_confirm).await?;
    } else {
        tracing::warn!(
            pc_device_id = %pc_device_id,
            "PC offline during pairing confirm; phone confirmed only (PC will not receive E2E material)"
        );
    }
    Ok(())
}
