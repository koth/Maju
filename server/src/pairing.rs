use relay_protocol::{
    Message, PairingConfirm, PairingInitiate, PairingRegister, PairingResume, SubscriptionStatus,
};
use tokio::sync::mpsc;
use uuid::Uuid;
use std::time::Duration;

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
    // PC registers the pairing code after minting the QR, but it must first
    // reconnect + authenticate to the relay, which can take seconds. The
    // phone can scan and reach the relay long before `PairingRegister` is
    // written. Wait generously for that race to settle instead of failing
    // on the first lookup — a pairing code costs nothing to retry.
    let mut pc_device_id = None;
    for _ in 0..50 {
        pc_device_id = state
            .db
            .take_pairing_code(pi.pairing_code.clone())
            .await?;
        if pc_device_id.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let Some(pc_device_id) = pc_device_id else {
        // Reply explicitly so the phone surfaces "invalid/expired code" and
        // falls back to re-scanning instead of hanging on connecting.
        send_message(tx, None, &pairing_error("invalid or expired pairing code")).await?;
        return Err(RelayError::InvalidPairingCode);
    };

    // The PC must be ONLINE for a pairing to be usable: the relay forwards the
    // phone's ephemeral key to the PC, and without it the PC can never derive
    // the E2E session key. Previously the relay confirmed to the phone anyway,
    // so the phone reported "paired" against a PC that would never answer a
    // single request — the "scanned the other PC's QR and nothing ever
    // happened" failure. Refuse instead, and leave the one-time code unused so
    // the same QR still works once the PC reconnects to the relay.
    let Some(pc_tx) = state.connections.get(&pc_device_id) else {
        tracing::warn!(
            pc_device_id = %pc_device_id,
            "pairing initiate: PC offline; refusing and leaving the code usable"
        );
        send_pairing_error(
            tx,
            "PC is not connected to the relay; check the PC shows 已连接 relay, then scan again",
        )
        .await;
        return Err(RelayError::Other("PC offline during pairing initiate".into()));
    };

    let pairing_token = Uuid::new_v4().to_string();
    let phone_ephemeral = pi.phone_ephemeral_pubkey.clone().unwrap_or_default();
    let pc_confirm = Message::PairingConfirm(PairingConfirm {
        error: None,
        pairing_token: pairing_token.clone(),
        session_key_material: phone_ephemeral,
        pc_device_id: pc_device_id.clone(),
        phone_device_id: phone_device_id.to_string(),
        // Forward the phone's wire capabilities so the PC can pick its
        // outbound ciphertext encoding (e.g. compact ciphertext_b64).
        capabilities: pi.capabilities.clone(),
    });

    // Send PC's confirm FIRST so it installs the session key before the phone
    // (which starts sending encrypted control requests immediately after
    // receiving its confirm). Reversing this order causes a race: the phone's
    // first encrypted frame can arrive at the PC before the PC has installed
    // the key, crashing the driver.
    if send_message(&pc_tx, None, &pc_confirm).await.is_err() {
        // Half-dead connection entry: evict it so the PC's reconnect
        // re-registers cleanly, and keep the code usable for a retry.
        state.connections.remove(&pc_device_id);
        tracing::warn!(
            pc_device_id = %pc_device_id,
            "pairing initiate: PC send failed; refusing and leaving the code usable"
        );
        send_pairing_error(
            tx,
            "PC is not connected to the relay; check the PC shows 已连接 relay, then scan again",
        )
        .await;
        return Err(RelayError::Other("PC unreachable during pairing initiate".into()));
    }

    // The PC owns the key material now; persist the binding and consume the
    // one-time code.
    state
        .db
        .create_pairing(
            pairing_token.clone(),
            pc_device_id.clone(),
            phone_device_id.to_string(),
            pi.pc_device_pubkey.clone(),
        )
        .await?;
    // Identity rotation: the phone's secure store may lose the identity key
    // (app reinstall, Keystore reset), producing a fresh device id on every
    // cold start. The phone proves control of the PC static key by scanning
    // a fresh QR, so rebind all of this PC's pairings to the new phone id —
    // persisted pairing tokens from previous identities stay resumable.
    if let Err(e) = state
        .db
        .rebind_pc_pairings_to_phone(pc_device_id.clone(), phone_device_id.to_string())
        .await
    {
        tracing::warn!(error = %e, "failed to rebind prior pairings to new phone identity");
    }
    state
        .db
        .mark_pairing_code_used(pi.pairing_code.clone())
        .await?;

    let phone_confirm = Message::PairingConfirm(PairingConfirm {
        error: None,
        pairing_token,
        session_key_material: pi.pc_device_pubkey.clone(),
        pc_device_id,
        phone_device_id: phone_device_id.to_string(),
        capabilities: pi.capabilities,
    });
    send_message(tx, None, &phone_confirm).await?;
    Ok(())
}

