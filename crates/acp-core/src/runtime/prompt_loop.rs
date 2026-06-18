use super::codebuddy::send_codebuddy_interruption_resolution;
use super::permissions::PermissionBroker;
use super::prompt_content::{
    prompt_contains_file, prompt_contains_image, prompt_content_to_acp, prompt_title_text,
};
use super::session_titles::emit_turn_finished;
use super::terminal::TerminalManager;
use super::tool_stop::ToolStopNotification;
use super::tool_stop::{KODEX_TOOL_STOP_METHOD, ToolExecutionRegistry};
use super::{RuntimeCommand, ShutdownSignal};
use crate::events::{ClientEvent, SessionConfig};
use crate::mapping::{
    append_notification_log, append_runtime_event_log, emit_notification, is_session_state_update,
    session_config_from_options,
};
use agent_client_protocol::schema::{
    CancelNotification, PromptRequest, PromptResponse, SessionNotification,
    SetSessionConfigOptionRequest, SetSessionModeRequest, SetSessionModelRequest, StopReason,
};
use agent_client_protocol::{ActiveSession, Agent, Dispatch};
use anyhow::anyhow;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use workspace_model::PromptInputCapabilities;

const KODEX_PROVIDER_VALUE_PREFIX: &str = "kodex-provider:";
const KODEX_PROVIDER_SLASH_VALUE_PREFIX: &str = "kodex-provider/";

