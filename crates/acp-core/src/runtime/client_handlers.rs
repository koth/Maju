use super::agent_process::AgentTransport;
use super::codebuddy::{send_codebuddy_interruption_resolution, send_codebuddy_plan_guidance};
use super::permissions::{
    CodeBuddyTerminalPermissionDecision, PermissionBroker, PermissionDecision,
    PermissionPolicyMode, PermissionResolution, apply_patch_retry_guidance,
    codebuddy_bash_write_hint_paths, decide_codebuddy_terminal_permission,
    decide_permission_with_edit_policy, path_prefers_apply_patch,
    shell_command_prefers_apply_patch_for_writes,
};
use super::terminal::TerminalManager;
use super::tool_stop::{ToolExecutionRegistry, terminal_tool_call_id_from_request_payload};
use super::workspace_paths::{
    read_remote_workspace_text_file, read_workspace_text_file, validate_client_file_path,
    validate_remote_client_file_path, write_remote_workspace_text_file, write_workspace_text_file,
};
use super::{RuntimeCommand, ShutdownSignal};
use crate::events::{AgentEditPolicy, ClientEvent, SessionConfig, agent_edit_policy_for_command};
use crate::mapping::{append_runtime_event_log, format_permission_options};
use agent_client_protocol::schema::{
    ContentBlock, CreateTerminalRequest, KillTerminalRequest, Meta,
    PermissionOptionKind as AcpPermissionOptionKind, ReadTextFileRequest, ReadTextFileResponse,
    ReleaseTerminalRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, TerminalOutputRequest, ToolCallContent,
    WaitForTerminalExitRequest, WriteTextFileRequest, WriteTextFileResponse,
};
use agent_client_protocol::{Agent, Client, ConnectionTo};
use anyhow::anyhow;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime};
use uuid::Uuid;
use workspace_model::{
    PermissionInputOption, PermissionInputQuestion, PermissionInputRequest, PermissionOption,
};

const KODEX_PERMISSION_GUIDANCE_META_KEY: &str = "kodex.ai/permissionGuidance";
const KODEX_USER_INPUT_ANSWERS_META_KEY: &str = "kodex.ai/userInputAnswers";
#[derive(Clone, Default)]
struct CodeBuddyTerminalDenials {
    inner: Arc<Mutex<Vec<CodeBuddyTerminalDenial>>>,
}

#[derive(Clone, Default)]
struct CodeBuddyTerminalApprovals {
    inner: Arc<Mutex<Vec<CodeBuddyTerminalApproval>>>,
}

#[derive(Clone, Default)]
struct FsWriteApprovals {
    inner: Arc<Mutex<Vec<PathBuf>>>,
}

struct CodeBuddyTerminalDenial {
    command: String,
    reason: String,
}

struct CodeBuddyTerminalApproval {
    command: String,
    paths: Vec<PathBuf>,
}

enum CodeBuddyTerminalCreateGate {
    Allow,
    Deny { reason: String },
}

impl CodeBuddyTerminalDenials {
    fn register(
        &self,
        command: impl Into<String>,
        reason: impl Into<String>,
    ) -> anyhow::Result<()> {
        let command = normalize_codebuddy_command(&command.into());
        let reason = reason.into();
        let mut denials = self
            .inner
            .lock()
            .map_err(|_| anyhow!("CodeBuddy terminal denial registry poisoned"))?;
        denials.push(CodeBuddyTerminalDenial { command, reason });
        Ok(())
    }

    fn take(&self, command: &str) -> anyhow::Result<Option<String>> {
        let command = normalize_codebuddy_command(command);
        let mut denials = self
            .inner
            .lock()
            .map_err(|_| anyhow!("CodeBuddy terminal denial registry poisoned"))?;
        let Some(index) = denials.iter().position(|denial| denial.command == command) else {
            return Ok(None);
        };
        Ok(Some(denials.remove(index).reason))
    }
}

impl CodeBuddyTerminalApprovals {
    fn register(&self, command: impl Into<String>, paths: Vec<PathBuf>) -> anyhow::Result<()> {
        let command = normalize_codebuddy_command(&command.into());
        let mut approvals = self
            .inner
            .lock()
            .map_err(|_| anyhow!("CodeBuddy terminal approval registry poisoned"))?;
        approvals.push(CodeBuddyTerminalApproval { command, paths });
        Ok(())
    }

    fn take(&self, command: &str) -> anyhow::Result<Option<Vec<PathBuf>>> {
        let command = normalize_codebuddy_command(command);
        let mut approvals = self
            .inner
            .lock()
            .map_err(|_| anyhow!("CodeBuddy terminal approval registry poisoned"))?;
        let Some(index) = approvals
            .iter()
            .position(|approval| approval.command == command)
        else {
            return Ok(None);
        };
        Ok(Some(approvals.remove(index).paths))
    }
}

