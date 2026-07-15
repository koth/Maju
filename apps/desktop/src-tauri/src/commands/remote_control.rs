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
    /// Subscription state when bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_active: Option<bool>,
    /// Whether the device is bound (persisted pairing) vs free (re-scan).
    pub bound: bool,
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
