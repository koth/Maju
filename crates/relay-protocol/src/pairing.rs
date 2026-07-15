use serde::{Deserialize, Serialize};

/// Phone -> relay: initiate pairing from a scanned QR code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingInitiate {
    pub pairing_code: String,
    /// PC device public key, base64-encoded.
    pub pc_device_pubkey: String,
    pub relay_endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_ephemeral_pubkey: Option<String>,
}

/// Relay -> both devices: pairing confirmed. The relay forwards the phone's
/// ephemeral public key to the PC and the PC's static public key to the phone
/// via `session_key_material`; the E2E session key is derived peer-side
/// (relay-client) via X25519 ECDH + HKDF and is never known to the relay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingConfirm {
    pub pairing_token: String,
    /// Relay-forwarded E2E key material, base64-encoded. Carries the phone
    /// ephemeral public key to the PC (and the PC static public key to the
    /// phone) for peer-side derivation.
    pub session_key_material: String,
    pub pc_device_id: String,
    pub phone_device_id: String,
}

/// PC -> relay: register a one-time pairing code against the sender's
/// connection so a scanning phone's `PairingInitiate` can be routed to this PC.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingRegister {
    pub pairing_code: String,
}

/// Device -> relay: authenticate an outbound connection with a device
/// keypair signature.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceAuth {
    pub device_id: String,
    /// Signature over `{device_id,timestamp_ms}`, base64-encoded.
    pub signature: String,
    pub timestamp_ms: u64,
}

/// Phone/PC -> relay: persist a pairing (account binding).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BindDeviceRequest {
    pub auth_token: String,
}

/// Relay -> device: bind result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BindDeviceResponse {
    pub ok: bool,
    pub bound_device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Relay -> both devices: subscription state, pushed on bind/expiry/renewal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubscriptionStatus {
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}
