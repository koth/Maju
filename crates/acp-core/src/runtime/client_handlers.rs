use super::agent_process::AgentTransport;
use super::permissions::{PermissionBroker, PermissionDecision, decide_permission};
use super::terminal::TerminalManager;
use super::workspace_paths::{
    read_workspace_text_file, validate_workspace_path, write_workspace_text_file,
};
use super::{RuntimeCommand, ShutdownSignal};
use crate::events::{ClientEvent, SessionConfig};
use crate::mapping::{append_runtime_event_log, format_permission_options};
use agent_client_protocol::schema::{
    ContentBlock, CreateTerminalRequest, KillTerminalRequest, ReadTextFileRequest,
    ReadTextFileResponse, ReleaseTerminalRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    TerminalOutputRequest, ToolCallContent, WaitForTerminalExitRequest, WriteTextFileRequest,
    WriteTextFileResponse,
};
use agent_client_protocol::{Agent, Client, ConnectionTo};
use anyhow::anyhow;
use serde_json::Value;
use std::fs;
use std::sync::{Arc, mpsc};
use workspace_model::PermissionOption;

pub(super) async fn connect_agent_client(
    agent: AgentTransport,
    config: SessionConfig,
    tx_events: mpsc::Sender<ClientEvent>,
    rx_commands: mpsc::Receiver<RuntimeCommand>,
    permission_broker: PermissionBroker,
    shutdown_signal: ShutdownSignal,
) -> anyhow::Result<()> {
    let tx_permissions = tx_events.clone();
    let permission_log_config = config.clone();
    let permission_workspace_root = config.workspace_root.clone();
    let permission_request_broker = permission_broker.clone();
    let fs_log_config = config.clone();
    let fs_write_log_config = config.clone();
    let fs_write_tx = tx_events.clone();
    let fs_write_workspace_root = config.workspace_root.clone();
    let fs_read_workspace_root = config.workspace_root.clone();
    let terminal_manager = Arc::new(TerminalManager::default());
    let terminal_create_log_config = config.clone();
    let terminal_output_log_config = config.clone();
    let terminal_wait_log_config = config.clone();
    let terminal_kill_log_config = config.clone();
    let terminal_release_log_config = config.clone();
    let terminal_create_manager = terminal_manager.clone();
    let terminal_output_manager = terminal_manager.clone();
    let terminal_wait_manager = terminal_manager.clone();
    let terminal_kill_manager = terminal_manager.clone();
    let terminal_release_manager = terminal_manager.clone();

    Client
        .builder()
        .name("acp-editor")
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _connection| {
                let request_payload =
                    serde_json::to_value(&request).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &permission_log_config,
                    "client/request_permission",
                    &request_payload,
                )?;

                let request_id = request.tool_call.tool_call_id.0.to_string();
                let options: Vec<PermissionOption> = request
                    .options
                    .iter()
                    .map(|option| option.name.clone())
                    .enumerate()
                    .map(|(index, label)| PermissionOption {
                        id: request.options[index].option_id.0.to_string(),
                        label,
                        kind: format!("{:?}", request.options[index].kind),
                    })
                    .collect();
                let option_labels = options
                    .iter()
                    .map(|option| option.label.clone())
                    .collect::<Vec<_>>();
                let decision = if is_codebuddy_exit_plan_permission(&request_payload) {
                    PermissionDecision::Ask
                } else {
                    decide_permission(
                        permission_request_broker.mode(),
                        &permission_workspace_root,
                        &request,
                    )
                };
                let selected_option_id = match decision {
                    PermissionDecision::Select(option_id) => Some(option_id),
                    PermissionDecision::Cancel => None,
                    PermissionDecision::Ask => {
                        let reply_rx = permission_request_broker.register(request_id.clone())?;

                        let _ = tx_permissions.send(ClientEvent::ToolPermissionRequest {
                            id: request_id.clone(),
                            name: permission_request_name(&request, &request_payload),
                            options: options.clone(),
                            details: permission_request_details(&request),
                        });
                        let _ = tx_permissions.send(ClientEvent::ToolProgress {
                            id: request_id.clone(),
                            content: format_permission_options(&option_labels),
                        });
                        reply_rx.recv().ok().flatten()
                    }
                };
                let selected_option = selected_option_id.as_deref().and_then(|option_id| {
                    request
                        .options
                        .iter()
                        .find(|option| option.option_id.0.as_ref() == option_id)
                });

                let response = match selected_option {
                    Some(option) => {
                        RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
                            SelectedPermissionOutcome::new(option.option_id.clone()),
                        ))
                    }
                    None => RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled),
                };

                let outcome = match &response.outcome {
                    RequestPermissionOutcome::Selected(selected) => {
                        let label = options
                            .iter()
                            .find(|option| option.id == selected.option_id.0.as_ref())
                            .map(|option| option.label.as_str())
                            .unwrap_or(selected.option_id.0.as_ref());
                        format!("Permission selected: {label}")
                    }
                    RequestPermissionOutcome::Cancelled => "Permission request cancelled".into(),
                    _ => "Permission handled".into(),
                };

                let _ = tx_permissions.send(ClientEvent::ToolPermissionResolved {
                    id: request_id,
                    outcome,
                });

                let response_payload =
                    serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &permission_log_config,
                    "client/request_permission_response",
                    &response_payload,
                )?;

                responder.respond(response)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: ReadTextFileRequest, responder, _connection| {
                let request_payload =
                    serde_json::to_value(&request).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &fs_log_config,
                    "client/fs_read_text_file",
                    &request_payload,
                )?;

                let content = read_workspace_text_file(&fs_read_workspace_root, &request)?;
                let response = ReadTextFileResponse::new(content);
                let response_payload =
                    serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &fs_log_config,
                    "client/fs_read_text_file_response",
                    &response_payload,
                )?;

                responder.respond(response)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: WriteTextFileRequest, responder, _connection| {
                let request_payload =
                    serde_json::to_value(&request).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &fs_write_log_config,
                    "client/fs_write_text_file",
                    &request_payload,
                )?;

                // Read old content before writing for diff tracking
                let path_for_diff =
                    validate_workspace_path(&fs_write_workspace_root, &request.path).ok();
                let old_text = path_for_diff
                    .as_ref()
                    .and_then(|p| fs::read_to_string(p).ok());

                write_workspace_text_file(&fs_write_log_config.workspace_root, &request)?;

                // Emit ToolDiff event so session changes can be tracked
                let path_display = path_for_diff
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| request.path.display().to_string());
                let _ = fs_write_tx.send(ClientEvent::ToolDiff {
                    id: format!("fs_write:{}", path_display),
                    path: path_display,
                    old_text,
                    new_text: request.content.clone(),
                });

                let response = WriteTextFileResponse::new();
                let response_payload =
                    serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &fs_write_log_config,
                    "client/fs_write_text_file_response",
                    &response_payload,
                )?;

                responder.respond(response)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: CreateTerminalRequest, responder, _connection| {
                let request_payload =
                    serde_json::to_value(&request).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &terminal_create_log_config,
                    "client/terminal_create",
                    &request_payload,
                )?;

                let response = terminal_create_manager
                    .create_terminal(&terminal_create_log_config.workspace_root, &request)?;
                let response_payload =
                    serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &terminal_create_log_config,
                    "client/terminal_create_response",
                    &response_payload,
                )?;

                responder.respond(response)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: TerminalOutputRequest, responder, _connection| {
                let request_payload =
                    serde_json::to_value(&request).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &terminal_output_log_config,
                    "client/terminal_output",
                    &request_payload,
                )?;

                let response = terminal_output_manager.terminal_output(&request)?;
                let response_payload =
                    serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &terminal_output_log_config,
                    "client/terminal_output_response",
                    &response_payload,
                )?;

                responder.respond(response)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: WaitForTerminalExitRequest, responder, _connection| {
                let request_payload =
                    serde_json::to_value(&request).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &terminal_wait_log_config,
                    "client/terminal_wait_for_exit",
                    &request_payload,
                )?;

                let response = terminal_wait_manager.wait_for_terminal_exit(&request)?;
                let response_payload =
                    serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &terminal_wait_log_config,
                    "client/terminal_wait_for_exit_response",
                    &response_payload,
                )?;

                responder.respond(response)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: KillTerminalRequest, responder, _connection| {
                let request_payload =
                    serde_json::to_value(&request).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &terminal_kill_log_config,
                    "client/terminal_kill",
                    &request_payload,
                )?;

                let response = terminal_kill_manager.kill_terminal(&request)?;
                let response_payload =
                    serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &terminal_kill_log_config,
                    "client/terminal_kill_response",
                    &response_payload,
                )?;

                responder.respond(response)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: ReleaseTerminalRequest, responder, _connection| {
                let request_payload =
                    serde_json::to_value(&request).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &terminal_release_log_config,
                    "client/terminal_release",
                    &request_payload,
                )?;

                let response = terminal_release_manager.release_terminal(&request)?;
                let response_payload =
                    serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &terminal_release_log_config,
                    "client/terminal_release_response",
                    &response_payload,
                )?;

                responder.respond(response)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent, move |connection: ConnectionTo<Agent>| {
            let tx_events = tx_events.clone();
            let config = config.clone();
            async move {
                let super::session_lifecycle::StartedSession {
                    mut session,
                    supports_session_list,
                    prompt_capabilities,
                } = super::session_lifecycle::start_session(&connection, &config, &tx_events)
                    .await?;
                super::prompt_loop::run_command_loop(
                    &mut session,
                    &config,
                    &tx_events,
                    rx_commands,
                    permission_broker,
                    shutdown_signal,
                    supports_session_list,
                    prompt_capabilities,
                )
                .await?;

                Ok(())
            }
        })
        .await
        .map_err(|err| anyhow!(err.to_string()))?;

    Ok(())
}

