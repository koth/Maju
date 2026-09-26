//! Map dsh `MuxFrame`/`HostFrame` variants into Kodex [`ClientEvent`]s.
//!
//! The mapping layer preserves the fidelity the dsh web UI receives: assistant
//! text and reasoning chunks, tool calls/results with render intent
//! (`ToolEventView`), plans (`todo/write`), turn endings, session config, and
//! approvals/questions. Unrepresentable view data is serialized into
//! `raw_output` JSON (recoverable, not dropped). Per the design doc, no raw
//! harness types leak to the frontend — translation stops at `ClientEvent`.

use acp_core::ClientEvent;
use serde_json::Value;
use uuid::Uuid;
use workspace_model::{
    AgentPlanEntry, AgentPlanEntryPriority, AgentPlanEntryStatus, DiffHunk, DiffLine, DiffLineKind,
    MessageRole, PermissionInputOption, PermissionInputQuestion, PermissionInputRequest,
    PermissionOption, TerminalOutput, UsageEvent, UsageEventScope, UsageTokenBreakdown,
};

use crate::frame::{
    AssistantChunkData, AssistantMessageData, ContentBlock, HostFrame, MuxFrame, SessionEvent,
    StreamChunk, TodoItem, TokenUsage, ToolCallData, ToolCallView, ToolEventView, ToolResultData,
    ToolResultView, TurnEndReason, UserMessageData,
};
use crate::host::{PendingApprovalKind, SessionSink};
use crate::mojibake::{self, StreamTextKind};

/// Outcome of mapping one frame: zero or more [`ClientEvent`]s to emit.
#[derive(Default)]
pub struct MappedEvents {
    pub events: Vec<ClientEvent>,
}

impl MappedEvents {
    fn single(event: ClientEvent) -> Self {
        Self {
            events: vec![event],
        }
    }
    fn many(events: Vec<ClientEvent>) -> Self {
        Self { events }
    }
}

