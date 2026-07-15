//! Outbound-only relay client for the kodex mobile remote-control plane.
//!
//! Dials a public relay over TLS WebSocket, authenticates with a local
//! device identity, routes inbound control requests to the `RemoteControl`
//! gateway, and pushes event frames outbound. Pairing, E2E encryption,
//! account binding, and subscription surfacing are added by the
//! implementing tasks.
//! Outbound-only relay client for the kodex mobile remote-control plane.
//!
//! Dials a public relay over TLS WebSocket, authenticates with a local
//! device identity, routes inbound control requests to the `RemoteControl`
//! gateway, and pushes event frames outbound. Pairing, E2E encryption,
//! account binding, and subscription surfacing are added by the
//! implementing tasks.
//!
//! Layering: this crate is TRANSPORT ONLY (WS + E2E + framing). It depends
//! on `relay-protocol` and `workspace-model`, NOT on `app-core`. The
//! `AppUpdate` -> `EventFrame` mapping is the shell's job (it bridges
//! app-core and relay-protocol); this crate sends/receives already-mapped
//! `Envelope`s.

mod crypto;
mod connection;
mod driver;
mod binding;
mod identity;
mod pairing;

pub use crypto::{SessionKey, decrypt, encrypt};
pub use connection::{RelayConnection, RelayTransport, WsTransport, dial_plain, spawn_mock_relay};
pub use driver::{ControlHandler, EventSource, RelayDriver};
pub use binding::{BindOutcome, BoundDevice, SubscriptionState};
pub use identity::DeviceIdentity;
pub use pairing::{PairingCode, PairingQrPayload, DEFAULT_PAIRING_TTL, build_qr_payload, ecdh_shared_secret};
