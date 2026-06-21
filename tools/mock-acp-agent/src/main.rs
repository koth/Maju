use agent_client_protocol::schema::{
    AgentCapabilities, AvailableCommand, AvailableCommandsUpdate, ContentBlock, ContentChunk, Diff,
    InitializeRequest, InitializeResponse, LoadSessionRequest, LoadSessionResponse, ModelInfo,
    NewSessionRequest, NewSessionResponse, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus,
    PromptCapabilities, PromptRequest, PromptResponse, SessionModelState, SetSessionModelRequest,
    SetSessionModelResponse, StopReason, ToolCall, ToolCallContent, ToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields, ToolKind,
};
use agent_client_protocol::{Agent, Client, ConnectionTo, Dispatch, Result};
use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[tokio::main]
async fn main() -> Result<()> {
    let port = parse_port_arg()?;
    if let Some(port) = port {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(agent_client_protocol::Error::into_internal_error)?;
        let (stream, _) = listener
            .accept()
            .await
            .map_err(agent_client_protocol::Error::into_internal_error)?;
        let (reader, writer) = stream.into_split();
        return run_agent(reader, writer).await;
    }

    run_agent(tokio::io::stdin(), tokio::io::stdout()).await
}

fn parse_port_arg() -> Result<Option<u16>> {
    let mut port = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--port" {
            let value = args.next().ok_or_else(|| {
                agent_client_protocol::util::internal_error("--port requires a value")
            })?;
            port = Some(value.parse::<u16>().map_err(|error| {
                agent_client_protocol::util::internal_error(format!(
                    "invalid --port value: {error}"
                ))
            })?);
        } else {
            return Err(agent_client_protocol::util::internal_error(format!(
                "unsupported argument: {arg}"
            )));
        }
    }
    Ok(port)
}