impl FsWriteApprovals {
    fn register(&self, paths: Vec<PathBuf>) -> anyhow::Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut approvals = self
            .inner
            .lock()
            .map_err(|_| anyhow!("file write approval registry poisoned"))?;
        for path in paths {
            if !approvals.iter().any(|existing| existing == &path) {
                approvals.push(path);
            }
        }
        Ok(())
    }

    fn take(&self, workspace_root: &str, path: &Path) -> anyhow::Result<bool> {
        let mut approvals = self
            .inner
            .lock()
            .map_err(|_| anyhow!("file write approval registry poisoned"))?;
        let Some(index) = approvals
            .iter()
            .position(|approval| fs_write_approval_path_matches(workspace_root, approval, path))
        else {
            return Ok(false);
        };
        approvals.remove(index);
        Ok(true)
    }
}

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
    let fs_write_permission_broker = permission_broker.clone();
    let fs_write_remote_ssh = config.remote_ssh.clone();
    let fs_read_workspace_root = config.workspace_root.clone();
    let fs_read_remote_ssh = config.remote_ssh.clone();
    let terminal_manager = Arc::new(TerminalManager::default());
    let tool_execution_registry = ToolExecutionRegistry::default();
    let terminal_create_log_config = config.clone();
    let terminal_create_agent_command = config.agent_command.clone();
    let terminal_create_workspace_root = config.workspace_root.clone();
    let terminal_create_remote_ssh = config.remote_ssh.clone();
    let terminal_create_tx = tx_events.clone();
    let terminal_create_permission_broker = permission_broker.clone();
    let terminal_output_log_config = config.clone();
    let terminal_wait_log_config = config.clone();
    let terminal_kill_log_config = config.clone();
    let terminal_release_log_config = config.clone();
    let terminal_release_tx = tx_events.clone();
    let terminal_create_manager = terminal_manager.clone();
    let terminal_output_manager = terminal_manager.clone();
    let terminal_wait_manager = terminal_manager.clone();
    let terminal_kill_manager = terminal_manager.clone();
    let terminal_release_manager = terminal_manager.clone();
    let permission_tool_registry = tool_execution_registry.clone();
    let terminal_create_tool_registry = tool_execution_registry.clone();
    let terminal_release_tool_registry = tool_execution_registry.clone();
    let prompt_tool_registry = tool_execution_registry.clone();
    let codebuddy_terminal_denials = CodeBuddyTerminalDenials::default();
    let codebuddy_terminal_approvals = CodeBuddyTerminalApprovals::default();
    let fs_write_approvals = FsWriteApprovals::default();
    let permission_terminal_denials = codebuddy_terminal_denials.clone();
    let permission_terminal_approvals = codebuddy_terminal_approvals.clone();
    let permission_fs_write_approvals = fs_write_approvals.clone();
    let terminal_create_denials = codebuddy_terminal_denials.clone();
    let terminal_create_approvals = codebuddy_terminal_approvals.clone();
    let fs_write_approval_cache = fs_write_approvals.clone();

    Client
        .builder()
        .name("acp-editor")
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, connection| {
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
                    decide_permission_with_edit_policy(
                        permission_request_broker.mode(),
                        agent_edit_policy_for_command(&permission_log_config.agent_command),
                        &permission_workspace_root,
                        &request,
                    )
                };
                let permission_resolution = match decision {
                    PermissionDecision::Select(option_id) => {
                        PermissionResolution::new(Some(option_id), None, None)
                    }
                    PermissionDecision::SelectWithGuidance(option_id, guidance) => {
                        PermissionResolution::new(Some(option_id), Some(guidance), None)
                    }
                    PermissionDecision::Cancel => PermissionResolution::default(),
                    PermissionDecision::Ask => {
                        let reply_rx = permission_request_broker.register(request_id.clone())?;

                        let _ = tx_permissions.send(ClientEvent::ToolPermissionRequest {
                            id: request_id.clone(),
                            name: permission_request_name(&request, &request_payload),
                            options: options.clone(),
                            details: permission_request_details(&request),
                            input: permission_input_request(&request),
                        });
                        let _ = tx_permissions.send(ClientEvent::ToolProgress {
                            id: request_id.clone(),
                            content: format_permission_options(&option_labels),
                        });
                        if let Ok(Some(event)) =
                            permission_tool_registry.register_permission(&request_id, &request_id)
                        {
                            let _ = tx_permissions.send(event);
                        }
                        reply_rx.recv().unwrap_or_default()
                    }
                };
                let selected_option_id = permission_resolution.option_id.as_deref();
                let response_option_id = codebuddy_bash_soft_reject_response_option(
                    &request,
                    &request_payload,
                    selected_option_id,
                    &permission_terminal_denials,
                    &permission_log_config,
                )?
                .or_else(|| permission_resolution.option_id.clone());
                register_codebuddy_bash_terminal_approval(
                    &request,
                    &request_payload,
                    selected_option_id,
                    &permission_terminal_approvals,
                    &permission_log_config,
                )?;
                register_fs_write_approval(
                    &request,
                    selected_option_id,
                    &permission_fs_write_approvals,
                    &permission_workspace_root,
                    &permission_log_config,
                )?;

                let selected_option = response_option_id.as_deref().and_then(|option_id| {
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
                let response = attach_permission_meta(response, &permission_resolution);
                let codebuddy_interruption_decision =
                    codebuddy_interruption_decision_for_permission(
                        &permission_log_config.agent_command,
                        &request_payload,
                        selected_option_id,
                    );

                let outcome = permission_resolution_outcome_for_display(
                    &request_payload,
                    &options,
                    selected_option_id,
                    codebuddy_interruption_decision.as_deref(),
                    permission_resolution.guidance.as_deref(),
                );

                let _ = tx_permissions.send(ClientEvent::ToolPermissionResolved {
                    id: request_id.clone(),
                    outcome,
                });
                if let Ok(Some(event)) =
                    permission_tool_registry.unregister_permission(&request_id, &request_id)
                {
                    let _ = tx_permissions.send(event);
                }

                let should_forward_codebuddy_plan_guidance =
                    codebuddy_interruption_decision.as_deref() == Some("deny")
                        && is_codebuddy_exit_plan_permission(&request_payload)
                        && permission_resolution
                            .guidance
                            .as_deref()
                            .map(str::trim)
                            .is_some_and(|guidance| !guidance.is_empty());
                if should_forward_codebuddy_plan_guidance
                    && let Some(guidance) = permission_resolution.guidance.as_deref()
                    && let Err(error) = send_codebuddy_plan_guidance(
                        &permission_log_config,
                        &connection,
                        request.session_id.0.as_ref(),
                        guidance,
                    )
                {
                    let _ = append_runtime_event_log(
                        &permission_log_config,
                        "codebuddy/inject_plan_guidance_error",
                        &json!({
                            "sessionId": request.session_id.0.as_ref(),
                            "toolCallId": request.tool_call.tool_call_id.0.as_ref(),
                            "error": error.to_string(),
                        }),
                    );
                }

                let response_payload =
                    serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &permission_log_config,
                    "client/request_permission_response",
                    &response_payload,
                )?;

                let respond_result = responder.respond(response);
                match &respond_result {
                    Ok(()) => append_runtime_event_log(
                        &permission_log_config,
                        "client/request_permission_response_sent",
                        &json!({
                            "sessionId": request.session_id.0.as_ref(),
                            "toolCallId": request.tool_call.tool_call_id.0.as_ref(),
                            "selectedOptionId": selected_option_id,
                        }),
                    )?,
                    Err(error) => append_runtime_event_log(
                        &permission_log_config,
                        "client/request_permission_response_send_error",
                        &json!({
                            "sessionId": request.session_id.0.as_ref(),
                            "toolCallId": request.tool_call.tool_call_id.0.as_ref(),
                            "selectedOptionId": selected_option_id,
                            "error": error.to_string(),
                        }),
                    )?,
                }
                if respond_result.is_ok()
                    && let Some(decision) = codebuddy_interruption_decision
                    && let Err(error) = send_codebuddy_interruption_resolution(
                        &permission_log_config,
                        &connection,
                        request.session_id.0.as_ref(),
                        request.tool_call.tool_call_id.0.as_ref(),
                        &decision,
                        permission_resolution.guidance.as_deref(),
                    )
                {
                    let _ = append_runtime_event_log(
                        &permission_log_config,
                        "codebuddy/resolve_interruption_error",
                        &json!({
                            "sessionId": request.session_id.0.as_ref(),
                            "toolCallId": request.tool_call.tool_call_id.0.as_ref(),
                            "decision": decision,
                            "guidance": permission_resolution.guidance.as_deref(),
                            "error": error.to_string(),
                        }),
                    );
                }

                respond_result
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

                let content_result = match fs_read_remote_ssh.as_ref() {
                    Some(remote_ssh) => read_remote_workspace_text_file(remote_ssh, &request),
                    None => read_workspace_text_file(&fs_read_workspace_root, &request),
                };
                let content = match content_result {
                    Ok(content) => content,
                    Err(error) => {
                        append_runtime_event_log(
                            &fs_log_config,
                            "client/fs_read_text_file_error",
                            &json!({ "error": error.to_string() }),
                        )?;
                        return responder.respond_with_internal_error(error.to_string());
                    }
                };
                let response = ReadTextFileResponse::new(content);
                let response_payload =
                    serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &fs_log_config,
                    "client/fs_read_text_file_response",
                    &response_payload,
                )?;

                let respond_result = responder.respond(response);
                let _ = append_runtime_event_log(
                    &fs_log_config,
                    "client/fs_read_text_file_response_sent",
                    &json!({
                        "ok": respond_result.is_ok(),
                        "error": respond_result.as_ref().err().map(|error| error.to_string()),
                    }),
                );
                respond_result
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

                let write_result = match fs_write_remote_ssh.as_ref() {
                    Some(remote_ssh) => {
                        let path_for_diff = validate_remote_client_file_path(
                            &remote_ssh.remote_workspace_root,
                            &request.path,
                        )
                        .map(PathBuf::from);
                        let result = path_for_diff.and_then(|path_for_diff| {
                            ensure_fs_write_permission(
                                &fs_write_permission_broker,
                                &fs_write_tx,
                                &fs_write_log_config,
                                &remote_ssh.remote_workspace_root,
                                &path_for_diff,
                                agent_edit_policy_for_command(&fs_write_log_config.agent_command),
                                &fs_write_approval_cache,
                            )?;
                            let outcome = write_remote_workspace_text_file(remote_ssh, &request)?;
                            Ok((outcome.path, outcome.old_text))
                        });
                        result
                    }
                    None => {
                        (|| {
                            // Read old content before writing for diff tracking
                            let path_for_diff =
                                validate_client_file_path(&fs_write_workspace_root, &request.path)?;
                            ensure_fs_write_permission(
                                &fs_write_permission_broker,
                                &fs_write_tx,
                                &fs_write_log_config,
                                &fs_write_workspace_root,
                                &path_for_diff,
                                agent_edit_policy_for_command(&fs_write_log_config.agent_command),
                                &fs_write_approval_cache,
                            )?;
                            let old_text = path_for_diff
                                .exists()
                                .then(|| fs::read_to_string(&path_for_diff).ok())
                                .flatten();

                            write_workspace_text_file(
                                &fs_write_log_config.workspace_root,
                                &request,
                            )?;

                            Ok((path_for_diff.display().to_string(), old_text))
                        })()
                    }
                };

                let (path_display, old_text) = match write_result {
                    Ok(result) => result,
                    Err(error) => {
                        append_runtime_event_log(
                            &fs_write_log_config,
                            "client/fs_write_text_file_error",
                            &json!({ "error": error.to_string() }),
                        )?;
                        return responder.respond_with_internal_error(error.to_string());
                    }
                };

                // Emit ToolDiff event so session changes can be tracked
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

                let terminal_gate = ensure_codebuddy_terminal_create_permission(
                    &terminal_create_permission_broker,
                    &terminal_create_tx,
                    &terminal_create_tool_registry,
                    &terminal_create_log_config,
                    &terminal_create_agent_command,
                    &terminal_create_workspace_root,
                    &terminal_create_denials,
                    &terminal_create_approvals,
                    &request,
                )?;

                let response_result = match terminal_gate {
                    CodeBuddyTerminalCreateGate::Allow => match terminal_create_remote_ssh.as_ref()
                    {
                        Some(remote_ssh) => {
                            terminal_create_manager.create_remote_terminal(remote_ssh, &request)
                        }
                        None => terminal_create_manager
                            .create_terminal(&terminal_create_log_config.workspace_root, &request),
                    },
                    CodeBuddyTerminalCreateGate::Deny { reason } => {
                        terminal_create_manager.create_denied_terminal(&request, &reason)
                    }
                };
                let response = match response_result {
                    Ok(response) => response,
                    Err(error) => {
                        append_runtime_event_log(
                            &terminal_create_log_config,
                            "client/terminal_create_error",
                            &json!({ "error": error.to_string() }),
                        )?;
                        return responder.respond_with_internal_error(error.to_string());
                    }
                };
                let response_payload =
                    serde_json::to_value(&response).map_err(|err| anyhow!(err.to_string()))?;
                append_runtime_event_log(
                    &terminal_create_log_config,
                    "client/terminal_create_response",
                    &response_payload,
                )?;
                if let Some(tool_call_id) =
                    terminal_tool_call_id_from_request_payload(&request_payload)
                    && let Ok(Some(event)) = terminal_create_tool_registry
                        .register_terminal(tool_call_id, response.terminal_id.0.to_string())
                {
                    let _ = terminal_create_tx.send(event);
                }

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
                match terminal_release_tool_registry
                    .unregister_terminal_id(request.terminal_id.0.as_ref())
                {
                    Ok(events) => {
                        for event in events {
                            let _ = terminal_release_tx.send(event);
                        }
                    }
                    Err(error) => {
                        append_runtime_event_log(
                            &terminal_release_log_config,
                            "client/terminal_release_stop_cleanup_error",
                            &json!({ "error": error.to_string() }),
                        )?;
                    }
                }

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
                    terminal_manager,
                    prompt_tool_registry,
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
    let request_payload = serde_json::to_value(request).ok();
    let is_exit_plan_request = request_payload
        .as_ref()
        .is_some_and(is_codebuddy_exit_plan_permission);

    if let Some(payload) = request_payload.as_ref()
        && is_exit_plan_request
        && let Some(plan) = codebuddy_plan_content(payload)
    {
        parts.push(plan);
    }

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

    if is_exit_plan_request
        && parts.is_empty()
        && let Some(plan) = latest_codebuddy_plan_file_content()
    {
        parts.push(plan);
    }

    if parts.is_empty()
        && let Some(raw_input) = &request.tool_call.fields.raw_input
    {
        if let Some(plan) = raw_input.get("plan").and_then(serde_json::Value::as_str) {
            let trimmed = plan.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
        }
        if parts.is_empty() {
            parts.extend(raw_input_permission_details(raw_input));
        }
    }

    for path in codebuddy_bash_write_hint_paths(request) {
        let detail = format!("Path: {}", path.display());
        if !parts.iter().any(|part| part == &detail) {
            parts.push(detail);
        }
    }

    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

