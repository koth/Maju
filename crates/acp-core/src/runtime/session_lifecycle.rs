use super::prompt_content::prompt_capabilities_from_acp;
use super::session_titles::{
    advertised_session_list_capability, command_implies_codex_session_list,
    supports_session_list_title_sync, sync_session_title_from_list,
};
use crate::events::{ClientEvent, SessionConfig};
use crate::mapping::{append_runtime_event_log, emit_notification, session_config_from_parts};
use agent_client_protocol::schema::{
    ClientCapabilities, FileSystemCapabilities, Implementation, InitializeRequest,
    LoadSessionRequest, NewSessionRequest, NewSessionResponse, ProtocolVersion, SessionId,
    SessionNotification, SessionUpdate,
};
use agent_client_protocol::{ActiveSession, Agent, ConnectionTo, Dispatch};
use anyhow::anyhow;
use serde_json::json;
use std::path::PathBuf;
use std::sync::mpsc;
use workspace_model::PromptInputCapabilities;

pub(super) struct StartedSession {
    pub(super) session: ActiveSession<'static, Agent>,
    pub(super) supports_session_list: bool,
    pub(super) prompt_capabilities: PromptInputCapabilities,
}

pub(super) async fn start_session(
    connection: &ConnectionTo<Agent>,
    config: &SessionConfig,
    tx_events: &mpsc::Sender<ClientEvent>,
) -> anyhow::Result<StartedSession> {
    let init = InitializeRequest::new(ProtocolVersion::V1)
        .client_capabilities(
            ClientCapabilities::new()
                .fs(FileSystemCapabilities::new()
                    .read_text_file(true)
                    .write_text_file(true))
                .terminal(true),
        )
        .client_info(Implementation::new("acp-editor", "0.1.0").title("ACP Editor Prototype"));

    let init_response = connection
        .send_request(init)
        .block_task()
        .await
        .map_err(|err| anyhow!(err.to_string()))?;

    let prompt_capabilities =
        prompt_capabilities_from_acp(&init_response.agent_capabilities.prompt_capabilities);
    let supports_load_session = init_response.agent_capabilities.load_session;
    let advertised_session_list =
        advertised_session_list_capability(&init_response.agent_capabilities);
    let codex_session_list_fallback =
        !advertised_session_list && command_implies_codex_session_list(config);
    let supports_session_list = supports_session_list_title_sync(config, advertised_session_list);
    append_runtime_event_log(
        config,
        "session/capabilities",
        &json!({
            "loadSession": supports_load_session,
            "sessionCapabilities": &init_response.agent_capabilities.session_capabilities,
            "advertisedSessionList": advertised_session_list,
            "codexAcpSessionListFallback": codex_session_list_fallback,
            "supportsSessionList": supports_session_list,
        }),
    )?;
    let has_resume_id = config.resume_session_id.is_some();

    let (mut session, initial_session_config) =
        if supports_load_session && config.resume_session_id.is_some() {
            let session_id_str = config.resume_session_id.as_ref().unwrap();
            let session_id: SessionId = session_id_str.clone().into();

            let load_req =
                LoadSessionRequest::new(session_id.clone(), PathBuf::from(&config.workspace_root));
            let load_response = connection
                .send_request(load_req)
                .block_task()
                .await
                .map_err(|err| anyhow!(err.to_string()))?;
            let initial_session_config = session_config_from_parts(
                load_response.config_options,
                load_response.modes.as_ref(),
                load_response.models.as_ref(),
            );

            let fake_response = NewSessionResponse::new(session_id);
            let session = connection
                .attach_session(fake_response, Default::default())
                .map_err(|err| anyhow!(err.to_string()))?;
            (session, initial_session_config)
        } else {
            let new_request = NewSessionRequest::new(PathBuf::from(&config.workspace_root));
            let new_response = connection
                .send_request_to(Agent, new_request)
                .block_task()
                .await
                .map_err(|err| anyhow!(err.to_string()))?;
            let initial_session_config = session_config_from_parts(
                new_response.config_options.clone(),
                new_response.modes.as_ref(),
                new_response.models.as_ref(),
            );
            let session = connection
                .attach_session(new_response, Default::default())
                .map_err(|err| anyhow!(err.to_string()))?;
            (session, initial_session_config)
        };

    if supports_load_session && has_resume_id {
        drain_loaded_session_replay(&mut session, tx_events, config).await;
    }

    let _ = tx_events.send(ClientEvent::SessionStarted {
        session_id: session.session_id().0.to_string(),
    });
    if supports_session_list {
        if let Err(error) =
            sync_session_title_from_list(config, tx_events, connection, session.session_id()).await
        {
            let _ = append_runtime_event_log(
                config,
                "session/list_title_sync_failed",
                &json!({
                    "phase": "startup",
                    "error": error.to_string(),
                }),
            );
        }
    }
    let _ = tx_events.send(ClientEvent::PromptCapabilitiesUpdated {
        capabilities: prompt_capabilities.clone(),
    });
    if initial_session_config.hydrated {
        let _ = tx_events.send(ClientEvent::SessionConfigUpdated {
            state: initial_session_config,
        });
    }

    Ok(StartedSession {
        session,
        supports_session_list,
        prompt_capabilities,
    })
}

async fn drain_loaded_session_replay(
    session: &mut ActiveSession<'static, Agent>,
    tx_events: &mpsc::Sender<ClientEvent>,
    config: &SessionConfig,
) {
    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), session.read_update())
            .await
        {
            Ok(Ok(update)) => {
                if let agent_client_protocol::SessionMessage::SessionMessage(dispatch) = update {
                    let _ = agent_client_protocol::util::MatchDispatch::new(dispatch)
                        .if_notification(async |notification: SessionNotification| {
                            match &notification.update {
                                SessionUpdate::AvailableCommandsUpdate(_)
                                | SessionUpdate::ConfigOptionUpdate(_)
                                | SessionUpdate::CurrentModeUpdate(_) => {
                                    let _ = emit_notification(
                                        tx_events,
                                        &config.workspace_root,
                                        notification,
                                    );
                                }
                                _ => {}
                            }
                            Ok(())
                        })
                        .await
                        .otherwise(|_dispatch: Dispatch| async { Ok(()) })
                        .await;
                }
            }
            Ok(Err(_)) | Err(_) => break,
        }
    }
}
