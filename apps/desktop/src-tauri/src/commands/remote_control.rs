//! Tauri commands for the mobile remote-control plane: generate a pairing
//! QR, query connection/subscription status, and toggle the kill switch.
//! The actual relay-client connection (dial/auth/route) is owned by the
//! relay driver task; these commands are the UI-facing surface.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

/// Snapshot of the remote-control plane surfaced to the UI.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RemoteControlStatus {
    /// Whether the relay-client is enabled (not kill-switched off).
    pub enabled: bool,
    /// Whether an outbound relay connection is currently established.
    pub connected: bool,
    /// The device id of this PC (for display).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// Active pairing QR payload (JSON), if a code is currently minted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_qr: Option<String>,
    /// Whether the displayed pairing code has been registered with (acked by)
    /// the relay. A rendered QR is not a usable QR: scanning one the relay has
    /// never seen fails with "invalid or expired pairing code".
    pub pairing_registered: bool,
    /// Seconds until the displayed pairing code expires (`None` with no code).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_expires_in_secs: Option<u64>,
    /// Subscription state when bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_active: Option<bool>,
    /// Whether the device is bound (persisted pairing) vs free (re-scan).
    pub bound: bool,
    /// Email of the logged-in account, when a session is stored locally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_email: Option<String>,
    /// Whether an account session (auth_token) is stored locally, ready to
    /// feed a `BindDeviceRequest`. Independent of `connected`/`bound`.
    pub logged_in: bool,
}

/// Kill switch: disable the relay-client (fail-open to "disconnected"; local
/// sessions are unaffected). Persists for the process lifetime.
#[tauri::command]
pub fn remote_control_set_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    state.remote_control().set_enabled(enabled);
    Ok(())
}

/// Mint a fresh pairing QR payload (short-lived one-time code + relay
/// endpoint + PC device public key). Returns the JSON string for the
/// frontend to render as a QR.
#[tauri::command]
pub fn remote_control_pairing_qr(
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    state.remote_control().mint_pairing_qr()
}

/// Current remote-control status for the UI status indicator.
#[tauri::command]
pub fn remote_control_status(state: State<'_, AppState>) -> Result<RemoteControlStatus, String> {
    Ok(state.remote_control().status())
}

/// Step 1 of the passwordless email-OTP login: ask the relay to email a
/// one-time code. Surfaces the relay's rate-limit / validation message on
/// error (e.g. "请求过于频繁，请稍后再试").
#[tauri::command]
pub async fn remote_control_send_login_code(
    state: State<'_, AppState>,
    email: String,
) -> Result<(), String> {
    state.remote_control().send_login_code(&email).await
}

/// Step 2: verify the emailed code; on success the relay issues an account
/// session that the manager persists locally (the `auth_token` later feeds
/// `BindDeviceRequest`). Surfaces the relay's error message on a wrong /
/// expired / consumed code.
#[tauri::command]
pub async fn remote_control_login(
    state: State<'_, AppState>,
    email: String,
    code: String,
) -> Result<(), String> {
    state
        .remote_control()
        .login_with_code(&email, &code)
        .await
}

/// Forget the locally stored account session (logout). Does not affect the
/// device key, an in-flight pairing, or local sessions.
#[tauri::command]
pub fn remote_control_logout(state: State<'_, AppState>) -> Result<(), String> {
    state.remote_control().logout()
}
