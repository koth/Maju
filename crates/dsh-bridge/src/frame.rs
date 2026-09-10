//! Lenient frame unions: `MuxFrame`, `HostFrame`, and the embedded
//! `SessionEvent` / `ToolEventView` mirrors.
//!
//! Mirrors the harness event schema (the host `apiproxy` `events.schema.ts` /
//! `sessions.schema.ts` pair up to dsh 0.1.2, the `api-session-controller`
//! types from 0.1.5 on). Unknown variants fall back to a generic `Other` arm
//! (carrying the raw JSON) so an additive harness schema change never breaks
//! the stream — the design doc's lenient-deserialization decision.
//!
//! The `session/follow` journal frames are mirrored here too: durable entries
//! reuse [`SessionEvent`], and the transient, opt-in live-chunk frames are
//! [`AssistantStreamFrame`].

use serde::Deserialize;
use serde_json::Value;

use crate::rpc_types::{ApprovalRequestId, RpcId, SessionId};

/// One `SessionEvent` — strict envelope (`type`/`seq`/`time`) + wide `data`.
/// `ignorable` marks an event a reader may skip when it does not recognize the
/// type (the dsh merge-extensibility guard).
#[derive(Debug, Clone, Deserialize)]
pub struct SessionEvent {
    #[serde(rename = "type")]
    pub type_tag: String,
    pub seq: u64,
    pub time: f64,
    pub data: Value,
    #[serde(default, rename = "sourceEventSeqs")]
    pub source_event_seqs: Option<Vec<u64>>,
    #[serde(default, rename = "surfaceOp")]
    pub surface_op: Option<Value>,
    #[serde(default)]
    pub ignorable: Option<bool>,
}

impl SessionEvent {
    pub fn data<T: serde::de::DeserializeOwned>(&self) -> Option<T> {
        serde_json::from_value(self.data.clone()).ok()
    }
}

/// `ToolEventView` — `{ for: "call"|"result", view: { card, ... } }`. The view
/// interior is held as opaque JSON and narrowed per `card` in the mapping layer
/// (mirrors dsh's own `toolEventViewSchema` which locks only the `for`
/// discriminant + presence of `view.card`).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "for")]
pub enum ToolEventView {
    #[serde(rename = "call")]
    Call { view: ToolCallView },
    #[serde(rename = "result")]
    Result { view: ToolResultView },
}

/// `ToolCallView` — a `card`-tagged union with a fallback for unknown cards.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "card")]
pub enum ToolCallView {
    #[serde(rename = "generic")]
    Generic(GenericCallView),
    #[serde(rename = "terminal")]
    Terminal(TerminalCallView),
    #[serde(rename = "diff")]
    Diff(DiffCallView),
    #[serde(other)]
    Other,
}