fn permission_request_details(request: &RequestPermissionRequest) -> Option<String> {
    let mut parts = Vec::new();

    if let Some(content) = &request.tool_call.fields.content {
        for item in content {
            if let ToolCallContent::Content(content) = item
                && let ContentBlock::Text(text) = &content.content
            {
                let trimmed = text.text.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed.to_string());
                }
            }
        }
    }

    if parts.is_empty()
        && let Some(raw_input) = &request.tool_call.fields.raw_input
        && let Some(plan) = raw_input.get("plan").and_then(serde_json::Value::as_str)
    {
        let trimmed = plan.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }

    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

fn permission_request_name(request: &RequestPermissionRequest, payload: &Value) -> String {
    request
        .tool_call
        .fields
        .title
        .as_ref()
        .filter(|title| !title.trim().is_empty())
        .cloned()
        .or_else(|| codebuddy_permission_tool_name(payload))
        .unwrap_or_else(|| "Permission request".into())
}

fn codebuddy_permission_tool_name(request: &Value) -> Option<String> {
    request
        .get("toolCall")
        .and_then(|tool_call| tool_call.get("_meta"))
        .and_then(|meta| meta.get("codebuddy.ai/toolName"))
        .and_then(Value::as_str)
        .filter(|tool_name| !tool_name.trim().is_empty())
        .map(str::to_string)
}

