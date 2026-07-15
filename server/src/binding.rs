use relay_protocol::{BindDeviceRequest, BindDeviceResponse, Message};
use tokio::sync::mpsc;

use crate::errors::Result;
use crate::state::AppState;
use crate::subscription::subscription_status_for;
use crate::wire::{push_subscription_status, send_message};

/// Handle `BindDeviceRequest`: verify the auth token and an active
/// subscription, bind the caller's pairing to the account, reply with
/// `BindDeviceResponse`, and push `SubscriptionStatus` to both peers.
pub async fn handle_bind_device(
    state: &AppState,
    req: BindDeviceRequest,
    from_device_id: &str,
    tx: &mpsc::Sender<String>,
) -> Result<()> {
    let account_id = match state.db.account_by_token(req.auth_token.clone()).await? {
        Some(id) => id,
        None => {
            send_message(tx, None, &deny(from_device_id, "active subscription required"))
                .await?;
            return Ok(());
        }
    };

    let sub = state.db.subscription_status(account_id.clone()).await?;
    if !sub.map(|(active, _, _)| active).unwrap_or(false) {
        send_message(tx, None, &deny(from_device_id, "active subscription required"))
            .await?;
        return Ok(());
    }

    match state.db.pairing_id_for(from_device_id.to_string()).await? {
        Some((pairing_id, pc, phone)) => {
            state.db.bind_pairing(pairing_id, account_id.clone()).await?;
            send_message(
                tx,
                None,
                &Message::BindDeviceResponse(BindDeviceResponse {
                    ok: true,
                    bound_device_id: from_device_id.to_string(),
                    message: None,
                }),
            )
            .await?;
            let status = subscription_status_for(&state.db, &account_id).await?;
            let _ = push_subscription_status(&state.connections, &pc, &phone, &status).await;
        }
        None => {
            send_message(tx, None, &deny(from_device_id, "no pairing to bind")).await?;
        }
    }
    Ok(())
}

fn deny(device_id: &str, message: &str) -> Message {
    Message::BindDeviceResponse(BindDeviceResponse {
        ok: false,
        bound_device_id: device_id.to_string(),
        message: Some(message.to_string()),
    })
}