pub(super) async fn run_command_loop(
    session: &mut ActiveSession<'static, Agent>,
    config: &SessionConfig,
    tx_events: &mpsc::Sender<ClientEvent>,
    rx_commands: mpsc::Receiver<RuntimeCommand>,
    permission_broker: PermissionBroker,
    terminal_manager: Arc<TerminalManager>,
    tool_execution_registry: ToolExecutionRegistry,
    shutdown_signal: ShutdownSignal,
    supports_session_list: bool,
    prompt_capabilities: PromptInputCapabilities,
) -> anyhow::Result<()> {
    loop {
        let command = loop {
            match rx_commands.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(command) => break command,
                Err(RecvTimeoutError::Timeout) => {
                    drain_idle_session_state_update(
                        session,
                        config,
                        tx_events,
                        &tool_execution_registry,
                    )
                    .await?;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(anyhow!("ACP command channel closed"));
                }
            }
        };

        match command {
            RuntimeCommand::SendPrompt(prompt) => {
                if prompt_contains_image(&prompt) && !prompt_capabilities.image {
                    let _ = tx_events.send(ClientEvent::Interrupted {
                        reason: "Active agent does not support image prompts".into(),
                    });
                    continue;
                }
                if prompt_contains_file(&prompt) && !prompt_capabilities.embedded_context {
                    let _ = tx_events.send(ClientEvent::Interrupted {
                        reason: "Active agent does not support file attachments".into(),
                    });
                    continue;
                }

                let title_source = prompt_title_text(&prompt);
                let mut content_blocks = Vec::new();
                let mut prompt_error = None;
                for content in prompt {
                    match prompt_content_to_acp(content, &config.workspace_root) {
                        Ok(blocks) => content_blocks.extend(blocks),
                        Err(error) => {
                            prompt_error = Some(error);
                            break;
                        }
                    }
                }
                if let Some(error) = prompt_error {
                    let _ = tx_events.send(ClientEvent::Interrupted {
                        reason: error.to_string(),
                    });
                    continue;
                }
                if content_blocks.is_empty() {
                    let _ = tx_events.send(ClientEvent::Interrupted {
                        reason: "Prompt cannot be empty".into(),
                    });
                    continue;
                }

                let (stop_tx, stop_rx) = mpsc::channel();
                session
                    .connection()
                    .send_request_to(
                        Agent,
                        PromptRequest::new(session.session_id().clone(), content_blocks),
                    )
                    .on_receiving_result(async move |result| {
                        let PromptResponse { stop_reason, .. } = result?;
                        stop_tx.send(stop_reason).map_err(|_| {
                            agent_client_protocol::util::internal_error(
                                "prompt stop channel closed",
                            )
                        })?;
                        Ok(())
                    })
                    .map_err(|err| anyhow!(err.to_string()))?;

                let mut cancel_sent = false;
                loop {
                    let next_message = tokio::time::timeout(
                        std::time::Duration::from_millis(50),
                        session.read_update(),
                    )
                    .await;

                    let message = match next_message {
                        Ok(Ok(message)) => Some(message),
                        Ok(Err(err)) => {
                            if let Some(reason) = recv_stop_reason_with_grace(&stop_rx).await {
                                append_runtime_event_log(
                                    &config,
                                    "session/read_update_closed_after_stop",
                                    &json!({ "error": err.to_string() }),
                                )?;
                                emit_turn_finished(
                                    &config,
                                    &tx_events,
                                    &session.connection(),
                                    session.session_id(),
                                    supports_session_list,
                                    title_source.as_deref(),
                                    reason,
                                )
                                .await?;
                                tool_execution_registry.clear();
                                return Ok(());
                            }
                            append_runtime_event_log(
                                &config,
                                "session/read_update_error",
                                &json!({ "error": err.to_string() }),
                            )?;
                            return Err(anyhow!(err.to_string()));
                        }
                        Err(_) => None,
                    };

                    while let Ok(command) = rx_commands.try_recv() {
                        match command {
                            RuntimeCommand::CancelPrompt { reply_tx } => {
                                let result = (|| -> anyhow::Result<()> {
                                    if cancel_sent {
                                        return Ok(());
                                    }
                                    permission_broker.cancel_all()?;
                                    session
                                        .connection()
                                        .send_notification_to(
                                            Agent,
                                            CancelNotification::new(session.session_id().clone()),
                                        )
                                        .map_err(|err| anyhow!(err.to_string()))?;
                                    append_runtime_event_log(
                                        &config,
                                        "session/cancel",
                                        &json!({
                                            "sessionId": session.session_id().0
                                        }),
                                    )?;
                                    cancel_sent = true;
                                    Ok(())
                                })();
                                let _ = reply_tx.send(result);
                            }
                            RuntimeCommand::StopTool {
                                tool_call_id,
                                reply_tx,
                            } => {
                                handle_stop_tool_command(
                                    tx_events,
                                    tool_call_id,
                                    reply_tx,
                                    &tool_execution_registry,
                                    &terminal_manager,
                                    &permission_broker,
                                    |tool_call_id| {
                                        send_agent_owned_stop(config, session, tool_call_id)
                                    },
                                );
                            }
                            RuntimeCommand::Shutdown => {
                                shutdown_signal.request_shutdown();
                                tool_execution_registry.clear();
                                return Ok(());
                            }
                            RuntimeCommand::ResolveCodeBuddyInterruption {
                                session_id,
                                tool_call_id,
                                decision,
                                reply_tx,
                            } => {
                                let connection = session.connection();
                                let result = send_codebuddy_interruption_resolution(
                                    &config,
                                    &connection,
                                    &session_id,
                                    &tool_call_id,
                                    &decision,
                                    None,
                                );
                                let _ = reply_tx.send(result);
                            }
                            RuntimeCommand::SendPrompt(_)
                            | RuntimeCommand::SetConfigOption { .. }
                            | RuntimeCommand::SetMode { .. }
                            | RuntimeCommand::SetModel { .. } => {}
                        }
                    }

                    let Some(message) = message else {
                        if let Ok(reason) = stop_rx.try_recv() {
                            emit_turn_finished(
                                &config,
                                &tx_events,
                                &session.connection(),
                                session.session_id(),
                                supports_session_list,
                                title_source.as_deref(),
                                reason,
                            )
                            .await?;
                            tool_execution_registry.clear();
                            break;
                        }
                        continue;
                    };

                    match message {
                        agent_client_protocol::SessionMessage::SessionMessage(dispatch) => {
                            handle_session_dispatch(
                                &config,
                                &tx_events,
                                &tool_execution_registry,
                                dispatch,
                                false,
                            )?;
                        }
                        agent_client_protocol::SessionMessage::StopReason(reason) => {
                            emit_turn_finished(
                                &config,
                                &tx_events,
                                &session.connection(),
                                session.session_id(),
                                supports_session_list,
                                title_source.as_deref(),
                                reason,
                            )
                            .await?;
                            tool_execution_registry.clear();
                            break;
                        }
                        other => {
                            let payload = json!({
                                "message": format!("{other:?}")
                            });
                            append_runtime_event_log(&config, "session/message_other", &payload)?;
                        }
                    }
                }
            }
            RuntimeCommand::SetConfigOption {
                config_id,
                value_id,
                provider,
                reply_tx,
            } => {
                let request_value_id = encode_model_value_with_provider(
                    config_id.as_str(),
                    value_id.clone(),
                    provider,
                );
                let result = async {
                    let response = session
                        .connection()
                        .send_request_to(
                            Agent,
                            SetSessionConfigOptionRequest::new(
                                session.session_id().clone(),
                                config_id,
                                request_value_id,
                            ),
                        )
                        .block_task()
                        .await
                        .map_err(|err| anyhow!(err.to_string()))?;
                    Ok(vec![ClientEvent::SessionConfigUpdated {
                        state: session_config_from_options(response.config_options),
                    }])
                }
                .await;
                let _ = reply_tx.send(result);
            }
            RuntimeCommand::SetMode { mode_id, reply_tx } => {
                let result = async {
                    session
                        .connection()
                        .send_request_to(
                            Agent,
                            SetSessionModeRequest::new(
                                session.session_id().clone(),
                                mode_id.clone(),
                            ),
                        )
                        .block_task()
                        .await
                        .map_err(|err| anyhow!(err.to_string()))?;
                    Ok(vec![ClientEvent::SessionConfigValueChanged {
                        control_id: "mode".into(),
                        value_id: mode_id,
                        value_label: None,
                    }])
                }
                .await;
                let _ = reply_tx.send(result);
            }
            RuntimeCommand::SetModel {
                model_id,
                provider,
                reply_tx,
            } => {
                let request_model_id = encode_provider_value(model_id.clone(), provider);
                let result = async {
                    session
                        .connection()
                        .send_request_to(
                            Agent,
                            SetSessionModelRequest::new(
                                session.session_id().clone(),
                                request_model_id,
                            ),
                        )
                        .block_task()
                        .await
                        .map_err(|err| anyhow!(err.to_string()))?;
                    Ok(vec![ClientEvent::SessionConfigValueChanged {
                        control_id: "model".into(),
                        value_id: model_id,
                        value_label: None,
                    }])
                }
                .await;
                let _ = reply_tx.send(result);
            }
            RuntimeCommand::ResolveCodeBuddyInterruption {
                session_id,
                tool_call_id,
                decision,
                reply_tx,
            } => {
                let connection = session.connection();
                let result = send_codebuddy_interruption_resolution(
                    &config,
                    &connection,
                    &session_id,
                    &tool_call_id,
                    &decision,
                    None,
                );
                let _ = reply_tx.send(result);
            }
            RuntimeCommand::CancelPrompt { reply_tx } => {
                let _ = permission_broker.cancel_all();
                let _ = reply_tx.send(Ok(()));
            }
            RuntimeCommand::StopTool {
                tool_call_id,
                reply_tx,
            } => {
                handle_stop_tool_command(
                    tx_events,
                    tool_call_id,
                    reply_tx,
                    &tool_execution_registry,
                    &terminal_manager,
                    &permission_broker,
                    |tool_call_id| send_agent_owned_stop(config, session, tool_call_id),
                );
            }
            RuntimeCommand::Shutdown => {
                shutdown_signal.request_shutdown();
                tool_execution_registry.clear();
                break;
            }
        }
    }

    Ok(())
}