fn is_codebuddy_exit_plan_permission(request: &Value) -> bool {
    codebuddy_permission_tool_name(request)
        .as_deref()
        .is_some_and(|tool_name| tool_name.eq_ignore_ascii_case("ExitPlanMode"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codebuddy_exit_plan_permission_is_detected_from_meta() {
        let payload = json!({
            "toolCall": {
                "_meta": {
                    "codebuddy.ai/toolName": "ExitPlanMode"
                },
                "rawInput": {
                    "allowedPrompts": []
                },
                "toolCallId": "call_exit_plan"
            }
        });

        assert!(is_codebuddy_exit_plan_permission(&payload));
    }

    #[test]
    fn ordinary_codebuddy_permission_is_not_exit_plan() {
        let payload = json!({
            "toolCall": {
                "_meta": {
                    "codebuddy.ai/toolName": "Bash"
                },
                "toolCallId": "call_bash"
            }
        });

        assert!(!is_codebuddy_exit_plan_permission(&payload));
    }

    #[test]
    fn codebuddy_permission_tool_name_falls_back_to_meta_tool_name() {
        let payload = json!({
            "toolCall": {
                "_meta": {
                    "codebuddy.ai/toolName": "Edit"
                },
                "toolCallId": "call_edit"
            }
        });

        assert_eq!(
            codebuddy_permission_tool_name(&payload).as_deref(),
            Some("Edit")
        );
    }
}
