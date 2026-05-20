use crate::events::SessionConfig;
use crate::mapping::append_runtime_event_log;
use agent_client_protocol::schema::{ClientRequest, ExtRequest};
use agent_client_protocol::{Agent, ConnectionTo};
use anyhow::anyhow;
use serde_json::json;
use std::sync::Arc;

pub(super) async fn send_codebuddy_interruption_resolution(
    config: &SessionConfig,
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    tool_call_id: &str,
    decision: &str,
) -> anyhow::Result<()> {
    let payload = json!({
        "sessionId": session_id,
        "toolCallId": tool_call_id,
        "decision": decision,
    });
    append_runtime_event_log(config, "codebuddy/resolve_interruption", &payload)?;

    let params: Arc<serde_json::value::RawValue> =
        serde_json::value::RawValue::from_string(payload.to_string())?.into();
    let request = ClientRequest::ExtMethodRequest(ExtRequest::new(
        "_codebuddy.ai/resolveInterruption",
        params,
    ));

    connection
        .send_request_to(Agent, request)
        .block_task()
        .await
        .map_err(|err| anyhow!(err.to_string()))?;

    Ok(())
}