fn codebuddy_plan_content(value: &Value) -> Option<String> {
    match value {
        Value::Object(object) => {
            if let Some(plan) = object
                .get("codebuddy.ai/planContent")
                .and_then(raw_input_detail_text)
            {
                return Some(plan);
            }
            if let Some(raw_response) = object.get("codebuddy.ai/rawResponse")
                && let Some(plan) = raw_response.get("plan").and_then(raw_input_detail_text)
            {
                return Some(plan);
            }
            object.values().find_map(codebuddy_plan_content)
        }
        Value::Array(items) => items.iter().find_map(codebuddy_plan_content),
        _ => None,
    }
}

fn latest_codebuddy_plan_file_content() -> Option<String> {
    let root = codebuddy_plan_root()?;
    latest_codebuddy_plan_file_content_from_root(&root, Duration::from_secs(30 * 60))
}

fn latest_codebuddy_plan_file_content_from_root(root: &Path, max_age: Duration) -> Option<String> {
    let now = SystemTime::now();
    let (_, path) = fs::read_dir(root)
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if !matches!(
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .map(|extension| extension.to_ascii_lowercase()),
                Some(extension) if matches!(extension.as_str(), "md" | "mdx")
            ) {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            let modified = metadata.modified().ok()?;
            if let Ok(age) = now.duration_since(modified)
                && age > max_age
            {
                return None;
            }
            Some((modified, path))
        })
        .max_by_key(|(modified, _)| *modified)?;
    fs::read_to_string(path)
        .ok()
        .map(|content| content.trim().to_string())
        .filter(|content| !content.is_empty())
}

fn codebuddy_plan_root() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(
        std::path::PathBuf::from(home)
            .join(".codebuddy")
            .join("plans"),
    )
}

fn permission_input_request(request: &RequestPermissionRequest) -> Option<PermissionInputRequest> {
    let raw_input = request.tool_call.fields.raw_input.as_ref()?;
    permission_input_request_from_value(raw_input)
}

fn permission_input_request_from_value(value: &Value) -> Option<PermissionInputRequest> {
    let questions = value.get("questions")?.as_array()?;
    let questions = questions
        .iter()
        .enumerate()
        .filter_map(permission_input_question_from_value)
        .collect::<Vec<_>>();
    (!questions.is_empty()).then_some(PermissionInputRequest { questions })
}

fn permission_input_question_from_value(
    (index, value): (usize, &Value),
) -> Option<PermissionInputQuestion> {
    let question = value.get("question").and_then(Value::as_str)?.trim();
    if question.is_empty() {
        return None;
    }

    let raw_id = value.get("id").and_then(Value::as_str).map(str::trim);
    let id = raw_id
        .filter(|id| !id.is_empty())
        .unwrap_or(question)
        .to_string();
    let header = value
        .get("header")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|header| !header.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Question {}", index + 1));
    let is_other = value
        .get("is_other")
        .or_else(|| value.get("isOther"))
        .and_then(Value::as_bool)
        .unwrap_or_else(|| value.get("is_other").is_none() && value.get("isOther").is_none());
    let is_secret = value
        .get("is_secret")
        .or_else(|| value.get("isSecret"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let multi_select = value
        .get("multi_select")
        .or_else(|| value.get("multiSelect"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let options = value
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(permission_input_option_from_value)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(PermissionInputQuestion {
        id,
        header,
        question: question.to_string(),
        is_other,
        is_secret,
        multi_select,
        options,
    })
}

fn permission_input_option_from_value(value: &Value) -> Option<PermissionInputOption> {
    let label = value.get("label").and_then(Value::as_str)?.trim();
    if label.is_empty() {
        return None;
    }
    let description = value
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    Some(PermissionInputOption {
        label: label.to_string(),
        description,
    })
}

fn raw_input_permission_details(value: &Value) -> Vec<String> {
    let mut details = Vec::new();
    collect_raw_input_permission_details(value, &mut details);
    details
}

fn collect_raw_input_permission_details(value: &Value, details: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let key_lower = key.to_ascii_lowercase();
                let label = if matches!(
                    key_lower.as_str(),
                    "path" | "file" | "file_path" | "filepath"
                ) || key_lower.ends_with("path")
                    || key_lower.ends_with("file")
                {
                    Some("Path")
                } else if matches!(
                    key_lower.as_str(),
                    "command" | "cmd" | "shell_command" | "command_line"
                ) {
                    Some("Command")
                } else {
                    None
                };

                if let Some(label) = label
                    && let Some(text) = raw_input_detail_text(value)
                {
                    details.push(format!("{label}: {text}"));
                    continue;
                }

                collect_raw_input_permission_details(value, details);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_raw_input_permission_details(item, details);
            }
        }
        _ => {}
    }
}

fn raw_input_detail_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join(" "))
        }
        _ => None,
    }
}