/// Serialize a JSON value to a compact string, keeping non-ASCII characters
/// literal (e.g. CJK text in tool `raw_input`/`raw_output`) instead of
/// rendering as `\u4f60\u597d` escape sequences that look like internal codes
/// in the UI. `serde_json::to_string` only emits the JSON-mandated escapes
/// (`"`, `\`, and control chars below 0x20); code points above 0x7F are
/// written as raw UTF-8, which is valid inside a JSON string (RFC 8259).
fn json_compact_no_escape(v: &serde_json::Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// Map a `MuxFrame` (already demuxed to the owning session by the router) into
/// [`ClientEvent`]s. `seq` of the embedded `SessionEvent` updates the sink's
/// `last_seq` so SSE reconnection can re-baseline from the exact gap.
///
/// `sink` is taken by `&SessionSink` so the mapping layer can record pending
/// approval/question ids for the bridge's respond path; it does **not** send
/// events to the sink (the caller does, after mapping).
pub fn map_mux_frame(frame: &MuxFrame, sink: &SessionSink) -> MappedEvents {
    match frame {
        MuxFrame::SessionEvent { event, view, .. } => {
            let mut events = map_session_event(event, view.as_ref(), sink);
            // Advance last_seq after mapping so re-baseline resumes from the gap.
            sink.last_seq
                .store(event.seq, std::sync::atomic::Ordering::Release);
            MappedEvents::many(events.drain(..).collect())
        }
        MuxFrame::SessionSubscribed { last_seq, .. } => {
            // Seed last_seq from the subscription baseline (lastSeq = last
            // delivered seq; the next event is lastSeq + 1).
            //
            // Monotonic: a fresh subscription reports 0, and a plain `store`
            // would CLOBBER a cursor the sink already holds. After a history
            // replay the sink sits at the session's log cut, so lowering it to
            // 0 let the follow re-deliver (and re-apply) the whole journal —
            // that flood is what rebuilt a fork child's transcript as tool
            // calls only, since the live mapping drops assistant text and user
            // prompts by design.
            sink.last_seq.fetch_max(
                (*last_seq).max(0) as u64,
                std::sync::atomic::Ordering::AcqRel,
            );
            MappedEvents::default()
        }
        MuxFrame::ApprovalRequested {
            approval_id,
            tool_name,
            reason,
            ..
        } => {
            sink.record_pending_approval(approval_id.clone(), PendingApprovalKind::Approval);
            MappedEvents::single(ClientEvent::ToolPermissionRequest {
                id: approval_id.clone(),
                name: tool_name.clone(),
                options: approval_options(),
                details: reason.clone(),
                input: None,
            })
        }
        MuxFrame::ApprovalResolved {
            approval_id,
            outcome,
            ..
        } => {
            sink.clear_pending_approval(approval_id);
            MappedEvents::single(ClientEvent::ToolPermissionResolved {
                id: approval_id.clone(),
                outcome: outcome.clone(),
            })
        }
        MuxFrame::QuestionRequested { questions, .. } => {
            // The bridge answers one ask() as a batch via the question's rpcId.
            // Use the first question's id as the request id surfaced to the UI;
            // the full batch is stored in the sink for the respond path.
            let request_id = questions
                .first()
                .map(|q| q.id.clone())
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            sink.record_pending_question(request_id.clone());
            sink.record_question_order(
                request_id.clone(),
                questions.iter().map(|q| q.id.clone()).collect(),
            );
            let input = PermissionInputRequest {
                questions: questions
                    .iter()
                    .map(|q| PermissionInputQuestion {
                        id: q.id.clone(),
                        header: q.header.clone().unwrap_or_default(),
                        question: q.question.clone(),
                        is_other: false,
                        is_secret: false,
                        multi_select: q.multi_select.unwrap_or(false),
                        options: q
                            .options
                            .iter()
                            .flatten()
                            .map(|o| PermissionInputOption {
                                label: o.label.clone(),
                                description: o.description.clone().unwrap_or_default(),
                            })
                            .collect(),
                    })
                    .collect(),
            };
            MappedEvents::single(ClientEvent::ToolPermissionRequest {
                id: request_id,
                name: "user_question".to_string(),
                options: question_options(),
                details: questions
                    .first()
                    .and_then(|q| q.detail.clone())
                    .or_else(|| questions.first().map(|q| q.question.clone())),
                input: Some(input),
            })
        }
        MuxFrame::QuestionResolved {
            question_rpc_id,
            outcome,
            ..
        } => MappedEvents::single(ClientEvent::ToolPermissionResolved {
            id: question_rpc_id.clone(),
            outcome: outcome.clone(),
        }),
        MuxFrame::SessionProjection { key, value, .. } => {
            if key == "agentPreset" {
                if let Some(preset) = value.as_str().filter(|preset| !preset.is_empty()) {
                    return MappedEvents::single(ClientEvent::SessionConfigValueChanged {
                        control_id: "agent_preset".to_string(),
                        value_id: preset.to_string(),
                        value_label: None,
                    });
                }
                return MappedEvents::default();
            }
            if key == "title" {
                if let Some(title) = value.as_str() {
                    return MappedEvents::single(ClientEvent::SessionTitleUpdated {
                        title: title.to_string(),
                    });
                }
                return MappedEvents::default();
            }
            if key == "modelSelection" {
                let selection = value
                    .get("next")
                    .or_else(|| value.get("lastUsed"))
                    .filter(|selection| selection.is_object());
                if let Some(selection) = selection
                    && let Some(event) = model_selection_config_event(selection)
                {
                    return MappedEvents::single(event);
                }
                return MappedEvents::default();
            }
            // dsh token-meter projections (see @deepseek-ai/dsh-token-meter):
            //   contextPressure — { pressureTokens?, projectedTokens?, contextWindow? }
            //     the harness's real context occupancy; `projectedTokens` already
            //     reacts to compaction immediately, so feeding it here makes the
            //     UI context bar track the harness instead of the cumulative-token
            //     estimate in the reducer.
            //   tokenUsage — { uncachedInputTokens, outputTokens, cacheReadTokens,
            //     cacheWriteTokens } — durable cumulative provider usage for the
            //     whole session, replacing the per-turn-delta estimate.
            if key == "contextPressure" {
                if let Some(event) = context_pressure_usage_event(&value) {
                    return MappedEvents::single(event);
                }
                return MappedEvents::default();
            }
            if key == "tokenUsage" {
                if let Some(event) = token_usage_projection_event(&value) {
                    return MappedEvents::single(event);
                }
                return MappedEvents::default();
            }
            MappedEvents::default()
        }
        MuxFrame::StreamError { error } => {
            tracing::warn!(target: "dsh-bridge::mapping", error = %error, "mux stream error frame");
            MappedEvents::single(ClientEvent::Interrupted {
                reason: format!("harness stream error: {error}"),
            })
        }
        // `session/jobs` carries the session's background-job snapshots (后台
        //任务): recorded into `dsh_bridge::jobs` for the context dock. They
        // are session-level state, not turn events — no ClientEvent.
        MuxFrame::SessionJobs { session_id, jobs } => {
            crate::jobs::record_session_jobs(&session_id, &jobs);
            MappedEvents::default()
        }
        // session/queue and unknown frames are not represented in ClientEvent
        // in v1; ignore (debug-logged by the router).
        MuxFrame::SessionQueue { .. } | MuxFrame::Other => MappedEvents::default(),
    }
}

/// Map a projection baseline's `values` map (from `session/page` or the
/// `session/follow` opening snapshot) into client events.
///
/// dsh 0.1.2 does not carry `agentPreset` on the resumed `session.create`
/// response; it restores it through a projection baseline. Without mapping the
/// whole baseline, kodex cannot distinguish a compaction-capable standard
/// session from a minimal one.
pub fn map_projection_values(values: &Value) -> Vec<ClientEvent> {
    let Some(values) = values.as_object() else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(|(key, value)| {
            let frame = MuxFrame::SessionProjection {
                session_id: String::new(),
                key: key.clone(),
                value: value.clone(),
                seq: 0,
            };
            let mapped = map_mux_frame(&frame, &SessionSink::new_for_projection_mapping());
            if mapped.events.is_empty() {
                None
            } else {
                Some(mapped.events)
            }
        })
        .flatten()
        .collect()
}

/// Convert a dsh `modelSelection` projection into a provider-qualified model
/// config change. `next` is authoritative for a blank session; `lastUsed` is
/// the restored selection for an existing session.
pub fn model_selection_config_event(selection: &serde_json::Value) -> Option<ClientEvent> {
    // dsh stores the selection as `{ lastUsed?: {provider, model}, next?: {provider, model} }`.
    // Prefer the active `next`, fall back to `lastUsed`.
    let active = selection
        .get("next")
        .or_else(|| selection.get("lastUsed"))
        .filter(|candidate| candidate.is_object())
        .unwrap_or(selection);
    // `next`/`lastUsed` may be `null` when the session has never explicitly
    // selected a model — the projection carries no durable choice. In that
    // case the harness falls back to the deployment default; the caller must
    // resolve it from the model catalog.
    if active.is_null() {
        return None;
    }
    let provider = active.get("provider")?.as_str()?.trim();
    let model = active.get("model")?.as_str()?.trim();
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some(ClientEvent::SessionConfigValueChanged {
        control_id: "model".to_string(),
        value_id: format!("kodex-provider/{provider}/{model}"),
        value_label: Some(model.to_string()),
    })
}

/// Map a `HostFrame` (demuxed by `sessionId` where present).
pub fn map_host_frame(frame: &HostFrame) -> MappedEvents {
    match frame {
        HostFrame::HostAgentError { message, .. } => {
            MappedEvents::single(ClientEvent::Interrupted {
                reason: format!("harness agent error: {message}"),
            })
        }
        HostFrame::HostSessionStatus { running: false, .. } => {
            // A session that stopped running without a turn/end (host-side
            // failure) surfaces as Interrupted so the UI can react. The live
            // `api-session/status` path decides this itself after
            // `TURN_END_SETTLE_GRACE` (see `host.rs`), because the idle
            // transition races the durable `turn/end` and this immediate
            // mapping used to interrupt every completed reply; this arm stays
            // for the legacy `events.host` frame path.
            MappedEvents::single(ClientEvent::Interrupted {
                reason: "harness session stopped".to_string(),
            })
        }
        HostFrame::StreamError { error } => {
            tracing::warn!(target: "dsh-bridge::mapping", error = %error, "host stream error frame");
            MappedEvents::single(ClientEvent::Interrupted {
                reason: format!("harness host stream error: {error}"),
            })
        }
        // session-added/removed, workspace-*, archived-*, remote-event are not
        // represented in v1 (host-global frames are ignored or broadcast per
        // the design doc's open question).
        _ => MappedEvents::default(),
    }
}

/// Map one model-stream chunk of an assistant step into [`ClientEvent`]s.
///
/// Shared by the two carriers the harness has used for live model output: the
/// durable `assistant/chunk` session event (dsh ≤ 0.1.4) and the transient
/// `assistant-stream` follow frames (dsh 0.1.5+, requested with
/// `assistantStream: true`). Both must behave identically — the text-delta arm
/// also marks the step as already streamed, which is what keeps the finalized
/// `assistant/message` from repeating the reply.
pub fn map_assistant_chunk(
    sink: &SessionSink,
    turn: u64,
    step: u64,
    chunk: StreamChunk,
) -> Vec<ClientEvent> {
    match chunk {
        StreamChunk::TextDelta { index, text } => {
            // History pages contain raw chunk deltas alongside the finalized
            // `assistant/message`, so remember when this sink already emitted
            // the step's text and let the finalized block skip it.
            sink.mark_text_seen(turn, step);
            // Mojibake trace: fingerprint the raw (pre-repair) text as it
            // enters the bridge, to compare against the raw WS frame.
            let mojibake_hits = mojibake::signature_hits(&text);
            let fffd = text.matches('\u{FFFD}').count();
            if fffd > 0 || mojibake_hits > 0 {
                tracing::debug!(
                    target: "dsh-bridge::mapping",
                    bytes = text.len(),
                    fffd,
                    signature_hits = mojibake_hits,
                    "assistant chunk mapped with mojibake markers"
                );
            }
            // Repair Latin-1 double-encoded corruption (observed from some
            // upstreams) across delta boundaries; may legitimately return an
            // empty string while a sequence accumulates.
            let repaired = sink.repair_stream_text(turn, step, index, StreamTextKind::Text, &text);
            if repaired.is_empty() {
                Vec::new()
            } else {
                // Close the current thinking segment before the reply text: dsh
                // never emits a reasoning-end signal, so without this the
                // reducer's thinking buffer keeps accumulating every model
                // call's reasoning for the WHOLE turn (multi-MB on long turns)
                // instead of one block per reasoning burst.
                vec![
                    ClientEvent::ThinkingActivity { active: false },
                    ClientEvent::MessageChunk {
                        role: MessageRole::Assistant,
                        content: repaired,
                    },
                ]
            }
        }
        StreamChunk::ReasoningDelta { index, text } => {
            let repaired =
                sink.repair_stream_text(turn, step, index, StreamTextKind::Reasoning, &text);
            let mut out = vec![ClientEvent::ThinkingActivity { active: true }];
            if !repaired.is_empty() {
                out.push(ClientEvent::ThinkingChunk { text: repaired });
            }
            out
        }
        StreamChunk::Usage { usage } => {
            // One model call surfaces the same `TokenUsage` twice (the terminal
            // `usage` chunk and the finalized message rollup) and history
            // replay re-delivers both; emit exactly one TurnDelta per call via
            // the sink's step claim.
            if sink.claim_usage_emission(turn, step) {
                vec![usage_event(&usage)]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

/// Map a `SessionEvent` (+ optional `ToolEventView`) into [`ClientEvent`]s.
pub fn map_session_event(
    event: &SessionEvent,
    view: Option<&ToolEventView>,
    sink: &SessionSink,
) -> Vec<ClientEvent> {
    // History replay rebuilds the transcript from SQLite; a replayed
    // `tool/call` must not re-emit `ToolStarted`, otherwise app-core
    // `persist_event` would overwrite the persisted row back to Running.
    // Record the call args so a later replayed `tool/result` can still
    // synthesize the diff preview.
    if sink.is_replaying() && event.type_tag == "tool/call" {
        let data: Option<ToolCallData> = event.data();
        if let Some(d) = data
            && let Ok(value) = serde_json::from_str::<Value>(&d.arguments)
        {
            sink.record_tool_call(d.call_id, d.name, value);
        }
        return Vec::new();
    }
    match event.type_tag.as_str() {
        "assistant/chunk" => {
            let data: Option<AssistantChunkData> = event.data();
            data.map(|d| map_assistant_chunk(sink, d.turn, d.step, d.chunk))
                .unwrap_or_default()
        }
        "assistant/message" => {
            // Live path: the assistant text was already streamed via
            // `assistant/chunk` text-deltas, so re-emitting the finalized
            // message's text blocks would duplicate every paragraph — consume
            // only the usage rollup. History-replay path: dsh history pages
            // include the raw chunk deltas too, so only emit finalized text for
            // steps whose text has not already streamed through this sink.
            let data: Option<AssistantMessageData> = event.data();
            let mut out = Vec::new();
            if let Some(data) = data {
                // Flush any mojibake-repair tails held for this step's block
                // streams (a corrupted stream can end mid-sequence or below
                // the pre-engagement threshold).
                for (kind, tail) in sink.flush_stream_repairs(data.turn, data.step) {
                    match kind {
                        StreamTextKind::Text => out.push(ClientEvent::MessageChunk {
                            role: MessageRole::Assistant,
                            content: tail,
                        }),
                        StreamTextKind::Reasoning => {
                            out.push(ClientEvent::ThinkingChunk { text: tail })
                        }
                    }
                }
                if sink.is_replaying() && !sink.text_seen(data.turn, data.step) {
                    for block in &data.message.content {
                        if let ContentBlock::Text { text } = block {
                            out.push(ClientEvent::MessageChunk {
                                role: MessageRole::Assistant,
                                content: mojibake::repair_mojibake(text),
                            });
                        }
                    }
                }
                if let Some(usage) = &data.usage {
                    // The rollup carries the same call's `TokenUsage` the
                    // terminal `usage` chunk already delivered; the sink claim
                    // keeps it to one TurnDelta per (turn, step).
                    if sink.claim_usage_emission(data.turn, data.step) {
                        out.push(usage_event(usage));
                    }
                }
            }
            out
        }
        "user/message" => {
            // History replay only. Live prompts are appended locally by
            // app-core at send time, so re-emitting the frame would duplicate
            // every user message. Rebuilt transcripts (resume into an empty
            // store, fork children) have no other source for the prompts —
            // and the fork cut anchors on them as turn boundaries.
            if !sink.is_replaying() {
                return Vec::new();
            }
            let data: Option<UserMessageData> = event.data();
            let Some(data) = data else {
                return Vec::new();
            };
            data.content
                .into_iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(ClientEvent::MessageChunk {
                        role: MessageRole::User,
                        content: mojibake::repair_mojibake(&text),
                    }),
                    _ => None,
                })
                .collect()
        }
        "tool/call" => {
            let data: Option<ToolCallData> = event.data();
            let (name, call_id, raw_input) = match data {
                Some(d) => (d.name, d.call_id, d.arguments.clone()),
                None => (String::new(), String::new(), String::new()),
            };
            let raw_input_value = serde_json::from_str::<Value>(&raw_input).ok();
            if let Some(value) = raw_input_value.clone() {
                sink.record_tool_call(call_id.clone(), name.clone(), value);
            }
            let (kind, summary) = match view {
                Some(ToolEventView::Call { view }) => {
                    let kind = match view {
                        ToolCallView::Terminal(_) => {
                            // Some dsh deployments wrap file tools in a terminal
                            // card even though the underlying operation is the
                            // Kodex workbench's read/edit surface. Infer the
                            // semantic kind so `view`/`str_replace` do not fall
                            // back to the shell card.
                            let inferred = infer_tool_kind(&name, raw_input_value.as_ref());
                            if inferred.is_empty() {
                                "execute".to_string()
                            } else {
                                inferred
                            }
                        }
                        ToolCallView::Diff(_) => "edit".to_string(),
                        // dsh renders file tools (view / str_replace / search...)
                        // with the generic card, which carries no `kind`. Infer
                        // the semantic kind from the tool name so the UI routes
                        // them to the read/edit surfaces instead of Shell.
                        ToolCallView::Generic(g) => g
                            .kind
                            .clone()
                            .unwrap_or_else(|| infer_tool_kind(&name, raw_input_value.as_ref())),
                        // Unknown cards (e.g. a `read` call card added by a
                        // newer dsh) still need kind inference so the UI does
                        // not fall back to the shell surface.
                        ToolCallView::Other => infer_tool_kind(&name, raw_input_value.as_ref()),
                    };
                    let summary = view
                        .title()
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| name.clone());
                    (kind, summary)
                }
                _ => (
                    infer_tool_kind(&name, raw_input_value.as_ref()),
                    name.clone(),
                ),
            };
            let summary = {
                let path = raw_input_value.as_ref().and_then(|v| {
                    v.get("path")
                        .or_else(|| v.get("file_path"))
                        .or_else(|| v.get("filePath"))
                        .and_then(Value::as_str)
                });
                if (kind == "read" || kind == "edit")
                    && let Some(path) = path
                    && !summary.contains(path)
                {
                    format!("{summary} {path}")
                } else {
                    summary
                }
            };
            vec![ClientEvent::ToolStarted {
                id: call_id,
                parent_id: None,
                name: name.clone(),
                kind,
                summary,
                is_subagent: false,
                raw_input: raw_input_value.as_ref().map(|v| json_compact_no_escape(v)),
            }]
        }
        "tool/result" => {
            let data: Option<ToolResultData> = event.data();
            // v4 (dsh ≥ 0.1.7): `message.toolCallId` on the first-class
            // tool-role message. v3 fallback: the single `tool-result`
            // wrapper block inside the user-role message.
            let call_id = data
                .as_ref()
                .and_then(|d| {
                    d.message.tool_call_id.clone().or_else(|| {
                        d.message.content.iter().find_map(|block| {
                            (block.get("type").and_then(Value::as_str) == Some("tool-result"))
                                .then(|| {
                                    block
                                        .get("toolCallId")
                                        .and_then(Value::as_str)
                                        .map(str::to_string)
                                })
                                .flatten()
                        })
                    })
                })
                .unwrap_or_default();
            let mut out = Vec::new();
            let mut had_diff_view = false;
            let recorded_call = sink.take_tool_call(&call_id);

            // Diff views → one ToolDiff per file (before ToolCompleted).
            if let Some(ToolEventView::Result { view }) = view {
                if let ToolResultView::Diff(diff_view) = view {
                    had_diff_view = true;
                    for fd in &diff_view.diffs {
                        out.push(ClientEvent::ToolDiff {
                            id: call_id.clone(),
                            path: fd.path.clone(),
                            old_text: fd.old_text.clone(),
                            new_text: fd.new_text.clone(),
                        });
                    }
                }
            }

            // File-editor tools sometimes arrive with a generic/terminal result
            // card, so the dsh-bridge must synthesize the diff preview from the
            // recorded tool-call arguments or the workbench will show a shell
            // card instead of an edit/change preview.
            if !had_diff_view
                && data.as_ref().is_none_or(|d| d.error.is_none())
                && let Some((name, args)) = recorded_call.as_ref()
                && let Some(diff) = synthetic_edit_diff(&call_id, name.as_str(), args)
            {
                out.push(diff);
            }

            let inferred_kind = recorded_call
                .as_ref()
                .map(|(name, args)| {
                    let args_value = serde_json::Value::Object(args.clone());
                    infer_tool_kind(name, Some(&args_value))
                })
                .unwrap_or_default();
            let (outcome, terminal_output, raw_output) =
                match (data.as_ref(), view, inferred_kind.as_str()) {
                    (
                        Some(d),
                        Some(ToolEventView::Result {
                            view: ToolResultView::Terminal(_),
                        }),
                        "read",
                    ) => {
                        // Keep read tools on the file-view surface even when dsh
                        // reported the result through a terminal card: use the
                        // model-facing text as raw_output instead of a shell output.
                        (result_outcome(d), None, result_text(d))
                    }
                    (Some(d), Some(ToolEventView::Result { view }), _) => render_result(d, view),
                    (Some(d), None, _) => (result_outcome(d), None, result_text(d)),
                    _ => ("completed".to_string(), None, None),
                };

            if sink.is_replaying() {
                // History replay only rebuilds the UI transcript. The tool row
                // already exists in SQLite with its terminal state; re-emitting
                // `ToolStarted`/`ToolCompleted` here would let app-core
                // `persist_event` overwrite the persisted row back to Running.
                // Keep any diff previews so the card still renders, but do not
                // send a terminal-state event.
                out.retain(|event| matches!(event, ClientEvent::ToolDiff { .. }));
            } else if data.as_ref().is_some_and(|d| d.error.is_some()) {
                out.push(ClientEvent::ToolFailed {
                    id: call_id,
                    name: None,
                    error: data
                        .as_ref()
                        .and_then(|d| d.error.as_ref())
                        .map(|e| {
                            // v4 carries a raw user-facing reason; fall back to
                            // the `name: code` identity pair when absent.
                            e.reason
                                .as_deref()
                                .map(str::trim)
                                .filter(|r| !r.is_empty())
                                .map(str::to_string)
                                .unwrap_or_else(|| format!("{}: {}", e.name, e.code))
                        })
                        .unwrap_or_else(|| "tool error".to_string()),
                    raw_output,
                    terminal_output,
                });
            } else {
                out.push(ClientEvent::ToolCompleted {
                    id: call_id,
                    name: None,
                    outcome,
                    raw_output,
                    terminal_output,
                });
            }
            out
        }
        "todo/write" => {
            let data: Option<TodoWriteData> = event.data();
            let entries = data
                .map(|d| d.todos)
                .unwrap_or_default()
                .into_iter()
                .map(todo_to_plan_entry)
                .collect();
            vec![ClientEvent::PlanUpdated { entries }]
        }
        // Turn boundaries are the only reliable "is a turn in flight" signal
        // the bridge has: the harness emits `api-session/status(<id>, false)`
        // (agent running→idle) at the end of EVERY turn as well as when a host
        // failure stops one mid-turn, and only the durable `turn/end` tells the
        // two apart. Record it so the host-status handler does not turn a
        // finished turn into an interrupted session. The boundary ALSO bumps
        // the sink's turn epoch, which cancels an interruption the
        // host-status handler already deferred (the status frame rides the
        // mux and can beat this `turn/end` on the follow stream).
        "turn/start" => {
            sink.set_turn_active(true);
            Vec::new()
        }
        "turn/end" => {
            sink.set_turn_active(false);
            let data: Option<TurnEndData> = event.data();
            let stop_reason = data
                .as_ref()
                .map(|d| turn_end_kind_to_stop_reason(&d.reason.kind))
                .unwrap_or_else(|| "end_turn".to_string());
            // For an upstream LLM failure (kind `error`), surface the real
            // `LlmFailure` (message / code / HTTP status) as detail so the
            // UI refusal notice can name the actual cause (e.g. `429`).
            let detail = data.as_ref().and_then(|d| {
                if d.reason.kind == "error" {
                    d.reason.rest.get("error").and_then(|v| {
                        serde_json::from_value::<LlmFailure>(v.clone())
                            .ok()
                            .as_ref()
                            .and_then(llm_failure_detail)
                    })
                } else {
                    None
                }
            });
            vec![ClientEvent::TurnFinished {
                stop_reason,
                detail,
            }]
        }
        "request/header" => {
            // The full header/config is rich; in v1 the model selector is
            // published from `session.models` by `emit_config_controls`. Emitting
            // an empty `SessionConfigUpdated` here would wipe the model control
            // (the reducer replaces `session_config` wholesale), so drop the
            // frame to keep the dropdown populated.
            Vec::new()
        }
        // dsh compaction lifecycle (see @deepseek-ai/dsh-compaction/types):
        // `compaction/start` and `compaction/end` are log-only session events
        // that bracket a context compaction. Map them to the same
        // `ContextCompactionStarted`/`ContextCompacted` notices CodeBuddy uses
        // so the UI shows "正在压缩上下文" → "上下文已压缩". The occupancy drop
        // itself arrives separately via the `contextPressure` projection, whose
        // `projectedTokens` reacts to compaction immediately.
        "compaction/start" => {
            let data: Option<CompactionStartData> = event.data();
            let compaction_id = data.and_then(|d| d.compaction_id);
            vec![ClientEvent::ContextCompactionStarted {
                message: compaction_id
                    .map(|id| format!("正在压缩上下文（{id}）"))
                    .unwrap_or_else(|| "正在压缩上下文".to_string()),
            }]
        }
        "compaction/end" => {
            let data: Option<CompactionEndData> = event.data();
            let message = match data.and_then(|d| d.error) {
                Some(error) if !error.trim().is_empty() => {
                    format!("上下文压缩未完成：{error}")
                }
                _ => "上下文已压缩".to_string(),
            };
            vec![ClientEvent::ContextCompacted { message }]
        }
        // Manual `/compact` outcome (see @deepseek-ai/dsh-commands): the
        // command runner emits `command/run` / `command/done` around the
        // execution. Track the compact runs so the paired `command/done`
        // renders the authoritative result text ("Compacted 12 history items
        // (~45k tokens).", the busy/no-history rejections, …) as the
        // completion notice — outcomes that never start a compaction produce
        // no `compaction/*` events at all. Other commands are ignored: kodex
        // executes no others today.
        "command/run" => {
            let data: Option<CommandRunData> = event.data();
            if let Some(data) = data
                && data.name == "compact"
            {
                sink.track_compact_command(data.command_id);
            }
            Vec::new()
        }
        "command/done" => {
            let data: Option<CommandDoneData> = event.data();
            match data {
                Some(data) if sink.take_compact_command(&data.command_id) => {
                    vec![ClientEvent::ContextCompacted {
                        message: compact_command_outcome_notice(&data.kind, data.text.as_deref()),
                    }]
                }
                _ => Vec::new(),
            }
        }
        // turn/start, step/start, step/end, user/message, request/context,
        // session/end-seed, compaction/summary, compaction/prune: log-only or
        // surface metadata not represented in ClientEvent in v1. Ignorable per
        // dsh's merge-extensibility guard.
        _ => Vec::new(),
    }
}

#[derive(serde::Deserialize)]
struct CompactionStartData {
    #[serde(default, rename = "compactionId")]
    compaction_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct CompactionEndData {
    #[serde(default)]
    error: Option<String>,
}

/// `command/run` session event payload (`@deepseek-ai/dsh-commands`):
/// `{ commandId, name, args?, source }`.
#[derive(serde::Deserialize)]
struct CommandRunData {
    #[serde(rename = "commandId")]
    command_id: String,
    name: String,
    #[serde(default)]
    args: Option<String>,
}

/// `command/done` session event payload: `{ commandId, kind, text?,
/// sourceEventSeq? }`.
#[derive(serde::Deserialize)]
struct CommandDoneData {
    #[serde(rename = "commandId")]
    command_id: String,
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

/// Human-facing notice for a manual `compact` command outcome. Mirrors the
/// wording of the bridge's other compaction notices.
fn compact_command_outcome_notice(kind: &str, text: Option<&str>) -> String {
    let text = text.map(str::trim).filter(|text| !text.is_empty());
    match (kind, text) {
        ("success", Some(text)) => format!("上下文压缩完成：{text}"),
        ("success", None) => "上下文已压缩".to_string(),
        (_, Some(text)) => format!("上下文压缩失败：{text}"),
        (_, None) => "上下文压缩失败".to_string(),
    }
}

#[derive(serde::Deserialize)]
struct TodoWriteData {
    #[serde(default)]
    todos: Vec<TodoItem>,
}

#[derive(serde::Deserialize)]
struct TurnEndData {
    reason: TurnEndReason,
}

/// The `error` payload on a `turn/end` of kind `error` — mirrors dsh's
/// `LlmFailure` (`@deepseek-ai/dsh-llm/types`): `{ message, code, status?,
/// providerRetryAfterMs?, requestId? }`. Only the human-facing fields are
/// narrowed; the rest stay opaque.
#[derive(serde::Deserialize)]
struct LlmFailure {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    status: Option<serde_json::Number>,
}

/// Build a short, user-facing detail string from a harness `LlmFailure`.
/// Includes the HTTP status (e.g. `429`) and message when present, so the
/// Kodex refusal notice can surface the real upstream cause instead of the
/// generic wording. Returns `None` only when the payload carried no usable
/// text at all.
fn llm_failure_detail(failure: &LlmFailure) -> Option<String> {
    let message = failure
        .message
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    let code = failure
        .code
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    let status = failure
        .status
        .as_ref()
        .and_then(serde_json::Number::as_u64)
        .map(|s| s.to_string());
    match (status.as_deref(), code.as_deref(), message) {
        (Some(s), Some(c), Some(m)) => Some(format!("HTTP {s} ({c}): {m}")),
        (Some(s), Some(c), None) => Some(format!("HTTP {s} ({c})")),
        (Some(s), None, Some(m)) => Some(format!("HTTP {s}: {m}")),
        (Some(s), None, None) => Some(format!("HTTP {s}")),
        (None, Some(c), Some(m)) => Some(format!("{c}: {m}")),
        (None, Some(c), None) => Some(c.to_string()),
        (None, None, Some(m)) => Some(m.to_string()),
        (None, None, None) => None,
    }
}

/// Map a dsh `turn/end` reason kind to Kodex's `TurnFinished` stop reason
/// vocabulary. Mirrors dsh's own `turnEndToStopReason`: `completed`→`end_turn`,
/// `max-tokens`→`max_tokens`, `interrupted`→`cancelled`, `aborted`/`blocked`→
/// `end_turn`. `error` (an upstream LLM failure) maps to `refusal` so the UI
/// surfaces the friendly "上游请求失败/被拒绝/限流" notice instead of a raw
/// `error` stop reason. The real `LlmFailure` (message / code / HTTP status)
/// is carried alongside as `TurnFinished.detail` by the `turn/end` handler.
fn turn_end_kind_to_stop_reason(kind: &str) -> String {
    match kind {
        "completed" => "end_turn".to_string(),
        "max-tokens" => "max_tokens".to_string(),
        "interrupted" => "cancelled".to_string(),
        "error" => "refusal".to_string(),
        _ => "end_turn".to_string(),
    }
}

fn approval_options() -> Vec<PermissionOption> {
    vec![
        PermissionOption {
            id: "allowed-once".to_string(),
            label: "Allow once".to_string(),
            kind: "allow_once".to_string(),
        },
        PermissionOption {
            id: "rejected".to_string(),
            label: "Reject".to_string(),
            kind: "reject_once".to_string(),
        },
    ]
}

/// Options for a `question/requested` input form. The workbench's question
/// panel locates the submit/cancel affordances by id (`submit`/`cancel`); the
/// ids are UI markers only — `build_harness_approval_result` builds the answer
/// from `input_response` and ignores the option id.
fn question_options() -> Vec<PermissionOption> {
    vec![
        PermissionOption {
            id: "submit".to_string(),
            label: "Submit".to_string(),
            kind: "allow_once".to_string(),
        },
        PermissionOption {
            id: "cancel".to_string(),
            label: "Cancel".to_string(),
            kind: "reject_once".to_string(),
        },
    ]
}

fn todo_to_plan_entry(todo: TodoItem) -> AgentPlanEntry {
    let status = match todo.status.as_str() {
        "in_progress" => AgentPlanEntryStatus::InProgress,
        "completed" => AgentPlanEntryStatus::Completed,
        _ => AgentPlanEntryStatus::Pending,
    };
    AgentPlanEntry {
        id: None,
        content: todo.content,
        priority: AgentPlanEntryPriority::Medium,
        status,
    }
}

/// Infer the semantic tool kind (`read` / `edit` / `execute` / `search`) from
/// the dsh tool name when the generic call view carries no explicit `kind`.
/// The workbench routes on `kind`: `read`/`search` render as exploration
/// cards, `edit` renders the diff/变更 surface, `execute` renders the shell
/// surface. Unknown tools stay empty so they keep the generic presentation.
fn infer_tool_kind(name: &str, raw_input: Option<&Value>) -> String {
    let lower = name.trim().to_lowercase();
    let normalized = lower.replace(['_', '-'], " ");
    let matches_any = |needles: &[&str]| {
        needles.iter().any(|needle| {
            normalized == *needle
                || normalized.starts_with(&format!("{needle} "))
                || normalized.ends_with(&format!(" {needle}"))
                || normalized.contains(&format!(" {needle} "))
        })
    };

    // Edit tools: replace/patch/write-shaped names, or any tool whose input
    // carries an old/new text pair (str_replace-style arguments).
    if matches_any(&[
        "edit",
        "str replace",
        "replace",
        "patch",
        "apply patch",
        "write",
        "create",
    ]) {
        return "edit".to_string();
    }
    if let Some(input) = raw_input {
        let has_old = input.get("old_string").is_some() || input.get("oldString").is_some();
        let has_new = input.get("new_string").is_some() || input.get("newString").is_some();
        if has_old && has_new {
            return "edit".to_string();
        }
    }

    if matches_any(&["view", "read", "open", "cat", "get file"]) {
        return "read".to_string();
    }
    if matches_any(&["search", "grep", "glob", "find", "list", "ls", "query"]) {
        return "search".to_string();
    }
    if matches_any(&["bash", "shell", "exec", "run", "terminal", "command", "cmd"]) {
        return "execute".to_string();
    }
    String::new()
}

/// Synthesize a `ToolDiff` event for a file-surgery tool when the dsh result
/// card did not carry a diff view. Uses the recorded tool-call arguments so
/// `str_replace`/`edit` tools still produce a change preview.
fn synthetic_edit_diff(
    call_id: &str,
    name: &str,
    args: &serde_json::Map<String, serde_json::Value>,
) -> Option<ClientEvent> {
    let lower = name.trim().to_lowercase().replace(['_', '-'], " ");
    let is_edit_tool = lower.contains("str_replace")
        || lower.contains("replace")
        || lower.contains("edit")
        || lower.contains("write");
    if !is_edit_tool {
        return None;
    }
    let path = args
        .get("file_path")
        .or_else(|| args.get("path"))
        .or_else(|| args.get("filePath"))?
        .as_str()?;

    // `str_replace_editor` with `command: "create"` (or any write tool whose
    // arguments carry `file_text` / `content` with no old text) creates a new
    // file: emit a whole-file-added diff so the workbench renders the diff
    // surface instead of a shell fallback card. The reducer treats an empty
    // `old_text` as a trustworthy "create" baseline (vs. `None`, which it
    // cannot trust for fragment detection).
    let create_text = args
        .get("file_text")
        .or_else(|| args.get("content"))
        .or_else(|| args.get("new_content"))
        .or_else(|| args.get("newContent"))
        .and_then(serde_json::Value::as_str);
    let command = args
        .get("command")
        .and_then(serde_json::Value::as_str)
        .map(|c| c.trim().to_lowercase());
    let has_old = args
        .get("old_string")
        .or_else(|| args.get("old_str"))
        .or_else(|| args.get("oldString"))
        .or_else(|| args.get("before"))
        .or_else(|| args.get("oldText"))
        .is_some();
    let is_create =
        matches!(command.as_deref(), Some("create")) || (create_text.is_some() && !has_old);
    if is_create {
        let new_text = create_text?;
        return Some(ClientEvent::ToolDiff {
            id: call_id.to_string(),
            path: path.to_string(),
            old_text: Some(String::new()),
            new_text: new_text.to_string(),
        });
    }

    let old = args
        .get("old_string")
        .or_else(|| args.get("old_str"))
        .or_else(|| args.get("oldString"))
        .and_then(serde_json::Value::as_str);
    let new = args
        .get("new_string")
        .or_else(|| args.get("new_str"))
        .or_else(|| args.get("newString"))?
        .as_str()?;
    Some(ClientEvent::ToolDiff {
        id: call_id.to_string(),
        path: path.to_string(),
        old_text: old.map(String::from),
        new_text: new.to_string(),
    })
}

fn render_result(
    data: &ToolResultData,
    view: &ToolResultView,
) -> (String, Option<TerminalOutput>, Option<String>) {
    match view {
        ToolResultView::Terminal(t) => {
            let outcome = if t.exit_code == Some(0) {
                "completed".to_string()
            } else {
                "failed".to_string()
            };
            let term = Some(TerminalOutput {
                exit_code: t.exit_code,
                output: t.output.clone().unwrap_or_default(),
            });
            (outcome, term, None)
        }
        ToolResultView::Diff(_) => {
            // Diffs already emitted as ToolDiff events; the completed card
            // carries the model-facing result text as raw_output.
            ("completed".to_string(), None, result_text(data))
        }
        ToolResultView::Read(v) => {
            // Read cards carry `{ path, lines: [{ number, text }] }` — render
            // them as numbered file content so the exploration card shows the
            // actual file instead of raw JSON.
            let rendered = render_read_view(v);
            (
                result_outcome(data),
                None,
                rendered.or_else(|| result_text(data)),
            )
        }
        ToolResultView::Search(v) | ToolResultView::Web(v) => {
            // Unrepresentable structured views → raw_output JSON (recoverable).
            (result_outcome(data), None, Some(json_compact_no_escape(v)))
        }
        ToolResultView::Generic(_) => ("completed".to_string(), None, result_text(data)),
        ToolResultView::Other => ("completed".to_string(), None, result_text(data)),
    }
}

fn result_outcome(data: &ToolResultData) -> String {
    if data.error.is_some() {
        "failed".to_string()
    } else {
        "completed".to_string()
    }
}

/// Model-facing result text: concatenate text blocks of the tool-result
/// message. In session format v4 (dsh ≥ 0.1.7) the message `content` holds
/// the result payload blocks directly; v3 wrapped them in a single
/// `tool-result` block whose `content` carried the payload. Blocks that are
/// not plain text (e.g. an MCP `generate_image` JSON result) are kept as
/// compact JSON instead of being dropped — the tool card's image preview and
/// the expanded raw view both read them from `raw_output`.
fn result_text(data: &ToolResultData) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    collect_result_text(&data.message.content, &mut parts);
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn collect_result_text(blocks: &[Value], out: &mut Vec<String>) {
    for block in blocks {
        let block_type = block.get("type").and_then(Value::as_str);
        if block_type == Some("text")
            && let Some(text) = block.get("text").and_then(Value::as_str)
        {
            out.push(text.to_string());
            continue;
        }
        // v3 wrapper: the payload lived one level down inside the
        // `tool-result` block's own `content` array.
        if block_type == Some("tool-result")
            && let Some(content) = block.get("content").and_then(Value::as_array)
        {
            collect_result_text(content, out);
            continue;
        }
        // Non-text payload (MCP JSON results, …) survives as compact JSON.
        if block.is_object() || block.is_array() {
            out.push(json_compact_no_escape(block));
        }
    }
}

/// Render a `read` result view (`{ path, lines: [{ number, text }] }`) as
/// numbered file content. Returns None when the shape is not recognized.
fn render_read_view(view: &Value) -> Option<String> {
    let path = view.get("path").and_then(Value::as_str);
    let lines = view.get("lines").and_then(Value::as_array)?;
    let mut out = String::new();
    if let Some(path) = path {
        out.push_str(path);
        out.push('\n');
    }
    let width = lines
        .last()
        .and_then(|line| line.get("number"))
        .and_then(Value::as_u64)
        .map(|n| n.to_string().len())
        .unwrap_or(1);
    for line in lines {
        let number = line.get("number").and_then(Value::as_u64).unwrap_or(0);
        let text = line.get("text").and_then(Value::as_str).unwrap_or_default();
        out.push_str(&format!("{number:>width$} | {text}\n", width = width));
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Map a dsh per-call `TokenUsage` into a Kodex `TurnDelta` usage event.
///
/// dsh's `TokenUsage` buckets are DISJOINT (`@deepseek-ai/dsh-llm/types`:
/// "`inputTokens` is uncached input only; cached input is reported separately
/// as `cacheReadTokens`/`cacheWriteTokens`; billed input = sum of the three").
/// Kodex's convention is the opposite — `input_tokens` is the cache-INCLUSIVE
/// prompt size and the cache axes are display-only subsets; the reducer,
/// session-store summaries and the UI all rely on that invariant (see
/// `codebuddy-proxy/src/usage.rs`, which normalizes the same Anthropic-shaped
/// disjoint semantics). Fold the cache axes into `input_tokens` here: without
/// it, cache reads exceed "input" by 10-100× and every total understates the
/// billed usage by exactly the cached prefix.
fn usage_event(usage: &TokenUsage) -> ClientEvent {
    let input_tokens = usage
        .input_tokens
        .saturating_add(usage.cache_read_tokens.unwrap_or(0))
        .saturating_add(usage.cache_write_tokens.unwrap_or(0));
    ClientEvent::UsageUpdated {
        usage: UsageEvent {
            scope: workspace_model::UsageEventScope::TurnDelta,
            model: None,
            provider: None,
            agent_cli: None,
            tokens: workspace_model::UsageTokenBreakdown {
                input_tokens: Some(input_tokens),
                output_tokens: Some(usage.output_tokens),
                cache_read_tokens: usage.cache_read_tokens,
                cache_write_tokens: usage.cache_write_tokens,
                reasoning_tokens: usage.reasoning_tokens,
                ..Default::default()
            },
            ..Default::default()
        },
    }
}

/// `session/projection` `contextPressure` value — the harness's real context
/// occupancy. `projectedTokens` already prices in surface movement (and reacts
/// to compaction immediately), so prefer it over the bare `pressureTokens`
/// sample. Returns None when the projection carries no usable figure yet.
#[derive(serde::Deserialize)]
struct ContextPressureProjection {
    #[serde(default, rename = "pressureTokens")]
    pressure_tokens: Option<u64>,
    #[serde(default, rename = "projectedTokens")]
    projected_tokens: Option<u64>,
    #[serde(default, rename = "contextWindow")]
    context_window: Option<u64>,
}

fn context_pressure_usage_event(value: &Value) -> Option<ClientEvent> {
    let pressure: ContextPressureProjection = serde_json::from_value(value.clone()).ok()?;
    let used_tokens = pressure.projected_tokens.or(pressure.pressure_tokens);
    // Only emit when at least one figure advanced; otherwise we'd overwrite a
    // known occupancy with empty values on a no-op projection tick.
    if used_tokens.is_none() && pressure.context_window.is_none() {
        return None;
    }
    Some(ClientEvent::UsageUpdated {
        usage: UsageEvent {
            scope: UsageEventScope::ContextSnapshot,
            context: workspace_model::UsageContextSnapshot {
                used_tokens,
                window_tokens: pressure.context_window,
                ..Default::default()
            },
            ..Default::default()
        },
    })
}

/// `session/projection` `tokenUsage` value — durable cumulative provider usage
/// for the whole session. dsh 0.1.2 nests the durable buckets under `totals`
/// and records the most recent model call under `last`; dsh 0.1.1 carried the
/// same buckets flat. Accept both wire versions.
///
/// The projection's four buckets are disjoint
/// (`@deepseek-ai/dsh-token-meter/projection`: "The four buckets are
/// disjoint"), so the uncached-input bucket alone must NOT become
/// `input_tokens` — fold in the cache axes to keep the cache-inclusive
/// convention the rest of Kodex relies on.
#[derive(serde::Deserialize)]
struct TokenUsageProjection {
    #[serde(default)]
    totals: Option<TokenUsageBuckets>,
    #[serde(default, rename = "uncachedInputTokens")]
    uncached_input_tokens: Option<u64>,
    #[serde(default, rename = "outputTokens")]
    output_tokens: Option<u64>,
    #[serde(default, rename = "cacheReadTokens")]
    cache_read_tokens: Option<u64>,
    #[serde(default, rename = "cacheWriteTokens")]
    cache_write_tokens: Option<u64>,
}

#[derive(serde::Deserialize)]
struct TokenUsageBuckets {
    #[serde(default, rename = "uncachedInputTokens")]
    uncached_input_tokens: u64,
    #[serde(default, rename = "outputTokens")]
    output_tokens: u64,
    #[serde(default, rename = "cacheReadTokens")]
    cache_read_tokens: u64,
    #[serde(default, rename = "cacheWriteTokens")]
    cache_write_tokens: u64,
}

fn token_usage_projection_event(value: &Value) -> Option<ClientEvent> {
    let projection: TokenUsageProjection = serde_json::from_value(value.clone()).ok()?;
    let usage = if let Some(totals) = projection.totals {
        totals
    } else {
        let (uncached_input_tokens, output_tokens) =
            match (projection.uncached_input_tokens, projection.output_tokens) {
                (Some(uncached_input_tokens), Some(output_tokens)) => {
                    (uncached_input_tokens, output_tokens)
                }
                _ => return None,
            };
        let cache_read_tokens = projection.cache_read_tokens?;
        let cache_write_tokens = projection.cache_write_tokens?;
        TokenUsageBuckets {
            uncached_input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
        }
    };
    let input_tokens = usage
        .uncached_input_tokens
        .saturating_add(usage.cache_read_tokens)
        .saturating_add(usage.cache_write_tokens);
    if input_tokens == 0 && usage.output_tokens == 0 {
        // Session-start projection before any model call: an all-zero
        // SessionTotal carries no information, and persisting it records a
        // row whose model is still unknown (the summary then surfaced the
        // agent label as a bogus "model" with 0 requests / 0 tokens).
        return None;
    }
    Some(ClientEvent::UsageUpdated {
        usage: UsageEvent {
            scope: UsageEventScope::SessionTotal,
            tokens: UsageTokenBreakdown {
                input_tokens: Some(input_tokens),
                output_tokens: Some(usage.output_tokens),
                cache_read_tokens: Some(usage.cache_read_tokens),
                cache_write_tokens: Some(usage.cache_write_tokens),
                ..Default::default()
            },
            ..Default::default()
        },
    })
}

/// Reconstruct a diff hunk list from a `FileDiff` (for `ToolDiffPreview`).
/// Used by history replay when the frontend wants hunk-level preview.
pub fn file_diff_to_hunks(path: &str, old: Option<&str>, new: &str) -> DiffHunk {
    let heading = path.to_string();
    let mut lines = Vec::new();
    if let Some(old) = old {
        for line in old.lines() {
            lines.push(DiffLine {
                kind: DiffLineKind::Context,
                content: line.to_string(),
            });
        }
    }
    for line in new.lines() {
        lines.push(DiffLine {
            kind: DiffLineKind::Added,
            content: line.to_string(),
        });
    }
    DiffHunk { heading, lines }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acp_core::PermissionBroker;
    use std::sync::mpsc;

    fn test_sink() -> (SessionSink, mpsc::Receiver<ClientEvent>) {
        let (tx, rx) = mpsc::channel();
        (SessionSink::new(tx, PermissionBroker::default()), rx)
    }

    fn mux(json: serde_json::Value) -> MuxFrame {
        serde_json::from_value(json).expect("fixture frame must deserialize")
    }

    #[test]
    fn subscription_baseline_never_lowers_the_cursor() {
        // A fresh follow reports `lastSeq: 0`. Storing that unconditionally
        // rewound the sink below the history replay's cursor, so the whole
        // journal passed the `apply_follow_frames` dedupe and was re-applied
        // live — which is how a fork child's transcript ended up holding tool
        // calls only (the live mapping drops assistant text and user prompts).
        let (sink, _rx) = test_sink();
        sink.last_seq.store(32, std::sync::atomic::Ordering::Release);

        map_mux_frame(
            &mux(serde_json::json!({
                "type": "session/subscribed",
                "sessionId": "s-1",
                "lastSeq": 0
            })),
            &sink,
        );
        assert_eq!(
            sink.last_seq.load(std::sync::atomic::Ordering::Acquire),
            32,
            "the subscription baseline must not rewind the cursor"
        );

        map_mux_frame(
            &mux(serde_json::json!({
                "type": "session/subscribed",
                "sessionId": "s-1",
                "lastSeq": 40
            })),
            &sink,
        );
        assert_eq!(
            sink.last_seq.load(std::sync::atomic::Ordering::Acquire),
            40,
            "a newer baseline still advances the cursor"
        );
    }

    #[test]
    fn json_compact_no_escape_keeps_non_ascii_literal() {
        // CJK in tool raw_input/raw_output must stay literal, not \u-escaped
        // (mojibake-looking `\u4f60\u597d` escape sequences in the UI). Built
        // from code points so the source stays pure ASCII and survives any
        // re-encoding of the file.
        let cjk: String = (0x4f60..=0x4f61)
            .map(|cp| char::from_u32(cp).unwrap())
            .collect();
        let v = serde_json::json!({ "q": cjk });
        let s = json_compact_no_escape(&v);
        assert!(s.contains(&cjk), "non-ASCII must stay literal: {s}");
        assert!(!s.contains("\\u4f60"), "unexpected \\u escape: {s}");
    }

    #[test]
    fn json_compact_no_escape_escapes_required_chars() {
        let v = serde_json::json!({ "q": "a\"b\\c\n\t" });
        let s = json_compact_no_escape(&v);
        assert!(s.contains("\\\""), "double-quote must be escaped: {s}");
        assert!(s.contains("\\\\"), "backslash must be escaped: {s}");
        assert!(s.contains("\\n"), "newline must be escaped: {s}");
        assert!(s.contains("\\t"), "tab must be escaped: {s}");
    }

    fn session_event(type_tag: &str, seq: u64, data: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": { "type": type_tag, "seq": seq, "time": 0.0, "data": data }
        })
    }

    #[test]
    fn maps_request_header_does_not_emit_session_config() {
        // `request/header` must not emit an empty `SessionConfigUpdated`,
        // which would wipe the model control published by `emit_config_controls`.
        let (sink, _rx) = test_sink();
        let frame = mux(session_event(
            "request/header",
            12,
            serde_json::json!({ "header": {}, "reason": "initial" }),
        ));
        let mapped = map_mux_frame(&frame, &sink);
        assert!(
            mapped.events.is_empty(),
            "request/header must not emit events: {:?}",
            mapped.events
        );
    }

    #[test]
    fn maps_assistant_chunk_text() {
        let (sink, _rx) = test_sink();
        let frame = mux(session_event(
            "assistant/chunk",
            1,
            serde_json::json!({ "turn": 1, "step": 1, "chunk": { "type": "text-delta", "index": 0, "text": "Hello" } }),
        ));
        let mapped = map_mux_frame(&frame, &sink);
        // The reply text closes the current thinking segment first: dsh emits
        // no reasoning-end signal, so without the marker the reducer's thinking
        // buffer would keep every model call's reasoning for the whole turn.
        assert_eq!(
            mapped.events,
            vec![
                ClientEvent::ThinkingActivity { active: false },
                ClientEvent::MessageChunk {
                    role: MessageRole::Assistant,
                    content: "Hello".to_string(),
                }
            ]
        );
        assert_eq!(sink.last_seq.load(std::sync::atomic::Ordering::Acquire), 1);
    }

    #[test]
    fn maps_assistant_reasoning_chunk_to_thinking() {
        let (sink, _rx) = test_sink();
        let frame = mux(session_event(
            "assistant/chunk",
            2,
            serde_json::json!({ "turn": 1, "step": 1, "chunk": { "type": "reasoning-delta", "index": 0, "text": "think..." } }),
        ));
        let mapped = map_mux_frame(&frame, &sink);
        assert_eq!(
            mapped.events,
            vec![
                ClientEvent::ThinkingActivity { active: true },
                ClientEvent::ThinkingChunk {
                    text: "think...".to_string(),
                },
            ]
        );
    }

    #[test]
    fn live_assistant_message_emits_no_text() {
        // Live stream: text already arrived via assistant/chunk text-deltas;
        // the finalized assistant/message must not re-emit it (only usage).
        let (sink, _rx) = test_sink();
        let frame = mux(session_event(
            "assistant/message",
            3,
            serde_json::json!({
                "turn": 1, "step": 1,
                "message": {
                    "role": "assistant",
                    "content": [{ "type": "text", "text": "already streamed" }]
                },
                "usage": { "inputTokens": 10, "outputTokens": 5 }
            }),
        ));
        let mapped = map_mux_frame(&frame, &sink);
        assert!(
            mapped
                .events
                .iter()
                .all(|e| !matches!(e, ClientEvent::MessageChunk { .. })),
            "live assistant/message must not emit text: {:?}",
            mapped.events
        );
        assert!(
            mapped
                .events
                .iter()
                .any(|e| matches!(e, ClientEvent::UsageUpdated { .. })),
            "usage rollup must still be emitted"
        );
    }

    #[test]
    fn replay_assistant_message_emits_text() {
        // History replay (resume / re-baseline): no live chunks exist, so the
        // finalized assistant/message is the only text source and must emit.
        let (sink, _rx) = test_sink();
        sink.set_replaying(true);
        let frame = mux(session_event(
            "assistant/message",
            4,
            serde_json::json!({
                "turn": 1, "step": 1,
                "message": {
                    "role": "assistant",
                    "content": [{ "type": "text", "text": "from history" }]
                }
            }),
        ));
        let mapped = map_mux_frame(&frame, &sink);
        assert_eq!(
            mapped.events,
            vec![ClientEvent::MessageChunk {
                role: MessageRole::Assistant,
                content: "from history".to_string(),
            }]
        );
        sink.set_replaying(false);
    }

    #[test]
    fn replay_user_message_emits_user_chunk_only_during_replay() {
        // Live frames never carry user text through the mapping (app-core
        // appends the prompt locally at send time — emitting it again would
        // duplicate every user message). History replay must emit it: rebuilt
        // transcripts (resume / fork child) have no other source for the
        // prompts, and the fork cut anchors on them as turn boundaries.
        let (sink, _rx) = test_sink();
        let frame = mux(session_event(
            "user/message",
            3,
            serde_json::json!({
                "id": "m-1",
                "role": "user",
                "content": [{ "type": "text", "text": "第一个问题" }]
            }),
        ));

        let live = map_mux_frame(&frame, &sink);
        assert!(
            live.events.is_empty(),
            "live user/message must stay unmapped"
        );

        sink.set_replaying(true);
        let replayed = map_mux_frame(&frame, &sink);
        assert_eq!(
            replayed.events,
            vec![ClientEvent::MessageChunk {
                role: MessageRole::User,
                content: "第一个问题".to_string(),
            }]
        );
        sink.set_replaying(false);
    }

    #[test]
    fn maps_tool_call_with_terminal_view() {
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/call",
                "seq": 3,
                "time": 0.0,
                "data": { "turn": 1, "step": 1, "callId": "call-1", "name": "bash", "arguments": "{\"command\":\"ls\"}" }
            },
            "view": { "for": "call", "view": { "card": "terminal", "title": "ls", "cwd": "/tmp" } }
        }));
        let mapped = map_mux_frame(&frame, &sink);
        assert!(matches!(
            &mapped.events[0],
            ClientEvent::ToolStarted {
                id, name, kind, summary, raw_input, ..
            } if id == "call-1" && name == "bash" && kind == "execute" && summary == "ls"
                && raw_input.as_deref() == Some("{\"command\":\"ls\"}")
        ));
    }

    #[test]
    fn generic_card_view_tool_is_classified_as_read() {
        // dsh renders `view` (file read) with the generic card, which carries
        // no `kind`. Without inference the UI routes it to the Shell surface.
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/call",
                "seq": 3,
                "time": 0.0,
                "data": { "turn": 1, "step": 1, "callId": "call-v", "name": "view", "arguments": "{\"path\":\"/a/b.rs\"}" }
            },
            "view": { "for": "call", "view": { "card": "generic", "title": "view /a/b.rs" } }
        }));
        let mapped = map_mux_frame(&frame, &sink);
        assert!(matches!(
            &mapped.events[0],
            ClientEvent::ToolStarted { id, kind, summary, .. }
                if id == "call-v" && kind == "read" && summary == "view /a/b.rs"
        ));
    }

    #[test]
    fn generic_card_str_replace_tool_is_classified_as_edit() {
        // `str_replace` edits must land on the edit/diff surface so the
        // changes panel picks up the verified file change.
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/call",
                "seq": 3,
                "time": 0.0,
                "data": { "turn": 1, "step": 1, "callId": "call-e", "name": "str_replace", "arguments": "{\"path\":\"/a/b.rs\",\"old_string\":\"x\",\"new_string\":\"y\"}" }
            },
            "view": { "for": "call", "view": { "card": "generic", "title": "str_replace /a/b.rs" } }
        }));
        let mapped = map_mux_frame(&frame, &sink);
        assert!(matches!(
            &mapped.events[0],
            ClientEvent::ToolStarted { id, kind, .. } if id == "call-e" && kind == "edit"
        ));
    }

    #[test]
    fn terminal_card_view_tool_is_classified_as_read_for_file_tools() {
        // Some dsh wraps file reads with a terminal card; the bridge must still
        // route `view` to the read surface instead of the shell card.
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/call",
                "seq": 3,
                "time": 0.0,
                "data": { "turn": 1, "step": 1, "callId": "call-v-term", "name": "view", "arguments": "{\"path\":\"/a/b.rs\"}" }
            },
            "view": { "for": "call", "view": { "card": "terminal", "title": "view", "cwd": "/tmp" } }
        }));
        let mapped = map_mux_frame(&frame, &sink);
        assert!(matches!(
            &mapped.events[0],
            ClientEvent::ToolStarted { id, kind, .. } if id == "call-v-term" && kind == "read"
        ));
    }

    #[test]
    fn terminal_result_for_str_replace_synthesizes_diff_preview() {
        let (sink, _rx) = test_sink();
        let call = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/call",
                "seq": 3,
                "time": 0.0,
                "data": { "turn": 1, "step": 1, "callId": "call-e-term", "name": "str_replace", "arguments": "{\"path\":\"/a/b.rs\",\"old_string\":\"x\",\"new_string\":\"y\"}" }
            },
            "view": { "for": "call", "view": { "card": "terminal", "title": "str_replace", "cwd": "/tmp" } }
        }));
        let _ = map_mux_frame(&call, &sink);
        let result = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/result",
                "seq": 4,
                "time": 0.0,
                "data": {
                    "turn": 1, "step": 1,
                    "message": {
                        "role": "user",
                        "content": [{ "type": "tool-result", "toolCallId": "call-e-term", "content": [] }]
                    }
                }
            },
            "view": { "for": "result", "view": { "card": "terminal", "output": "ok", "exitCode": 0 } }
        }));
        let mapped = map_mux_frame(&result, &sink);
        assert!(matches!(
            &mapped.events[0],
            ClientEvent::ToolDiff { id, path, old_text, new_text }
                if id == "call-e-term" && path == "/a/b.rs" && old_text.as_deref() == Some("x") && new_text == "y"
        ));
        assert!(matches!(
            &mapped.events[1],
            ClientEvent::ToolCompleted { id, .. } if id == "call-e-term"
        ));
    }

    #[test]
    fn generic_result_for_create_synthesizes_whole_file_diff() {
        // A `str_replace_editor` `create` call carries the new file body in
        // `file_text` with no old/new pair. Without a diff view from dsh the
        // bridge must still synthesize a whole-file-added `ToolDiff` (empty
        // `old_text` => trustworthy "create" baseline) so the workbench shows
        // the diff surface instead of a shell fallback card.
        let (sink, _rx) = test_sink();
        let call = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/call",
                "seq": 3,
                "time": 0.0,
                "data": { "turn": 1, "step": 1, "callId": "call-c", "name": "str_replace_editor", "arguments": "{\"command\":\"create\",\"path\":\"/a/b.tsx\",\"file_text\":\"import React from 'react';\\n\\nexport const X = () => null;\\n\"}" }
            },
            "view": { "for": "call", "view": { "card": "generic", "title": "str_replace_editor /a/b.tsx" } }
        }));
        let _ = map_mux_frame(&call, &sink);
        let result = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/result",
                "seq": 4,
                "time": 0.0,
                "data": {
                    "turn": 1, "step": 1,
                    "message": {
                        "role": "user",
                        "content": [{ "type": "tool-result", "toolCallId": "call-c", "content": [] }]
                    }
                }
            },
            "view": { "for": "result", "view": { "card": "generic" } }
        }));
        let mapped = map_mux_frame(&result, &sink);
        assert!(matches!(
            &mapped.events[0],
            ClientEvent::ToolDiff { id, path, old_text, new_text }
                if id == "call-c"
                    && path == "/a/b.tsx"
                    && old_text.as_deref() == Some("")
                    && new_text == "import React from 'react';\n\nexport const X = () => null;\n"
        ));
    }

    #[test]
    fn missing_view_still_infers_kind_from_tool_name() {
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/call",
                "seq": 3,
                "time": 0.0,
                "data": { "turn": 1, "step": 1, "callId": "call-r", "name": "read_file", "arguments": "{\"path\":\"/a/b.rs\"}" }
            }
        }));
        let mapped = map_mux_frame(&frame, &sink);
        assert!(matches!(
            &mapped.events[0],
            ClientEvent::ToolStarted { id, kind, .. } if id == "call-r" && kind == "read"
        ));
    }

    #[test]
    fn maps_tool_result_diff_view_to_tool_diff_plus_completed() {
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/result",
                "seq": 4,
                "time": 0.0,
                "data": {
                    "turn": 1, "step": 1,
                    "message": {
                        "role": "user",
                        "content": [{ "type": "tool-result", "toolCallId": "call-1", "content": [] }]
                    }
                }
            },
            "view": {
                "for": "result",
                "view": {
                    "card": "diff",
                    "diffs": [{ "path": "a.txt", "oldText": "old", "newText": "new" }]
                }
            }
        }));
        let mapped = map_mux_frame(&frame, &sink);
        assert_eq!(
            mapped.events,
            vec![
                ClientEvent::ToolDiff {
                    id: "call-1".to_string(),
                    path: "a.txt".to_string(),
                    old_text: Some("old".to_string()),
                    new_text: "new".to_string(),
                },
                ClientEvent::ToolCompleted {
                    id: "call-1".to_string(),
                    name: None,
                    outcome: "completed".to_string(),
                    raw_output: None,
                    terminal_output: None,
                },
            ]
        );
    }

    #[test]
    fn maps_tool_result_terminal_view_to_terminal_output() {
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/result",
                "seq": 5,
                "time": 0.0,
                "data": {
                    "turn": 1, "step": 1,
                    "message": {
                        "role": "user",
                        "content": [{ "type": "tool-result", "toolCallId": "call-2", "content": [] }]
                    }
                }
            },
            "view": { "for": "result", "view": { "card": "terminal", "output": "out", "exitCode": 0 } }
        }));
        let mapped = map_mux_frame(&frame, &sink);
        assert!(matches!(
            &mapped.events[0],
            ClientEvent::ToolCompleted {
                terminal_output: Some(TerminalOutput { output, exit_code: Some(0), .. }),
                ..
            } if output == "out"
        ));
    }

    #[test]
    fn maps_todo_write_to_plan() {
        let (sink, _rx) = test_sink();
        let frame = mux(session_event(
            "todo/write",
            6,
            serde_json::json!({
                "todos": [
                    { "content": "Read code", "status": "in_progress" },
                    { "content": "Fix bug", "status": "pending" },
                    { "content": "Test", "status": "completed" },
                ]
            }),
        ));
        let mapped = map_mux_frame(&frame, &sink);
        assert!(matches!(
            &mapped.events[0],
            ClientEvent::PlanUpdated { entries } if entries.len() == 3
                && entries[0].status == AgentPlanEntryStatus::InProgress
                && entries[1].status == AgentPlanEntryStatus::Pending
                && entries[2].status == AgentPlanEntryStatus::Completed
        ));
    }

    #[test]
    fn maps_turn_end_to_finished() {
        let (sink, _rx) = test_sink();
        let frame = mux(session_event(
            "turn/end",
            7,
            serde_json::json!({ "turn": 1, "reason": { "kind": "completed" } }),
        ));
        let mapped = map_mux_frame(&frame, &sink);
        assert_eq!(
            mapped.events,
            vec![ClientEvent::TurnFinished {
                stop_reason: "end_turn".to_string(),
                detail: None,
            }]
        );
    }

    #[test]
    fn maps_turn_end_error_to_refusal() {
        // dsh reports upstream LLM failures as `kind: "error"`; the bridge
        // maps it to `refusal` and carries the real `LlmFailure` message as
        // detail so the UI notice can name the actual upstream cause instead
        // of only the generic refusal wording.
        let (sink, _rx) = test_sink();
        let frame = mux(session_event(
            "turn/end",
            8,
            serde_json::json!({ "turn": 1, "reason": { "kind": "error", "error": { "message": "rate limited", "code": "RATE_LIMIT", "status": 429 } } }),
        ));
        let mapped = map_mux_frame(&frame, &sink);
        assert_eq!(
            mapped.events,
            vec![ClientEvent::TurnFinished {
                stop_reason: "refusal".to_string(),
                detail: Some("HTTP 429 (RATE_LIMIT): rate limited".to_string()),
            }]
        );
    }

    #[test]
    fn maps_turn_end_error_detail_without_status() {
        // A failure payload carrying only a message still produces a usable
        // detail string (no HTTP status / code prefix).
        let (sink, _rx) = test_sink();
        let frame = mux(session_event(
            "turn/end",
            9,
            serde_json::json!({ "turn": 1, "reason": { "kind": "error", "error": { "message": "boom" } } }),
        ));
        let mapped = map_mux_frame(&frame, &sink);
        assert_eq!(
            mapped.events,
            vec![ClientEvent::TurnFinished {
                stop_reason: "refusal".to_string(),
                detail: Some("boom".to_string()),
            }]
        );
    }

    #[test]
    fn maps_approval_requested_and_resolved() {
        let (sink, rx) = test_sink();
        let requested = mux(serde_json::json!({
            "type": "approval/requested",
            "sessionId": "s-1",
            "approvalId": "a-1",
            "toolName": "bash",
            "callId": "call-1",
            "reason": "shell"
        }));
        let mapped = map_mux_frame(&requested, &sink);
        assert!(matches!(
            &mapped.events[0],
            ClientEvent::ToolPermissionRequest {
                id, name, options, details, input: None, ..
            } if id == "a-1" && name == "bash" && options.len() == 2 && details.as_deref() == Some("shell")
        ));

        let resolved = mux(serde_json::json!({
            "type": "approval/resolved",
            "sessionId": "s-1",
            "approvalId": "a-1",
            "outcome": "allowed-once"
        }));
        let mapped = map_mux_frame(&resolved, &sink);
        assert_eq!(
            mapped.events,
            vec![ClientEvent::ToolPermissionResolved {
                id: "a-1".to_string(),
                outcome: "allowed-once".to_string(),
            }]
        );
        drop(rx);
    }

    #[test]
    fn maps_question_requested_to_input_request() {
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "question/requested",
            "sessionId": "s-1",
            "questions": [
                { "id": "q1", "question": "Proceed?", "options": [{ "label": "Yes" }, { "label": "No" }] }
            ]
        }));
        let mapped = map_mux_frame(&frame, &sink);
        assert!(matches!(
            &mapped.events[0],
            ClientEvent::ToolPermissionRequest {
                id, name, options, input: Some(PermissionInputRequest { questions }), ..
            } if id == "q1" && name == "user_question" && questions.len() == 1
                && questions[0].options.len() == 2
                // The workbench's question panel enables 提交回答 only when it
                // finds a `submit` option; without one the form is unsubmittable.
                && options.iter().any(|option| option.id == "submit")
                && options.iter().any(|option| option.id == "cancel")
        ));
    }

    #[test]
    fn maps_session_projection_title() {
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/projection",
            "sessionId": "s-1",
            "key": "title",
            "value": "Fix auth bug",
            "seq": 8
        }));
        let mapped = map_mux_frame(&frame, &sink);
        assert_eq!(
            mapped.events,
            vec![ClientEvent::SessionTitleUpdated {
                title: "Fix auth bug".to_string(),
            }]
        );
    }

    #[test]
    fn maps_context_pressure_projection_to_context_snapshot() {
        // The harness's real context occupancy rides the `contextPressure`
        // projection. `projectedTokens` (not the bare pressure sample) is the
        // numerator, and `contextWindow` the denominator — feeding both lets
        // the UI bar track the harness instead of the cumulative estimate.
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/projection",
            "sessionId": "s-1",
            "key": "contextPressure",
            "value": {
                "pressureTokens": 12000,
                "projectedTokens": 12500,
                "contextWindow": 1000000
            },
            "seq": 9
        }));
        let mapped = map_mux_frame(&frame, &sink);
        assert_eq!(mapped.events.len(), 1);
        match &mapped.events[0] {
            ClientEvent::UsageUpdated { usage } => {
                assert_eq!(usage.scope, UsageEventScope::ContextSnapshot);
                assert_eq!(usage.context.used_tokens, Some(12_500));
                assert_eq!(usage.context.window_tokens, Some(1_000_000));
            }
            other => panic!("expected UsageUpdated, got {other:?}"),
        }
    }

    #[test]
    fn maps_context_pressure_projection_prefers_projected_tokens() {
        // When only the bare pressure sample is present (no projection yet),
        // fall back to it so the bar still renders.
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/projection",
            "sessionId": "s-1",
            "key": "contextPressure",
            "value": { "pressureTokens": 8000, "contextWindow": 200000 },
            "seq": 10
        }));
        let mapped = map_mux_frame(&frame, &sink);
        match &mapped.events[0] {
            ClientEvent::UsageUpdated { usage } => {
                assert_eq!(usage.context.used_tokens, Some(8_000));
                assert_eq!(usage.context.window_tokens, Some(200_000));
            }
            other => panic!("expected UsageUpdated, got {other:?}"),
        }
    }

    #[test]
    fn maps_context_pressure_projection_empty_is_noop() {
        // A projection tick with no figures must not wipe a known occupancy.
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/projection",
            "sessionId": "s-1",
            "key": "contextPressure",
            "value": {},
            "seq": 11
        }));
        let mapped = map_mux_frame(&frame, &sink);
        assert!(mapped.events.is_empty(), "empty projection must not emit");
    }

    #[test]
    fn zero_token_usage_projection_is_noop() {
        // The session-start `tokenUsage` projection reports all-zero buckets
        // before any model call. Persisting it wrote a zero-token SessionTotal
        // while the session model was still unknown, which surfaced in the
        // usage summary as an agent-named "model" row with 0 requests.
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/projection",
            "sessionId": "s-1",
            "key": "tokenUsage",
            "value": {
                "totals": {
                    "uncachedInputTokens": 0,
                    "outputTokens": 0,
                    "cacheReadTokens": 0,
                    "cacheWriteTokens": 0
                }
            },
            "seq": 12
        }));
        let mapped = map_mux_frame(&frame, &sink);
        assert!(
            mapped.events.is_empty(),
            "all-zero token usage projection must not emit"
        );
    }

    #[test]
    fn maps_token_usage_projection_to_session_total() {
        // The durable cumulative `tokenUsage` projection replaces the
        // per-turn-delta estimate with the harness's authoritative total.
        // The projection's four buckets are disjoint (dsh-token-meter), so
        // `input_tokens` must fold in the cache axes: 1000 + 200 + 50 = 1250
        // is the cache-inclusive billed input, with the cache axes kept as
        // display-only subsets (the invariant the reducer/session-store/UI
        // rely on).
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/projection",
            "sessionId": "s-1",
            "key": "tokenUsage",
            "value": {
                "uncachedInputTokens": 1000,
                "outputTokens": 500,
                "cacheReadTokens": 200,
                "cacheWriteTokens": 50
            },
            "seq": 12
        }));
        let mapped = map_mux_frame(&frame, &sink);
        assert_eq!(mapped.events.len(), 1);
        match &mapped.events[0] {
            ClientEvent::UsageUpdated { usage } => {
                assert_eq!(usage.scope, UsageEventScope::SessionTotal);
                assert_eq!(usage.tokens.input_tokens, Some(1_250));
                assert_eq!(usage.tokens.output_tokens, Some(500));
                assert_eq!(usage.tokens.cache_read_tokens, Some(200));
                assert_eq!(usage.tokens.cache_write_tokens, Some(50));
            }
            other => panic!("expected UsageUpdated, got {other:?}"),
        }
    }

    #[test]
    fn maps_nested_token_usage_projection_to_session_total() {
        // dsh 0.1.2 nests the durable cumulative buckets under `totals`;
        // accepting only the old flat shape silently dropped every update.
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/projection",
            "sessionId": "s-1",
            "key": "tokenUsage",
            "value": {
                "totals": {
                    "uncachedInputTokens": 476,
                    "outputTokens": 670,
                    "cacheReadTokens": 319_808,
                    "cacheWriteTokens": 0
                },
                "last": {
                    "turn": 151,
                    "step": 6,
                    "buckets": {
                        "uncachedInputTokens": 100,
                        "outputTokens": 20,
                        "cacheReadTokens": 1_000,
                        "cacheWriteTokens": 0
                    }
                }
            },
            "seq": 13
        }));
        let mapped = map_mux_frame(&frame, &sink);
        assert_eq!(mapped.events.len(), 1);
        match &mapped.events[0] {
            ClientEvent::UsageUpdated { usage } => {
                assert_eq!(usage.scope, UsageEventScope::SessionTotal);
                assert_eq!(usage.tokens.input_tokens, Some(320_284));
                assert_eq!(usage.tokens.output_tokens, Some(670));
                assert_eq!(usage.tokens.cache_read_tokens, Some(319_808));
                assert_eq!(usage.tokens.cache_write_tokens, Some(0));
            }
            other => panic!("expected UsageUpdated, got {other:?}"),
        }
    }

    #[test]
    fn maps_projection_baseline_model_and_agent_preset() {
        // dsh 0.1.2 restores the session's actual preset through the projection
        // baseline, not through a resumed `session.create` response. Mapping
        // only `modelSelection` previously made compaction-capable resumed
        // sessions look preset-less.
        let values = serde_json::json!({
            "modelSelection": {
                "next": { "provider": "custom_tencent", "model": "glm-5.3-ioa" }
            },
            "agentPreset": "standard",
            "tokenUsage": {
                "totals": {
                    "uncachedInputTokens": 10,
                    "outputTokens": 5,
                    "cacheReadTokens": 0,
                    "cacheWriteTokens": 0
                }
            }
        });
        let events = map_projection_values(&values);
        assert!(events.iter().any(|event| matches!(
            event,
            ClientEvent::SessionConfigValueChanged { control_id, value_id, .. }
                if control_id == "model"
                    && value_id == "kodex-provider/custom_tencent/glm-5.3-ioa"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ClientEvent::SessionConfigValueChanged { control_id, value_id, .. }
                if control_id == "agent_preset" && value_id == "standard"
        )));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ClientEvent::UsageUpdated { .. }))
        );
    }

    #[test]
    fn usage_chunk_folds_disjoint_cache_buckets_into_input() {
        // dsh's per-call `TokenUsage` is disjoint: `inputTokens` counts
        // uncached input only and the cached prefix lives in
        // `cacheReadTokens`/`cacheWriteTokens`. Kodex's `input_tokens` is
        // cache-inclusive, so 512 + 101568 + 0 must land as the input while
        // the cache axes stay subsets.
        let (sink, _rx) = test_sink();
        let frame = mux(session_event(
            "assistant/chunk",
            1,
            serde_json::json!({
                "turn": 1, "step": 1,
                "chunk": { "type": "usage", "usage": {
                    "inputTokens": 512,
                    "outputTokens": 39,
                    "cacheReadTokens": 101568,
                    "reasoningTokens": 20
                } }
            }),
        ));
        let mapped = map_mux_frame(&frame, &sink);
        assert_eq!(mapped.events.len(), 1);
        match &mapped.events[0] {
            ClientEvent::UsageUpdated { usage } => {
                assert_eq!(usage.scope, UsageEventScope::TurnDelta);
                assert_eq!(usage.tokens.input_tokens, Some(512 + 101_568));
                assert_eq!(usage.tokens.output_tokens, Some(39));
                assert_eq!(usage.tokens.cache_read_tokens, Some(101_568));
                assert_eq!(usage.tokens.cache_write_tokens, None);
                assert_eq!(usage.tokens.reasoning_tokens, Some(20));
            }
            other => panic!("expected UsageUpdated, got {other:?}"),
        }
    }

    #[test]
    fn usage_chunk_and_message_rollup_emit_a_single_turn_delta() {
        // One model call surfaces its usage twice (terminal `usage` chunk then
        // the finalized `assistant/message` rollup). The sink's step claim must
        // keep exactly one TurnDelta per call — the live duplicates doubled
        // every dsh usage row in SQLite.
        let (sink, _rx) = test_sink();
        let usage_data = serde_json::json!({
            "inputTokens": 512, "outputTokens": 39, "cacheReadTokens": 101568
        });
        let chunk = mux(session_event(
            "assistant/chunk",
            1,
            serde_json::json!({
                "turn": 3, "step": 2,
                "chunk": { "type": "usage", "usage": usage_data }
            }),
        ));
        let message = mux(session_event(
            "assistant/message",
            2,
            serde_json::json!({
                "turn": 3, "step": 2,
                "message": { "role": "assistant", "content": [] },
                "usage": usage_data
            }),
        ));
        let mut events = Vec::new();
        for frame in [&chunk, &message] {
            events.extend(map_mux_frame(frame, &sink).events);
        }
        let usage_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, ClientEvent::UsageUpdated { .. }))
            .collect();
        assert_eq!(usage_events.len(), 1, "one call = one usage event");
    }

    #[test]
    fn replayed_usage_is_not_re_emitted_across_passes() {
        // History replay (resume / stream-gap re-baseline) re-delivers the
        // whole session log. Each call's usage must still be emitted exactly
        // once per sink lifetime — repeated passes previously re-appended the
        // full usage history (observed 2×–18× row inflation).
        let (sink, _rx) = test_sink();
        let frame = mux(session_event(
            "assistant/message",
            1,
            serde_json::json!({
                "turn": 1, "step": 1,
                "message": { "role": "assistant", "content": [] },
                "usage": { "inputTokens": 100, "outputTokens": 10 }
            }),
        ));
        let first = map_mux_frame(&frame, &sink);
        assert_eq!(
            first
                .events
                .iter()
                .filter(|e| matches!(e, ClientEvent::UsageUpdated { .. }))
                .count(),
            1
        );
        // Second pass (re-baseline): same frames re-delivered by the host.
        let second = map_mux_frame(&frame, &sink);
        assert!(
            second
                .events
                .iter()
                .all(|e| !matches!(e, ClientEvent::UsageUpdated { .. })),
            "replayed usage must not re-emit"
        );
        // A different step is a different call and must still emit.
        let other_step = mux(session_event(
            "assistant/message",
            2,
            serde_json::json!({
                "turn": 1, "step": 2,
                "message": { "role": "assistant", "content": [] },
                "usage": { "inputTokens": 100, "outputTokens": 10 }
            }),
        ));
        let third = map_mux_frame(&other_step, &sink);
        assert!(
            third
                .events
                .iter()
                .any(|e| matches!(e, ClientEvent::UsageUpdated { .. })),
            "the next call's usage must still emit"
        );
    }

    #[test]
    fn usage_chunk_without_message_rollup_still_emits_once() {
        // An aborted step can carry the usage chunk but never finalize its
        // message; the consumed tokens must still be reported (exactly once).
        let (sink, _rx) = test_sink();
        let chunk = mux(session_event(
            "assistant/chunk",
            1,
            serde_json::json!({
                "turn": 1, "step": 1,
                "chunk": { "type": "usage", "usage": { "inputTokens": 42, "outputTokens": 7 } }
            }),
        ));
        assert_eq!(map_mux_frame(&chunk, &sink).events.len(), 1);
        let message = mux(session_event(
            "assistant/message",
            2,
            serde_json::json!({
                "turn": 1, "step": 1,
                "message": { "role": "assistant", "content": [] }
            }),
        ));
        assert!(map_mux_frame(&message, &sink).events.is_empty());
    }

    #[test]
    fn maps_compaction_start_to_context_compaction_started() {
        let (sink, _rx) = test_sink();
        let frame = mux(session_event(
            "compaction/start",
            20,
            serde_json::json!({ "compactionId": "cmp-1", "turn": null }),
        ));
        let mapped = map_mux_frame(&frame, &sink);
        assert_eq!(mapped.events.len(), 1);
        match &mapped.events[0] {
            ClientEvent::ContextCompactionStarted { message } => {
                assert!(message.contains("正在压缩上下文"), "message={message}");
                assert!(
                    message.contains("cmp-1"),
                    "message should name compaction id: {message}"
                );
            }
            other => panic!("expected ContextCompactionStarted, got {other:?}"),
        }
    }

    #[test]
    fn maps_compact_command_run_and_done_to_outcome_notice() {
        let (sink, _rx) = test_sink();
        let run = mux(session_event(
            "command/run",
            20,
            serde_json::json!({ "commandId": "cmd-c1", "name": "compact", "source": "user" }),
        ));
        assert!(map_mux_frame(&run, &sink).events.is_empty());

        let done = mux(session_event(
            "command/done",
            21,
            serde_json::json!({
                "commandId": "cmd-c1",
                "kind": "success",
                "text": "Compacted 12 history items (~45k tokens)."
            }),
        ));
        let mapped = map_mux_frame(&done, &sink);
        match &mapped.events[0] {
            ClientEvent::ContextCompacted { message } => {
                assert_eq!(
                    message,
                    "上下文压缩完成：Compacted 12 history items (~45k tokens)."
                );
            }
            other => panic!("expected ContextCompacted, got {other:?}"),
        }
    }

    #[test]
    fn maps_compaction_end_to_context_compacted() {
        let (sink, _rx) = test_sink();
        let frame = mux(session_event(
            "compaction/end",
            21,
            serde_json::json!({ "compactionId": "cmp-1", "turn": null }),
        ));
        let mapped = map_mux_frame(&frame, &sink);
        match &mapped.events[0] {
            ClientEvent::ContextCompacted { message } => {
                assert_eq!(message, "上下文已压缩");
            }
            other => panic!("expected ContextCompacted, got {other:?}"),
        }
    }

    #[test]
    fn maps_compaction_end_with_error_surfaces_it() {
        let (sink, _rx) = test_sink();
        let frame = mux(session_event(
            "compaction/end",
            22,
            serde_json::json!({ "compactionId": "cmp-1", "turn": null, "error": "summarize failed" }),
        ));
        let mapped = map_mux_frame(&frame, &sink);
        match &mapped.events[0] {
            ClientEvent::ContextCompacted { message } => {
                assert!(message.contains("未完成"), "message={message}");
                assert!(message.contains("summarize failed"), "message={message}");
            }
            other => panic!("expected ContextCompacted, got {other:?}"),
        }
    }

    #[test]
    fn command_run_and_done_pair_maps_outcome_text() {
        let (sink, _rx) = test_sink();
        let run = mux(session_event(
            "command/run",
            30,
            serde_json::json!({ "commandId": "cmd-c1", "name": "compact" }),
        ));
        assert!(map_mux_frame(&run, &sink).events.is_empty());

        let done = mux(session_event(
            "command/done",
            31,
            serde_json::json!({
                "commandId": "cmd-c1",
                "kind": "success",
                "text": "Compacted 12 history items (~45k tokens)."
            }),
        ));
        let mapped = map_mux_frame(&done, &sink);
        match &mapped.events[0] {
            ClientEvent::ContextCompacted { message } => {
                assert!(
                    message.contains("Compacted 12 history items"),
                    "message={message}"
                );
                assert!(message.starts_with("上下文压缩完成："), "message={message}");
            }
            other => panic!("expected ContextCompacted, got {other:?}"),
        }

        // The outcome is one-shot: a duplicate done for the same id is ignored.
        let again = map_mux_frame(&done, &sink);
        assert!(again.events.is_empty());
    }

    #[test]
    fn compact_command_done_error_surfaces_rejection() {
        let (sink, _rx) = test_sink();
        let run = mux(session_event(
            "command/run",
            30,
            serde_json::json!({ "commandId": "cmd-c2", "name": "compact" }),
        ));
        assert!(map_mux_frame(&run, &sink).events.is_empty());

        let done = mux(session_event(
            "command/done",
            31,
            serde_json::json!({
                "commandId": "cmd-c2",
                "kind": "error",
                "text": "Compaction is unavailable because this process has an active compaction, or the agent is not idle."
            }),
        ));
        let mapped = map_mux_frame(&done, &sink);
        match &mapped.events[0] {
            ClientEvent::ContextCompacted { message } => {
                assert!(message.contains("失败"), "message={message}");
                assert!(message.contains("not idle"), "message={message}");
            }
            other => panic!("expected ContextCompacted, got {other:?}"),
        }
    }

    #[test]
    fn command_done_for_other_commands_is_ignored() {
        let (sink, _rx) = test_sink();
        let frame = mux(session_event(
            "command/done",
            31,
            serde_json::json!({ "commandId": "cmd-other", "kind": "success", "text": "done" }),
        ));
        let mapped = map_mux_frame(&frame, &sink);
        assert!(mapped.events.is_empty());
    }

    #[test]
    fn maps_host_agent_error_to_interrupted() {
        let frame: HostFrame = serde_json::from_value(serde_json::json!({
            "type": "host/agent-error",
            "sessionId": "s-1",
            "message": "boom"
        }))
        .unwrap();
        let mapped = map_host_frame(&frame);
        assert_eq!(
            mapped.events,
            vec![ClientEvent::Interrupted {
                reason: "harness agent error: boom".to_string(),
            }]
        );
    }

    #[test]
    fn unknown_event_type_is_ignored_not_fatal() {
        // Additive harness schema change: a new event type we do not know must
        // not produce events or break the stream (12.7).
        let (sink, _rx) = test_sink();
        let frame = mux(session_event(
            "session/future-event",
            9,
            serde_json::json!({ "anything": true }),
        ));
        let mapped = map_mux_frame(&frame, &sink);
        assert!(mapped.events.is_empty());
        // last_seq still advances so re-baseline resumes past the unknown frame.
        assert_eq!(sink.last_seq.load(std::sync::atomic::Ordering::Acquire), 9);
    }

    #[test]
    fn unknown_tool_card_falls_back_to_generic() {
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/call",
                "seq": 10,
                "time": 0.0,
                "data": { "turn": 1, "step": 1, "callId": "call-9", "name": "future-tool", "arguments": "{}" }
            },
            "view": { "for": "call", "view": { "card": "future-card", "title": "x" } }
        }));
        let mapped = map_mux_frame(&frame, &sink);
        assert!(matches!(
            &mapped.events[0],
            ClientEvent::ToolStarted { id, .. } if id == "call-9"
        ));
    }

    #[test]
    fn read_result_view_renders_numbered_file_content() {
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/result",
                "seq": 11,
                "time": 0.0,
                "data": {
                    "turn": 1, "step": 1,
                    "message": {
                        "role": "user",
                        "content": [{ "type": "tool-result", "toolCallId": "call-10", "content": [] }]
                    }
                }
            },
            "view": { "for": "result", "view": { "card": "read", "path": "a.rs", "lines": [{ "number": 1, "text": "fn main() {}" }] } }
        }));
        let mapped = map_mux_frame(&frame, &sink);
        match &mapped.events[0] {
            ClientEvent::ToolCompleted {
                raw_output: Some(raw),
                ..
            } => {
                assert!(raw.contains("a.rs"), "path header missing: {raw}");
                assert!(
                    raw.contains("1 | fn main() {}"),
                    "numbered line missing: {raw}"
                );
                assert!(!raw.contains("\"lines\""), "must not be raw JSON: {raw}");
            }
            other => panic!("expected ToolCompleted with raw_output, got {other:?}"),
        }
    }

    #[test]
    fn unrepresentable_view_serializes_to_raw_output_json() {
        let (sink, _rx) = test_sink();
        let frame = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/result",
                "seq": 11,
                "time": 0.0,
                "data": {
                    "turn": 1, "step": 1,
                    "message": {
                        "role": "user",
                        "content": [{ "type": "tool-result", "toolCallId": "call-11", "content": [] }]
                    }
                }
            },
            "view": { "for": "result", "view": { "card": "search", "query": "foo", "hits": [] } }
        }));
        let mapped = map_mux_frame(&frame, &sink);
        assert!(matches!(
            &mapped.events[0],
            ClientEvent::ToolCompleted { raw_output: Some(raw), .. } if raw.contains("foo")
        ));
    }

    #[test]
    fn history_replay_reconstructs_client_event_sequence() {
        // 8.3: a fixture history page (assistant text + tool call + turn end)
        // reconstructs the expected ClientEvent sequence.
        let (sink, _rx) = test_sink();
        sink.set_replaying(true);
        let history = serde_json::json!([
            session_event(
                "assistant/message",
                1,
                serde_json::json!({
                    "turn": 1, "step": 1,
                    "message": {
                        "role": "assistant",
                        "content": [{ "type": "text", "text": "Working on it" }]
                    }
                })
            ),
            session_event(
                "tool/call",
                2,
                serde_json::json!({
                    "turn": 1, "step": 1, "callId": "call-1", "name": "bash", "arguments": "{\"command\":\"ls\"}"
                })
            ),
            session_event(
                "tool/result",
                3,
                serde_json::json!({
                    "turn": 1, "step": 1,
                    "message": {
                        "role": "user",
                        "content": [{ "type": "tool-result", "toolCallId": "call-1", "content": [{ "type": "text", "text": "done" }] }]
                    }
                })
            ),
            session_event(
                "turn/end",
                4,
                serde_json::json!({ "turn": 1, "reason": { "kind": "completed" } })
            ),
        ]);
        let frames: Vec<MuxFrame> = history
            .as_array()
            .unwrap()
            .iter()
            .cloned()
            .map(mux)
            .collect();
        let mut events = Vec::new();
        for frame in &frames {
            events.extend(map_mux_frame(frame, &sink).events);
        }
        // assistant message → (replayed tool/call is filtered — SQLite
        // already holds the row; replaying ToolStarted would reset it to
        // Running) → (replayed tool/result is filtered for the same reason)
        // → turn finished
        assert!(
            matches!(&events[0], ClientEvent::MessageChunk { role: MessageRole::Assistant, content } if content == "Working on it")
        );
        assert!(matches!(&events[1], ClientEvent::TurnFinished { .. }));
        // last_seq ends at the final event so a re-baseline resumes past it.
        assert_eq!(sink.last_seq.load(std::sync::atomic::Ordering::Acquire), 4);
    }

    #[test]
    fn nested_mcp_tool_result_payload_is_kept_as_raw_output_json() {
        // generate_image 类 MCP 工具的结果嵌在 tool-result 块里且不是 text
        // 块（JSON payload）：必须以 JSON 保留进 raw_output——生图卡的图片
        // 预览和展开的原始视图都从这里恢复 `images[].path`。
        let (sink, _rx) = test_sink();
        let result = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/result",
                "seq": 2,
                "time": 0.0,
                "data": {
                    "turn": 1, "step": 1,
                    "message": {
                        "role": "user",
                        "content": [{
                            "type": "tool-result",
                            "toolCallId": "call-img",
                            "content": [{
                                "type": "json",
                                "images": [{ "path": "file:///C:/x/.kodex/generated-images/a.png" }]
                            }]
                        }]
                    }
                }
            }
        }));
        let mapped = map_mux_frame(&result, &sink);
        let raw = mapped
            .events
            .iter()
            .find_map(|event| match event {
                ClientEvent::ToolCompleted { raw_output, .. } => raw_output.clone(),
                _ => None,
            })
            .expect("expected ToolCompleted carrying raw_output");
        assert!(raw.contains("generated-images"), "raw_output: {raw}");
    }

    #[test]
    fn v4_first_class_tool_result_completes_the_call() {
        // Session format v4 (dsh ≥ 0.1.7): the tool result is a first-class
        // `role: "tool"` message — `toolCallId`/`isError` live on the message
        // and `content` holds the payload blocks directly (no `tool-result`
        // wrapper). The completion must correlate with the running call and
        // carry the payload text as raw_output.
        let (sink, _rx) = test_sink();
        let call = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/call",
                "seq": 2,
                "time": 0.0,
                "data": { "turn": 1, "step": 1, "callId": "call-v4", "name": "bash", "arguments": "{\"command\":\"ls\"}" }
            }
        }));
        let _ = map_mux_frame(&call, &sink);

        let result = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/result",
                "seq": 3,
                "time": 0.0,
                "data": {
                    "turn": 1, "step": 1,
                    "message": {
                        "id": "msg-1",
                        "role": "tool",
                        "source": { "kind": "tool", "callId": "call-v4" },
                        "toolCallId": "call-v4",
                        "content": [{ "type": "text", "text": "file-a\nfile-b" }]
                    }
                }
            }
        }));
        let mapped = map_mux_frame(&result, &sink);
        let completed = mapped
            .events
            .iter()
            .find_map(|event| match event {
                ClientEvent::ToolCompleted {
                    id, raw_output, ..
                } => Some((id.clone(), raw_output.clone())),
                _ => None,
            })
            .expect("expected ToolCompleted for the v4 tool result");
        assert_eq!(completed.0, "call-v4", "completion must carry the call id");
        assert!(
            completed.1.as_deref().is_some_and(|raw| raw.contains("file-a")),
            "raw_output must carry the v4 payload text: {:?}",
            completed.1
        );
    }

    #[test]
    fn v4_tool_failure_surfaces_the_reason() {
        // v4 failure: `error.reason` is the raw user-facing reason and the
        // closer messages dsh synthesizes at a fork boundary rely on it.
        let (sink, _rx) = test_sink();
        let result = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/result",
                "seq": 3,
                "time": 0.0,
                "data": {
                    "turn": 1, "step": 1,
                    "message": {
                        "id": "msg-2",
                        "role": "tool",
                        "source": { "kind": "tool", "callId": "call-x" },
                        "toolCallId": "call-x",
                        "isError": true,
                        "content": [{ "type": "text", "text": "tool outcome is unknown" }]
                    },
                    "error": {
                        "name": "ToolOutcomeUnknownError",
                        "code": "tool/outcome-unknown",
                        "reason": "会话在工具结果落地前被分叉，该工具的结局未知。"
                    }
                }
            }
        }));
        let mapped = map_mux_frame(&result, &sink);
        let failed = mapped
            .events
            .iter()
            .find_map(|event| match event {
                ClientEvent::ToolFailed { id, error, .. } => Some((id.clone(), error.clone())),
                _ => None,
            })
            .expect("expected ToolFailed for the v4 error result");
        assert_eq!(failed.0, "call-x");
        assert_eq!(failed.1, "会话在工具结果落地前被分叉，该工具的结局未知。");
    }

    #[test]
    fn v4_tool_result_keeps_mcp_json_payload_as_raw_output() {
        // v4 shape + an MCP JSON payload block (generate_image): the payload
        // must survive into raw_output for the image preview.
        let (sink, _rx) = test_sink();
        let result = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/result",
                "seq": 3,
                "time": 0.0,
                "data": {
                    "turn": 1, "step": 1,
                    "message": {
                        "id": "msg-3",
                        "role": "tool",
                        "source": { "kind": "tool", "callId": "call-img" },
                        "toolCallId": "call-img",
                        "content": [{
                            "type": "json",
                            "images": [{ "path": "file:///x/.kodex/generated-images/a.png" }]
                        }]
                    }
                }
            }
        }));
        let mapped = map_mux_frame(&result, &sink);
        let raw = mapped
            .events
            .iter()
            .find_map(|event| match event {
                ClientEvent::ToolCompleted { raw_output, .. } => raw_output.clone(),
                _ => None,
            })
            .expect("expected ToolCompleted carrying raw_output");
        assert!(raw.contains("generated-images"), "raw_output: {raw}");
    }

    #[test]
    fn replayed_tool_result_keeps_sqlite_terminal_state() {
        // During history replay the tool row already exists in SQLite with its
        // terminal state. The replayed `tool/result` must not re-emit
        // `ToolCompleted`/`ToolFailed`, otherwise app-core `persist_event`
        // overwrites the persisted row back to Running. Any diff previews are
        // still forwarded so the card renders the patch surface.
        let (sink, _rx) = test_sink();
        sink.set_replaying(true);
        let call = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/call",
                "seq": 2,
                "time": 0.0,
                "data": { "turn": 1, "step": 1, "callId": "call-1", "name": "bash", "arguments": "{\"command\":\"ls\"}" }
            },
            "view": { "for": "call", "view": { "card": "generic", "title": "bash" } }
        }));
        let _ = map_mux_frame(&call, &sink);

        let result = mux(serde_json::json!({
            "type": "session/event",
            "sessionId": "s-1",
            "event": {
                "type": "tool/result",
                "seq": 3,
                "time": 0.0,
                "data": {
                    "turn": 1, "step": 1,
                    "message": {
                        "role": "user",
                        "content": [{ "type": "tool-result", "toolCallId": "call-1", "content": [{ "type": "text", "text": "done" }] }]
                    }
                }
            },
            "view": { "for": "result", "view": { "card": "terminal", "output": "done", "exitCode": 0 } }
        }));
        let mapped = map_mux_frame(&result, &sink);
        assert!(
            mapped.events.iter().all(|e| !matches!(
                e,
                ClientEvent::ToolCompleted { .. } | ClientEvent::ToolFailed { .. }
            )),
            "replay must not emit terminal tool events, got {events:?}",
            events = mapped.events,
        );
    }

    #[test]
    fn history_replay_does_not_duplicate_text_when_chunks_and_final_share_history() {
        let (sink, _rx) = test_sink();
        sink.set_replaying(true);
        let history = serde_json::json!([
            session_event(
                "assistant/chunk",
                1,
                serde_json::json!({
                    "turn": 1, "step": 1,
                    "chunk": { "type": "text-delta", "index": 0, "text": "Hello" }
                })
            ),
            session_event(
                "assistant/message",
                2,
                serde_json::json!({
                    "turn": 1, "step": 1,
                    "message": {
                        "role": "assistant",
                        "content": [{ "type": "text", "text": "Hello world" }]
                    }
                })
            ),
            session_event(
                "turn/end",
                3,
                serde_json::json!({ "turn": 1, "reason": { "kind": "completed" } })
            ),
        ]);
        let frames: Vec<MuxFrame> = history
            .as_array()
            .unwrap()
            .iter()
            .cloned()
            .map(mux)
            .collect();
        let mut events = Vec::new();
        for frame in &frames {
            events.extend(map_mux_frame(frame, &sink).events);
        }
        let assistant_texts: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                ClientEvent::MessageChunk {
                    role: MessageRole::Assistant,
                    content,
                    ..
                } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(assistant_texts, vec!["Hello"]);
        assert!(matches!(
            events.last(),
            Some(ClientEvent::TurnFinished { .. })
        ));
    }
}
