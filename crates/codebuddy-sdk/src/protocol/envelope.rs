use serde::{Deserialize, Serialize};
use serde_json::Value;
/// Top-level `control_request` we send to the CLI.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ControlRequest {
    #[serde(rename = "type")]
    pub type_field: String,
    pub request_id: String,
    pub request: Value,
}
impl ControlRequest {
    pub fn new(request_id: impl Into<String>, request: Value) -> Self {
        Self {
            type_field: "control_request".to_string(),
            request_id: request_id.into(),
            request,
        }
    }
}
/// Top-level `control_response` the CLI sends back (answer to our
/// `control_request`), or a reverse `control_request` the CLI sends us
/// (e.g. `can_use_tool`, `mcp_message`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ControlResponse {
    #[serde(rename = "type")]
    pub type_field: String,
    #[serde(default)]
    pub response: Option<ControlResponseBody>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub request: Option<Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ControlResponseBody {
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub response: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
}
/// Top-level `user` message envelope we write to stdin.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserMessage {
    #[serde(rename = "type")]
    pub type_field: String,
    pub session_id: String,
    pub message: UserMessageInner,
    pub parent_tool_use_id: Option<Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserMessageInner {
    pub role: String,
    pub content: Value,
}
impl UserMessage {
    pub fn text(session_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            type_field: "user".to_string(),
            session_id: session_id.into(),
            message: UserMessageInner {
                role: "user".to_string(),
                content: Value::String(content.into()),
            },
            parent_tool_use_id: None,
        }
    }
    pub fn structured(session_id: impl Into<String>, content: Value) -> Self {
        Self {
            type_field: "user".to_string(),
            session_id: session_id.into(),
            message: UserMessageInner {
                role: "user".to_string(),
                content,
            },
            parent_tool_use_id: None,
        }
    }
}