impl ToolCallView {
    pub fn card(&self) -> &'static str {
        match self {
            Self::Generic(_) => "generic",
            Self::Terminal(_) => "terminal",
            Self::Diff(_) => "diff",
            Self::Other => "other",
        }
    }

    pub fn title(&self) -> Option<&str> {
        match self {
            Self::Generic(v) => Some(&v.title),
            Self::Terminal(v) => Some(&v.title),
            Self::Diff(v) => Some(&v.title),
            Self::Other => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenericCallView {
    pub title: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default, rename = "rawInput")]
    pub raw_input: Option<Value>,
    #[serde(default)]
    pub content: Option<Vec<Value>>,
    #[serde(default)]
    pub locations: Option<Vec<FileLocation>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalCallView {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiffCallView {
    pub title: String,
    pub diffs: Vec<FileDiff>,
    #[serde(default)]
    pub locations: Option<Vec<FileLocation>>,
}

/// `ToolResultView` — a `card`-tagged union with a fallback for unknown cards.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "card")]
pub enum ToolResultView {
    #[serde(rename = "generic")]
    Generic(GenericResultView),
    #[serde(rename = "terminal")]
    Terminal(TerminalResultView),
    #[serde(rename = "diff")]
    Diff(DiffResultView),
    #[serde(rename = "search")]
    Search(Value),
    #[serde(rename = "read")]
    Read(Value),
    #[serde(rename = "web")]
    Web(Value),
    #[serde(other)]
    Other,
}

impl ToolResultView {
    pub fn card(&self) -> &'static str {
        match self {
            Self::Generic(_) => "generic",
            Self::Terminal(_) => "terminal",
            Self::Diff(_) => "diff",
            Self::Search(_) => "search",
            Self::Read(_) => "read",
            Self::Web(_) => "web",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenericResultView {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub content: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalResultView {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default, rename = "exitCode")]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub signal: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiffResultView {
    #[serde(default)]
    pub title: Option<String>,
    pub diffs: Vec<FileDiff>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileDiff {
    pub path: String,
    #[serde(default, rename = "oldText")]
    pub old_text: Option<String>,
    #[serde(rename = "newText")]
    pub new_text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileLocation {
    pub path: String,
    #[serde(default)]
    pub line: Option<u32>,
}

/// `MuxFrame` union — the payload slot of an `events.mux` `ServerRequest`.
/// Unknown `type` variants fall back to `Other` carrying the raw JSON.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum MuxFrame {
    #[serde(rename = "session/event")]
    SessionEvent {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        event: SessionEvent,
        #[serde(default)]
        view: Option<ToolEventView>,
    },
    #[serde(rename = "session/subscribed")]
    SessionSubscribed {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[serde(rename = "lastSeq")]
        last_seq: i64,
    },
    #[serde(rename = "approval/requested")]
    ApprovalRequested {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[serde(rename = "approvalId")]
        approval_id: ApprovalRequestId,
        #[serde(rename = "toolName")]
        tool_name: String,
        #[serde(default, rename = "callId")]
        call_id: Option<String>,
        #[serde(default)]
        reason: Option<String>,
    },
    #[serde(rename = "approval/resolved")]
    ApprovalResolved {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[serde(rename = "approvalId")]
        approval_id: ApprovalRequestId,
        outcome: String,
    },
    #[serde(rename = "question/requested")]
    QuestionRequested {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        questions: Vec<AskUserQuestionItem>,
    },
    #[serde(rename = "question/resolved")]
    QuestionResolved {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[serde(rename = "questionRpcId")]
        question_rpc_id: RpcId,
        outcome: String,
    },
    #[serde(rename = "session/queue")]
    SessionQueue {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[serde(default)]
        items: Vec<Value>,
    },
    #[serde(rename = "session/jobs")]
    SessionJobs {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[serde(default)]
        jobs: Vec<Value>,
    },
    #[serde(rename = "session/projection")]
    SessionProjection {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        key: String,
        value: Value,
        seq: u64,
    },
    #[serde(rename = "stream/error")]
    StreamError { error: crate::rpc_types::RpcError },
    #[serde(other)]
    Other,
}

impl MuxFrame {
    pub fn session_id(&self) -> Option<&SessionId> {
        match self {
            MuxFrame::SessionEvent { session_id, .. }
            | MuxFrame::SessionSubscribed { session_id, .. }
            | MuxFrame::ApprovalRequested { session_id, .. }
            | MuxFrame::ApprovalResolved { session_id, .. }
            | MuxFrame::QuestionRequested { session_id, .. }
            | MuxFrame::QuestionResolved { session_id, .. }
            | MuxFrame::SessionQueue { session_id, .. }
            | MuxFrame::SessionJobs { session_id, .. }
            | MuxFrame::SessionProjection { session_id, .. } => Some(session_id),
            MuxFrame::StreamError { .. } | MuxFrame::Other => None,
        }
    }
}

/// One `session/control` frame (the dsh 0.1.5+ `SessionControlFrame` union).
///
/// dsh 0.1.5 moved the host-wide session control channel — per-session queues,
/// jobs, and **projection updates** — onto its own `session/control` logical
/// stream, opened over the same `/api/remote.mux` WebSocket. The `$events` mux
/// now carries only forwarded Remote events (`{type:"emit"}`).
///
/// Projections are where the live token figures live: `contextPressure`
/// (context occupancy, and the only thing that reacts to a `/compact`) and
/// `tokenUsage` (durable cumulative usage). A bridge that follows only
/// `session/follow` sees them once, in that stream's opening baseline, so the
/// usage dock freezes at the value from session load — the "usage never
/// updates, not even after compaction" regression.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ControlFrame {
    /// Opening snapshot: every session's queues, jobs, and projection values.
    #[serde(rename = "baseline")]
    Baseline { value: Value },
    /// One advanced projection value for one session.
    #[serde(rename = "projection")]
    Projection {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        key: String,
        value: Value,
    },
    #[serde(rename = "queue")]
    Queue {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[serde(default)]
        items: Vec<Value>,
    },
    #[serde(rename = "jobs")]
    Jobs {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[serde(default)]
        jobs: Vec<Value>,
    },
    #[serde(other)]
    Other,
}

/// `HostFrame` union — the payload slot of an `events.host` `ServerRequest`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum HostFrame {
    #[serde(rename = "host/session-added")]
    HostSessionAdded {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[serde(default)]
        blank: bool,
        #[serde(default, rename = "parentSessionId")]
        parent_session_id: Option<SessionId>,
        #[serde(default)]
        origin: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default, rename = "agentPreset")]
        agent_preset: Option<String>,
    },
    #[serde(rename = "host/session-removed")]
    HostSessionRemoved {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
    },
    #[serde(rename = "host/session-status")]
    HostSessionStatus {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        running: bool,
    },
    #[serde(rename = "host/agent-error")]
    HostAgentError {
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        message: String,
    },
    #[serde(rename = "host/workspace-changed")]
    HostWorkspaceChanged { workspace: Value },
    #[serde(rename = "host/workspace-removed")]
    HostWorkspaceRemoved { workspace_id: String },
    #[serde(rename = "host/workspace-order-changed")]
    HostWorkspaceOrderChanged { workspace_ids: Vec<String> },
    #[serde(rename = "host/archived-sessions-changed")]
    HostArchivedSessionsChanged {
        archived_session_ids: Vec<SessionId>,
    },
    #[serde(rename = "host/remote-event")]
    HostRemoteEvent { event: String, args: Vec<Value> },
    #[serde(rename = "stream/error")]
    StreamError { error: crate::rpc_types::RpcError },
    #[serde(other)]
    Other,
}

impl HostFrame {
    pub fn session_id(&self) -> Option<&SessionId> {
        match self {
            HostFrame::HostSessionAdded { session_id, .. }
            | HostFrame::HostSessionRemoved { session_id }
            | HostFrame::HostSessionStatus { session_id, .. }
            | HostFrame::HostAgentError { session_id, .. } => Some(session_id),
            HostFrame::HostWorkspaceChanged { .. }
            | HostFrame::HostWorkspaceRemoved { .. }
            | HostFrame::HostWorkspaceOrderChanged { .. }
            | HostFrame::HostArchivedSessionsChanged { .. }
            | HostFrame::HostRemoteEvent { .. }
            | HostFrame::StreamError { .. }
            | HostFrame::Other => None,
        }
    }
}

/// One user-question item (the dsh `AskUserQuestionItem`).
#[derive(Debug, Clone, Deserialize)]
pub struct AskUserQuestionItem {
    pub id: String,
    pub question: String,
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub options: Option<Vec<AskUserQuestionOption>>,
    #[serde(default, rename = "multiSelect")]
    pub multi_select: Option<bool>,
    #[serde(default)]
    pub intent: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AskUserQuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// `TodoItem` from a `todo/write` event.
#[derive(Debug, Clone, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: String,
}

/// `turn/end` reason — `{ kind, ... }`. The rest is held as opaque JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct TurnEndReason {
    pub kind: String,
    #[serde(flatten)]
    pub rest: Value,
}

