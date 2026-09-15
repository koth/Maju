//! Approval/question bridging: translate harness `approval/requested` /
//! `question/requested` into Kodex `ToolPermissionRequest`, and carry the
//! user's decision back to the harness.
//!
//! Two answer carriers exist, and the host generation picks both the carrier
//! and the *shape of the answer value* (see [`AnswerProtocol`]):
//!
//! * dsh ≥ 0.1.5 forwards approvals and questions as `$events` waterfalls, so
//!   both are answered through `/api/$events/result` with the bare resolved
//!   value (`{ answers }` for a question batch, the outcome string for an
//!   approval). The endpoint sits behind the shared Connection RPC envelope.
//! * dsh ≤ 0.1.4 answered questions through the same endpoint with a
//!   `{ sessionId, answer }` wrapper, and approvals through the legacy
//!   `/api/respond` `client-response` carrier.
//!
//! Pending entries are keyed by the dsh `rpcId`/`approvalId` (globally unique
//! UUID), stored in the session's own `PermissionBroker`-adjacent table on the
//! `SessionSink`.

use acp_core::{HarnessApprovalOutcome, HarnessApprovalResult, HarnessQuestionAnswer};

use crate::host::{PendingApprovalKind, SessionSink};
use serde_json::Value;

use crate::rpc_types::{
    AnswerProtocol, ApprovalOutcomeWire, ApprovalResponsePayload, AskUserQuestionAnswerItemWire,
    AskUserQuestionAnswerWire, QuestionResponsePayload, RemoteEventOutcome, RemoteEventRejection,
    RemoteEventResultArgs, RpcId,
};

/// One answer ready to send, rendered for the protocol in use.
#[derive(Debug, Clone)]
pub enum AnswerRequest {
    /// POST `/api/$events/result` with these `{ clientId, eventId, outcome }`
    /// args (the forwarded-waterfall carrier).
    Waterfall(Value),
    /// POST the legacy `/api/respond` carrier with this `client-response` body.
    Legacy(Value),
}

impl AnswerRequest {
    /// The exact wire body, for logging and assertions.
    pub fn body(&self) -> &Value {
        match self {
            AnswerRequest::Waterfall(value) | AnswerRequest::Legacy(value) => value,
        }
    }
}

/// Snapshot of a session's pending approvals/questions, used by the session
/// loop to build the answer payload for a `ResolveHarnessApproval`.
#[derive(Debug, Default, Clone)]
pub struct PendingApprovals {
    entries: Vec<PendingEntryView>,
    waterfall_event_ids: Vec<(String, RpcId)>,
}

#[derive(Debug, Clone)]
struct PendingEntryView {
    pub ui_id: String,
    pub kind: PendingApprovalKind,
    pub approval_id: String,
}

