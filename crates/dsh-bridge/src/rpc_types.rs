//! Lenient serde mirrors of the dsh host RPC envelope and the control-method
//! payloads the bridge uses.
//!
//! The dsh schema is a private, versioned contract with no stability guarantee
//! (see `deepseek-harness/packages/host/apiproxy/src/api/rpc.schema.ts`).
//! Deserialization is lenient: unknown fields are ignored (`#[serde(default)]`
//! on optional fields, opaque [`serde_json::Value`] for variable payloads), so
//! additive schema changes do not break the stream. Only removals or shape
//! changes of consumed fields break, which integration tests pin.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// RPC correlation id (a UUID string on the wire; branded `RpcId` in dsh).
pub type RpcId = String;

/// dsh session id (a non-empty string; branded `SessionId` in dsh).
pub type SessionId = String;

/// dsh approval request id (a non-empty string).
pub type ApprovalRequestId = String;

/// `ClientRequest` full form — the body of `POST /api/<method>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientRequest {
    /// Wire tag — always the literal `"client-request"`.
    #[serde(rename = "type")]
    pub type_tag: String,
    pub rpcId: RpcId,
    pub method: String,
    pub payload: Value,
}

impl ClientRequest {
    pub fn new(rpc_id: RpcId, method: impl Into<String>, payload: Value) -> Self {
        Self {
            type_tag: "client-request".to_string(),
            rpcId: rpc_id,
            method: method.into(),
            payload,
        }
    }
}

/// `ServerResponse` full form — the HTTP response body of a control POST.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerResponse {
    /// Wire tag — always the literal `"server-response"`.
    #[serde(rename = "type")]
    pub type_tag: String,
    pub rpcId: RpcId,
    pub result: RpcResult<Value>,
}

/// `ServerRequest` full form — one SSE frame (a server-initiated message).
/// `method` is the frame's `type` (e.g. `session/event`); `payload` is the
/// `MuxFrame`/`HostFrame` body.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerRequest {
    /// Wire tag — always the literal `"server-request"`.
    #[serde(rename = "type")]
    pub type_tag: String,
    #[serde(default)]
    pub rpcId: RpcId,
    pub method: String,
    pub payload: Value,
}

/// `ClientResponse` full form — the body of `POST /api/respond`, answering a
/// server-request (approval/question) by echoing its `rpcId`.
#[derive(Debug, Clone, Serialize)]
pub struct ClientResponse {
    /// Wire tag — always the literal `"client-response"`.
    #[serde(rename = "type")]
    pub type_tag: String,
    pub rpcId: RpcId,
    pub result: RpcResult<Value>,
}

impl ClientResponse {
    pub fn ok(rpc_id: RpcId, value: Value) -> Self {
        Self {
            type_tag: "client-response".to_string(),
            rpcId: rpc_id,
            result: RpcResult::Ok { ok: true, value },
        }
    }
}

/// Result args for the gateway-internal `POST /api/$events/result` carrier.
/// dsh 0.1.2 resolves `$events` waterfalls through this endpoint instead of
/// the legacy `client-response` envelope.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteEventResultArgs {
    #[serde(rename = "clientId")]
    pub client_id: String,
    #[serde(rename = "eventId")]
    pub event_id: String,
    pub outcome: RemoteEventOutcome,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteEventOutcome {
    pub kind: &'static str,
    pub value: Value,
}

/// Business success/failure result. The error arm is held as opaque JSON so an
/// unknown error `code` does not break deserialization. Manual serde keeps
/// error parsing authoritative while accepting the typert void result
/// (`{"ok":true}` with no `value` field).
#[derive(Debug, Clone)]
pub enum RpcResult<T> {
    Err { ok: bool, error: RpcError },
    Ok { ok: bool, value: T },
}