/// `assistant/chunk` data — `{ turn, step, chunk: StreamChunk }`.
#[derive(Debug, Clone, Deserialize)]
pub struct AssistantChunkData {
    #[serde(default)]
    pub turn: u64,
    #[serde(default)]
    pub step: u64,
    pub chunk: StreamChunk,
}

/// `StreamChunk` union (text-delta / reasoning-delta / tool-call-delta / ...).
/// Only the consumed variants are typed; the rest fall through to `Other`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum StreamChunk {
    #[serde(rename = "text-delta")]
    TextDelta { index: u64, text: String },
    #[serde(rename = "reasoning-delta")]
    ReasoningDelta { index: u64, text: String },
    #[serde(rename = "tool-call-delta")]
    ToolCallDelta {
        index: u64,
        id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(rename = "argumentsDelta")]
        arguments_delta: String,
    },
    #[serde(rename = "usage")]
    Usage { usage: TokenUsage },
    #[serde(rename = "finish")]
    Finish { reason: Value },
    #[serde(other)]
    Other,
}

/// One `assistant-stream` item of a `session/follow` stream — the 0.1.5+
/// carrier for live model output.
///
/// dsh 0.1.5 stopped appending the durable `assistant/chunk` session event and
/// moved token streaming to this process-local, opt-in presentation channel: a
/// follower must ask for it with `assistantStream: true` and fold the dense
/// frames itself. Each frame is transient — it carries an `attemptId` and a
/// dense `index`, never a durable `seq`.
#[derive(Debug, Clone, Deserialize)]
pub struct AssistantStreamItem {
    pub frame: AssistantStreamFrame,
}

