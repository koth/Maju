use crate::events::{ClientEvent, SessionConfig};
use crate::mapping::{append_runtime_event_log, format_stop_reason};
use agent_client_protocol::schema::{
    AgentCapabilities, ListSessionsRequest, SessionId, SessionInfo, StopReason,
};
use agent_client_protocol::{Agent, ConnectionTo};
use anyhow::anyhow;
use serde_json::json;
use std::path::PathBuf;
use std::sync::mpsc;

const TITLE_SYNC_RETRY_DELAYS_MS: [u64; 6] = [120, 400, 900, 2_000, 5_000, 10_000];
const TITLE_SYNC_TIMEOUT_MS: u64 = 2_000;

pub(super) async fn emit_turn_finished(
    config: &SessionConfig,
    tx_events: &mpsc::Sender<ClientEvent>,
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
    supports_session_list: bool,
    reason: StopReason,
) -> anyhow::Result<()> {
    let stop_reason = format_stop_reason(reason);
    append_runtime_event_log(
        config,
        "session/stop_reason",
        &json!({ "stopReason": stop_reason.clone() }),
    )?;

    let _ = tx_events.send(ClientEvent::TurnFinished { stop_reason });
    if supports_session_list {
        sync_session_title_from_list_after_turn(config, tx_events, connection, session_id).await;
    }
    Ok(())
}

pub(super) fn advertised_session_list_capability(agent_capabilities: &AgentCapabilities) -> bool {
    agent_capabilities.session_capabilities.list.is_some()
}

pub(super) fn command_implies_codex_session_list(config: &SessionConfig) -> bool {
    config
        .agent_command
        .to_ascii_lowercase()
        .contains("codex-acp")
}

pub(super) fn supports_session_list_title_sync(
    config: &SessionConfig,
    advertised_session_list: bool,
) -> bool {
    if command_uses_claude_agent_titles(config) {
        return false;
    }

    advertised_session_list || command_implies_codex_session_list(config)
}

fn command_uses_claude_agent_titles(config: &SessionConfig) -> bool {
    let command = config.agent_command.to_ascii_lowercase();
    command.contains("claude-agent-acp") || command.contains("claude-acp")
}

pub(super) async fn sync_session_title_from_list(
    config: &SessionConfig,
    tx_events: &mpsc::Sender<ClientEvent>,
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
) -> anyhow::Result<bool> {
    append_runtime_event_log(
        config,
        "session/list_title_sync_start",
        &json!({
            "sessionId": session_id.0.as_ref(),
            "cwd": config.workspace_root,
            "timeoutMs": TITLE_SYNC_TIMEOUT_MS,
        }),
    )?;
    let request = ListSessionsRequest::new().cwd(PathBuf::from(&config.workspace_root));
    let response = tokio::time::timeout(
        std::time::Duration::from_millis(TITLE_SYNC_TIMEOUT_MS),
        connection.send_request_to(Agent, request).block_task(),
    )
    .await
    .map_err(|_| anyhow!("session/list timed out after {TITLE_SYNC_TIMEOUT_MS}ms"))?
    .map_err(|err| anyhow!(err.to_string()))?;
    let session_count = response.sessions.len();

    let title = select_session_title_for_sync(&response.sessions, session_id);
    let matched = response
        .sessions
        .iter()
        .find(|session| session.session_id == *session_id);

    if let Some((title, matched_by)) = title {
        append_runtime_event_log(
            config,
            "session/list_title_sync",
            &json!({
                "sessionId": session_id.0.as_ref(),
                "title": title,
                "matchedBy": matched_by,
            }),
        )?;
        let _ = tx_events.send(ClientEvent::SessionTitleUpdated { title });
        return Ok(true);
    }

    append_runtime_event_log(
        config,
        "session/list_title_sync_empty",
        &json!({
            "sessionId": session_id.0.as_ref(),
            "sessionCount": session_count,
            "matched": matched.is_some(),
            "hasTitle": matched.and_then(|session| session.title.as_ref()).is_some(),
        }),
    )?;

    Ok(false)
}

pub(super) async fn sync_session_title_from_list_after_turn(
    config: &SessionConfig,
    tx_events: &mpsc::Sender<ClientEvent>,
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
) {
    match sync_session_title_from_list(config, tx_events, connection, session_id).await {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            let _ = append_runtime_event_log(
                config,
                "session/list_title_sync_failed",
                &json!({ "error": error.to_string() }),
            );
            return;
        }
    }

    for delay_ms in TITLE_SYNC_RETRY_DELAYS_MS {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        match sync_session_title_from_list(config, tx_events, connection, session_id).await {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                let _ = append_runtime_event_log(
                    config,
                    "session/list_title_sync_retry_failed",
                    &json!({ "delayMs": delay_ms, "error": error.to_string() }),
                );
                return;
            }
        }
    }
}

pub(super) fn select_session_title_for_sync(
    sessions: &[SessionInfo],
    session_id: &SessionId,
) -> Option<(String, &'static str)> {
    sessions
        .iter()
        .find(|session| session.session_id == *session_id)
        .and_then(trimmed_session_title)
        .map(|title| (title, "sessionId"))
}

fn trimmed_session_title(session: &SessionInfo) -> Option<String> {
    let title = session.title.as_deref()?.trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}