impl<'de, T> Deserialize<'de> for RpcResult<T>
where
    T: serde::de::DeserializeOwned,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = Value::deserialize(deserializer)?;
        let ok = raw.get("ok").and_then(Value::as_bool).unwrap_or(false);
        if raw.get("error").is_some_and(|value| !value.is_null()) {
            let error =
                serde_json::from_value(raw["error"].clone()).map_err(serde::de::Error::custom)?;
            return Ok(Self::Err { ok, error });
        }
        let value = match raw.get("value") {
            Some(value) => {
                serde_json::from_value(value.clone()).map_err(serde::de::Error::custom)?
            }
            None => serde_json::from_value(Value::Null).map_err(serde::de::Error::custom)?,
        };
        Ok(Self::Ok { ok, value })
    }
}

impl<T> RpcResult<T> {
    pub fn is_ok(&self) -> bool {
        matches!(self, RpcResult::Ok { .. })
    }

    pub fn ok_value(&self) -> Option<&T> {
        match self {
            RpcResult::Ok { value, .. } => Some(value),
            RpcResult::Err { .. } => None,
        }
    }

    pub fn err(&self) -> Option<&RpcError> {
        match self {
            RpcResult::Ok { .. } => None,
            RpcResult::Err { error, .. } => Some(error),
        }
    }
}

impl<T: Serialize> Serialize for RpcResult<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Err { ok, error } => {
                use serde::ser::SerializeStruct;
                let mut state = serializer.serialize_struct("RpcResult", 2)?;
                state.serialize_field("ok", ok)?;
                state.serialize_field("error", error)?;
                state.end()
            }
            Self::Ok { ok, value } => {
                use serde::ser::SerializeStruct;
                let mut state = serializer.serialize_struct("RpcResult", 2)?;
                state.serialize_field("ok", ok)?;
                state.serialize_field("value", value)?;
                state.end()
            }
        }
    }
}

