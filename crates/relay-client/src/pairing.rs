//! Scan-code pairing: short-lived one-time codes + QR payload.
//!
//! Kodex generates a single-use pairing code with a configurable TTL (default
//! <= 120s) and encodes `{relay_endpoint, pairing_code, pc_device_pubkey}`
//! into a QR. The phone scans it and posts a `PairingInitiate` to the relay;
//! the relay binds the two device identities and returns confirmation + E2E
//! key-derivation material. The E2E session key is derived from an X25519
//! ECDH between the PC static key and an ephemeral phone key (carried in the
//! confirm message) — the relay routes the ECDH material but cannot compute
//! the shared secret without the PC private key.

use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use x25519_dalek::{PublicKey, StaticSecret};

/// Default pairing-code lifetime.
pub const DEFAULT_PAIRING_TTL: Duration = Duration::from_secs(120);

/// A short-lived, single-use pairing code.
pub struct PairingCode {
    code: String,
    expires_at: Instant,
    used: bool,
}

impl PairingCode {
    /// Mint a fresh 8-character one-time code (base32-ish alphabet, no
    /// ambiguous glyphs) valid for `ttl`.
    pub fn mint(ttl: Duration) -> Self {
        const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        let mut idx = [0u8; 8];
        rand_core::RngCore::fill_bytes(&mut OsRng, &mut idx);
        let code: String = idx
            .iter()
            .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
            .collect();
        Self {
            code,
            expires_at: Instant::now() + ttl,
            used: false,
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }

    /// Consume the code for a single pairing attempt. Returns `false` if
    /// already used or expired.
    pub fn consume(&mut self) -> bool {
        if self.used || self.is_expired() {
            return false;
        }
        self.used = true;
        true
    }
}

/// QR payload scanned by the phone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingQrPayload {
    pub relay_endpoint: String,
    pub pairing_code: String,
    pub pc_device_pubkey: String,
    /// Friendly name of this PC (its hostname). Carried so the phone can label
    /// the bound machine with something recognizable instead of a device-id
    /// prefix like "PC UTNAZ0ask1". Optional: older PCs omit it and the phone
    /// falls back to the id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pc_name: Option<String>,
    /// The PC's OWN address on its network (the local IP it uses to reach the
    /// relay). Deliberately not the relay host: that is identical for every
    /// paired PC, so showing it left the machines list with no way to tell two
    /// machines apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pc_ip: Option<String>,
}

impl PairingQrPayload {
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }
}

/// Build the QR payload from a relay endpoint, a freshly minted code, the PC
/// device public key (base64), and this PC's friendly name (hostname) + its
/// own network address.
pub fn build_qr_payload(
    relay_endpoint: &str,
    code: &PairingCode,
    pc_device_pubkey_b64: &str,
    pc_name: Option<&str>,
    pc_ip: Option<&str>,
) -> PairingQrPayload {
    // A blank value is the same as no value: the phone's fallback is better
    // than an empty string.
    let clean = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    PairingQrPayload {
        relay_endpoint: relay_endpoint.to_string(),
        pairing_code: code.code().to_string(),
        pc_device_pubkey: pc_device_pubkey_b64.to_string(),
        pc_name: clean(pc_name),
        pc_ip: clean(pc_ip),
    }
}

/// Derive the E2E session key material from an X25519 ECDH between the PC
/// static secret and the phone's ephemeral public key. Returns the raw
/// shared secret bytes (fed to `SessionKey::derive`).
pub fn ecdh_shared_secret(
    pc_secret: &StaticSecret,
    phone_ephemeral_public: &PublicKey,
) -> [u8; 32] {
    pc_secret.diffie_hellman(phone_ephemeral_public).to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_code_is_single_use() {
        let mut code = PairingCode::mint(DEFAULT_PAIRING_TTL);
        assert!(!code.is_expired());
        assert!(code.consume(), "first use succeeds");
        assert!(!code.consume(), "second use rejected");
    }

    #[test]
    fn expired_code_is_rejected() {
        let mut code = PairingCode::mint(Duration::from_millis(0));
        std::thread::sleep(Duration::from_millis(2));
        assert!(code.is_expired());
        assert!(!code.consume());
    }

    #[test]
    fn qr_payload_roundtrips() {
        let code = PairingCode::mint(DEFAULT_PAIRING_TTL);
        let payload = build_qr_payload(
            "wss://relay.example.com",
            &code,
            "pubkey-b64",
            Some("Koth-MacBook"),
            Some("192.168.1.24"),
        );
        let json = payload.to_json().unwrap();
        let back: PairingQrPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, payload);
        assert_eq!(back.relay_endpoint, "wss://relay.example.com");
        assert_eq!(back.pairing_code, code.code());
        assert_eq!(back.pc_name.as_deref(), Some("Koth-MacBook"));
        assert_eq!(back.pc_ip.as_deref(), Some("192.168.1.24"));
        // The PC's own address, never the relay's — the relay host is the same
        // for every machine and therefore useless for telling them apart.
        assert_ne!(back.pc_ip.as_deref(), Some("relay.example.com"));
    }

    #[test]
    fn qr_payload_omits_blank_identity_fields() {
        let code = PairingCode::mint(DEFAULT_PAIRING_TTL);
        for name in [None, Some(""), Some("   ")] {
            let payload = build_qr_payload("wss://relay.example.com", &code, "pk", name, None);
            assert_eq!(payload.pc_name, None);
            assert_eq!(payload.pc_ip, None);
            // Absent, not null: a phone parsing an older payload still works.
            let json = payload.to_json().unwrap();
            assert!(!json.contains("pc_name"));
            assert!(!json.contains("pc_ip"));
        }
    }

    #[test]
    fn qr_payload_backfills_a_pc_name_from_an_older_pc() {
        // A QR minted by a PC that predates `pc_name` must still parse.
        let json = r#"{"relay_endpoint":"wss://r","pairing_code":"ABCD","pc_device_pubkey":"pk"}"#;
        let payload: PairingQrPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.pc_name, None);
    }

    #[test]
    fn ecdh_yields_symmetric_shared_secret() {
        let pc_secret = StaticSecret::random_from_rng(&mut OsRng);
        let pc_public = PublicKey::from(&pc_secret);
        let phone_secret = StaticSecret::random_from_rng(&mut OsRng);
        let phone_public = PublicKey::from(&phone_secret);

        let shared_pc = ecdh_shared_secret(&pc_secret, &phone_public);
        let shared_phone = ecdh_shared_secret(&phone_secret, &pc_public);
        assert_eq!(shared_pc, shared_phone);
    }
}