fn ensure_codebuddy_terminal_create_permission(
    permission_broker: &PermissionBroker,
    tx_events: &mpsc::Sender<ClientEvent>,
    tool_execution_registry: &ToolExecutionRegistry,
    log_config: &SessionConfig,
    agent_command: &str,
    workspace_root: &str,
    terminal_denials: &CodeBuddyTerminalDenials,
    terminal_approvals: &CodeBuddyTerminalApprovals,
    request: &CreateTerminalRequest,
) -> anyhow::Result<CodeBuddyTerminalCreateGate> {
    if !agent_command_uses_terminal_permission_gate(agent_command) {
        return Ok(CodeBuddyTerminalCreateGate::Allow);
    }

    let command = terminal_request_command_text(request);
    if permission_broker.mode() != PermissionPolicyMode::FullAccess
        && agent_edit_policy_for_command(agent_command) == AgentEditPolicy::PreferApplyPatch
        && shell_command_prefers_apply_patch_for_writes(workspace_root, &command)
    {
        let reason = apply_patch_retry_guidance().to_string();
        append_runtime_event_log(
            log_config,
            "client/terminal_create_apply_patch_policy_rejected",
            &json!({
                "command": command,
                "reason": reason,
            }),
        )?;
        return Ok(CodeBuddyTerminalCreateGate::Deny { reason });
    }

    if let Some(reason) = terminal_denials.take(&command)? {
        append_runtime_event_log(
            log_config,
            "client/terminal_create_soft_rejected",
            &json!({
                "command": command,
                "reason": reason,
            }),
        )?;
        return Ok(CodeBuddyTerminalCreateGate::Deny { reason });
    }

    if let Some(paths) = terminal_approvals.take(&command)? {
        append_runtime_event_log(
            log_config,
            "client/terminal_create_permission_reused",
            &json!({
                "command": command,
                "paths": display_paths(&paths),
            }),
        )?;
        return Ok(CodeBuddyTerminalCreateGate::Allow);
    }

    match decide_codebuddy_terminal_permission(workspace_root, &command) {
        CodeBuddyTerminalPermissionDecision::Allow => Ok(CodeBuddyTerminalCreateGate::Allow),
        CodeBuddyTerminalPermissionDecision::Reject => {
            let reason = "Terminal command blocked: command is not clearly read-only and no static write path could be extracted. Command was not executed.".to_string();
            append_runtime_event_log(
                log_config,
                "client/terminal_create_permission_rejected",
                &json!({
                    "command": command,
                    "reason": "not clearly read-only and no static write path could be extracted",
                }),
            )?;
            Ok(CodeBuddyTerminalCreateGate::Deny { reason })
        }
        CodeBuddyTerminalPermissionDecision::Ask(paths) => {
            let request_id = format!("terminal_create:{}", Uuid::new_v4());
            let options = vec![
                PermissionOption {
                    id: "allow".into(),
                    label: "Allow".into(),
                    kind: "allow_once".into(),
                },
                PermissionOption {
                    id: "reject".into(),
                    label: "Reject".into(),
                    kind: "reject_once".into(),
                },
            ];
            let option_labels = options
                .iter()
                .map(|option| option.label.clone())
                .collect::<Vec<_>>();
            let reply_rx = permission_broker.register(request_id.clone())?;

            append_runtime_event_log(
                log_config,
                "client/terminal_create_permission_request",
                &json!({
                    "requestId": request_id,
                    "command": command,
                    "paths": display_paths(&paths),
                    "options": options,
                }),
            )?;

            let _ = tx_events.send(ClientEvent::ToolPermissionRequest {
                id: request_id.clone(),
                name: "Bash".into(),
                options: options.clone(),
                details: Some(codebuddy_terminal_permission_details(&command, &paths)),
                input: None,
            });
            let _ = tx_events.send(ClientEvent::ToolProgress {
                id: request_id.clone(),
                content: format_permission_options(&option_labels),
            });
            if let Ok(Some(event)) =
                tool_execution_registry.register_permission(&request_id, &request_id)
            {
                let _ = tx_events.send(event);
            }

            let selected_option_id = reply_rx
                .recv()
                .ok()
                .and_then(|resolution| resolution.option_id);
            let allowed = selected_option_id
                .as_deref()
                .is_some_and(|option_id| option_id.eq_ignore_ascii_case("allow"));
            let outcome = selected_option_id
                .as_deref()
                .and_then(|option_id| options.iter().find(|option| option.id == option_id))
                .map(|option| format!("Permission selected: {}", option.label))
                .unwrap_or_else(|| "Permission request cancelled".into());

            append_runtime_event_log(
                log_config,
                "client/terminal_create_permission_response",
                &json!({
                    "requestId": request_id,
                    "command": command,
                    "paths": display_paths(&paths),
                    "optionId": selected_option_id,
                    "allowed": allowed,
                }),
            )?;

            let _ = tx_events.send(ClientEvent::ToolPermissionResolved {
                id: request_id.clone(),
                outcome,
            });
            if let Ok(Some(event)) =
                tool_execution_registry.unregister_permission(&request_id, &request_id)
            {
                let _ = tx_events.send(event);
            }

            if allowed {
                Ok(CodeBuddyTerminalCreateGate::Allow)
            } else {
                Ok(CodeBuddyTerminalCreateGate::Deny {
                    reason: "Permission rejected by user. Command was not executed.".into(),
                })
            }
        }
    }
}

fn agent_command_uses_terminal_permission_gate(agent_command: &str) -> bool {
    agent_command_is_codebuddy(agent_command)
        || agent_command_is_codex(agent_command)
        || agent_command_is_claude(agent_command)
}

fn agent_command_is_codebuddy(agent_command: &str) -> bool {
    agent_command.to_ascii_lowercase().contains("codebuddy")
}

fn agent_command_is_codex(agent_command: &str) -> bool {
    let normalized = agent_command.to_ascii_lowercase();
    normalized.contains("codex-acp") || normalized.contains("codex")
}

fn agent_command_is_claude(agent_command: &str) -> bool {
    let normalized = agent_command.to_ascii_lowercase();
    normalized.contains("claude-agent-acp")
        || normalized.contains("claude-acp")
        || normalized.contains("claude-code")
        || command_basename_lower(agent_command).starts_with("claude")
}

fn terminal_request_command_text(request: &CreateTerminalRequest) -> String {
    if let Some(script) = terminal_request_shell_script(request) {
        return script;
    }

    if request.args.is_empty() {
        return request.command.clone();
    }

    let mut command = request.command.clone();
    for arg in &request.args {
        command.push(' ');
        command.push_str(arg);
    }
    command
}

fn terminal_request_shell_script(request: &CreateTerminalRequest) -> Option<String> {
    if !terminal_command_is_shell(&request.command) {
        return None;
    }

    request.args.iter().enumerate().find_map(|(index, arg)| {
        if shell_arg_runs_command(arg) {
            request
                .args
                .get(index + 1)
                .map(|script| script.trim())
                .filter(|script| !script.is_empty())
                .map(str::to_string)
        } else {
            None
        }
    })
}

fn terminal_command_is_shell(command: &str) -> bool {
    matches!(
        command_basename_lower(command).as_str(),
        "bash" | "dash" | "fish" | "sh" | "zsh" | "pwsh" | "powershell"
    )
}

fn shell_arg_runs_command(arg: &str) -> bool {
    let arg = arg.trim();
    if matches!(arg, "-c" | "-lc" | "-ic" | "-ilc" | "-Command" | "-command") {
        return true;
    }

    let Some(flags) = arg.strip_prefix('-') else {
        return false;
    };
    !flags.starts_with('-')
        && flags
            .chars()
            .all(|ch| matches!(ch, 'c' | 'e' | 'i' | 'l' | 'u' | 'x'))
        && flags.chars().any(|ch| ch == 'c')
}

fn command_basename_lower(command: &str) -> String {
    let basename = command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command)
        .trim_matches(['"', '\'', '`'])
        .to_ascii_lowercase();
    basename
        .strip_suffix(".exe")
        .unwrap_or(&basename)
        .to_string()
}

fn codebuddy_bash_soft_reject_response_option(
    request: &RequestPermissionRequest,
    request_payload: &Value,
    selected_option_id: Option<&str>,
    terminal_denials: &CodeBuddyTerminalDenials,
    log_config: &SessionConfig,
) -> anyhow::Result<Option<String>> {
    if !codebuddy_permission_tool_name(request_payload)
        .as_deref()
        .is_some_and(|tool_name| tool_name.eq_ignore_ascii_case("Bash"))
    {
        return Ok(None);
    }
    if !request_option_is_reject(request, selected_option_id) {
        return Ok(None);
    }

    let Some(command) = codebuddy_bash_permission_command(request) else {
        return Ok(None);
    };
    let Some(allow_option_id) = request_allow_option_id(request) else {
        return Ok(None);
    };

    let reason = "Permission rejected by user. Command was not executed.";
    terminal_denials.register(command.clone(), reason)?;
    append_runtime_event_log(
        log_config,
        "client/codebuddy_bash_permission_soft_reject",
        &json!({
            "toolCallId": request.tool_call.tool_call_id.0.as_ref(),
            "command": command,
            "responseOptionId": allow_option_id,
        }),
    )?;

    Ok(Some(allow_option_id))
}

fn register_codebuddy_bash_terminal_approval(
    request: &RequestPermissionRequest,
    request_payload: &Value,
    selected_option_id: Option<&str>,
    terminal_approvals: &CodeBuddyTerminalApprovals,
    log_config: &SessionConfig,
) -> anyhow::Result<()> {
    if !codebuddy_permission_tool_name(request_payload)
        .as_deref()
        .is_some_and(|tool_name| tool_name.eq_ignore_ascii_case("Bash"))
    {
        return Ok(());
    }
    if !request_option_is_allow(request, selected_option_id) {
        return Ok(());
    }

    let Some(command) = codebuddy_bash_permission_command(request) else {
        return Ok(());
    };
    let paths = codebuddy_bash_write_hint_paths(request);

    terminal_approvals.register(command.clone(), paths.clone())?;
    append_runtime_event_log(
        log_config,
        "client/codebuddy_bash_permission_terminal_approval",
        &json!({
            "toolCallId": request.tool_call.tool_call_id.0.as_ref(),
            "command": command,
            "paths": display_paths(&paths),
        }),
    )?;
    Ok(())
}

fn register_fs_write_approval(
    request: &RequestPermissionRequest,
    selected_option_id: Option<&str>,
    fs_write_approvals: &FsWriteApprovals,
    workspace_root: &str,
    log_config: &SessionConfig,
) -> anyhow::Result<()> {
    if !request_option_is_allow(request, selected_option_id) {
        return Ok(());
    }

    let paths = permission_request_write_paths(request, workspace_root);
    if paths.is_empty() {
        return Ok(());
    }

    fs_write_approvals.register(paths.clone())?;
    append_runtime_event_log(
        log_config,
        "client/fs_write_permission_preapproval",
        &json!({
            "toolCallId": request.tool_call.tool_call_id.0.as_ref(),
            "paths": display_paths(&paths),
        }),
    )?;
    Ok(())
}