/// dsh `RpcError` — `{ code, message, details }`. `details` is opaque; `code`
/// is a string so unknown codes deserialize without failing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub details: Value,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// Extract the dsh error `code` from a `call()` failure. `call()` formats
/// business errors via `RpcError`'s Display as `"{code}: {message}"`, so the
/// code is recoverable from the message prefix before the first `": "`.
pub fn rpc_error_code(err: &anyhow::Error) -> Option<String> {
    let msg = format!("{err}");
    msg.split(": ")
        .next()
        .filter(|code| {
            !code.is_empty() && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        .map(|code| code.to_string())
}

/// Whether a `commands/execute` failure is the gateway refusing the args shape
/// over the ATTACHMENT field name.
///
/// The descriptor renamed `images` → `submittedAttachments` in dsh 0.1.5, and
/// the rejection names both sides of the mismatch, e.g.
/// `gateway/arguments-invalid: type=t gateway: commands/execute: args fields do
/// not match the descriptor: missing "submittedAttachments"; unexpected
/// "images"`. Requiring the `arguments-invalid` code keeps a genuinely bad
/// command line (unknown name, malformed syntax) from triggering a retry.
pub fn is_command_attachment_field_mismatch(err: &anyhow::Error) -> bool {
    let message = format!("{err}");
    message.contains("arguments-invalid")
        && (message.contains("submittedAttachments") || message.contains("images"))
}

/// Marker embedded in the transport error raised when a host answered a
/// forwarded-waterfall answer through the *other* answer protocol's carrier.
/// Only that error may trigger a protocol retry.
pub const ANSWER_PROTOCOL_MISMATCH: &str = "answer-protocol-mismatch";

/// Whether an `$events/result` failure means the host speaks the other
/// [`AnswerProtocol`] than the one we used. The answer value itself is
/// protocol-dependent, so the caller must rebuild it before retrying.
pub fn is_answer_protocol_mismatch(err: &anyhow::Error) -> bool {
    format!("{err}").contains(ANSWER_PROTOCOL_MISMATCH)
}

/// How this host accepts a forwarded-waterfall answer (`$events/result`), and
/// therefore how the answer value itself is shaped.
///
/// dsh 0.1.5 moved the gateway-internal `$events/result` endpoint onto the
/// shared Connection RPC interceptor: the request must now be a full
/// `client-request` envelope, and the forwarded waterfall resolves with the
/// **bare** answer value. dsh ≤ 0.1.4 served a bare `{ args }` body and
/// resolved user questions with a `{ sessionId, answer }` wrapper, while
/// approvals rode the separate `/api/respond` `client-response` carrier.
///
/// The bridge has no version negotiation (see [`crate::rpc_types`] `host.describe`
/// — it is a local stub), so the protocol is learned from the `$events` item
/// envelope (0.1.5 wraps every logical-stream value) and corrected by one
/// retry if the host answers with the other protocol's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerProtocol {
    /// dsh ≥ 0.1.5 — `client-request` envelope, bare waterfall value.
    Envelope,
    /// dsh ≤ 0.1.4 — bare `{ args }` body, wrapped question value.
    Legacy,
}

impl AnswerProtocol {
    /// The other protocol (the one to try when the host rejects this carrier).
    pub fn toggled(self) -> Self {
        match self {
            AnswerProtocol::Envelope => AnswerProtocol::Legacy,
            AnswerProtocol::Legacy => AnswerProtocol::Envelope,
        }
    }
}

/// `RpcReceipt` — the HTTP response body of legacy `/api/respond` or the
/// gateway's `$events/result` RPC. A late/duplicate result yields
/// `not-pending`; a malformed body yields `bad-response`.
#[derive(Debug, Clone)]
pub enum RpcReceipt {
    Legacy { accepted: bool, reason: String },
    Gateway { is_ok: bool, message: String },
}

impl RpcReceipt {
    pub fn accepted(&self) -> bool {
        match self {
            RpcReceipt::Legacy { accepted, .. } => *accepted,
            RpcReceipt::Gateway { is_ok, .. } => *is_ok,
        }
    }

    /// Human-readable rejection reason. For gateway errors this prefers the
    /// embedded `error.message`; for legacy receipts it uses `reason`.
    pub fn rejection_reason(&self) -> String {
        match self {
            RpcReceipt::Legacy { reason, .. } => reason.clone(),
            RpcReceipt::Gateway { message, .. } => message.clone(),
        }
    }
}

// ---- Control-method payloads (the ones the bridge issues) ----
// These are `Serialize` (request) / `Deserialize` (response) mirrors. Optional
// fields use `#[serde(default)]` and `skip_serializing_if = "Option::is_none"`
// so absent fields stay absent on the wire (dsh schemas use
// exactOptionalPropertyTypes).

/// `session.create` request (`{ cwd?, workspaceId?, sessionId?, agentPreset? }`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SessionCreatePayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "sessionId")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "agentPreset")]
    pub agent_preset: Option<String>,
}

/// `session.create` response value.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionCreateValue {
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    #[serde(default, rename = "agentPreset")]
    pub agent_preset: Option<String>,
}

/// `session.fork` request (`{ sessionId, atSeq? }`). The harness cuts the seed
/// at the first `turn/end` event with `seq >= atSeq`; an omitted `atSeq` keeps
/// the last completed turn. Anchoring inside an open turn is rejected by the
/// host with `fork-unavailable`.
#[derive(Debug, Clone, Serialize)]
pub struct SessionForkPayload {
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    #[serde(skip_serializing_if = "Option::is_none", rename = "atSeq")]
    pub at_seq: Option<u64>,
}

/// `session.fork` response value — the child (forked) session id.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionForkValue {
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
}

/// `agentPreset.list` request payload (empty object).
#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentPresetListPayload {}

/// One entry of `agentPreset.list` (`{ id, trust, isDefault, name?, description?, broken? }`).
#[derive(Debug, Clone, Deserialize)]
pub struct AgentPresetEntry {
    pub id: String,
    #[serde(default)]
    pub trust: Option<String>,
    #[serde(default, rename = "isDefault")]
    pub is_default: bool,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// `agentPreset.list` response value.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentPresetListValue {
    #[serde(default)]
    pub presets: Vec<AgentPresetEntry>,
}

