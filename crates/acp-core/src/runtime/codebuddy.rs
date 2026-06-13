use crate::events::SessionConfig;
use crate::mapping::append_runtime_event_log;
use agent_client_protocol::schema::{ClientRequest, ExtRequest};
use agent_client_protocol::{Agent, ConnectionTo};
use anyhow::anyhow;
use serde_json::{Value, json};
use std::sync::Arc;

pub(super) fn send_codebuddy_interruption_resolution(
    config: &SessionConfig,
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    tool_call_id: &str,
    decision: &str,
) -> anyhow::Result<()> {
    let payload = codebuddy_interruption_resolution_payload(session_id, tool_call_id, decision);
    append_runtime_event_log(config, "codebuddy/resolve_interruption", &payload)?;

    let params: Arc<serde_json::value::RawValue> =
        serde_json::value::RawValue::from_string(payload.to_string())?.into();
    let request = ClientRequest::ExtMethodRequest(ExtRequest::new(
        "_codebuddy.ai/resolveInterruption",
        params,
    ));

    let log_config = config.clone();
    let session_id = session_id.to_string();
    let tool_call_id = tool_call_id.to_string();
    let decision = decision.to_string();
    connection
        .send_request_to(Agent, request)
        .on_receiving_result(move |result| async move {
            match result {
                Ok(response) => append_runtime_event_log(
                    &log_config,
                    "codebuddy/resolve_interruption_response",
                    &json!({
                        "sessionId": session_id,
                        "toolCallId": tool_call_id,
                        "decision": decision,
                        "response": response,
                    }),
                )?,
                Err(error) => append_runtime_event_log(
                    &log_config,
                    "codebuddy/resolve_interruption_error",
                    &json!({
                        "sessionId": session_id,
                        "toolCallId": tool_call_id,
                        "decision": decision,
                        "error": error.to_string(),
                    }),
                )?,
            }
            Ok(())
        })
        .map_err(|err| anyhow!(err.to_string()))?;

    Ok(())
}

fn codebuddy_interruption_resolution_payload(
    session_id: &str,
    tool_call_id: &str,
    decision: &str,
) -> Value {
    json!({
        "sessionId": session_id,
        "toolCallId": tool_call_id,
        "interruptionId": codebuddy_interruption_id_for_tool_call(tool_call_id),
        "decision": decision,
    })
}

fn codebuddy_interruption_id_for_tool_call(tool_call_id: &str) -> String {
    if tool_call_id.starts_with("ir-") {
        tool_call_id.to_string()
    } else {
        format!("ir-{tool_call_id}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interruption_resolution_payload_includes_codebuddy_interruption_id() {
        let payload =
            codebuddy_interruption_resolution_payload("session-1", "call_00_abc", "allow");

        assert_eq!(payload["sessionId"], "session-1");
        assert_eq!(payload["toolCallId"], "call_00_abc");
        assert_eq!(payload["interruptionId"], "ir-call_00_abc");
        assert_eq!(payload["decision"], "allow");
    }

    #[test]
    fn interruption_resolution_payload_does_not_double_prefix_interruption_id() {
        let payload =
            codebuddy_interruption_resolution_payload("session-1", "ir-call_00_abc", "deny");

        assert_eq!(payload["toolCallId"], "ir-call_00_abc");
        assert_eq!(payload["interruptionId"], "ir-call_00_abc");
    }
}