fn request_option_is_allow(
    request: &RequestPermissionRequest,
    selected_option_id: Option<&str>,
) -> bool {
    let Some(selected_option_id) = selected_option_id else {
        return false;
    };
    request
        .options
        .iter()
        .find(|option| option.option_id.0.as_ref() == selected_option_id)
        .is_some_and(|option| {
            matches!(
                option.kind,
                AcpPermissionOptionKind::AllowOnce | AcpPermissionOptionKind::AllowAlways
            ) || option.option_id.0.as_ref().eq_ignore_ascii_case("allow")
                || option.name.eq_ignore_ascii_case("allow")
        })
}

fn request_option_is_reject(
    request: &RequestPermissionRequest,
    selected_option_id: Option<&str>,
) -> bool {
    let Some(selected_option_id) = selected_option_id else {
        return false;
    };
    request
        .options
        .iter()
        .find(|option| option.option_id.0.as_ref() == selected_option_id)
        .is_some_and(|option| {
            matches!(
                option.kind,
                AcpPermissionOptionKind::RejectOnce | AcpPermissionOptionKind::RejectAlways
            ) || option.option_id.0.as_ref().eq_ignore_ascii_case("reject")
                || option.name.eq_ignore_ascii_case("reject")
        })
}

fn request_allow_option_id(request: &RequestPermissionRequest) -> Option<String> {
    request
        .options
        .iter()
        .find(|option| option.kind == AcpPermissionOptionKind::AllowOnce)
        .or_else(|| {
            request
                .options
                .iter()
                .find(|option| option.kind == AcpPermissionOptionKind::AllowAlways)
        })
        .map(|option| option.option_id.0.to_string())
}

fn codebuddy_bash_permission_command(request: &RequestPermissionRequest) -> Option<String> {
    let command = request
        .tool_call
        .fields
        .raw_input
        .as_ref()?
        .get("command")
        .and_then(Value::as_str)?
        .trim();
    (!command.is_empty()).then(|| command.to_string())
}

fn normalize_codebuddy_command(command: &str) -> String {
    command.trim().replace("\r\n", "\n")
}

fn codebuddy_terminal_permission_details(command: &str, paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return format!("Command:\n{command}\n\nPaths: not detected");
    }
    let paths = display_paths(paths)
        .into_iter()
        .map(|path| format!("- {path}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("Command:\n{command}\n\nPaths:\n{paths}")
}

fn display_paths(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}

fn permission_request_write_paths(
    request: &RequestPermissionRequest,
    workspace_root: &str,
) -> Vec<PathBuf> {
    let Some(raw_input) = request.tool_call.fields.raw_input.as_ref() else {
        return Vec::new();
    };
    let mut path_texts = Vec::new();
    collect_raw_input_write_path_texts(raw_input, &mut path_texts);

    let mut seen = Vec::new();
    let mut paths = Vec::new();
    for path_text in path_texts {
        let normalized = normalize_path_text_for_compare(workspace_root, &path_text);
        if normalized.is_empty() || seen.iter().any(|existing| existing == &normalized) {
            continue;
        }
        seen.push(normalized.clone());
        paths.push(PathBuf::from(normalized));
    }
    paths
}

fn collect_raw_input_write_path_texts(value: &Value, path_texts: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if raw_input_write_path_key(key) {
                    collect_raw_input_path_values(value, path_texts);
                } else {
                    collect_raw_input_write_path_texts(value, path_texts);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_raw_input_write_path_texts(value, path_texts);
            }
        }
        _ => {}
    }
}

fn collect_raw_input_path_values(value: &Value, path_texts: &mut Vec<String>) {
    match value {
        Value::String(value) => {
            let value = value.trim();
            if !value.is_empty() {
                path_texts.push(value.to_string());
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_raw_input_path_values(value, path_texts);
            }
        }
        Value::Object(map) => {
            for (key, value) in map {
                if raw_input_write_path_key(key) {
                    collect_raw_input_path_values(value, path_texts);
                }
            }
        }
        _ => {}
    }
}

fn raw_input_write_path_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| *ch != '_' && *ch != '-')
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "path" | "paths" | "file" | "files" | "filepath" | "filename"
    ) || normalized.ends_with("path")
        || normalized.ends_with("file")
}

fn fs_write_approval_path_matches(workspace_root: &str, approved: &Path, requested: &Path) -> bool {
    normalize_path_for_compare(workspace_root, approved)
        == normalize_path_for_compare(workspace_root, requested)
}

fn normalize_path_for_compare(workspace_root: &str, path: &Path) -> String {
    normalize_path_text_for_compare(workspace_root, &path.display().to_string())
}

fn normalize_path_text_for_compare(workspace_root: &str, path_text: &str) -> String {
    let path_text = path_text.trim().replace('\\', "/");
    if path_text.is_empty() {
        return String::new();
    }
    let path_text = if path_text_is_absolute_like(&path_text) || workspace_root.trim().is_empty() {
        path_text
    } else {
        let root = workspace_root.trim().replace('\\', "/");
        format!(
            "{}/{}",
            root.trim_end_matches('/'),
            path_text.trim_start_matches("./")
        )
    };
    normalize_slash_path(&path_text)
}

fn path_text_is_absolute_like(path_text: &str) -> bool {
    let bytes = path_text.as_bytes();
    path_text.starts_with('/')
        || (bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'/' || bytes[2] == b'\\'))
}

fn normalize_slash_path(path_text: &str) -> String {
    let path_text = path_text.trim().replace('\\', "/");
    let mut rest = path_text.as_str();
    let mut prefix = String::new();
    let bytes = rest.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'/' {
        prefix = rest[..3].to_string();
        rest = &rest[3..];
    } else if rest.starts_with("//") {
        prefix = "//".into();
        rest = &rest[2..];
    } else if rest.starts_with('/') {
        prefix = "/".into();
        rest = &rest[1..];
    }

    let mut parts = Vec::new();
    for part in rest.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            if !parts.is_empty() {
                parts.pop();
            }
            continue;
        }
        parts.push(part);
    }

    let body = parts.join("/");
    if prefix.is_empty() {
        body
    } else if body.is_empty() {
        prefix
    } else {
        format!("{prefix}{body}")
    }
}

fn attach_permission_meta(
    response: RequestPermissionResponse,
    resolution: &PermissionResolution,
) -> RequestPermissionResponse {
    let mut meta = Meta::new();
    if let Some(guidance) = resolution
        .guidance
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        meta.insert(
            KODEX_PERMISSION_GUIDANCE_META_KEY.to_string(),
            Value::String(guidance.to_string()),
        );
    }
    if let Some(input_response) = resolution.input_response.as_ref()
        && let Ok(value) = serde_json::to_value(input_response)
    {
        meta.insert(KODEX_USER_INPUT_ANSWERS_META_KEY.to_string(), value);
    }
    if meta.is_empty() {
        return response;
    }
    response.meta(meta)
}

fn ensure_fs_write_permission(
    permission_broker: &PermissionBroker,
    tx_events: &mpsc::Sender<ClientEvent>,
    log_config: &SessionConfig,
    workspace_root: &str,
    path: &Path,
    edit_policy: AgentEditPolicy,
    fs_write_approvals: &FsWriteApprovals,
) -> anyhow::Result<()> {
    let prefer_apply_patch =
        edit_policy == AgentEditPolicy::PreferApplyPatch && path_prefers_apply_patch(path);
    if fs_write_is_auto_allowed(permission_broker.mode(), workspace_root, path) {
        return Ok(());
    }
    if fs_write_approvals.take(workspace_root, path)? {
        append_runtime_event_log(
            log_config,
            "client/fs_write_permission_reused",
            &json!({
                "path": path.display().to_string(),
            }),
        )?;
        return Ok(());
    }

    let request_id = format!("fs_write:{}", Uuid::new_v4());
    let options = vec![
        PermissionOption {
            id: "allow".into(),
            label: "Allow".into(),
            kind: "allow_once".into(),
        },
        PermissionOption {
            id: "reject".into(),
            label: "Reject".into(),
            kind: "reject_once".into(),
        },
    ];
    let option_labels = options
        .iter()
        .map(|option| option.label.clone())
        .collect::<Vec<_>>();
    let reply_rx = permission_broker.register(request_id.clone())?;

    append_runtime_event_log(
        log_config,
        "client/fs_write_permission_request",
        &json!({
            "requestId": request_id,
            "path": path.display().to_string(),
            "preferApplyPatch": prefer_apply_patch,
            "options": options,
        }),
    )?;

    let guidance = prefer_apply_patch.then_some(apply_patch_retry_guidance());
    let details = match guidance {
        Some(guidance) => format!("Write file\n{}\n\n{}", path.display(), guidance),
        None => format!("Write file\n{}", path.display()),
    };
    let _ = tx_events.send(ClientEvent::ToolPermissionRequest {
        id: request_id.clone(),
        name: "Write".into(),
        options: options.clone(),
        details: Some(details),
        input: None,
    });
    let _ = tx_events.send(ClientEvent::ToolProgress {
        id: request_id.clone(),
        content: format_permission_options(&option_labels),
    });

    let selected_option_id = reply_rx
        .recv()
        .ok()
        .and_then(|resolution| resolution.option_id);
    let allowed = selected_option_id
        .as_deref()
        .is_some_and(|option_id| option_id.eq_ignore_ascii_case("allow"));
    let outcome = selected_option_id
        .as_deref()
        .and_then(|option_id| options.iter().find(|option| option.id == option_id))
        .map(|option| format!("Permission selected: {}", option.label))
        .unwrap_or_else(|| "Permission request cancelled".into());

    append_runtime_event_log(
        log_config,
        "client/fs_write_permission_response",
        &json!({
            "requestId": request_id,
            "path": path.display().to_string(),
            "optionId": selected_option_id,
            "allowed": allowed,
        }),
    )?;

    let _ = tx_events.send(ClientEvent::ToolPermissionResolved {
        id: request_id,
        outcome,
    });

    if allowed {
        Ok(())
    } else if let Some(guidance) = guidance {
        Err(anyhow!("{}", guidance))
    } else {
        Err(anyhow!(
            "write permission denied by user: {}",
            path.display()
        ))
    }
}

