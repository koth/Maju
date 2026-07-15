//! Owner of the mobile remote-control plane's shell-side state: device
//! identity, the current pairing code/QR, connection + subscription status,
//! and the kill switch. The long-lived relay-client driver task (dial,
//! auth, route, reconnect) is started separately; this manager is the
//! shared state it and the Tauri commands both touch.
//!
//! Fail-open: when disabled or disconnected, local sessions are entirely
//! unaffected — this manager never blocks the local command bridge.

use crate::commands::remote_control::RemoteControlStatus;
use relay_client::{
    DeviceIdentity, PairingCode, DEFAULT_PAIRING_TTL, build_qr_payload,
};
use std::sync::Mutex;

pub struct RemoteControlManager {
    inner: Mutex<Inner>,
    app_paths: app_core::AppPaths,
}

struct Inner {
    enabled: bool,
    connected: bool,
    device_id: Option<String>,
    pairing_code: Option<PairingCode>,
    pairing_qr: Option<String>,
    subscription_active: Option<bool>,
    bound: bool,
    relay_endpoint: String,
}

impl RemoteControlManager {
    pub fn new(app_paths: app_core::AppPaths) -> Self {
        let enabled = std::env::var("KODEX_REMOTE_CONTROL")
            .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off"))
            .unwrap_or(true);
        let relay_endpoint = std::env::var("KODEX_RELAY_ENDPOINT")
            .unwrap_or_else(|_| "wss://relay.kodex.app".to_string());
        Self {
            inner: Mutex::new(Inner {
                enabled,
                connected: false,
                device_id: None,
                pairing_code: None,
                pairing_qr: None,
                subscription_active: None,
                bound: false,
                relay_endpoint,
            }),
            app_paths,
        }
    }

    /// Load (or create) the device identity and record its id. Called once
    /// at app setup so `status()` can show the device id without touching
    /// the filesystem on every call.
    pub fn ensure_device_identity(&self) -> anyhow::Result<()> {
        let key_path = self.app_paths.root().join("remote-control-device.key");
        let identity = DeviceIdentity::load_or_create(&key_path)?;
        let device_id = identity.device_id();
        let mut inner = self.inner.lock().expect("rc manager mutex poisoned");
        inner.device_id = Some(device_id);
        Ok(())
    }

    pub fn set_enabled(&self, enabled: bool) {
        let mut inner = self.inner.lock().expect("rc manager mutex poisoned");
        inner.enabled = enabled;
        if !enabled {
            inner.connected = false;
        }
    }

    /// Mint a fresh one-time pairing code + QR payload. Invalidates any
    /// previous code. Returns the QR JSON for the frontend, or None when
    /// the plane is disabled.
    pub fn mint_pairing_qr(&self) -> Result<Option<String>, String> {
        let mut inner = self.inner.lock().expect("rc manager mutex poisoned");
        if !inner.enabled {
            return Ok(None);
        }
        let key_path = self.app_paths.root().join("remote-control-device.key");
        let identity = DeviceIdentity::load_or_create(&key_path)
            .map_err(|e| format!("load device identity: {e}"))?;
        let code = PairingCode::mint(DEFAULT_PAIRING_TTL);
        let payload = build_qr_payload(&inner.relay_endpoint, &code, &identity.public_b64());
        let json = payload
            .to_json()
            .map_err(|e| format!("encode qr payload: {e}"))?;
        inner.pairing_code = Some(code);
        inner.pairing_qr = Some(json.clone());
        Ok(Some(json))
    }

    pub fn set_connected(&self, connected: bool) {
        let mut inner = self.inner.lock().expect("rc manager mutex poisoned");
        inner.connected = inner.enabled && connected;
    }

    pub fn set_subscription_active(&self, active: bool) {
        let mut inner = self.inner.lock().expect("rc manager mutex poisoned");
        inner.subscription_active = Some(active);
    }

    pub fn set_bound(&self, bound: bool) {
        let mut inner = self.inner.lock().expect("rc manager mutex poisoned");
        inner.bound = bound;
    }

    pub fn status(&self) -> RemoteControlStatus {
        let inner = self.inner.lock().expect("rc manager mutex poisoned");
        RemoteControlStatus {
            enabled: inner.enabled,
            connected: inner.connected,
            device_id: inner.device_id.clone(),
            pairing_qr: inner.pairing_qr.clone(),
            subscription_active: inner.subscription_active,
            bound: inner.bound,
        }
    }
}
