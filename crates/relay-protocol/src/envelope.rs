use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::control::{ControlRequest, ControlResponse};
use crate::events::EventFrame;
use crate::pairing::{
    BindDeviceRequest, BindDeviceResponse, DeviceAuth, PairingConfirm, PairingInitiate,
    PairingRegister, SubscriptionStatus,
};

/// Wire protocol version. Bumped only on incompatible envelope/message
/// changes. Adding new message types is forward-compatible (unknown
/// discriminators map to [`Message::Unknown`]) and does not require a bump.
pub const PROTO_VERSION: u32 = 1;

/// The raw wire frame exchanged between PC, relay, and phone.
///
/// `message_type` (serialized as `type`) is a free-form string so that
/// unknown discriminators always deserialize successfully. Typed
/// interpretation is done via [`Envelope::into_message`] /
/// [`Envelope::from_message`], which maps unknown types to
/// [`Message::Unknown`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Envelope {
    pub proto_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    #[serde(rename = "type")]
    pub message_type: String,
    #[serde(default = "default_payload")]
    pub payload: Value,
}

fn default_payload() -> Value {
    Value::Null
}

/// Typed view of an [`Envelope`] payload, reached via
/// [`Envelope::into_message`]. Serialized adjacently
/// (`{"type":"..","payload":{..}}`) on the typed path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Message {
    ControlRequest(ControlRequest),
    ControlResponse(ControlResponse),
    Event(EventFrame),
    PairingInitiate(PairingInitiate),
    PairingConfirm(PairingConfirm),
    PairingRegister(PairingRegister),
    DeviceAuth(DeviceAuth),
    BindDeviceRequest(BindDeviceRequest),
    BindDeviceResponse(BindDeviceResponse),
    SubscriptionStatus(SubscriptionStatus),
    /// Catch-all for unrecognized wire discriminators; carries the raw
    /// payload so newer peers' messages are not dropped by older peers.
    Unknown(Value),
}

/// Outer relay-routing shape that wraps a serialized [`Envelope`]. The
/// relay routes by `to_device_id` only and never inspects `ciphertext`;
/// encrypt/decrypt is owned by `relay-client`, not this crate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncryptedEnvelope {
    pub to_device_id: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

impl Envelope {
    /// Build an envelope from a typed message, assigning the given
    /// request/response `id` (None for unsolicited events).
    pub fn from_message(id: Option<Uuid>, message: &Message) -> serde_json::Result<Self> {
        let value = serde_json::to_value(message)?;
        let (message_type, payload) = split_typed(value);
        Ok(Self {
            proto_version: PROTO_VERSION,
            id,
            message_type,
            payload,
        })
    }

    /// Interpret this envelope as a typed message. Unknown discriminators
    /// map to [`Message::Unknown`] carrying the raw payload.
    pub fn into_message(&self) -> serde_json::Result<Message> {
        Ok(match self.message_type.as_str() {
            "control_request" => {
                Message::ControlRequest(serde_json::from_value(self.payload.clone())?)
            }
            "control_response" => {
                Message::ControlResponse(serde_json::from_value(self.payload.clone())?)
            }
            "event" => Message::Event(serde_json::from_value(self.payload.clone())?),
            "pairing_initiate" => {
                Message::PairingInitiate(serde_json::from_value(self.payload.clone())?)
            }
            "pairing_confirm" => {
                Message::PairingConfirm(serde_json::from_value(self.payload.clone())?)
            }
            "pairing_register" => {
                Message::PairingRegister(serde_json::from_value(self.payload.clone())?)
            }
            "device_auth" => Message::DeviceAuth(serde_json::from_value(self.payload.clone())?),
            "bind_device_request" => {
                Message::BindDeviceRequest(serde_json::from_value(self.payload.clone())?)
            }
            "bind_device_response" => {
                Message::BindDeviceResponse(serde_json::from_value(self.payload.clone())?)
            }
            "subscription_status" => {
                Message::SubscriptionStatus(serde_json::from_value(self.payload.clone())?)
            }
            other => {
                let _ = other;
                Message::Unknown(self.payload.clone())
            }
        })
    }
}

/// Split an adjacently-tagged `Message` Value into its `(type, payload)`.
fn split_typed(value: Value) -> (String, Value) {
    match value {
        Value::Object(ref map) => {
            let message_type = map
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let payload = map.get("payload").cloned().unwrap_or(Value::Null);
            (message_type, payload)
        }
        other => ("unknown".to_string(), other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::ControlRequest;
    use uuid::Uuid;
    use workspace_model::SessionStatus;

    #[test]
    fn envelope_roundtrip_preserves_all_fields() {
        let id = Uuid::new_v4();
        let env = Envelope {
            proto_version: PROTO_VERSION,
            id: Some(id),
            message_type: "control_request".to_string(),
            payload: serde_json::json!({"op":"cancel","request_id": id.to_string()}),
        };
        let json = serde_json::to_string(&env).unwrap();
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
    }

    #[test]
    fn unknown_type_lands_in_unknown_variant() {
        let env = Envelope {
            proto_version: PROTO_VERSION,
            id: None,
            message_type: "some_future_message".to_string(),
            payload: serde_json::json!({"anything": 42}),
        };
        let msg = env.into_message().unwrap();
        match msg {
            Message::Unknown(v) => assert_eq!(v, serde_json::json!({"anything": 42})),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn control_request_roundtrips_and_echoes_request_id() {
        let request_id = Uuid::new_v4();
        let req = ControlRequest::Cancel { request_id };
        let env =
            Envelope::from_message(Some(request_id), &Message::ControlRequest(req.clone()))
                .unwrap();
        assert_eq!(env.id, Some(request_id));
        assert_eq!(env.message_type, "control_request");
        let msg = env.into_message().unwrap();
        match msg {
            Message::ControlRequest(ControlRequest::Cancel { request_id: rid }) => {
                assert_eq!(rid, request_id);
            }
            other => panic!("expected ControlRequest::Cancel, got {other:?}"),
        }
    }

    #[test]
    fn event_frame_roundtrips_through_envelope() {
        let env = Envelope::from_message(
            None,
            &Message::Event(EventFrame::SessionStatusChanged {
                session_id: "s-1".to_string(),
                status: SessionStatus::Idle,
            }),
        )
        .unwrap();
        assert_eq!(env.message_type, "event");
        match env.into_message().unwrap() {
            Message::Event(EventFrame::SessionStatusChanged { session_id, status }) => {
                assert_eq!(session_id, "s-1");
                assert_eq!(status, SessionStatus::Idle);
            }
            other => panic!("expected SessionStatusChanged, got {other:?}"),
        }
    }

    #[test]
    fn encrypted_envelope_exposes_no_plaintext() {
        let enc = EncryptedEnvelope {
            to_device_id: "dev-1".to_string(),
            nonce: vec![1, 2, 3],
            ciphertext: vec![4, 5, 6],
        };
        let json = serde_json::to_value(&enc).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(obj.len(), 3);
        assert!(obj.contains_key("to_device_id"));
        assert!(obj.contains_key("nonce"));
        assert!(obj.contains_key("ciphertext"));
        for key in ["payload", "type", "id", "message_type", "proto_version"] {
            assert!(!obj.contains_key(key), "unexpected key {key}");
        }
    }
}