/// `agentPreset.select` request payload (`{ sessionId, agentPreset }`).
#[derive(Debug, Clone, Serialize)]
pub struct AgentPresetSelectPayload {
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    #[serde(rename = "agentPreset")]
    pub agent_preset: String,
}

/// `agentPreset.select` response value.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentPresetSelectValue {
    #[serde(rename = "agentPreset")]
    pub agent_preset: String,
}

/// `session.prompt` request. `mode` is `queue` (a new turn) or `steer`
/// (steering input into an active turn).
#[derive(Debug, Clone, Serialize)]
pub struct SessionPromptPayload {
    #[serde(rename = "requestId")]
    pub request_id: SessionId,
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    pub mode: PromptMode,
    pub content: Vec<PromptContentPart>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "clientTimeZone")]
    pub client_time_zone: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PromptMode {
    #[serde(rename = "queue")]
    Queue,
    #[serde(rename = "steer")]
    Steer,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PromptContentPart {
    Text {
        text: String,
    },
    Image {
        #[serde(rename = "mediaType")]
        media_type: String,
        data: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

impl PromptContentPart {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}

/// `session.prompt` response value.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionPromptValue {
    pub accepted: bool,
}

/// `session.cancel` request.
#[derive(Debug, Clone, Serialize)]
pub struct SessionCancelPayload {
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
}

/// `session.cancel` response value.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionCancelValue {
    pub accepted: bool,
}

/// `session.history` request (`{ sessionId, beforeSeq?, maxMessages? }`).
#[derive(Debug, Clone, Serialize)]
pub struct SessionHistoryPayload {
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    #[serde(skip_serializing_if = "Option::is_none", rename = "beforeSeq")]
    pub before_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "maxMessages")]
    pub max_messages: Option<u32>,
}

/// `session.history` response value. `events` is the page of `HistoryEntry`s
/// (each `{ event, view? }`); `has_more` indicates an older page exists.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionHistoryValue {
    #[serde(default, rename = "records")]
    pub events: Vec<HistoryEntryRaw>,
    #[serde(default, rename = "hasMore")]
    pub has_more: bool,
    #[serde(default)]
    pub projections: Option<Value>,
}

/// One history entry, held as opaque JSON so the mapping layer can narrow it.
#[derive(Debug, Clone, Deserialize)]
pub struct HistoryEntryRaw {
    pub event: Value,
    #[serde(default)]
    pub view: Option<Value>,
}

/// `session.models` request.
#[derive(Debug, Clone, Serialize)]
pub struct SessionModelsPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionAddress {
    pub kind: &'static str,
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
}

impl SessionAddress {
    pub fn session(session_id: SessionId) -> Self {
        Self {
            kind: "session",
            session_id,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionPageRequest {
    pub address: SessionAddress,
    #[serde(rename = "throughSeq")]
    pub through_seq: i64,
    #[serde(skip_serializing_if = "Option::is_none", rename = "beforeSeq")]
    pub before_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "maxMessages")]
    pub max_messages: Option<u32>,
}

/// `session.selectModel` request.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSelectModelPayload {
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    pub provider: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "reasoningEffort")]
    pub reasoning_effort: Option<String>,
}

/// `session.list` request (`{ cursor? }`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SessionListPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// `host.describe` request (empty object).
#[derive(Debug, Clone, Default, Serialize)]
pub struct HostDescribePayload {}

/// `host.describe` response value — used for the startup probe and version pin.
#[derive(Debug, Clone, Deserialize)]
pub struct HostDescribeValue {
    pub version: String,
    pub cwd: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(rename = "attachedSessions", default)]
    pub attached_sessions: u32,
    #[serde(rename = "canOpenPath", default)]
    pub can_open_path: bool,
}

/// `session.list` response value — used for the startup probe fallback.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionListValue {
    #[serde(default)]
    pub items: Vec<Value>,
}