fn encode_model_value_with_provider(
    config_id: &str,
    value_id: String,
    provider: Option<String>,
) -> String {
    if config_id == "model" {
        encode_provider_value(value_id, provider)
    } else {
        value_id
    }
}

fn encode_provider_value(value_id: String, provider: Option<String>) -> String {
    if value_id.starts_with(KODEX_PROVIDER_VALUE_PREFIX)
        || value_id.starts_with(KODEX_PROVIDER_SLASH_VALUE_PREFIX)
    {
        return value_id;
    }
    let Some(provider) = provider.map(|provider| provider.trim().to_string()) else {
        return value_id;
    };
    if provider.is_empty() {
        return value_id;
    }
    format!("{KODEX_PROVIDER_VALUE_PREFIX}{provider}:{value_id}")
}

async fn drain_idle_session_state_update(
    session: &mut ActiveSession<'static, Agent>,
    config: &SessionConfig,
    tx_events: &mpsc::Sender<ClientEvent>,
    tool_execution_registry: &ToolExecutionRegistry,
) -> anyhow::Result<()> {
    let next_message =
        tokio::time::timeout(std::time::Duration::from_millis(1), session.read_update()).await;

    let message = match next_message {
        Ok(Ok(message)) => message,
        Ok(Err(err)) => return Err(anyhow!(err.to_string())),
        Err(_) => return Ok(()),
    };

    if let agent_client_protocol::SessionMessage::SessionMessage(dispatch) = message {
        handle_session_dispatch(config, tx_events, tool_execution_registry, dispatch, true)?;
    }

    Ok(())
}

fn handle_stop_tool_command<F>(
    tx_events: &mpsc::Sender<ClientEvent>,
    tool_call_id: String,
    reply_tx: mpsc::Sender<anyhow::Result<Vec<ClientEvent>>>,
    tool_execution_registry: &ToolExecutionRegistry,
    terminal_manager: &TerminalManager,
    permission_broker: &PermissionBroker,
    stop_agent_owned: F,
) where
    F: FnMut(&str) -> anyhow::Result<bool>,
{
    let result = stop_tool_events_with_agent_owned(
        &tool_call_id,
        tool_execution_registry,
        terminal_manager,
        permission_broker,
        stop_agent_owned,
    );
    if let Ok(events) = &result {
        for event in events {
            let _ = tx_events.send(event.clone());
        }
    }
    let _ = reply_tx.send(result);
}