impl PendingApprovals {
    pub fn from_entries(
        entries: Vec<crate::host::PendingEntry>,
        waterfall_event_ids: Vec<(String, RpcId)>,
    ) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|e| PendingEntryView {
                    ui_id: e.ui_id,
                    kind: e.kind,
                    approval_id: e.approval_id,
                })
                .collect(),
            waterfall_event_ids,
        }
    }

    /// Build the answer for a resolved approval/question, for the protocol the
    /// host speaks. Returns `None` when no pending entry matches `rpc_id` or the
    /// UI sent the wrong result shape for this id.
    pub fn build_response(
        &self,
        sink: &SessionSink,
        remote_event_client_id: Option<String>,
        protocol: AnswerProtocol,
        rpc_id: &str,
        result: &HarnessApprovalResult,
    ) -> Option<AnswerRequest> {
        let session_id = sink.session_id()?;
        // Find the pending entry by the ui_id (== approvalId for approvals; ==
        // first question id for questions).
        let entry = self.entries.iter().find(|e| e.ui_id == rpc_id)?;
        match (entry.kind, result) {
            (
                PendingApprovalKind::Approval,
                HarnessApprovalResult::Approval {
                    approval_id,
                    outcome,
                },
            ) => {
                let wire_outcome = match outcome {
                    HarnessApprovalOutcome::AllowedOnce => ApprovalOutcomeWire::AllowedOnce,
                    HarnessApprovalOutcome::Rejected => ApprovalOutcomeWire::Rejected,
                };
                if protocol == AnswerProtocol::Envelope {
                    // dsh ≥ 0.1.5: the forwarded `approval/request` waterfall
                    // resolves with the bare closed outcome. The waterfall's
                    // `eventId` is the request id the UI was given (the request
                    // carries no approval id of its own any more).
                    let event_id = self
                        .waterfall_event_id(rpc_id)
                        .unwrap_or_else(|| rpc_id.to_string());
                    let value = match wire_outcome {
                        ApprovalOutcomeWire::AllowedOnce => "allowed-once",
                        ApprovalOutcomeWire::Rejected => "rejected",
                    };
                    return Self::waterfall_args(
                        remote_event_client_id?,
                        event_id,
                        Value::String(value.to_string()),
                    )
                    .map(AnswerRequest::Waterfall);
                }
                let payload = ApprovalResponsePayload {
                    session_id,
                    approval_id: approval_id.clone(),
                    outcome: wire_outcome,
                };
                serde_json::to_value(payload).ok().map(|value| {
                    AnswerRequest::Legacy(serde_json::json!({
                        "type": "client-response",
                        "rpcId": rpc_id,
                        "result": { "ok": true, "value": value }
                    }))
                })
            }
            (PendingApprovalKind::Question, HarnessApprovalResult::Question { answers }) => {
                // dsh's answer validation is POSITIONAL (`answer[i].id ===
                // questions[i].id`), so the batch must be re-ordered to the
                // exact question order before sending — the UI's answers arrive
                // keyed by id (a map), whose iteration order need not match the
                // question order.
                let order = sink.question_order(rpc_id);
                let mut sorted: Vec<&HarnessQuestionAnswer> = answers.iter().collect();
                if !order.is_empty() {
                    sorted.sort_by_key(|a| {
                        order
                            .iter()
                            .position(|id| id == &a.question_id)
                            .unwrap_or(usize::MAX)
                    });
                }
                let wire_answers: Vec<AskUserQuestionAnswerItemWire> = sorted
                    .iter()
                    .map(|a| AskUserQuestionAnswerItemWire {
                        id: a.question_id.clone(),
                        selected: a.selected.clone(),
                        custom: a.custom.clone(),
                    })
                    .collect();
                // dsh ≤ 0.1.4 resolved the question waterfall with a
                // `{ sessionId, answer }` payload that its `matchesQuestions`
                // unwrapped; dsh ≥ 0.1.5 resolves it with the bare
                // `AskUserQuestionAnswer` the tool feeds straight back to the
                // model.
                let value = if protocol == AnswerProtocol::Envelope {
                    serde_json::json!({ "answers": wire_answers })
                } else {
                    let payload = QuestionResponsePayload {
                        session_id,
                        answer: AskUserQuestionAnswerWire {
                            answers: wire_answers,
                        },
                    };
                    serde_json::to_value(payload).ok()?
                };
                // The event id is the `$events` waterfall's `eventId`, not the
                // UI-facing question id: `question/resolved` and the answer
                // correlation both name the batch by the waterfall event.
                let event_id = self.waterfall_event_id(rpc_id)?;
                Self::waterfall_args(remote_event_client_id?, event_id, value)
                    .map(AnswerRequest::Waterfall)
            }
            // The user dismissed the question batch. dsh resolves a cancelled
            // ask by *rejecting* the waterfall, not by answering it: its own Web
            // client rejects with `UserQuestionError`/`ASK_CANCELLED`, and the
            // harness then fails the `ask_user_question` call (the model gets an
            // abort reason) and clears the pending entry. Sending anything else
            // here leaves the ask pending, so the question panel kept coming
            // back after 取消.
            (PendingApprovalKind::Question, HarnessApprovalResult::QuestionCancelled) => {
                let event_id = self.waterfall_event_id(rpc_id)?;
                Self::waterfall_rejection_args(remote_event_client_id?, event_id)
                    .map(AnswerRequest::Waterfall)
            }
            // Kind/result mismatch — the UI sent the wrong shape for this id.
            _ => None,
        }
    }

    /// The waterfall `eventId` recorded for one UI-facing request id.
    fn waterfall_event_id(&self, ui_id: &str) -> Option<String> {
        self.waterfall_event_ids
            .iter()
            .find(|(id, _)| id == ui_id)
            .map(|(_, event_id)| event_id.clone())
    }

    /// The `$events/result` args for one resolved waterfall.
    fn waterfall_args(client_id: String, event_id: String, value: Value) -> Option<Value> {
        serde_json::to_value(RemoteEventResultArgs {
            client_id,
            event_id,
            outcome: RemoteEventOutcome::Result { value },
        })
        .ok()
    }

    /// The `$events/result` args that refuse one waterfall, using the exact
    /// error dsh's own question UI rejects with so the harness maps it to the
    /// same `ask_user_question` abort instead of treating it as a bad answer.
    fn waterfall_rejection_args(client_id: String, event_id: String) -> Option<Value> {
        serde_json::to_value(RemoteEventResultArgs {
            client_id,
            event_id,
            outcome: RemoteEventOutcome::Rejected {
                error: RemoteEventRejection {
                    name: "UserQuestionError".to_string(),
                    message: "the user cancelled ask_user_question".to_string(),
                    code: Some("ASK_CANCELLED".to_string()),
                },
            },
        })
        .ok()
    }
}
