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
    AccountSession, DeviceIdentity, LoginClient, PairingCode, DEFAULT_PAIRING_TTL,
    auth_base_url_from_ws_endpoint, build_qr_payload,
};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::Notify;

pub struct RemoteControlManager {
    inner: Mutex<Inner>,
    app_paths: app_core::AppPaths,
    login: LoginClient,
    /// Notified when a fresh pairing code is minted so the driver loop can
    /// re-register it with the relay on the current connection (or reconnect
    /// to do so). Shared with the driver task.
    pairing_notify: Arc<Notify>,
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
    account_session: Option<AccountSession>,
    insecure_tls: bool,
    /// Wall-clock deadline of the minted pairing code. Tracked next to the
    /// code so the UI can show a countdown and re-mint before it expires —
    /// an expired QR stays scannable-looking but can never pair.
    pairing_expires_at: Option<Instant>,
    /// Whether the CURRENT minted code has been acked by the relay. Rendered
    /// QR is not the same as a registered code: showing one before the relay
    /// has it makes every scan fail with "invalid or expired pairing code".
    pairing_is_registered: bool,
    /// Set when a freshly minted pairing code has been registered with the
    /// relay; the QR UI waits for this before showing the code as scannable.
    pairing_registered: Option<Arc<Notify>>,
}

