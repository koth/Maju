use super::codebuddy::send_codebuddy_interruption_resolution;
use super::permissions::PermissionBroker;
use super::prompt_content::{prompt_contains_file, prompt_contains_image, prompt_content_to_acp};
use super::session_titles::emit_turn_finished;
use super::{RuntimeCommand, ShutdownSignal};
use crate::events::{ClientEvent, SessionConfig};
use crate::mapping::{
    append_notification_log, append_runtime_event_log, append_typed_notification_log,
    emit_notification, session_config_from_options,
};
use agent_client_protocol::schema::{
    CancelNotification, PromptRequest, PromptResponse, SessionNotification,
    SetSessionConfigOptionRequest, SetSessionModeRequest, SetSessionModelRequest, StopReason,
};
use agent_client_protocol::{ActiveSession, Agent, Dispatch};
use anyhow::anyhow;
use serde_json::json;
use std::sync::mpsc;
use workspace_model::PromptInputCapabilities;

pub(super) async fn run_command_loop(
    session: &mut ActiveSession<'static, Agent>,
    config: &SessionConfig,
    tx_events: &mpsc::Sender<ClientEvent>,
    rx_commands: mpsc::Receiver<RuntimeCommand>,
    permission_broker: PermissionBroker,
    shutdown_signal: ShutdownSignal,
    supports_session_list: bool,
    prompt_capabilities: PromptInputCapabilities,
) -> anyhow::Result<()> {
    loop {
        let command = rx_commands
            .recv()
            .map_err(|_| anyhow!("ACP command channel closed"))?;

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

                let content_blocks = prompt
                    .into_iter()
                    .filter_map(prompt_content_to_acp)
                    .collect::<Vec<_>>();
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
                                    reason,
                                )
                                .await?;
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
                            RuntimeCommand::Shutdown => {
                                shutdown_signal.request_shutdown();
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
                                )
                                .await;
                                if result.is_ok() {
                                    let _ = permission_broker.clear_early_resolution(&tool_call_id);
                                }
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
                                reason,
                            )
                            .await?;
                            break;
                        }
                        continue;
                    };

                    match message {
                        agent_client_protocol::SessionMessage::SessionMessage(dispatch) => {
                            agent_client_protocol::util::MatchDispatch::new(dispatch)
                                .if_notification(async |notification: SessionNotification| {
                                    append_typed_notification_log(&config, &notification)?;
                                    emit_notification(
                                        &tx_events,
                                        &config.workspace_root,
                                        notification,
                                    )?;
                                    Ok(())
                                })
                                .await
                                .otherwise(|dispatch: Dispatch| async {
                                    if let Dispatch::Notification(untyped) = dispatch {
                                        let (method, payload) = untyped.into_parts();
                                        append_notification_log(&config, &method, &payload)?;
                                    }
                                    Ok(())
                                })
                                .await
                                .map_err(|err| anyhow!(err.to_string()))?;
                        }
                        agent_client_protocol::SessionMessage::StopReason(reason) => {
                            emit_turn_finished(
                                &config,
                                &tx_events,
                                &session.connection(),
                                session.session_id(),
                                supports_session_list,
                                reason,
                            )
                            .await?;
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
                reply_tx,
            } => {
                let result = async {
                    let response = session
                        .connection()
                        .send_request_to(
                            Agent,
                            SetSessionConfigOptionRequest::new(
                                session.session_id().clone(),
                                config_id,
                                value_id,
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
            RuntimeCommand::SetModel { model_id, reply_tx } => {
                let result = async {
                    session
                        .connection()
                        .send_request_to(
                            Agent,
                            SetSessionModelRequest::new(
                                session.session_id().clone(),
                                model_id.clone(),
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
                )
                .await;
                if result.is_ok() {
                    let _ = permission_broker.clear_early_resolution(&tool_call_id);
                }
                let _ = reply_tx.send(result);
            }
            RuntimeCommand::CancelPrompt { reply_tx } => {
                let _ = permission_broker.cancel_all();
                let _ = reply_tx.send(Ok(()));
            }
            RuntimeCommand::Shutdown => {
                shutdown_signal.request_shutdown();
                break;
            }
        }
    }

    Ok(())
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
