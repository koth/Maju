use relay_protocol::SubscriptionStatus;

use crate::db::Db;
use crate::errors::Result;
use crate::state::AppState;
use crate::wire::push_subscription_status;

/// Build the current `SubscriptionStatus` for an account (free/unbound when
/// no subscription row exists).
pub async fn subscription_status_for(db: &Db, account_id: &str) -> Result<SubscriptionStatus> {
    let sub = db.subscription_status(account_id.to_string()).await?;
    Ok(match sub {
        Some((active, plan, expires_at)) => SubscriptionStatus {
            active,
            plan,
            expires_at: Some(expires_at as u64),
        },
        None => SubscriptionStatus {
            active: false,
            plan: None,
            expires_at: None,
        },
    })
}

/// Scan for subscriptions whose `expires_at` has passed, deactivate them, and
/// push `SubscriptionStatus { active: false }` to each paired device. The
/// session itself is never force-disconnected on expiry (requirements doc
/// §5.3); peers learn the degraded state and must re-pair to rebind.
pub async fn sweep_expired_subscriptions(state: &AppState) -> Result<()> {
    let expired = state.db.expired_subscriptions().await?;
    for (account_id, plan, expires_at) in expired {
        state.db.deactivate_subscription(account_id.clone()).await?;
        let status = SubscriptionStatus {
            active: false,
            plan,
            expires_at: Some(expires_at as u64),
        };
        let pairings = state.db.pairings_for_account(account_id.clone()).await?;
        for (pc, phone) in pairings {
            let _ = push_subscription_status(&state.connections, &pc, &phone, &status).await;
        }
        tracing::info!(
            account_id = %account_id,
            "subscription expired; pushed active=false to paired devices (session not disconnected)"
        );
    }
    Ok(())
}

/// Background loop that periodically sweeps expired subscriptions.
pub async fn run_sweeper(state: AppState) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        if let Err(e) = sweep_expired_subscriptions(&state).await {
            tracing::warn!(error = %e, "subscription sweep error");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_protocol::{Envelope, Message};
    use tokio::sync::mpsc;

    fn app_state() -> AppState {
        AppState {
            config: crate::config::Config::default(),
            db: crate::db::Db::open_in_memory().unwrap(),
            connections: crate::connections::Connections::new(),
            rate_limiter: crate::ratelimit::RateLimiter::new(10, 300),
        }
    }

    #[tokio::test]
    async fn sweep_deactivates_and_pushes_inactive_status() {
        let state = app_state();
        state
            .db
            .blocking(|c| {
                c.execute_batch(
                    "INSERT INTO accounts (account_id, credentials, auth_token) \
                     VALUES ('acct','{}','tok'); \
                     INSERT INTO subscriptions (account_id, plan, active, expires_at) \
                     VALUES ('acct','monthly',1, 1); \
                     INSERT INTO devices (device_id, public_key, registered_at) \
                     VALUES ('pc','pk',0), ('ph','pk',0); \
                     INSERT INTO pairings (pairing_id, pc_device_id, phone_device_id, \
                     created_at, bound, account_id) VALUES ('p1','pc','ph',0,1,'acct');",
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let (tx, mut rx) = mpsc::channel::<String>(8);
        state.connections.insert("pc", tx.clone());
        state.connections.insert("ph", tx);

        sweep_expired_subscriptions(&state).await.unwrap();

        let mut saw_inactive = false;
        while let Ok(Some(text)) =
            tokio::time::timeout(std::time::Duration::from_millis(300), rx.recv()).await
        {
            if let Ok(env) = serde_json::from_str::<Envelope>(&text) {
                if let Ok(Message::SubscriptionStatus(s)) = env.into_message() {
                    assert!(!s.active);
                    saw_inactive = true;
                }
            }
        }
        assert!(saw_inactive, "expected an inactive SubscriptionStatus push");
    }
}
