//! Device identity for relay authentication.
//!
//! Each kodex instance generates an X25519 static keypair on first launch
//! and persists the secret locally (never transmitted in plaintext). The
//! device id is a stable base64(SHA-256(public_key)) identifier.
//! Authentication to the relay signs `{device_id}:{timestamp_ms}` with
//! HMAC-SHA256 keyed by the secret bytes — a symmetric proof of identity
//! that avoids pulling in an Ed25519 signature stack (and its `rand`
//! version conflicts). X25519 is also reused for the E2E key exchange.

use anyhow::{Context, Result};
use base64::Engine;
use hmac::{Hmac, Mac};
use rand_core::OsRng;
use sha2::{Digest, Sha256};
use std::path::Path;
use x25519_dalek::{PublicKey, StaticSecret};

type HmacSha256 = Hmac<Sha256>;

/// A device keypair + derived identity material.
#[derive(Clone)]
pub struct DeviceIdentity {
    secret: StaticSecret,
    public: PublicKey,
}

impl DeviceIdentity {
    /// Generate a fresh identity using the OS RNG.
    pub fn generate() -> Self {
        let secret = StaticSecret::random_from_rng(&mut OsRng);
        let public = PublicKey::from(&secret);
        Self { secret, public }
    }

    /// Reconstruct from stored secret bytes (32). Derives the public key.
    pub fn from_bytes(secret_bytes: &[u8; 32]) -> Self {
        let secret = StaticSecret::from(*secret_bytes);
        let public = PublicKey::from(&secret);
        Self { secret, public }
    }

    /// Raw 32-byte secret (for persistence / HMAC keying).
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.secret.to_bytes()
    }

    /// Public key bytes (for the QR payload / relay registration).
    pub fn public_bytes(&self) -> [u8; 32] {
        self.public.to_bytes()
    }

    /// Stable device identifier: base64(SHA-256(public_key)).
    pub fn device_id(&self) -> String {
        let hash = Sha256::digest(self.public.to_bytes());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash)
    }

    /// Public key, base64-encoded for the QR pairing payload.
    pub fn public_b64(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.public.to_bytes())
    }

    /// HMAC-SHA256 signature over `{device_id}:{timestamp_ms}`, base64.
    /// The relay verifies this with... note: the relay knows the device's
    /// public key but HMAC is symmetric — so the relay stores the device's
    /// secret-derived HMAC key at registration time, OR we use a challenge
    /// response. For the MVP, the relay stores the public key and the device
    /// proves possession of the secret by deriving the same HMAC key; see
    /// design open questions. The signature format is stable either way.
    pub fn auth_signature(&self, timestamp_ms: u64) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.secret.to_bytes())
            .expect("HMAC accepts any key length");
        mac.update(format!("{}:{timestamp_ms}", self.device_id()).as_bytes());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }

    /// Persist the secret to `path` (32 raw bytes). Created if missing.
    pub fn persist(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create device-key dir {:?}", parent))?;
        }
        std::fs::write(path, self.secret.to_bytes())
            .with_context(|| format!("write device key {:?}", path))?;
        Ok(())
    }

    /// Load the identity from `path`, generating + persisting a fresh one if
    /// the file does not exist.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            let bytes = std::fs::read(path).with_context(|| format!("read device key {:?}", path))?;
            if bytes.len() != 32 {
                anyhow::bail!("device key file is {} bytes, expected 32", bytes.len());
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            return Ok(Self::from_bytes(&arr));
        }
        let identity = Self::generate();
        identity.persist(path)?;
        Ok(identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_is_stable_for_same_secret() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::from_bytes(&a.secret_bytes());
        assert_eq!(a.device_id(), b.device_id());
        assert_eq!(a.public_bytes(), b.public_bytes());
    }

    #[test]
    fn different_identities_have_different_ids() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        assert_ne!(a.device_id(), b.device_id());
    }

    #[test]
    fn auth_signature_is_deterministic_for_same_timestamp() {
        let id = DeviceIdentity::generate();
        let sig_a = id.auth_signature(1_700_000_000_000);
        let sig_b = id.auth_signature(1_700_000_000_000);
        assert_eq!(sig_a, sig_b);
        let sig_other_ts = id.auth_signature(1_700_000_000_001);
        assert_ne!(sig_a, sig_other_ts);
    }

    #[test]
    fn persist_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.key");
        let id = DeviceIdentity::load_or_create(&path).unwrap();
        let original_id = id.device_id();
        let reloaded = DeviceIdentity::load_or_create(&path).unwrap();
        assert_eq!(reloaded.device_id(), original_id);
    }
}
