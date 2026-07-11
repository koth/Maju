use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OaiMessage {
    pub role: String,
    #[serde(default)]
    pub content: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OaiTool {
    #[serde(rename = "type")]
    pub type_field: String,
    pub function: OaiToolFunction,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OaiToolFunction {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OaiChatRequest {
    #[serde(default)]
    pub model: Option<String>,
    pub messages: Vec<OaiMessage>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub tools: Option<Vec<OaiTool>>,
    #[serde(default)]
    pub max_tokens: Option<Value>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(flatten)]
    pub extra: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OaiChatResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<OaiChoice>,
    pub usage: OaiUsage,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OaiChoice {
    pub index: u32,
    pub message: OaiChoiceMessage,
    pub finish_reason: String,
    pub logprobs: Option<Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OaiChoiceMessage {
    pub role: String,
    pub content: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<Value>>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OaiUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    /// OpenAI-style nested cache hit count. `codex_api_proxy` reads this via
    /// `prompt_tokens_details.cached_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<OaiPromptTokensDetails>,
    /// Anthropic-style cache-read count. Kept as a top-level alias so
    /// `usage_cached_input_tokens` can pick it up without nested details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    /// Anthropic-style cache-write / creation count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
}

impl OaiUsage {
    pub fn zero() -> Self {
        Self {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            prompt_tokens_details: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OaiPromptTokensDetails {
    pub cached_tokens: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OaiChatChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<OaiChunkChoice>,
    /// OpenAI streams a terminal `choices: []` chunk carrying `usage` when
    /// `stream_options.include_usage` is set. We emit it unconditionally
    /// (internal reverse proxy for codex) so clients can account per-turn
    /// tokens even without opting in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<OaiUsage>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OaiChunkChoice {
    pub index: u32,
    pub delta: Value,
    pub finish_reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OaiModelsResponse {
    pub object: String,
    pub data: Vec<Value>,
}