/// One dense live frame of the active model attempt (`SessionAssistantStreamFrame`).
///
/// Only `start` carries `turn`/`step`; chunks are matched to their attempt by
/// `attemptId`, so a follower that attaches mid-attempt needs the snapshot
/// baseline to resolve them.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum AssistantStreamFrame {
    #[serde(rename = "start")]
    Start {
        #[serde(rename = "attemptId")]
        attempt_id: String,
        #[serde(default)]
        turn: u64,
        #[serde(default)]
        step: u64,
    },
    #[serde(rename = "chunk")]
    Chunk {
        #[serde(rename = "attemptId")]
        attempt_id: String,
        chunk: StreamChunk,
    },
    /// Terminal marker; `outcome.kind` is `committed` (with the settlement
    /// `seq`) or `abandoned`.
    #[serde(rename = "end")]
    End {
        #[serde(rename = "attemptId")]
        attempt_id: String,
        #[serde(default)]
        outcome: Option<Value>,
    },
    #[serde(other)]
    Other,
}

/// `snapshot.assistantStream` — present only when the follow request opted in.
/// A follower attaching mid-attempt resolves later `chunk` frames through
/// `activeAttempt`.
#[derive(Debug, Clone, Deserialize)]
pub struct AssistantStreamBaseline {
    #[serde(default, rename = "activeAttempt")]
    pub active_attempt: Option<AssistantStreamAttempt>,
}

/// The attempt already streaming when a follow generation opened.
#[derive(Debug, Clone, Deserialize)]
pub struct AssistantStreamAttempt {
    #[serde(rename = "attemptId")]
    pub attempt_id: String,
    #[serde(default)]
    pub turn: u64,
    #[serde(default)]
    pub step: u64,
}

/// `assistant/message` data — `{ turn, step, message, usage? }`.
#[derive(Debug, Clone, Deserialize)]
pub struct AssistantMessageData {
    #[serde(default)]
    pub turn: u64,
    #[serde(default)]
    pub step: u64,
    pub message: AssistantMessage,
    #[serde(default)]
    pub usage: Option<TokenUsage>,
}

/// `AssistantMessage` — `{ id, role, content: [ContentBlock], source }`. Only
/// text/reasoning blocks are narrowed; the rest stay opaque.
#[derive(Debug, Clone, Deserialize)]
pub struct AssistantMessage {
    #[serde(default)]
    pub id: Option<String>,
    pub role: String,
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub source: Option<Value>,
}

/// `user/message` data — a user-role message on the shared message shape
/// (`{ id, role: 'user', content: [ContentBlock] }`; no turn/step wrapper).
#[derive(Debug, Clone, Deserialize)]
pub struct UserMessageData {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub role: String,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "reasoning")]
    Reasoning { text: String },
    #[serde(rename = "tool-call")]
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    #[serde(rename = "tool-result")]
    ToolResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        content: Vec<ContentBlock>,
        #[serde(default)]
        #[serde(rename = "isError")]
        is_error: Option<bool>,
    },
    #[serde(other)]
    Other,
}

/// `tool/call` data — `{ turn, step, callId, name, arguments }`.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallData {
    #[serde(default)]
    pub turn: u64,
    #[serde(default)]
    pub step: u64,
    #[serde(rename = "callId")]
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

/// `tool/result` data — `{ turn, step, message, error?, meta? }`.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolResultData {
    #[serde(default)]
    pub turn: u64,
    #[serde(default)]
    pub step: u64,
    pub message: ToolResultMessage,
    #[serde(default)]
    pub error: Option<ToolResultError>,
    #[serde(default)]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolResultMessage {
    pub role: String,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolResultError {
    pub name: String,
    pub code: String,
}

/// `TokenUsage` — `{ inputTokens, outputTokens, cacheReadTokens?, ... }`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TokenUsage {
    #[serde(rename = "inputTokens", default)]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens", default)]
    pub output_tokens: u64,
    #[serde(rename = "cacheReadTokens", default)]
    pub cache_read_tokens: Option<u64>,
    #[serde(rename = "cacheWriteTokens", default)]
    pub cache_write_tokens: Option<u64>,
    #[serde(rename = "reasoningTokens", default)]
    pub reasoning_tokens: Option<u64>,
}