/// `respond` payload for an approval answer.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalResponsePayload {
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    #[serde(rename = "approvalId")]
    pub approval_id: ApprovalRequestId,
    pub outcome: ApprovalOutcomeWire,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ApprovalOutcomeWire {
    #[serde(rename = "allowed-once")]
    AllowedOnce,
    #[serde(rename = "rejected")]
    Rejected,
}

/// `respond` payload for a question answer batch.
#[derive(Debug, Clone, Serialize)]
pub struct QuestionResponsePayload {
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    pub answer: AskUserQuestionAnswerWire,
}

#[derive(Debug, Clone, Serialize)]
pub struct AskUserQuestionAnswerWire {
    pub answers: Vec<AskUserQuestionAnswerItemWire>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AskUserQuestionAnswerItemWire {
    pub id: String,
    pub selected: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom: Option<String>,
}

// ---- `commands/execute` (typert Remote surface) ----
//
// Unlike the dotted legacy methods (`session.create`, ...), typert Remote
// endpoints live at `POST /api/<namespace>/<method>` and require the payload
// to be exactly `{ "args": { ...named wire fields... } }`. The gateway
// validates the args shape against the generated descriptor and rejects
// anything else with `arguments-invalid`.

/// Wire name of the `commands/execute` attachment parameter.
///
/// dsh 0.1.5 replaced the `images` field with `submittedAttachments` (encoded
/// images plus staged file receipts). The typert gateway validates the args
/// object against its generated descriptor and reports unknown fields as
/// `gateway/arguments-invalid`, so exactly one of the two names may be sent —
/// the bridge picks by trial and remembers the answer (see
/// `HttpClient::commands_execute`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAttachmentField {
    /// dsh ≥ 0.1.5.
    SubmittedAttachments,
    /// dsh ≤ 0.1.4.
    Images,
}

impl CommandAttachmentField {
    /// The other wire name — what to retry with after an `arguments-invalid`
    /// rejection that names the attachment field.
    pub fn toggled(self) -> Self {
        match self {
            CommandAttachmentField::SubmittedAttachments => CommandAttachmentField::Images,
            CommandAttachmentField::Images => CommandAttachmentField::SubmittedAttachments,
        }
    }
}

/// `commands/execute` request payload: the descriptor's named wire fields
/// themselves. `remote_payload` wraps this into the single `{ "args": … }`
/// envelope the gateway validates — carrying an `args` field here would
/// double-wrap into `{ "args": { "args": … } }`, which the typert gateway
/// rejects with `arguments-invalid` (observed live against dsh 0.1.2-rc.1:
/// `missing "agentId", "line", "images"; unexpected "args"`).
///
/// Exactly one attachment field is serialized, chosen by
/// [`CommandsExecutePayload::new`]: dsh 0.1.5 renamed `images` to
/// `submittedAttachments` and rejects the other name as unexpected.
#[derive(Debug, Clone, Serialize)]
pub struct CommandsExecutePayload {
    /// Wire name for the descriptor's `agent` lookup parameter: the session id.
    #[serde(rename = "agentId")]
    pub agent_id: String,
    /// Full command line including the leading slash (e.g. `/compact`).
    pub line: String,
    /// Attachments for dsh ≥ 0.1.5; always empty for the commands kodex issues.
    #[serde(
        rename = "submittedAttachments",
        skip_serializing_if = "Option::is_none"
    )]
    pub submitted_attachments: Option<Vec<Value>>,
    /// Attachments for dsh ≤ 0.1.4; always empty for the commands kodex issues.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<Value>>,
}

impl CommandsExecutePayload {
    /// Build a payload whose attachment parameter uses `field`'s wire name.
    pub fn new(agent_id: &str, line: &str, field: CommandAttachmentField) -> Self {
        let (submitted_attachments, images) = match field {
            CommandAttachmentField::SubmittedAttachments => (Some(Vec::new()), None),
            CommandAttachmentField::Images => (None, Some(Vec::new())),
        };
        Self {
            agent_id: agent_id.to_string(),
            line: line.to_string(),
            submitted_attachments,
            images,
        }
    }
}