fn fs_write_is_auto_allowed(mode: PermissionPolicyMode, workspace_root: &str, path: &Path) -> bool {
    let _ = (mode, workspace_root, path);
    false
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

fn codebuddy_interruption_decision_for_permission(
    agent_command: &str,
    request: &Value,
    selected_option_id: Option<&str>,
) -> Option<String> {
    if !agent_command_is_codebuddy(agent_command) {
        return None;
    }
    codebuddy_permission_tool_name(request)?;
    if is_codebuddy_exit_plan_permission(request) {
        Some(normalize_codebuddy_exit_plan_decision(selected_option_id))
    } else {
        Some(normalize_codebuddy_interruption_decision(
            selected_option_id,
        ))
    }
}

fn normalize_codebuddy_interruption_decision(option_id: Option<&str>) -> String {
    let Some(option_id) = option_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return "deny".into();
    };
    let normalized = option_id
        .chars()
        .filter(|ch| *ch != '-' && *ch != '_')
        .collect::<String>()
        .to_ascii_lowercase();
    match normalized.as_str() {
        "allowalways" | "alwaysallow" | "allowall" => "allowAll".into(),
        "allowonce" | "allow" => "allow".into(),
        "rejectonce" | "rejectalways" | "reject" | "deny" | "cancel" | "cancelled" | "canceled" => {
            "deny".into()
        }
        _ => option_id.to_string(),
    }
}

fn permission_resolution_outcome_for_display(
    request: &Value,
    options: &[PermissionOption],
    selected_option_id: Option<&str>,
    codebuddy_interruption_decision: Option<&str>,
    guidance: Option<&str>,
) -> String {
    if is_codebuddy_exit_plan_permission(request) {
        match codebuddy_interruption_decision {
            Some("deny") => {
                if guidance
                    .map(str::trim)
                    .is_some_and(|value| !value.is_empty())
                {
                    return "继续规划：已发送调整要求".into();
                }
                return "继续规划".into();
            }
            Some("rejectAndExitPlan") => return "计划已终止".into(),
            Some("allow") | Some("allowAll") => return "Permission selected: Allow".into(),
            _ => {}
        }
    }

    if selected_option_rejects_patch_edit(options, selected_option_id) {
        return "编辑已拒绝".into();
    }

    match selected_option_id {
        Some(option_id) => {
            let Some(option) = options.iter().find(|option| option.id == option_id) else {
                return format!("Permission selected: {option_id}");
            };
            let kind = option.kind.to_ascii_lowercase();
            if kind.contains("allow") {
                return "Permission selected: Allow".into();
            }
            if kind.contains("reject") {
                return "Permission selected: Reject".into();
            }
            format!("Permission selected: {}", option.label)
        }
        None => "Permission request cancelled".into(),
    }
}

fn selected_option_rejects_patch_edit(
    options: &[PermissionOption],
    selected_option_id: Option<&str>,
) -> bool {
    let Some(option_id) = selected_option_id else {
        return false;
    };
    let Some(option) = options.iter().find(|option| option.id == option_id) else {
        return false;
    };
    let id = option.id.trim().to_ascii_lowercase();
    if !matches!(id.as_str(), "abort" | "timed_out") {
        return false;
    }
    option
        .label
        .trim()
        .eq_ignore_ascii_case("No, provide feedback")
}

fn normalize_codebuddy_exit_plan_decision(option_id: Option<&str>) -> String {
    let Some(option_id) = option_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return "deny".into();
    };
    let normalized = option_id
        .chars()
        .filter(|ch| *ch != '-' && *ch != '_')
        .collect::<String>()
        .to_ascii_lowercase();
    match normalized.as_str() {
        "allowalways" | "alwaysallow" | "allowall" => "allowAll".into(),
        "allowonce" | "allow" | "default" => "allow".into(),
        "plan" | "rejectonce" | "reject" | "deny" | "cancel" | "cancelled" | "canceled" => {
            "deny".into()
        }
        "rejectandexitplan" | "denyandexitplan" => "rejectAndExitPlan".into(),
        _ => option_id.to_string(),
    }
}

