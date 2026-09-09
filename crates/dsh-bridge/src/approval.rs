//! Approval/question bridging: translate harness `approval/requested` /
//! `question/requested` into Kodex `ToolPermissionRequest`, and carry the
//! user's decision back to the harness (`/api/$events/result` for questions;
//! approvals retain the legacy `/api/respond` carrier).
//!
//! Pending entries are keyed by the dsh `rpcId`/`approvalId` (globally unique
//! UUID), stored in the session's own `PermissionBroker`-adjacent table on the
//! `SessionSink`. The harness's global pending registry cross-checks
//! `sessionId` on respond, so a misrouted answer is rejected as `bad-response`.

use acp_core::{HarnessApprovalOutcome, HarnessApprovalResult, HarnessQuestionAnswer};

use crate::host::{PendingApprovalKind, SessionSink};
use serde_json::Value;

use crate::rpc_types::{
    ApprovalOutcomeWire, ApprovalResponsePayload, AskUserQuestionAnswerItemWire,
    AskUserQuestionAnswerWire, QuestionResponsePayload, RemoteEventOutcome, RemoteEventResultArgs,
    RpcId,
};

/// Snapshot of a session's pending approvals/questions, used by the session
/// loop to build the `/api/respond` payload for a `ResolveHarnessApproval`.
#[derive(Debug, Default, Clone)]
pub struct PendingApprovals {
    entries: Vec<PendingEntryView>,
    question_event_ids: Vec<(String, RpcId)>,
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
        question_event_ids: Vec<(String, RpcId)>,
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
            question_event_ids,
        }
    }

    /// Build the wire response for a resolved approval/question. Approvals use
    /// the legacy `client-response` envelope; questions use the gateway's
    /// strict `$events/result` args.
    pub fn build_response(
        &self,
        sink: &SessionSink,
        remote_event_client_id: Option<String>,
        rpc_id: &str,
        result: &HarnessApprovalResult,
    ) -> Option<Value> {
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
                let payload = ApprovalResponsePayload {
                    session_id,
                    approval_id: approval_id.clone(),
                    outcome: wire_outcome,
                };
                serde_json::to_value(payload).ok().map(|value| {
                    serde_json::json!({
                        "type": "client-response",
                        "rpcId": rpc_id,
                        "result": { "ok": true, "value": value }
                    })
                })
            }
            (PendingApprovalKind::Question, HarnessApprovalResult::Question { answers }) => {
                // The event id is the `$events` waterfall's `eventId`, not the
                // UI-facing question id. dsh 0.1.2 resolves the waterfall by
                // `(clientId, eventId)` and validates outcome/value strictly.
                let event_id = self
                    .question_event_ids
                    .iter()
                    .find(|(id, _)| id == rpc_id)
                    .map(|(_, event_id)| event_id.clone())?;
                let client_id = remote_event_client_id.unwrap_or_default();
                // dsh's `matchesQuestions` validates answers POSITIONALLY
                // (`answer[i].id === questions[i].id`), so the batch must be
                // re-ordered to the exact question order before sending —
                // the UI's answers arrive keyed by id (a map), whose
                // iteration order does not match the question order.
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
                let payload = QuestionResponsePayload {
                    session_id,
                    answer: AskUserQuestionAnswerWire {
                        answers: wire_answers,
                    },
                };
                let args = RemoteEventResultArgs {
                    client_id,
                    event_id,
                    outcome: RemoteEventOutcome {
                        kind: "result",
                        value: serde_json::to_value(payload).ok()?,
                    },
                };
                serde_json::to_value(args).ok()
            }
            // Kind/result mismatch — the UI sent the wrong shape for this id.
            _ => None,
        }
    }
}
