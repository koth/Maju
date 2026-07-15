//! Wire contract for the kodex mobile remote-control plane.
//!
//! Pure serde types only — no IO, no network, no crypto. Vendored by the
//! PC control gateway (`relay-client`), the (out-of-scope) public relay
//! service, and the (out-of-scope) phone companion app so all three peers
//! agree on the shape of every frame.

mod control;
mod envelope;
mod events;
mod pairing;

pub use control::{ControlRequest, ControlResponse};
pub use envelope::{EncryptedEnvelope, Envelope, Message, PROTO_VERSION};
pub use events::EventFrame;
pub use pairing::{
    BindDeviceRequest, BindDeviceResponse, DeviceAuth, PairingConfirm, PairingInitiate,
    PairingRegister, SubscriptionStatus,
};