/// Build a relay->device `PairingConfirm` carrying a failure reason (the
/// success fields are placeholders; receivers key off `error`).
fn pairing_error(reason: &str) -> Message {
    Message::PairingConfirm(PairingConfirm {
        error: Some(reason.to_string()),
        pairing_token: String::new(),
        session_key_material: String::new(),
        pc_device_id: String::new(),
        phone_device_id: String::new(),
        capabilities: Vec::new(),
    })
}

/// Best-effort: tell the phone a pairing/resume failed so it can surface the
/// reason and fall back to re-scanning instead of hanging on connecting.
async fn send_pairing_error(tx: &mpsc::Sender<String>, reason: &str) {
    let _ = send_message(tx, None, &pairing_error(reason)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::connections::Connections;
    use crate::db::Db;
    use crate::ratelimit::RateLimiter;
    use crate::state::AppState;
    use relay_protocol::Envelope;

    fn app_state() -> AppState {
        AppState {
            config: Config::default(),
            db: Db::open_in_memory().unwrap(),
            connections: Connections::new(),
            rate_limiter: RateLimiter::new(10, 300),
        }
    }

    async fn seed_bound_pairing(state: &AppState, pc: &str, phone: &str, token: &str) {
        state.db.register_device(pc.to_string(), "pc-ed25519".into()).await.unwrap();
        state.db.register_device(phone.to_string(), "ph-ed25519".into()).await.unwrap();
        let token = token.to_string();
        state.db
            .create_pairing(token.to_string(), pc.to_string(), phone.to_string(), "pc-x25519".into())
            .await
            .unwrap();
        state.db.blocking(move |c| {
            c.execute(
                "INSERT INTO accounts (account_id, credentials, auth_token, email) \
                 VALUES ('acct', '{}', 'tok', 'a@example.com')",
                [],
            )?;
            c.execute(
                "UPDATE pairings SET bound = 1, account_id = 'acct' WHERE pairing_id = ?1",
                rusqlite::params![token.clone()],
            )?;
            c.execute(
                "INSERT INTO subscriptions (account_id, plan, active, expires_at) \
                 VALUES ('acct', 'monthly', 1, 9999999999999)",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn resume_forwards_fresh_phone_ephemeral_to_online_pc() {
        let state = app_state();
        seed_bound_pairing(&state, "pc", "phone", "token").await;

        let (pc_tx, mut pc_rx) = mpsc::channel::<String>(8);
        state.connections.insert("pc", 1, pc_tx.clone());

        let (phone_tx, mut phone_rx) = mpsc::channel::<String>(8);
        handle_pairing_resume(
            &state,
            PairingResume {
                pairing_token: "token".into(),
                phone_ephemeral_pubkey: "eph".into(),
                capabilities: Vec::new(),
            },
            "phone",
            &phone_tx,
        )
        .await
        .unwrap();

        let pc_text = pc_rx.recv().await.unwrap();
        let pc_env: Envelope = serde_json::from_str(&pc_text).unwrap();
        match pc_env.into_message().unwrap() {
            Message::PairingConfirm(confirm) => {
                assert_eq!(confirm.pairing_token, "token");
                assert_eq!(confirm.session_key_material, "eph");
            }
            other => panic!("expected PairingConfirm to PC, got {other:?}"),
        }

        let phone_text = phone_rx.recv().await.unwrap();
        let phone_env: Envelope = serde_json::from_str(&phone_text).unwrap();
        match phone_env.into_message().unwrap() {
            Message::PairingConfirm(confirm) => {
                assert_eq!(confirm.session_key_material, "pc-x25519");
            }
            other => panic!("expected PairingConfirm to phone, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_forwards_phone_capabilities_to_pc_confirm() {
        // The PC picks its outbound ciphertext encoding from the phone's
        // advertised capabilities, which ride PairingResume -> PairingConfirm.
        let state = app_state();
        seed_bound_pairing(&state, "pc", "phone", "token").await;

        let (pc_tx, mut pc_rx) = mpsc::channel::<String>(8);
        state.connections.insert("pc", 1, pc_tx.clone());
        let (phone_tx, mut phone_rx) = mpsc::channel::<String>(8);
        handle_pairing_resume(
            &state,
            PairingResume {
                pairing_token: "token".into(),
                phone_ephemeral_pubkey: "eph".into(),
                capabilities: vec![relay_protocol::CAPABILITY_CIPHERTEXT_B64.into()],
            },
            "phone",
            &phone_tx,
        )
        .await
        .unwrap();

        let pc_text = pc_rx.recv().await.unwrap();
        let pc_env: Envelope = serde_json::from_str(&pc_text).unwrap();
        match pc_env.into_message().unwrap() {
            Message::PairingConfirm(confirm) => {
                assert_eq!(
                    confirm.capabilities,
                    vec![relay_protocol::CAPABILITY_CIPHERTEXT_B64.to_string()],
                    "PC confirm carries the phone's wire capabilities"
                );
            }
            other => panic!("expected PairingConfirm to PC, got {other:?}"),
        }
        let _ = phone_rx.recv().await.unwrap();
    }

    #[tokio::test]
    async fn resume_works_without_active_subscription() {
        let state = app_state();
        seed_bound_pairing(&state, "pc", "phone", "token").await;
        state.db.deactivate_subscription("acct".into()).await.unwrap();

        let (pc_tx, _pc_rx) = mpsc::channel::<String>(8);
        state.connections.insert("pc", 1, pc_tx);
        let (phone_tx, mut phone_rx) = mpsc::channel::<String>(8);
        handle_pairing_resume(
            &state,
            PairingResume {
                pairing_token: "token".into(),
                phone_ephemeral_pubkey: "eph".into(),
                capabilities: Vec::new(),
            },
            "phone",
            &phone_tx,
        )
        .await
        .unwrap();

        let text = phone_rx.recv().await.unwrap();
        let env: Envelope = serde_json::from_str(&text).unwrap();
        assert!(matches!(
            env.into_message().unwrap(),
            Message::PairingConfirm(_)
        ));
    }

    #[tokio::test]
    async fn reregister_within_old_code_ttl_replaces_stale_code() {
        let state = app_state();
        state.db.register_device("pc".into(), "pc-ed25519".into()).await.unwrap();
        state
            .db
            .register_pairing_code("OLDCODE1".into(), "pc".into(), 120)
            .await
            .unwrap();
        // Same PC re-registers a fresh code while the old one is still in
        // its TTL (used or not). This must not fail on the PRIMARY KEY.
        state.db.mark_pairing_code_used("OLDCODE1".into()).await.unwrap();
        state
            .db
            .register_pairing_code("NEWCODE2".into(), "pc".into(), 120)
            .await
            .unwrap();
        assert_eq!(
            state.db.take_pairing_code("NEWCODE2".into()).await.unwrap().as_deref(),
            Some("pc")
        );
        assert!(
            state.db.take_pairing_code("OLDCODE1".into()).await.unwrap().is_none(),
            "stale code replaced"
        );
    }

    #[tokio::test]
    async fn initiate_with_invalid_code_replies_with_error() {
        let state = app_state();
        let (tx, mut rx) = mpsc::channel::<String>(8);
        let result = handle_pairing_initiate(
            &state,
            PairingInitiate {
                pairing_code: "NOPE1234".into(),
                pc_device_pubkey: "pc-x25519".into(),
                relay_endpoint: "ws://relay".into(),
                phone_ephemeral_pubkey: None,
                capabilities: Vec::new(),
            },
            "phone",
            &tx,
        )
        .await;
        assert!(result.is_err());
        let text = rx.recv().await.expect("phone receives an explicit error");
        let env: Envelope = serde_json::from_str(&text).unwrap();
        match env.into_message().unwrap() {
            Message::PairingConfirm(confirm) => {
                assert!(confirm.error.is_some(), "error reason present");
            }
            other => panic!("expected error PairingConfirm, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_with_offline_pc_replies_with_error() {
        let state = app_state();
        seed_bound_pairing(&state, "pc", "phone", "token").await;
        // PC never connects.
        let (tx, mut rx) = mpsc::channel::<String>(8);
        let result = handle_pairing_resume(
            &state,
            PairingResume {
                pairing_token: "token".into(),
                phone_ephemeral_pubkey: "eph".into(),
                capabilities: Vec::new(),
            },
            "phone",
            &tx,
        )
        .await;
        assert!(result.is_err());
        let text = rx.recv().await.expect("phone receives an explicit error");
        let env: Envelope = serde_json::from_str(&text).unwrap();
        match env.into_message().unwrap() {
            Message::PairingConfirm(confirm) => {
                assert!(confirm.error.is_some(), "error reason present");
            }
            other => panic!("expected error PairingConfirm, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_with_stale_pc_entry_evicts_and_replies_error() {
        let state = app_state();
        seed_bound_pairing(&state, "pc", "phone", "token").await;
        // Stale entry: registered but its receiver is dropped (dead WS).
        let (pc_tx, pc_rx) = mpsc::channel::<String>(8);
        drop(pc_rx);
        state.connections.insert("pc", 1, pc_tx);
        let (tx, mut rx) = mpsc::channel::<String>(8);
        let result = handle_pairing_resume(
            &state,
            PairingResume {
                pairing_token: "token".into(),
                phone_ephemeral_pubkey: "eph".into(),
                capabilities: Vec::new(),
            },
            "phone",
            &tx,
        )
        .await;
        assert!(result.is_err());
        assert!(
            state.connections.get("pc").is_none(),
            "stale PC connection evicted"
        );
        let text = rx.recv().await.expect("phone receives an explicit error");
        let env: Envelope = serde_json::from_str(&text).unwrap();
        match env.into_message().unwrap() {
            Message::PairingConfirm(confirm) => assert!(confirm.error.is_some()),
            other => panic!("expected error PairingConfirm, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn initiate_rebinds_prior_pairings_to_rotated_phone_identity() {
        let state = app_state();
        state.db.register_device("pc".into(), "pc-ed".into()).await.unwrap();
        state
            .db
            .register_device("phone-old".into(), "ph-ed".into())
            .await
            .unwrap();
        state
            .db
            .create_pairing("old-token".into(), "pc".into(), "phone-old".into(), "pc-x".into())
            .await
            .unwrap();
        state
            .db
            .register_pairing_code("FRESHCODE".into(), "pc".into(), 120)
            .await
            .unwrap();
        state
            .db
            .register_device("phone-new".into(), "ph-ed2".into())
            .await
            .unwrap();

        let (pc_tx, mut pc_rx) = mpsc::channel::<String>(8);
        state.connections.insert("pc", 1, pc_tx);
        let (phone_tx, mut phone_rx) = mpsc::channel::<String>(8);
        handle_pairing_initiate(
            &state,
            PairingInitiate {
                pairing_code: "FRESHCODE".into(),
                pc_device_pubkey: "pc-x".into(),
                relay_endpoint: "ws://relay".into(),
                phone_ephemeral_pubkey: Some("eph".into()),
                capabilities: Vec::new(),
            },
            "phone-new",
            &phone_tx,
        )
        .await
        .unwrap();

        // The old pairing token now resolves to the rotated phone identity,
        // so a resume with the persisted old BoundDevice still matches.
        let (_, paired_phone, _, _) = state
            .db
            .pairing_by_token("old-token".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(paired_phone, "phone-new");

        // Both peers got their confirms.
        let _ = pc_rx.recv().await.unwrap();
        let _ = phone_rx.recv().await.unwrap();
    }

    #[tokio::test]
    async fn initiate_with_offline_pc_replies_error_and_keeps_code_usable() {
        let state = app_state();
        state
            .db
            .register_device("pc".into(), "pc-ed".into())
            .await
            .unwrap();
        state
            .db
            .register_pairing_code("LIVECODE".into(), "pc".into(), 120)
            .await
            .unwrap();
        // The PC registered the code but is NOT currently connected to the
        // relay (its link dropped after registering). A regenerate-on-reconnect
        // desktop can be in exactly this state while still showing a QR.
        let (phone_tx, mut phone_rx) = mpsc::channel::<String>(8);
        let result = handle_pairing_initiate(
            &state,
            PairingInitiate {
                pairing_code: "LIVECODE".into(),
                pc_device_pubkey: "pc-x25519".into(),
                relay_endpoint: "ws://relay".into(),
                phone_ephemeral_pubkey: Some("eph".into()),
                capabilities: Vec::new(),
            },
            "phone",
            &phone_tx,
        )
        .await;
        assert!(result.is_err(), "offline PC must not produce a success");

        let text = phone_rx.recv().await.expect("phone receives an explicit error");
        let env: Envelope = serde_json::from_str(&text).unwrap();
        match env.into_message().unwrap() {
            Message::PairingConfirm(confirm) => {
                let reason = confirm.error.expect("error reason present");
                assert!(
                    reason.contains("not connected to the relay"),
                    "reason names the real cause: {reason}"
                );
            }
            other => panic!("expected error PairingConfirm, got {other:?}"),
        }

        // The one-time code must survive the failed attempt so the SAME QR
        // works once the PC reconnects.
        assert_eq!(
            state
                .db
                .take_pairing_code("LIVECODE".into())
                .await
                .unwrap()
                .as_deref(),
            Some("pc"),
            "failed initiate leaves the code unused"
        );
    }

    #[tokio::test]
    async fn initiate_with_online_pc_forwards_to_pc_and_consumes_code() {
        let state = app_state();
        state
            .db
            .register_device("pc".into(), "pc-ed".into())
            .await
            .unwrap();
        state
            .db
            .register_device("phone".into(), "ph-ed".into())
            .await
            .unwrap();
        state
            .db
            .register_pairing_code("LIVECODE".into(), "pc".into(), 120)
            .await
            .unwrap();
        let (pc_tx, mut pc_rx) = mpsc::channel::<String>(8);
        state.connections.insert("pc", 1, pc_tx);
        let (phone_tx, mut phone_rx) = mpsc::channel::<String>(8);

        handle_pairing_initiate(
            &state,
            PairingInitiate {
                pairing_code: "LIVECODE".into(),
                pc_device_pubkey: "pc-x25519".into(),
                relay_endpoint: "ws://relay".into(),
                phone_ephemeral_pubkey: Some("eph".into()),
                capabilities: Vec::new(),
            },
            "phone",
            &phone_tx,
        )
        .await
        .unwrap();

        let pc_text = pc_rx.recv().await.expect("PC gets the phone ephemeral key");
        let pc_env: Envelope = serde_json::from_str(&pc_text).unwrap();
        match pc_env.into_message().unwrap() {
            Message::PairingConfirm(confirm) => {
                assert_eq!(confirm.session_key_material, "eph");
                assert!(confirm.error.is_none());
            }
            other => panic!("expected PairingConfirm to PC, got {other:?}"),
        }

        let phone_text = phone_rx.recv().await.expect("phone gets its confirm");
        let phone_env: Envelope = serde_json::from_str(&phone_text).unwrap();
        match phone_env.into_message().unwrap() {
            Message::PairingConfirm(confirm) => {
                assert!(confirm.error.is_none());
                assert_eq!(confirm.session_key_material, "pc-x25519");
            }
            other => panic!("expected PairingConfirm to phone, got {other:?}"),
        }

        assert!(
            state
                .db
                .take_pairing_code("LIVECODE".into())
                .await
                .unwrap()
                .is_none(),
            "successful initiate consumes the one-time code"
        );
    }
}

/// Phone/PC -> relay: resume an already-created pairing without re-scanning.
/// The phone mints a fresh ephemeral keypair and sends its public key; the
/// relay validates the persisted pairing token, then forwards the fresh
/// material to the paired PC as a `PairingConfirm`.
/// Both peers derive the same new E2E session key from their own secret.
pub async fn handle_pairing_resume(
    state: &AppState,
    req: PairingResume,
    phone_device_id: &str,
    tx: &mpsc::Sender<String>,
) -> Result<()> {
    tracing::info!(
        phone_device_id = %phone_device_id,
        pairing_token = %req.pairing_token,
        "pairing resume requested"
    );
    let Some((pc_device_id, paired_phone, _account_id, pc_x25519_pubkey)) = state
        .db
        .pairing_by_token(req.pairing_token.clone())
        .await?
    else {
        tracing::warn!(pairing_token = %req.pairing_token, "resume: pairing token unknown");
        send_pairing_error(tx, "pairing token unknown; scan a new code").await;
        return Err(RelayError::InvalidPairingCode);
    };
    if paired_phone != phone_device_id {
        tracing::warn!(
            phone_device_id = %phone_device_id,
            paired_phone = %paired_phone,
            "resume: phone identity mismatch"
        );
        send_pairing_error(tx, "pairing token does not belong to this device").await;
        return Err(RelayError::NotPaired);
    }

    let pairing_token = req.pairing_token;
    let pc_confirm = Message::PairingConfirm(PairingConfirm {
        error: None,
        pairing_token: pairing_token.clone(),
        session_key_material: req.phone_ephemeral_pubkey,
        pc_device_id: pc_device_id.clone(),
        phone_device_id: phone_device_id.to_string(),
        // Forward the phone's wire capabilities so the PC can pick its
        // outbound ciphertext encoding (e.g. compact ciphertext_b64).
        capabilities: req.capabilities,
    });

    let Some(pc_tx) = state.connections.get(&pc_device_id) else {
        tracing::warn!(pc_device_id = %pc_device_id, "resume: paired PC offline");
        send_pairing_error(tx, "paired PC is offline; scan a new code").await;
        return Err(RelayError::Other("paired PC is offline".into()));
    };
    if send_message(&pc_tx, None, &pc_confirm).await.is_err() {
        // The PC entry is a stale half-dead connection: evict it so the
        // phone's error reply reflects reality and the PC's reconnect
        // re-registers cleanly.
        state.connections.remove(&pc_device_id);
        send_pairing_error(tx, "paired PC is offline; scan a new code").await;
        return Err(RelayError::Other("paired PC is offline".into()));
    }

    // Acknowledge the phone with the same confirm so it can derive the
    // matching key; it already holds the PC static public key used for ECDH
    // from the bound-device record, so `session_key_material` is informational.
    let phone_confirm = Message::PairingConfirm(PairingConfirm {
        error: None,
        pairing_token,
        session_key_material: pc_x25519_pubkey,
        pc_device_id,
        phone_device_id: phone_device_id.to_string(),
        capabilities: Vec::new(),
    });
    send_message(tx, None, &phone_confirm).await?;
    Ok(())
}
