use crate::events::{ClientEvent, SessionConfig};
use crate::mapping::append_runtime_event_log;
use anyhow::Context;
use serde_json::json;
use std::sync::mpsc;
use workspace_model::UserPromptContent;

mod agent_process;
mod client_handlers;
mod codebuddy;
mod permissions;
mod process;
mod prompt_content;
mod prompt_loop;
mod session_lifecycle;
mod session_titles;
mod shutdown;
mod terminal;
#[cfg(test)]
mod tests;
mod workspace_paths;
use agent_process::{AgentTransport, HiddenAgentProcess, TcpAgentProcess};
pub(crate) use permissions::PermissionBroker;
pub(crate) use shutdown::ShutdownSignal;

pub(crate) enum RuntimeCommand {
    SendPrompt(Vec<UserPromptContent>),
    SetConfigOption {
        config_id: String,
        value_id: String,
        reply_tx: mpsc::Sender<anyhow::Result<Vec<ClientEvent>>>,
    },
    SetMode {
        mode_id: String,
        reply_tx: mpsc::Sender<anyhow::Result<Vec<ClientEvent>>>,
    },
    SetModel {
        model_id: String,
        reply_tx: mpsc::Sender<anyhow::Result<Vec<ClientEvent>>>,
    },
    ResolveCodeBuddyInterruption {
        session_id: String,
        tool_call_id: String,
        decision: String,
        reply_tx: mpsc::Sender<anyhow::Result<()>>,
    },
    CancelPrompt {
        reply_tx: mpsc::Sender<anyhow::Result<()>>,
    },
    Shutdown,
}

pub(crate) fn run_session(
    config: SessionConfig,
    tx_events: mpsc::Sender<ClientEvent>,
    rx_commands: mpsc::Receiver<RuntimeCommand>,
    permission_broker: PermissionBroker,
    shutdown_signal: ShutdownSignal,
) -> anyhow::Result<()> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            let _ = append_runtime_event_log(
                &config,
                "runtime/session_result",
                &json!({
                    "status": "error",
                    "error": format!("failed to create tokio runtime: {err}")
                }),
            );
            return Err(err).context("failed to create tokio runtime");
        }
    };

    let log_config = config.clone();
    let result: anyhow::Result<()> = runtime.block_on(async move {
        let agent = if config.acp_port > 0 {
            AgentTransport::Tcp(
                TcpAgentProcess::from_config(&config)?.shutdown_signal(shutdown_signal.clone()),
            )
        } else {
            AgentTransport::Stdio(
                HiddenAgentProcess::from_config(&config)?.shutdown_signal(shutdown_signal.clone()),
            )
        };
        client_handlers::connect_agent_client(
            agent,
            config,
            tx_events,
            rx_commands,
            permission_broker,
            shutdown_signal,
        )
        .await?;

        Ok(())
    });

    let payload = match &result {
        Ok(()) => json!({ "status": "ok" }),
        Err(error) => json!({ "status": "error", "error": error.to_string() }),
    };
    let _ = append_runtime_event_log(&log_config, "runtime/session_result", &payload);

    result
}
