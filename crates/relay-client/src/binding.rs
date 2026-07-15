//! Account binding + subscription surfacing.
//!
//! Free tier: scan-pair works with no login; pairing is per-session
//! (re-scan on relay restart). "Bind device" persists the pairing so it
//! survives restarts without re-scanning, and requires an account login
//! whose subscription is active (monthly). The relay enforces subscription
//! status and pushes `SubscriptionStatus` on bind/expiry/renewal; this
//! module owns the client-side state: persisting the binding, tracking
//! subscription state, and deciding whether a reconnect needs re-scan
//! (free) or can use stored credentials (bound).

use anyhow::{Context, Result};
use relay_protocol::SubscriptionStatus;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Persisted binding record. Stored locally when bind succeeds; loaded on
/// restart to reconnect without re-scanning. `auth_token` is the account
/// session token; `pairing_token` is the device pairing token from the
/// relay. Neither is the E2E session key (that is re-derived per pairing).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoundDevice {
    pub device_id: String,
    pub auth_token: String,
    pub pairing_token: String,
    pub peer_device_id: String,
}

impl BoundDevice {
    /// Persist the binding as JSON at `path`.
    pub fn persist(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create binding dir {:?}", parent))?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json).with_context(|| format!("write binding {:?}", path))?;
        Ok(())
    }

    /// Load a stored binding, or `Ok(None)` if none exists.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let json = std::fs::read_to_string(path).with_context(|| format!("read binding {:?}", path))?;
        let bound = serde_json::from_str(&json).context("parse binding")?;
        Ok(Some(bound))
    }

    /// Delete the binding (e.g. on explicit unbind or subscription expiry
    /// with no renewal).
    pub fn clear(path: &Path) -> Result<()> {
        if path.exists() {
            std::fs::remove_file(path).with_context(|| format!("remove binding {:?}", path))?;
        }
        Ok(())
    }
}

/// Client-side subscription state, updated by relay-pushed
/// `SubscriptionStatus` messages. Drives the UI and the reconnect
/// strategy (bound + active => reconnect with credentials; inactive or
/// free => require re-scan).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubscriptionState {
    pub active: bool,
    pub plan: Option<String>,
    pub expires_at: Option<u64>,
}

impl SubscriptionState {
    pub fn from_status(status: &SubscriptionStatus) -> Self {
        Self {
            active: status.active,
            plan: status.plan.clone(),
            expires_at: status.expires_at,
        }
    }

    /// Whether reconnect can use stored credentials (bound + active sub).
    /// Free tier (no binding) or expired subscription requires re-scan.
    pub fn can_reconnect_without_rescan(&self, bound: Option<&BoundDevice>) -> bool {
        self.active && bound.is_some()
    }
}

/// Outcome of a bind attempt. The relay rejects binds without an active
/// subscription; the client surfaces this so the UI can prompt to subscribe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindOutcome {
    Bound(BoundDevice),
    SubscriptionRequired,
    Failed(String),
}

impl BindOutcome {
    pub fn from_response(
        response: &relay_protocol::BindDeviceResponse,
        auth_token: &str,
        pairing_token: &str,
        peer_device_id: &str,
    ) -> Self {
        if response.ok {
            BindOutcome::Bound(BoundDevice {
                device_id: response.bound_device_id.clone(),
                auth_token: auth_token.to_string(),
                pairing_token: pairing_token.to_string(),
                peer_device_id: peer_device_id.to_string(),
            })
        } else {
            let msg = response.message.as_deref().unwrap_or("bind rejected");
            if msg.to_ascii_lowercase().contains("subscription") {
                BindOutcome::SubscriptionRequired
            } else {
                BindOutcome::Failed(msg.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_protocol::BindDeviceResponse;

    #[test]
    fn bound_device_persists_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binding.json");
        let bound = BoundDevice {
            device_id: "dev-1".to_string(),
            auth_token: "tok".to_string(),
            pairing_token: "pair".to_string(),
            peer_device_id: "dev-phone".to_string(),
        };
        bound.persist(&path).unwrap();
        let loaded = BoundDevice::load(&path).unwrap().unwrap();
        assert_eq!(loaded, bound);
    }

    #[test]
    fn load_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.json");
        assert!(BoundDevice::load(&path).unwrap().is_none());
    }

    #[test]
    fn clear_removes_binding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binding.json");
        let bound = BoundDevice {
            device_id: "d".to_string(),
            auth_token: "t".to_string(),
            pairing_token: "p".to_string(),
            peer_device_id: "ph".to_string(),
        };
        bound.persist(&path).unwrap();
        BoundDevice::clear(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn bind_outcome_bound_when_ok() {
        let resp = BindDeviceResponse {
            ok: true,
            bound_device_id: "dev-1".to_string(),
            message: None,
        };
        let outcome = BindOutcome::from_response(&resp, "tok", "pair", "dev-phone");
        assert!(matches!(outcome, BindOutcome::Bound(_)));
    }

    #[test]
    fn bind_outcome_subscription_required_when_rejected_for_sub() {
        let resp = BindDeviceResponse {
            ok: false,
            bound_device_id: String::new(),
            message: Some("active subscription required".to_string()),
        };
        let outcome = BindOutcome::from_response(&resp, "tok", "pair", "dev-phone");
        assert_eq!(outcome, BindOutcome::SubscriptionRequired);
    }

    #[test]
    fn subscription_state_can_reconnect_only_when_bound_and_active() {
        let bound = BoundDevice {
            device_id: "d".to_string(),
            auth_token: "t".to_string(),
            pairing_token: "p".to_string(),
            peer_device_id: "ph".to_string(),
        };
        let active = SubscriptionState {
            active: true,
            plan: Some("monthly".to_string()),
            expires_at: None,
        };
        assert!(active.can_reconnect_without_rescan(Some(&bound)));

        let inactive = SubscriptionState {
            active: false,
            ..Default::default()
        };
        assert!(!inactive.can_reconnect_without_rescan(Some(&bound)));
        assert!(!active.can_reconnect_without_rescan(None));
    }

    #[test]
    fn subscription_state_from_status_maps_fields() {
        let status = SubscriptionStatus {
            active: true,
            plan: Some("monthly".to_string()),
            expires_at: Some(1_700_000_000),
        };
        let state = SubscriptionState::from_status(&status);
        assert!(state.active);
        assert_eq!(state.plan.as_deref(), Some("monthly"));
        assert_eq!(state.expires_at, Some(1_700_000_000));
    }
}