/// `commands/execute` response value: the settled `CommandExecution`, present
/// only when the line resolved to a registered command. Absent (void) means
/// unknown or malformed command.
#[derive(Debug, Clone, Deserialize)]
pub struct CommandsExecuteValue {
    #[serde(rename = "commandId")]
    pub command_id: String,
    pub result: CommandsExecuteResult,
}

/// One settled command outcome. `sourceEventSeq` rides only on success.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind")]
pub enum CommandsExecuteResult {
    #[serde(rename = "success")]
    Success {
        #[serde(default)]
        text: Option<String>,
        #[serde(rename = "sourceEventSeq", default)]
        source_event_seq: Option<u64>,
    },
    #[serde(rename = "error")]
    Error { text: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_request_round_trip() {
        let req = ClientRequest::new(
            "rpc-1".into(),
            "session.create",
            serde_json::json!({ "cwd": "/tmp" }),
        );
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["type"], "client-request");
        assert_eq!(json["rpcId"], "rpc-1");
        assert_eq!(json["method"], "session.create");
        assert_eq!(json["payload"]["cwd"], "/tmp");
    }

    #[test]
    fn server_response_ok_parse() {
        let raw = serde_json::json!({
            "type": "server-response",
            "rpcId": "rpc-1",
            "result": { "ok": true, "value": { "sessionId": "s-1" } },
        });
        let resp: ServerResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(resp.rpcId, "rpc-1");
        assert!(resp.result.is_ok());
        assert_eq!(resp.result.ok_value().unwrap()["sessionId"], "s-1");
    }

    #[test]
    fn server_response_err_parse() {
        let raw = serde_json::json!({
            "type": "server-response",
            "rpcId": "rpc-1",
            "result": {
                "ok": false,
                "error": { "code": "session-not-found", "message": "nope", "details": { "sessionId": "s-1" } }
            },
        });
        let resp: ServerResponse = serde_json::from_value(raw).unwrap();
        let err = resp.result.err().unwrap();
        assert_eq!(err.code, "session-not-found");
        assert_eq!(err.message, "nope");
    }

    #[test]
    fn server_response_void_ok_parse() {
        // Typert void business result: no `value` field at all. Must parse as
        // Ok with `Value::Null` — and must not be mistaken for an error.
        let raw = serde_json::json!({
            "type": "server-response",
            "rpcId": "rpc-1",
            "result": { "ok": true },
        });
        let resp: ServerResponse = serde_json::from_value(raw).unwrap();
        assert!(resp.result.is_ok());
        assert_eq!(resp.result.ok_value().unwrap(), &serde_json::Value::Null);
    }