/// `request/header` data — `{ header, reason }`. Held as opaque JSON (the
/// mapping layer extracts only what `SessionConfigUpdated` needs).
#[derive(Debug, Clone, Deserialize)]
pub struct RequestHeaderData {
    pub header: Value,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mux_frame_session_event_parse() {
        let raw = serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "assistant/chunk",
                "seq": 5,
                "time": 1700000000.0,
                "data": { "turn": 1, "step": 1, "chunk": { "type": "text-delta", "index": 0, "text": "hi" } }
            }
        });
        let frame: MuxFrame = serde_json::from_value(raw).unwrap();
        match frame {
            MuxFrame::SessionEvent { event, .. } => {
                assert_eq!(event.type_tag, "assistant/chunk");
                let data: AssistantChunkData = event.data().unwrap();
                assert!(matches!(data.chunk, StreamChunk::TextDelta { text, .. } if text == "hi"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn mux_frame_unknown_type_falls_back() {
        let raw = serde_json::json!({ "type": "session/future-event", "sessionId": "s-1" });
        let frame: MuxFrame = serde_json::from_value(raw).unwrap();
        assert!(matches!(frame, MuxFrame::Other));
    }

    #[test]
    fn control_frame_projection_parse() {
        // The 0.1.5 session control stream's live projection tick — the shape
        // the usage dock depends on for context occupancy and token totals.
        let raw = serde_json::json!({
            "type": "projection",
            "sessionId": "s-1",
            "key": "contextPressure",
            "value": { "pressureTokens": 12000, "projectedTokens": 9000, "contextWindow": 200000 },
            "seq": 42
        });
        let frame: ControlFrame = serde_json::from_value(raw).unwrap();
        match &frame {
            ControlFrame::Projection {
                session_id,
                key,
                value,
            } => {
                assert_eq!(session_id, "s-1");
                assert_eq!(key, "contextPressure");
                assert_eq!(value["projectedTokens"], 9000);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn control_frame_baseline_and_unknown_parse() {
        let raw = serde_json::json!({
            "type": "baseline",
            "value": {
                "queues": {},
                "jobs": {},
                "projections": {
                    "s-1": { "asOfSeq": 3, "values": { "title": "hello" } }
                }
            }
        });
        let frame: ControlFrame = serde_json::from_value(raw).unwrap();
        match frame {
            ControlFrame::Baseline { value } => {
                assert_eq!(value["projections"]["s-1"]["values"]["title"], "hello");
            }
            other => panic!("wrong variant: {other:?}"),
        }

        // Additive frame kinds must degrade to `Other`, never break the loop.
        let raw = serde_json::json!({ "type": "future-control-frame" });
        let frame: ControlFrame = serde_json::from_value(raw).unwrap();
        assert!(matches!(frame, ControlFrame::Other));
    }

    #[test]
    fn tool_call_view_unknown_card_falls_back() {
        let raw =
            serde_json::json!({ "for": "call", "view": { "card": "future-card", "title": "x" } });
        let view: ToolEventView = serde_json::from_value(raw).unwrap();
        match view {
            ToolEventView::Call { view } => assert!(matches!(view, ToolCallView::Other)),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn tool_call_view_terminal_parse() {
        let raw = serde_json::json!({ "for": "call", "view": { "card": "terminal", "title": "ls", "cwd": "/tmp" } });
        let view: ToolEventView = serde_json::from_value(raw).unwrap();
        match view {
            ToolEventView::Call { view } => assert!(matches!(view, ToolCallView::Terminal(_))),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn tool_result_view_diff_parse() {
        let raw = serde_json::json!({
            "for": "result",
            "view": { "card": "diff", "diffs": [{ "path": "a.txt", "oldText": null, "newText": "hi" }] }
        });
        let view: ToolEventView = serde_json::from_value(raw).unwrap();
        match view {
            ToolEventView::Result { view } => match view {
                ToolResultView::Diff(d) => assert_eq!(d.diffs.len(), 1),
                _ => panic!("wrong result variant"),
            },
            _ => panic!("wrong for"),
        }
    }

    #[test]
    fn host_frame_session_status_parse() {
        let raw = serde_json::json!({ "type": "host/session-status", "sessionId": "s-1", "running": true });
        let frame: HostFrame = serde_json::from_value(raw).unwrap();
        match frame {
            HostFrame::HostSessionStatus { running, .. } => assert!(running),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn session_event_ignorable_default_none() {
        let raw = serde_json::json!({
            "type": "turn/start", "seq": 1, "time": 0.0, "data": { "turn": 1 }
        });
        let event: SessionEvent = serde_json::from_value(raw).unwrap();
        assert_eq!(event.ignorable, None);
    }

    #[test]
    fn approval_requested_parse() {
        let raw = serde_json::json!({
            "type": "approval/requested",
            "sessionId": "s-1",
            "approvalId": "a-1",
            "toolName": "bash",
            "callId": "c-1",
            "reason": "shell"
        });
        let frame: MuxFrame = serde_json::from_value(raw).unwrap();
        match frame {
            MuxFrame::ApprovalRequested { tool_name, .. } => assert_eq!(tool_name, "bash"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn question_requested_parse() {
        let raw = serde_json::json!({
            "type": "question/requested",
            "sessionId": "s-1",
            "questions": [{ "id": "q1", "question": "ok?" }]
        });
        let frame: MuxFrame = serde_json::from_value(raw).unwrap();
        match frame {
            MuxFrame::QuestionRequested { questions, .. } => assert_eq!(questions.len(), 1),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn assistant_stream_start_carries_turn_step() {
        let raw = serde_json::json!({
            "type": "assistant-stream",
            "frame": {
                "type": "start",
                "attemptId": "attempt-1",
                "revision": 2,
                "startedAfterSeq": 7,
                "turn": 1,
                "step": 2
            }
        });
        let item: AssistantStreamItem = serde_json::from_value(raw).unwrap();
        match item.frame {
            AssistantStreamFrame::Start {
                attempt_id,
                turn,
                step,
            } => {
                assert_eq!(attempt_id, "attempt-1");
                assert_eq!((turn, step), (1, 2));
            }
            _ => panic!("wrong frame variant"),
        }
    }

    /// The chunk frame carries no `turn`/`step` (they belong to the attempt's
    /// `start` frame), so the bridge must resolve them through the attempt id
    /// registered from `start` or the snapshot baseline.
    #[test]
    fn assistant_stream_chunk_is_sparse() {
        let raw = serde_json::json!({
            "type": "assistant-stream",
            "frame": {
                "type": "chunk",
                "attemptId": "attempt-1",
                "revision": 2,
                "index": 0,
                "time": 1700000000.0,
                "chunk": { "type": "reasoning-delta", "index": 0, "text": "hmm" }
            }
        });
        let item: AssistantStreamItem = serde_json::from_value(raw).unwrap();
        match item.frame {
            AssistantStreamFrame::Chunk { attempt_id, chunk } => {
                assert_eq!(attempt_id, "attempt-1");
                assert!(
                    matches!(chunk, StreamChunk::ReasoningDelta { ref text, .. } if text == "hmm")
                );
            }
            _ => panic!("wrong frame variant"),
        }
    }

    #[test]
    fn assistant_stream_end_outcome_parse() {
        let raw = serde_json::json!({
            "type": "assistant-stream",
            "frame": {
                "type": "end",
                "attemptId": "attempt-1",
                "revision": 2,
                "index": 5,
                "outcome": { "kind": "committed", "eventType": "assistant/message", "seq": 9 }
            }
        });
        let item: AssistantStreamItem = serde_json::from_value(raw).unwrap();
        match item.frame {
            AssistantStreamFrame::End {
                attempt_id,
                outcome,
            } => {
                assert_eq!(attempt_id, "attempt-1");
                assert_eq!(outcome.unwrap()["kind"], "committed");
            }
            _ => panic!("wrong frame variant"),
        }
    }

    #[test]
    fn assistant_stream_baseline_parse() {
        let raw = serde_json::json!({
            "revision": 3,
            "activeAttempt": {
                "attemptId": "attempt-2",
                "startedAfterSeq": 4,
                "turn": 2,
                "step": 1,
                "nextIndex": 5,
                "stream": []
            }
        });
        let baseline: AssistantStreamBaseline = serde_json::from_value(raw).unwrap();
        let attempt = baseline.active_attempt.unwrap();
        assert_eq!(attempt.attempt_id, "attempt-2");
        assert_eq!((attempt.turn, attempt.step), (2, 1));
    }

    #[test]
    fn assistant_stream_unknown_frame_falls_back() {
        let raw = serde_json::json!({
            "type": "assistant-stream",
            "frame": { "type": "future-frame", "attemptId": "attempt-1" }
        });
        let item: AssistantStreamItem = serde_json::from_value(raw).unwrap();
        assert!(matches!(item.frame, AssistantStreamFrame::Other));
    }
}
