use serde::{Deserialize, Serialize};
use serde_json::Value;
/// A raw, loosely-typed message from the CLI's stdout.
///
/// We deserialize the full JSON but keep the body as [`Value`] to stay
/// resilient to CLI version drift (unknown fields ignored, missing
/// fields default). The adapter layer interprets the fields it needs
/// via `Value` accessors.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    #[serde(rename = "type")]
    pub type_field: String,
    #[serde(flatten)]
    pub rest: Value,
}
impl Message {
    pub fn msg_type(&self) -> &str {
        &self.type_field
    }
    pub fn session_id(&self) -> Option<&str> {
        self.rest.get("session_id").and_then(Value::as_str)
    }
    pub fn subtype(&self) -> Option<&str> {
        self.rest.get("subtype").and_then(Value::as_str)
    }
    /// For `assistant` messages: the `message.content` array.
    pub fn content_blocks(&self) -> Option<&Vec<Value>> {
        self.rest
            .get("message")
 .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
    }
    /// For `assistant` messages: the `message.stop_reason` string.
    pub fn stop_reason(&self) -> Option<&str> {
        self.rest
            .get("message")
            .and_then(|m| m.get("stop_reason"))
            .and_then(Value::as_str)
    }
    /// For `assistant` messages: the `message.model` string.
    pub fn model(&self) -> Option<&str> {
        self.rest
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
    }
    /// For `assistant` messages: the `message.usage` object.
    pub fn usage(&self) -> Option<&Value> {
        self.rest.get("message").and_then(|m| m.get("usage"))
    }
    /// For `result` messages: the `usage` object.
    pub fn result_usage(&self) -> Option<&Value> {
        self.rest.get("usage")
    }
    /// For `result` messages: the `is_error` flag.
    pub fn is_error(&self) -> Option<bool> {
        self.rest.get("is_error").and_then(Value::as_bool)
    }
    /// For `stream_event` messages: the `event` object.
    pub fn event(&self) -> Option<&Value> {
        self.rest.get("event")
    }
    /// For `error` messages: the `error` string.
    pub fn error_text(&self) -> Option<&str> {
        self.rest.get("error").and_then(Value::as_str)
    }
}