    #[test]
    fn commands_execute_value_parse() {
        let raw = serde_json::json!({
            "commandId": "cmd-1",
            "result": { "kind": "success", "text": "Compacted 3 history items (~1.2k tokens)." }
        });
        let value: crate::rpc_types::CommandsExecuteValue = serde_json::from_value(raw).unwrap();
        assert_eq!(value.command_id, "cmd-1");
        match value.result {
            crate::rpc_types::CommandsExecuteResult::Success { text, .. } => {
                assert!(text.unwrap().contains("Compacted"));
            }
            other => panic!("expected success, got {other:?}"),
        }

        let raw = serde_json::json!({
            "commandId": "cmd-2",
            "result": { "kind": "error", "text": "Compaction cancelled." }
        });
        let value: crate::rpc_types::CommandsExecuteValue = serde_json::from_value(raw).unwrap();
        match value.result {
            crate::rpc_types::CommandsExecuteResult::Error { text } => {
                assert_eq!(text, "Compaction cancelled.");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn commands_execute_payload_wire_shape() {
        // The payload carries the descriptor's wire fields BARE — the single
        // `{ "args": … }` envelope is added by `remote_payload` (transport).
        // A payload with its own `args` field would double-wrap and the
        // gateway would reject it: missing "agentId", "line", "images";
        // unexpected "args" (seen live on dsh 0.1.2-rc.1).
        //
        // Exactly ONE attachment field may be present: dsh 0.1.5 renamed
        // `images` to `submittedAttachments` and rejects the other name.
        let current = crate::rpc_types::CommandsExecutePayload::new(
            "s-1",
            "/compact",
            crate::rpc_types::CommandAttachmentField::SubmittedAttachments,
        );
        let json = serde_json::to_value(&current).unwrap();
        assert_eq!(json["agentId"], "s-1");
        assert_eq!(json["line"], "/compact");
        assert_eq!(json["submittedAttachments"], serde_json::json!([]));
        assert_eq!(json.as_object().unwrap().len(), 3);
        assert!(
            json.get("images").is_none(),
            "the pre-0.1.5 field must not ride along: {json}"
        );

        // Wire shape for harnesses that still declare `images`.
        let legacy = crate::rpc_types::CommandsExecutePayload::new(
            "s-1",
            "/compact",
            crate::rpc_types::CommandAttachmentField::Images,
        );
        let json = serde_json::to_value(&legacy).unwrap();
        assert_eq!(json["images"], serde_json::json!([]));
        assert_eq!(json.as_object().unwrap().len(), 3);
        assert!(json.get("submittedAttachments").is_none());
    }

    #[test]
    fn detects_attachment_field_argument_mismatch() {
        // The live dsh 0.1.5.1-rc.1 rejection that broke /compact.
        let err = anyhow::anyhow!(
            "gateway/arguments-invalid: type=t gateway: commands/execute: args fields do not match \
             the descriptor: missing \"submittedAttachments\"; unexpected \"images\""
        );
        assert!(is_command_attachment_field_mismatch(&err));

        // The reverse direction (a 0.1.5-shaped payload against an older host).
        let err = anyhow::anyhow!(
            "gateway/arguments-invalid: type=t gateway: commands/execute: args fields do not match \
             the descriptor: missing \"images\"; unexpected \"submittedAttachments\""
        );
        assert!(is_command_attachment_field_mismatch(&err));

        // Other failures must not trigger an attachment retry.
        let err = anyhow::anyhow!("unknown-command: /compact is not registered");
        assert!(!is_command_attachment_field_mismatch(&err));
        let err = anyhow::anyhow!(
            "gateway/arguments-invalid: type=t gateway: commands/execute: args fields do not match \
             the descriptor: missing \"line\""
        );
        assert!(!is_command_attachment_field_mismatch(&err));
    }

    #[test]
    fn server_request_parse() {
        let raw = serde_json::json!({
            "type": "server-request",
            "rpcId": "rpc-2",
            "method": "session/event",
            "payload": { "type": "session/subscribed", "sessionId": "s-1", "lastSeq": 3 },
        });
        let req: ServerRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.rpcId, "rpc-2");
        assert_eq!(req.method, "session/event");
        assert_eq!(req.payload["type"], "session/subscribed");
    }

    #[test]
    fn rpc_receipt_gateway_accepted() {
        let receipt = RpcReceipt::Gateway {
            is_ok: true,
            message: String::new(),
        };
        assert!(receipt.accepted());
    }

    #[test]
    fn rpc_receipt_gateway_not_pending() {
        let receipt = RpcReceipt::Gateway {
            is_ok: false,
            message: "not-pending".to_string(),
        };
        assert!(!receipt.accepted());
    }

    #[test]
    fn server_response_with_extra_fields_tolerated() {
        // Additive schema change: a new top-level field must not break parsing.
        let raw = serde_json::json!({
            "type": "server-response",
            "rpcId": "rpc-1",
            "result": { "ok": true, "value": {} },
            "traceId": "t-9",
        });
        let resp: ServerResponse = serde_json::from_value(raw).unwrap();
        assert!(resp.result.is_ok());
    }
}