fn is_codebuddy_exit_plan_permission(request: &Value) -> bool {
    codebuddy_permission_tool_name(request)
        .as_deref()
        .is_some_and(|tool_name| tool_name.eq_ignore_ascii_case("ExitPlanMode"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{
        PermissionOption, PermissionOptionKind, RequestPermissionRequest, SessionId,
        ToolCallUpdate, ToolCallUpdateFields,
    };
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    fn codebuddy_interruption_decision_is_derived_for_codebuddy_permissions() {
        let payload = json!({
            "toolCall": {
                "_meta": {
                    "codebuddy.ai/toolName": "Bash"
                },
                "toolCallId": "call_bash"
            }
        });

        assert_eq!(
            codebuddy_interruption_decision_for_permission(
                "codebuddy --acp",
                &payload,
                Some("allow_always")
            )
            .as_deref(),
            Some("allowAll")
        );
        assert_eq!(
            codebuddy_interruption_decision_for_permission(
                "codebuddy --acp",
                &payload,
                Some("reject")
            )
            .as_deref(),
            Some("deny")
        );
        assert_eq!(
            codebuddy_interruption_decision_for_permission("codex-acp", &payload, Some("allow")),
            None
        );
    }

    #[test]
    fn codebuddy_exit_plan_interruption_decision_uses_official_actions() {
        let payload = json!({
            "toolCall": {
                "_meta": {
                    "codebuddy.ai/toolName": "ExitPlanMode"
                },
                "toolCallId": "call_exit_plan"
            }
        });

        assert_eq!(
            codebuddy_interruption_decision_for_permission(
                "codebuddy --acp",
                &payload,
                Some("reject")
            )
            .as_deref(),
            Some("deny")
        );
        assert_eq!(
            codebuddy_interruption_decision_for_permission(
                "codebuddy --acp",
                &payload,
                Some("reject_and_exit_plan")
            )
            .as_deref(),
            Some("rejectAndExitPlan")
        );
        assert_eq!(
            codebuddy_interruption_decision_for_permission(
                "codebuddy --acp",
                &payload,
                Some("plan")
            )
            .as_deref(),
            Some("deny")
        );
        assert_eq!(
            codebuddy_interruption_decision_for_permission(
                "codebuddy --acp",
                &payload,
                Some("deny_and_exit_plan")
            )
            .as_deref(),
            Some("rejectAndExitPlan")
        );
    }

    #[test]
    fn codebuddy_exit_plan_replan_outcome_is_not_a_reject_display() {
        let payload = json!({
            "toolCall": {
                "_meta": {
                    "codebuddy.ai/toolName": "ExitPlanMode"
                },
                "toolCallId": "call_exit_plan"
            }
        });
        let options = vec![workspace_model::PermissionOption {
            id: "reject".into(),
            label: "Reject".into(),
            kind: "RejectOnce".into(),
        }];

        assert_eq!(
            permission_resolution_outcome_for_display(
                &payload,
                &options,
                Some("reject"),
                Some("deny"),
                Some(" 补充风险和验证步骤 ")
            ),
            "继续规划：已发送调整要求"
        );
    }

    #[test]
    fn codex_patch_reject_permission_outcome_is_edit_rejected_display() {
        let payload = json!({
            "toolCall": {
                "toolCallId": "call_patch"
            }
        });
        let options = vec![workspace_model::PermissionOption {
            id: "abort".into(),
            label: "No, provide feedback".into(),
            kind: "RejectOnce".into(),
        }];

        assert_eq!(
            permission_resolution_outcome_for_display(
                &payload,
                &options,
                Some("abort"),
                None,
                Some("不要改这里")
            ),
            "编辑已拒绝"
        );
    }

    #[test]
    fn permission_resolution_outcome_uses_option_kind_for_allow_display() {
        let payload = json!({
            "toolCall": {
                "toolCallId": "call_shell"
            }
        });
        let options = vec![workspace_model::PermissionOption {
            id: "approved".into(),
            label: "Yes".into(),
            kind: "AllowOnce".into(),
        }];

        assert_eq!(
            permission_resolution_outcome_for_display(
                &payload,
                &options,
                Some("approved"),
                None,
                None,
            ),
            "Permission selected: Allow"
        );
    }

    #[test]
    fn agent_command_codex_detection_does_not_match_codebuddy() {
        assert!(agent_command_is_codex(
            "C:/Users/yvonchen/.kodex/bin/codex-acp.exe"
        ));
        assert!(agent_command_is_codex("codex"));
        assert!(!agent_command_is_codex("codebuddy"));
        assert!(!agent_command_is_codex("kodex-desktop"));
    }

    #[test]
    fn agent_command_terminal_gate_detection_covers_supported_agents() {
        assert!(agent_command_uses_terminal_permission_gate(
            "C:/Users/yvonchen/.kodex/bin/codebuddy.exe"
        ));
        assert!(agent_command_uses_terminal_permission_gate(
            "C:/Users/yvonchen/.kodex/bin/codex-acp.exe"
        ));
        assert!(agent_command_uses_terminal_permission_gate(
            "C:/Users/yvonchen/.kodex/bin/claude-agent-acp.exe"
        ));
        assert!(agent_command_uses_terminal_permission_gate("claude"));
        assert!(!agent_command_uses_terminal_permission_gate("test-agent"));
    }

    #[test]
    fn terminal_request_command_text_extracts_shell_script_for_login_shell() {
        let request = CreateTerminalRequest::new("session-1", "/bin/zsh".to_string()).args(vec![
            "-lc".into(),
            "mkdir -p src/server/routes && echo ok".into(),
        ]);

        assert_eq!(
            terminal_request_command_text(&request),
            "mkdir -p src/server/routes && echo ok"
        );
    }

    #[test]
    fn codebuddy_terminal_permission_details_include_command_and_paths() {
        let details = codebuddy_terminal_permission_details(
            "python - <<'PY'\nfrom pathlib import Path\np=Path('src/main.ts')\np.write_text('ok')\nPY",
            &[PathBuf::from("src/main.ts")],
        );

        assert!(details.contains("Command:\npython - <<'PY'"));
        assert!(details.contains("Paths:\n- src/main.ts"));
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

    #[test]
    fn permission_request_details_include_raw_input_file_path() {
        let request = permission_request_with_raw_input(json!({
            "file_path": "C:/Users/yvonchen/.codebuddy/plans/blazing-vortex-turing.md"
        }));

        let details = permission_request_details(&request).unwrap();

        assert!(
            details.contains("Path: C:/Users/yvonchen/.codebuddy/plans/blazing-vortex-turing.md")
        );
    }

    #[test]
    fn permission_request_details_include_raw_input_command() {
        let request = permission_request_with_raw_input(json!({
            "command": "find /d/work/ArtAssets -type f | head -20"
        }));

        let details = permission_request_details(&request).unwrap();

        assert!(details.contains("Command: find /d/work/ArtAssets -type f | head -20"));
    }

    #[test]
    fn permission_input_request_parses_raw_input_questions() {
        let request = permission_request_with_raw_input(json!({
            "questions": [
                {
                    "id": "approach",
                    "header": "Approach",
                    "question": "Which implementation approach should I use?",
                    "multiSelect": true,
                    "options": [
                        { "label": "Fast", "description": "Smallest viable change" },
                        { "label": "Robust", "description": "Add tests and validation" }
                    ]
                }
            ]
        }));

        let input = permission_input_request(&request).expect("questions should parse");

        assert_eq!(input.questions.len(), 1);
        assert_eq!(input.questions[0].id, "approach");
        assert_eq!(input.questions[0].header, "Approach");
        assert!(input.questions[0].multi_select);
        assert_eq!(input.questions[0].options[1].label, "Robust");
    }

    #[test]
    fn codebuddy_bash_permission_details_include_extracted_write_path() {
        let request = codebuddy_bash_permission_request(json!({
            "command": "python - <<'PY'\nfrom pathlib import Path\np=Path('packages/backend/src/service.ts')\np.write_text('ok')\nPY"
        }));

        let details = permission_request_details(&request).unwrap();

        assert!(details.contains("Command: python - <<'PY'"));
        assert!(details.contains("Path: packages/backend/src/service.ts"));
    }

    #[test]
    fn codebuddy_bash_reject_is_softened_to_allow_and_terminal_denial() {
        let request = codebuddy_bash_permission_request(json!({
            "command": "pnpm build"
        }));
        let payload = serde_json::to_value(&request).expect("request should serialize");
        let denials = CodeBuddyTerminalDenials::default();
        let root = temp_workspace("soft-reject");
        let config = test_session_config(&root);

        let response_option = codebuddy_bash_soft_reject_response_option(
            &request,
            &payload,
            Some("reject"),
            &denials,
            &config,
        )
        .expect("soft reject should not fail");

        assert_eq!(response_option.as_deref(), Some("allow"));
        assert_eq!(
            denials
                .take("pnpm build")
                .expect("denial registry should not be poisoned")
                .as_deref(),
            Some("Permission rejected by user. Command was not executed.")
        );
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn codebuddy_bash_allow_registers_terminal_approval_for_write_paths() {
        let command = "python - <<'PY'\nfrom pathlib import Path\np=Path('src/main.ts')\np.write_text('ok')\nPY";
        let request = codebuddy_bash_permission_request(json!({
            "command": command
        }));
        let payload = serde_json::to_value(&request).expect("request should serialize");
        let approvals = CodeBuddyTerminalApprovals::default();
        let root = temp_workspace("terminal-approval-register");
        let config = test_session_config(&root);

        register_codebuddy_bash_terminal_approval(
            &request,
            &payload,
            Some("allow"),
            &approvals,
            &config,
        )
        .expect("approval registration should not fail");

        assert_eq!(
            approvals
                .take(command)
                .expect("approval registry should not be poisoned"),
            Some(vec![PathBuf::from("src/main.ts")])
        );
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn codebuddy_bash_allow_registers_terminal_approval_without_write_paths() {
        let command = "pnpm build";
        let request = codebuddy_bash_permission_request(json!({
            "command": command
        }));
        let payload = serde_json::to_value(&request).expect("request should serialize");
        let approvals = CodeBuddyTerminalApprovals::default();
        let root = temp_workspace("terminal-approval-register-empty-paths");
        let config = test_session_config(&root);

        register_codebuddy_bash_terminal_approval(
            &request,
            &payload,
            Some("allow"),
            &approvals,
            &config,
        )
        .expect("approval registration should not fail");

        assert_eq!(
            approvals
                .take(command)
                .expect("approval registry should not be poisoned"),
            Some(Vec::new())
        );
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn codebuddy_registered_terminal_approval_skips_second_permission_request() {
        let command = "python - <<'PY'\nfrom pathlib import Path\np=Path('src/main.ts')\np.write_text('ok')\nPY";
        let approvals = CodeBuddyTerminalApprovals::default();
        approvals
            .register(command, vec![PathBuf::from("src/main.ts")])
            .unwrap();
        let denials = CodeBuddyTerminalDenials::default();
        let (tx, rx) = mpsc::channel();
        let tool_execution_registry = ToolExecutionRegistry::default();
        let broker = PermissionBroker::default();
        let root = temp_workspace("terminal-approval");
        let mut config = test_session_config(&root);
        config.agent_command = "codebuddy".into();
        let request = CreateTerminalRequest::new("session-1", "python".to_string()).args(vec![
            "-".into(),
            "<<'PY'\nfrom pathlib import Path\np=Path('src/main.ts')\np.write_text('ok')\nPY"
                .into(),
        ]);

        let gate = ensure_codebuddy_terminal_create_permission(
            &broker,
            &tx,
            &tool_execution_registry,
            &config,
            &config.agent_command,
            &config.workspace_root,
            &denials,
            &approvals,
            &request,
        )
        .expect("permission gate should not fail");

        assert!(matches!(gate, CodeBuddyTerminalCreateGate::Allow));
        assert!(rx.try_recv().is_err());
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn codebuddy_registered_pathless_terminal_approval_skips_second_permission_request() {
        let approvals = CodeBuddyTerminalApprovals::default();
        approvals.register("pnpm build", Vec::new()).unwrap();
        let denials = CodeBuddyTerminalDenials::default();
        let (tx, rx) = mpsc::channel();
        let tool_execution_registry = ToolExecutionRegistry::default();
        let broker = PermissionBroker::default();
        let root = temp_workspace("terminal-pathless-approval");
        let mut config = test_session_config(&root);
        config.agent_command = "codebuddy".into();
        let request =
            CreateTerminalRequest::new("session-1", "pnpm".to_string()).args(vec!["build".into()]);

        let gate = ensure_codebuddy_terminal_create_permission(
            &broker,
            &tx,
            &tool_execution_registry,
            &config,
            &config.agent_command,
            &config.workspace_root,
            &denials,
            &approvals,
            &request,
        )
        .expect("permission gate should not fail");

        assert!(matches!(gate, CodeBuddyTerminalCreateGate::Allow));
        assert!(rx.try_recv().is_err());
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn codex_terminal_mkdir_requests_permission_with_paths() {
        assert_terminal_mkdir_requests_permission("C:/Users/yvonchen/.kodex/bin/codex-acp.exe");
    }

    #[test]
    fn claude_terminal_mkdir_requests_permission_with_paths() {
        assert_terminal_mkdir_requests_permission(
            "C:/Users/yvonchen/.kodex/bin/claude-agent-acp.exe",
        );
    }

    #[test]
    fn codebuddy_registered_terminal_denial_blocks_matching_create_request() {
        let denials = CodeBuddyTerminalDenials::default();
        denials
            .register(
                "pnpm build",
                "Permission rejected by user. Command was not executed.",
            )
            .unwrap();
        let approvals = CodeBuddyTerminalApprovals::default();
        let (tx, _rx) = mpsc::channel();
        let tool_execution_registry = ToolExecutionRegistry::default();
        let broker = PermissionBroker::default();
        let root = temp_workspace("terminal-denial");
        let mut config = test_session_config(&root);
        config.agent_command = "codebuddy".into();
        let request =
            CreateTerminalRequest::new("session-1", "pnpm".to_string()).args(vec!["build".into()]);

        let gate = ensure_codebuddy_terminal_create_permission(
            &broker,
            &tx,
            &tool_execution_registry,
            &config,
            &config.agent_command,
            &config.workspace_root,
            &denials,
            &approvals,
            &request,
        )
        .expect("permission gate should not fail");

        assert!(matches!(gate, CodeBuddyTerminalCreateGate::Deny { .. }));
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    fn assert_terminal_mkdir_requests_permission(agent_command: &str) {
        let broker = PermissionBroker::default();
        let broker_for_gate = broker.clone();
        let denials = CodeBuddyTerminalDenials::default();
        let approvals = CodeBuddyTerminalApprovals::default();
        let tool_execution_registry = ToolExecutionRegistry::default();
        let (tx, rx) = mpsc::channel();
        let root = temp_workspace("terminal-mkdir-request");
        let mut config = test_session_config(&root);
        config.agent_command = agent_command.into();
        let request = CreateTerminalRequest::new("session-1", "/bin/zsh".to_string()).args(vec![
            "-lc".into(),
            "mkdir -p src/server/routes && echo ok".into(),
        ]);
        let config_for_gate = config.clone();

        let handle = thread::spawn(move || {
            ensure_codebuddy_terminal_create_permission(
                &broker_for_gate,
                &tx,
                &tool_execution_registry,
                &config_for_gate,
                &config_for_gate.agent_command,
                &config_for_gate.workspace_root,
                &denials,
                &approvals,
                &request,
            )
        });

        let request_id = match rx
            .recv_timeout(Duration::from_secs(1))
            .expect("terminal gate should emit a permission request")
        {
            ClientEvent::ToolPermissionRequest {
                id, name, details, ..
            } => {
                assert_eq!(name, "Bash");
                let details = details.expect("permission request should include details");
                assert!(details.contains("Command:\nmkdir -p src/server/routes && echo ok"));
                assert!(details.contains("Paths:\n- src/server/routes"));
                id
            }
            event => panic!("unexpected event: {event:?}"),
        };

        broker
            .resolve(&request_id, Some("allow".into()), None, None)
            .expect("permission response should be delivered");
        let gate = handle
            .join()
            .expect("permission gate thread should not panic")
            .expect("permission gate should not fail");
        assert!(matches!(gate, CodeBuddyTerminalCreateGate::Allow));
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn codebuddy_exit_plan_permission_details_include_meta_plan_content() {
        let request = codebuddy_exit_plan_permission_request(
            json!({
                "allowedPrompts": []
            }),
            "# Negative Terms Plan\n\nShip the scoring change.",
        );

        let details = permission_request_details(&request).unwrap();

        assert!(details.contains("# Negative Terms Plan"));
        assert!(!details.contains("allowedPrompts"));
    }

    #[test]
    fn latest_codebuddy_plan_file_content_reads_newest_recent_markdown() {
        let root = temp_workspace("codebuddy-plan-root");
        let plan_root = root.parent().unwrap().join("plans");
        fs::create_dir_all(&plan_root).unwrap();
        fs::write(plan_root.join("old.md"), "# Old Plan\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(plan_root.join("new.md"), "# New Plan\n\nShip it.").unwrap();
        fs::write(plan_root.join("ignored.txt"), "# Ignored").unwrap();

        let content = latest_codebuddy_plan_file_content_from_root(
            &plan_root,
            std::time::Duration::from_secs(60),
        )
        .unwrap();

        assert!(content.contains("# New Plan"));

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn write_permission_request_registers_fs_write_approval() {
        let root = temp_workspace("fs-write-approval-register");
        let request = permission_request_with_raw_input(json!({
            "file_path": "src/main.ts"
        }));
        let approvals = FsWriteApprovals::default();
        let config = test_session_config(&root);

        register_fs_write_approval(
            &request,
            Some("allow"),
            &approvals,
            root.to_str().unwrap(),
            &config,
        )
        .expect("file write approval should register");

        assert!(
            approvals
                .take(root.to_str().unwrap(), &root.join("src/main.ts"))
                .expect("approval registry should not be poisoned")
        );
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn fs_write_registered_approval_skips_second_permission_request() {
        let root = temp_workspace("fs-write-approval-skip");
        let path = root.join("src").join("main.ts");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let approvals = FsWriteApprovals::default();
        approvals
            .register(vec![PathBuf::from("src/main.ts")])
            .expect("approval should register");
        let broker = PermissionBroker::default();
        let (tx, rx) = mpsc::channel();
        let config = test_session_config(&root);

        ensure_fs_write_permission(
            &broker,
            &tx,
            &config,
            root.to_str().unwrap(),
            &path,
            AgentEditPolicy::None,
            &approvals,
        )
        .expect("registered approval should allow write");

        assert!(rx.try_recv().is_err());
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn read_only_fs_write_requires_user_permission_even_inside_workspace() {
        let root = temp_workspace("plan-write");
        let path = root.join("notes.md");

        assert!(!fs_write_is_auto_allowed(
            PermissionPolicyMode::ReadOnly,
            root.to_str().unwrap(),
            &path,
        ));

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn build_fs_write_inside_workspace_requires_user_permission() {
        let root = temp_workspace("build-write");
        let path = root.join("src").join("lib.rs");

        assert!(!fs_write_is_auto_allowed(
            PermissionPolicyMode::Build,
            root.to_str().unwrap(),
            &path,
        ));

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn build_fs_write_outside_workspace_requires_user_permission() {
        let root = temp_workspace("outside-write");
        let outside = root.parent().unwrap().join("outside.md");

        assert!(!fs_write_is_auto_allowed(
            PermissionPolicyMode::Build,
            root.to_str().unwrap(),
            &outside,
        ));

        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    fn temp_workspace(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .join(format!("kodex-fs-write-permission-{label}-{unique}"))
            .join("workspace");
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_session_config(root: &Path) -> SessionConfig {
        SessionConfig {
            workspace_root: root.display().to_string(),
            app_data_root: root
                .parent()
                .unwrap()
                .join("app-data")
                .display()
                .to_string(),
            model: "test-model".into(),
            agent_command: "test-agent".into(),
            agent_env: Vec::new(),
            resume_session_id: None,
            log_id: "test-log".into(),
            acp_port: 0,
            remote_ssh: None,
        }
    }

    fn permission_request_with_raw_input(raw_input: Value) -> RequestPermissionRequest {
        RequestPermissionRequest::new(
            SessionId::new("session-1"),
            ToolCallUpdate::new(
                "permission-1",
                ToolCallUpdateFields::new()
                    .title("Read".to_string())
                    .raw_input(raw_input),
            ),
            vec![
                PermissionOption::new("allow", "Allow", PermissionOptionKind::AllowOnce),
                PermissionOption::new("reject", "Reject", PermissionOptionKind::RejectOnce),
            ],
        )
    }

    fn codebuddy_bash_permission_request(raw_input: Value) -> RequestPermissionRequest {
        let mut payload = serde_json::to_value(permission_request_with_raw_input(raw_input))
            .expect("request should serialize");
        insert_codebuddy_tool_meta(
            &mut payload,
            json!({
                "codebuddy.ai/toolName": "Bash"
            }),
        );
        serde_json::from_value(payload).expect("request should deserialize")
    }

    fn codebuddy_exit_plan_permission_request(
        raw_input: Value,
        plan_content: &str,
    ) -> RequestPermissionRequest {
        let mut payload = serde_json::to_value(permission_request_with_raw_input(raw_input))
            .expect("request should serialize");
        insert_codebuddy_tool_meta(
            &mut payload,
            json!({
                "codebuddy.ai/toolName": "ExitPlanMode",
                "codebuddy.ai/planContent": plan_content
            }),
        );
        serde_json::from_value(payload).expect("request should deserialize")
    }

    fn insert_codebuddy_tool_meta(payload: &mut Value, meta: Value) {
        let tool_call_key = if payload.get("toolCall").is_some() {
            "toolCall"
        } else {
            "tool_call"
        };
        let tool_call = payload
            .get_mut(tool_call_key)
            .and_then(Value::as_object_mut)
            .expect("request should serialize a tool call object");
        tool_call.insert("_meta".into(), meta);
    }
}