impl RemoteControlManager {
    pub fn new(app_paths: app_core::AppPaths) -> Self {
        let enabled = std::env::var("KODEX_REMOTE_CONTROL")
            .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off"))
            .unwrap_or(true);
        // Plain ws:// during the no-domain dev window: the mobile companion's
        // React Native WebSocket can't skip self-signed TLS verification, so
        // the phone dials plain ws:// through Nginx :80. The PC driver follows
        // the same endpoint so the pairing QR it mints is phone-compatible.
        // Flip to wss://120.48.49.190 + insecure_tls=true once the phone can
        // trust a cert, and to wss://relay.kodex.app once a real cert exists.
        let relay_endpoint = std::env::var("KODEX_RELAY_ENDPOINT")
            .unwrap_or_else(|_| "ws://120.48.49.190".to_string());
        let insecure_tls = std::env::var("KODEX_RELAY_INSECURE_TLS")
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on"))
            .unwrap_or(false);
        // Auth HTTP origin for the passwordless login endpoints. Prefer an
        // explicit override (dev relay on a separate port); otherwise derive
        // it from the WebSocket endpoint (prod reverse-proxies `/auth/*` on
        // the same origin). Empty when neither is configured — login
        // commands then fail fast with a clear message instead of dialing
        // nowhere.
        let auth_base = std::env::var("KODEX_RELAY_AUTH_ENDPOINT")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| auth_base_url_from_ws_endpoint(&relay_endpoint))
            .unwrap_or_default();
        let login = LoginClient::new(auth_base, insecure_tls);
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
                insecure_tls,
                account_session: None,
                pairing_expires_at: None,
                pairing_is_registered: false,
                pairing_registered: None,
            }),
            app_paths,
            login,
            pairing_notify: Arc::new(Notify::new()),
        }
    }

    /// A `Notify` handle the driver loop selects on so a freshly minted
    /// pairing code can be re-registered on the current relay connection.
    pub fn pairing_notify(&self) -> Arc<Notify> {
        self.pairing_notify.clone()
    }

    /// Load (or create) the device identity and record its id. Called once
    /// at app setup so `status()` can show the device id without touching
    /// the filesystem on every call.
    pub fn ensure_device_identity(&self) -> anyhow::Result<()> {
        let key_path = self.app_paths.root().join("remote-control-device.key");
        let identity = DeviceIdentity::load_or_create(&key_path)?;
        let device_id = identity.device_id();
        // Best-effort: load a previously stored account session so the UI
        // can surface logged-in state (and a later bind can reuse the
        // auth_token) without re-prompting for a login code.
        let account = AccountSession::load(&self.account_session_path()).unwrap_or(None);
        let mut inner = self.inner.lock().expect("rc manager mutex poisoned");
        inner.device_id = Some(device_id);
        inner.account_session = account;
        Ok(())
    }

    /// Path to the persisted account session JSON
    /// (`~/.kodex/remote-control-account.json`), next to the device key.
    /// Holds the email-OTP-acquired `auth_token` that feeds a subsequent
    /// `BindDeviceRequest`. Neither the E2E session key nor the device
    /// private key is stored here.
    fn account_session_path(&self) -> std::path::PathBuf {
        self.app_paths.root().join("remote-control-account.json")
    }

    /// `POST /auth/send-code { email }` on the relay's auth HTTP origin.
    /// Surfaces the server's rate-limit / validation messages verbatim.
    pub async fn send_login_code(&self, email: &str) -> Result<(), String> {
        self.login.send_code(email).await.map_err(|e| e.to_string())
    }

    /// `POST /auth/login { email, code }`, then persist the issued account
    /// session locally so it survives restarts (a later bind reuses the
    /// `auth_token`). The HTTP call runs without holding the manager mutex.
    pub async fn login_with_code(&self, email: &str, code: &str) -> Result<(), String> {
        let session = self
            .login
            .login(email, code)
            .await
            .map_err(|e| e.to_string())?;
        let path = self.account_session_path();
        session.persist(&path).map_err(|e| e.to_string())?;
        let mut inner = self.inner.lock().expect("rc manager mutex poisoned");
        inner.account_session = Some(session);
        Ok(())
    }

    /// Forget the stored account session (logout). Does not touch the
    /// device key or any in-flight pairing/binding.
    pub fn logout(&self) -> Result<(), String> {
        AccountSession::clear(&self.account_session_path()).map_err(|e| e.to_string())?;
        let mut inner = self.inner.lock().expect("rc manager mutex poisoned");
        inner.account_session = None;
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
        tracing::info!(target: "remote_control", code = %code.code(), "mint pairing code");
        let payload = build_qr_payload(&inner.relay_endpoint, &code, &identity.public_b64());
        let json = payload
            .to_json()
            .map_err(|e| format!("encode qr payload: {e}"))?;
        inner.pairing_code = Some(code);
        inner.pairing_qr = Some(json.clone());
        inner.pairing_expires_at = Some(Instant::now() + DEFAULT_PAIRING_TTL);
        // A fresh mint is not registered until the driver acks it.
        inner.pairing_is_registered = false;
        let registered = Arc::new(Notify::new());
        inner.pairing_registered = Some(registered.clone());
        // Wake the driver loop so it re-registers this code with the relay
        // on the current connection (reconnecting if needed).
        drop(inner);
        self.pairing_notify.notify_one();
        Ok(Some(json))
    }

    /// Notify handle that fires once the minted pairing code has been
    /// registered with the relay. `None` when no pairing is in flight.
    pub fn pairing_registered_notify(&self) -> Option<Arc<Notify>> {
        self.inner
            .lock()
            .expect("rc manager mutex poisoned")
            .pairing_registered
            .clone()
    }

    /// Mark the in-flight pairing code as registered with the relay (driver
    /// calls this after the relay acks `PairingRegister`).
    pub fn mark_pairing_registered(&self) {
        let notify = {
            let mut inner = self.inner.lock().expect("rc manager mutex poisoned");
            inner.pairing_is_registered = true;
            inner.pairing_registered.clone()
        };
        if let Some(notify) = notify {
            notify.notify_waiters();
        }
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
        let mut inner = self.inner.lock().expect("rc manager mutex poisoned");
        // An expired code is dead: drop it (and its "registered" flag) so the
        // UI never offers a QR the relay would reject.
        if let Some(deadline) = inner.pairing_expires_at {
            if Instant::now() >= deadline {
                inner.pairing_code = None;
                inner.pairing_qr = None;
                inner.pairing_expires_at = None;
                inner.pairing_is_registered = false;
            }
        }
        let expires_in = inner
            .pairing_expires_at
            .map(|deadline| deadline.saturating_duration_since(Instant::now()).as_secs());
        RemoteControlStatus {
            enabled: inner.enabled,
            connected: inner.connected,
            device_id: inner.device_id.clone(),
            pairing_qr: inner.pairing_qr.clone(),
            pairing_registered: inner.pairing_is_registered,
            pairing_expires_in_secs: expires_in,
            subscription_active: inner.subscription_active,
            bound: inner.bound,
            account_email: inner.account_session.as_ref().map(|s| s.email.clone()),
            logged_in: inner.account_session.is_some(),
        }
    }

    /// Whether TLS certificate verification is skipped for this relay
    /// endpoint (development against a self-signed host). Driven by the
    /// `KODEX_RELAY_INSECURE_TLS` env var; defaults to off.
    pub fn insecure_tls(&self) -> bool {
        self.inner.lock().expect("rc manager mutex poisoned").insecure_tls
    }

    /// Load (or create) the device identity from the persisted key file.
    /// Used by the driver loop to authenticate to the relay.
    pub fn device_identity(&self) -> anyhow::Result<DeviceIdentity> {
        let key_path = self.app_paths.root().join("remote-control-device.key");
        DeviceIdentity::load_or_create(&key_path)
    }

    /// The currently minted pairing code, if any (and not yet expired).
    /// The driver registers this with the relay after authenticating so a
    /// scanning phone's `PairingInitiate` can be routed to this PC.
    pub fn current_pairing_code(&self) -> Option<String> {
        let mut inner = self.inner.lock().expect("rc manager mutex poisoned");
        let code = inner.pairing_code.as_mut()?;
        if code.is_expired() {
            return None;
        }
        Some(code.code().to_string())
    }
}