fn stop_tool_events_with_agent_owned<F>(
    tool_call_id: &str,
    tool_execution_registry: &ToolExecutionRegistry,
    terminal_manager: &TerminalManager,
    permission_broker: &PermissionBroker,
    stop_agent_owned: F,
) -> anyhow::Result<Vec<ClientEvent>>
where
    F: FnMut(&str) -> anyhow::Result<bool>,
{
    tool_execution_registry.stop_tool(
        tool_call_id,
        terminal_manager,
        permission_broker,
        stop_agent_owned,
    )
}

fn send_agent_owned_stop(
    config: &SessionConfig,
    session: &ActiveSession<'static, Agent>,
    tool_call_id: &str,
) -> anyhow::Result<bool> {
    session
        .connection()
        .send_notification_to(
            Agent,
            ToolStopNotification::new(session.session_id().0.as_ref(), tool_call_id),
        )
        .map_err(|err| anyhow!(err.to_string()))?;
    append_runtime_event_log(
        config,
        KODEX_TOOL_STOP_METHOD,
        &json!({
            "sessionId": session.session_id().0.as_ref(),
            "toolCallId": tool_call_id,
        }),
    )
    .ok();
    Ok(true)
}

fn handle_session_dispatch(
    config: &SessionConfig,
    tx_events: &mpsc::Sender<ClientEvent>,
    tool_execution_registry: &ToolExecutionRegistry,
    dispatch: Dispatch,
    state_only: bool,
) -> anyhow::Result<()> {
    let Dispatch::Notification(untyped) = dispatch else {
        return Ok(());
    };
    let (method, payload) = untyped.into_parts();
    append_notification_log(config, &method, &payload)?;

    if method != "session/update" {
        return Ok(());
    }
    if is_codebuddy_session_end_update(&payload) {
        append_runtime_event_log(config, "session/session_end_ignored", &payload)?;
        return Ok(());
    }

    let stop_events = tool_execution_registry.events_from_session_payload(&payload)?;
    let notification: SessionNotification =
        serde_json::from_value(payload).map_err(|err| anyhow!(err.to_string()))?;
    if !state_only || is_session_state_update(&notification.update) {
        emit_notification(tx_events, &config.workspace_root, notification)?;
        for event in stop_events {
            let _ = tx_events.send(event);
        }
    }
    Ok(())
}

fn is_codebuddy_session_end_update(payload: &Value) -> bool {
    payload
        .get("update")
        .and_then(|update| update.get("sessionUpdate"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "session_end")
}

async fn recv_stop_reason_with_grace(stop_rx: &mpsc::Receiver<StopReason>) -> Option<StopReason> {
    for attempt in 0..5 {
        match stop_rx.try_recv() {
            Ok(reason) => return Some(reason),
            Err(mpsc::TryRecvError::Disconnected) => return None,
            Err(mpsc::TryRecvError::Empty) if attempt < 4 => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(mpsc::TryRecvError::Empty) => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn codebuddy_session_end_update_is_ignored() {
        let payload = json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "session_end",
                "stopReason": "end_turn"
            }
        });

        assert!(is_codebuddy_session_end_update(&payload));
    }

    #[test]
    fn slash_provider_model_values_are_not_encoded_again() {
        assert_eq!(
            encode_provider_value(
                "kodex-provider/kimi_code/kimi-for-coding".to_string(),
                Some("kimi_code".to_string()),
            ),
            "kodex-provider/kimi_code/kimi-for-coding"
        );
    }

    #[test]
    fn in_flight_stop_tool_command_reaches_agent_owned_stop_path() {
        let (event_tx, event_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let registry = ToolExecutionRegistry::default();
        registry
            .register_agent_owned("tool-agent-1")
            .expect("agent-owned handle should register");
        let terminal_manager = TerminalManager::default();
        let permission_broker = PermissionBroker::default();
        let mut stopped = Vec::new();

        handle_stop_tool_command(
            &event_tx,
            "tool-agent-1".into(),
            reply_tx,
            &registry,
            &terminal_manager,
            &permission_broker,
            |tool_call_id| {
                stopped.push(tool_call_id.to_string());
                Ok(true)
            },
        );

        let events = reply_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("stop reply should be sent")
            .expect("stop should succeed");
        assert_eq!(stopped, vec!["tool-agent-1"]);
        assert!(events.iter().any(|event| {
            matches!(
                event,
                ClientEvent::ToolStopped { id, .. } if id == "tool-agent-1"
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                ClientEvent::ToolStopAvailability { id, can_stop: false, .. }
                    if id == "tool-agent-1"
            )
        }));

        let emitted: Vec<_> = event_rx.try_iter().collect();
        assert_eq!(emitted, events);
    }
}