async fn run_agent<R, W>(reader: R, writer: W) -> Result<()>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let prompt_count = Arc::new(AtomicUsize::new(0));
    let prompt_count_for_handler = prompt_count.clone();
    Agent
        .builder()
        .name("mock-acp-agent")
        .on_receive_request(
            async move |request: InitializeRequest, responder, _connection| {
                responder.respond(
                    InitializeResponse::new(request.protocol_version)
                        .agent_capabilities(
                            AgentCapabilities::new()
                                .load_session(true)
                                .prompt_capabilities(
                                    PromptCapabilities::new().image(true).embedded_context(true),
                                ),
                        )
                        .agent_info(
                            agent_client_protocol::schema::Implementation::new(
                                "mock-acp-agent",
                                "0.1.0",
                            )
                            .title("Mock ACP Agent"),
                        ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_: LoadSessionRequest, responder, _connection| {
                responder.respond(LoadSessionResponse::new().models(SessionModelState::new(
                    "mock-loaded",
                    vec![
                        ModelInfo::new("mock-loaded", "Mock Loaded"),
                        ModelInfo::new("mock-smart", "Mock Smart"),
                        ModelInfo::new("mock-loaded-alt", "Mock Loaded Alt"),
                    ],
                )))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_: NewSessionRequest, responder, connection: ConnectionTo<Client>| {
                responder.respond(NewSessionResponse::new("mock-session").models(
                    SessionModelState::new(
                        "mock-fast",
                        vec![
                            ModelInfo::new("mock-fast", "Mock Fast"),
                            ModelInfo::new("mock-smart", "Mock Smart"),
                        ],
                    ),
                ))?;

                connection.send_notification(
                    agent_client_protocol::schema::SessionNotification::new(
                        "mock-session",
                        agent_client_protocol::schema::SessionUpdate::AvailableCommandsUpdate(
                            AvailableCommandsUpdate::new(vec![AvailableCommand::new(
                                "mock",
                                "Mock startup slash command",
                            )]),
                        ),
                    ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_: SetSessionModelRequest, responder, _connection| {
                responder.respond(SetSessionModelResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: PromptRequest, responder, connection: ConnectionTo<Client>| {
                let session_id = request.session_id.clone();
                let prompt_index = prompt_count_for_handler.fetch_add(1, Ordering::SeqCst);
                let steer_test = env::var("KODEX_MOCK_ACP_STEER_TEST").ok().as_deref() == Some("1");
                let steer_never_responds = env::var("KODEX_MOCK_ACP_STEER_NEVER_RESPONDS")
                    .ok()
                    .as_deref()
                    == Some("1");
                let prompt_never_responds = env::var("KODEX_MOCK_ACP_PROMPT_NEVER_RESPONDS")
                    .ok()
                    .as_deref()
                    == Some("1");
                let prompt_delay_ms = env::var("KODEX_MOCK_ACP_PROMPT_DELAY_MS")
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0);
                let prompt_text = prompt_text(&request.prompt);

                for item in request.prompt {
                    connection.send_notification(
                        agent_client_protocol::schema::SessionNotification::new(
                            session_id.clone(),
                            agent_client_protocol::schema::SessionUpdate::UserMessageChunk(
                                ContentChunk::new(item),
                            ),
                        ),
                    )?;
                }

                if steer_test {
                    if prompt_index == 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    } else {
                        connection.send_notification(
                            agent_client_protocol::schema::SessionNotification::new(
                                session_id.clone(),
                                agent_client_protocol::schema::SessionUpdate::AgentMessageChunk(
                                    ContentChunk::new(
                                        format!("Steer accepted: {prompt_text}").into(),
                                    ),
                                ),
                            ),
                        )?;
                        if steer_never_responds {
                            std::future::pending::<()>().await;
                        }
                    }
                    return responder.respond(PromptResponse::new(StopReason::EndTurn));
                }

                if prompt_never_responds {
                    std::future::pending::<()>().await;
                }
                if prompt_delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(prompt_delay_ms)).await;
                }

                connection.send_notification(
                    agent_client_protocol::schema::SessionNotification::new(
                        session_id.clone(),
                        agent_client_protocol::schema::SessionUpdate::Plan(Plan::new(vec![
                            PlanEntry::new(
                                "Inspect the workspace state",
                                PlanEntryPriority::High,
                                PlanEntryStatus::InProgress,
                            ),
                            PlanEntry::new(
                                "Review tool output",
                                PlanEntryPriority::Medium,
                                PlanEntryStatus::Pending,
                            ),
                            PlanEntry::new(
                                "Summarize findings",
                                PlanEntryPriority::Low,
                                PlanEntryStatus::Pending,
                            ),
                        ])),
                    ),
                )?;

                let tool_call = ToolCall::new("tool-1", "Reviewing workspace")
                    .kind(ToolKind::Search)
                    .status(ToolCallStatus::Pending);
                connection.send_notification(
                    agent_client_protocol::schema::SessionNotification::new(
                        session_id.clone(),
                        agent_client_protocol::schema::SessionUpdate::ToolCall(tool_call),
                    ),
                )?;

                connection.send_notification(
                    agent_client_protocol::schema::SessionNotification::new(
                        session_id.clone(),
                        agent_client_protocol::schema::SessionUpdate::Plan(Plan::new(vec![
                            PlanEntry::new(
                                "Inspect the workspace state",
                                PlanEntryPriority::High,
                                PlanEntryStatus::Completed,
                            ),
                            PlanEntry::new(
                                "Review tool output",
                                PlanEntryPriority::Medium,
                                PlanEntryStatus::InProgress,
                            ),
                            PlanEntry::new(
                                "Summarize findings",
                                PlanEntryPriority::Low,
                                PlanEntryStatus::Pending,
                            ),
                        ])),
                    ),
                )?;

                connection.send_notification(
                    agent_client_protocol::schema::SessionNotification::new(
                        session_id.clone(),
                        agent_client_protocol::schema::SessionUpdate::ToolCallUpdate(
                            ToolCallUpdate::new(
                                "tool-1",
                                ToolCallUpdateFields::new()
                                    .status(ToolCallStatus::InProgress)
                                    .title("Scanning Git worktree")
                                    .content(vec![ToolCallContent::from(
                                        "Found repository changes",
                                    )]),
                            ),
                        ),
                    ),
                )?;

                connection.send_notification(
                    agent_client_protocol::schema::SessionNotification::new(
                        session_id.clone(),
                        agent_client_protocol::schema::SessionUpdate::ToolCallUpdate(
                            ToolCallUpdate::new(
                                "tool-1",
                                ToolCallUpdateFields::new()
                                    .status(ToolCallStatus::Completed)
                                    .title("Workspace review complete")
                                    .content(vec![ToolCallContent::Diff(
                                        Diff::new(
                                            "src/main.rs",
                                            "fn main() {\n    println!(\"hello acp\");\n}\n",
                                        )
                                        .old_text("fn main() {\n    println!(\"hello\");\n}\n"),
                                    )]),
                            ),
                        ),
                    ),
                )?;

                connection.send_notification(
                    agent_client_protocol::schema::SessionNotification::new(
                        session_id.clone(),
                        agent_client_protocol::schema::SessionUpdate::Plan(Plan::new(vec![
                            PlanEntry::new(
                                "Inspect the workspace state",
                                PlanEntryPriority::High,
                                PlanEntryStatus::Completed,
                            ),
                            PlanEntry::new(
                                "Review tool output",
                                PlanEntryPriority::Medium,
                                PlanEntryStatus::Completed,
                            ),
                            PlanEntry::new(
                                "Summarize findings",
                                PlanEntryPriority::Low,
                                PlanEntryStatus::InProgress,
                            ),
                        ])),
                    ),
                )?;

                connection
                    .send_notification(agent_client_protocol::schema::SessionNotification::new(
                    session_id,
                    agent_client_protocol::schema::SessionUpdate::AgentMessageChunk(
                        ContentChunk::new(
                            "Real ACP session connected. Tool activity and diffs are streaming."
                                .into(),
                        ),
                    ),
                ))?;

                responder.respond(PromptResponse::new(StopReason::EndTurn))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_dispatch(
            async move |message: Dispatch, cx: ConnectionTo<Client>| {
                message.respond_with_error(
                    agent_client_protocol::util::internal_error("unsupported request"),
                    cx,
                )
            },
            agent_client_protocol::on_receive_dispatch!(),
        )
        .connect_to(agent_client_protocol::ByteStreams::new(
            writer.compat_write(),
            reader.compat(),
        ))
        .await
}

fn prompt_text(prompt: &[ContentBlock]) -> String {
    prompt
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.trim()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
