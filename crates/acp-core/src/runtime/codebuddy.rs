use crate::events::SessionConfig;
use crate::mapping::append_runtime_event_log;
use agent_client_protocol::schema::{ClientRequest, ExtRequest};
use agent_client_protocol::{Agent, ConnectionTo};
use anyhow::anyhow;
use serde_json::{Value, json};
use std::sync::Arc;
use workspace_model::PermissionInputResponse;

pub(super) fn send_codebuddy_interruption_resolution(
    config: &SessionConfig,
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    tool_call_id: &str,
    decision: &str,
    guidance: Option<&str>,
    input_response: Option<&PermissionInputResponse>,
) -> anyhow::Result<()> {
    let payload = codebuddy_interruption_resolution_payload(
        session_id,
        tool_call_id,
        decision,
        guidance,
        input_response,
    );
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
    let guidance = guidance
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
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
                        "guidance": guidance.as_deref(),
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
                        "guidance": guidance.as_deref(),
                        "error": error.to_string(),
                    }),
                )?,
            }
            Ok(())
        })
        .map_err(|err| anyhow!(err.to_string()))?;

    Ok(())
}

pub(super) fn send_codebuddy_plan_guidance(
    config: &SessionConfig,
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    guidance: &str,
) -> anyhow::Result<()> {
    let guidance = guidance.trim();
    if guidance.is_empty() {
        return Ok(());
    }

    let payload = codebuddy_plan_guidance_payload(session_id, guidance);
    append_runtime_event_log(config, "codebuddy/inject_plan_guidance", &payload)?;

    let params: Arc<serde_json::value::RawValue> =
        serde_json::value::RawValue::from_string(payload.to_string())?.into();
    let request =
        ClientRequest::ExtMethodRequest(ExtRequest::new("session/inject_history", params));

    let log_config = config.clone();
    let session_id = session_id.to_string();
    connection
        .send_request_to(Agent, request)
        .on_receiving_result(move |result| async move {
            match result {
                Ok(response) => append_runtime_event_log(
                    &log_config,
                    "codebuddy/inject_plan_guidance_response",
                    &json!({
                        "sessionId": session_id,
                        "response": response,
                    }),
                )?,
                Err(error) => append_runtime_event_log(
                    &log_config,
                    "codebuddy/inject_plan_guidance_error",
                    &json!({
                        "sessionId": session_id,
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
    guidance: Option<&str>,
    input_response: Option<&PermissionInputResponse>,
) -> Value {
    let mut payload = json!({
        "sessionId": session_id,
        "toolCallId": tool_call_id,
        "interruptionId": codebuddy_interruption_id_for_tool_call(tool_call_id),
        "decision": decision,
    });
    if let Some(answers) = codebuddy_interruption_answers(guidance, input_response)
        && let Value::Object(fields) = &mut payload
    {
        fields.insert("answers".into(), answers);
    }
    payload
}

fn codebuddy_interruption_answers(
    guidance: Option<&str>,
    input_response: Option<&PermissionInputResponse>,
) -> Option<Value> {
    let mut answers = serde_json::Map::new();
    if let Some(guidance) = guidance.map(str::trim).filter(|value| !value.is_empty()) {
        answers.insert("guidance".into(), Value::String(guidance.to_string()));
    }
    if let Some(input_response) = input_response {
        for (question_id, values) in &input_response.answers {
            let values = values
                .iter()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>();
            match values.as_slice() {
                [] => {}
                [value] => {
                    answers.insert(question_id.clone(), Value::String((*value).to_string()));
                }
                _ => {
                    answers.insert(question_id.clone(), json!(values));
                }
            }
        }
    }

    (!answers.is_empty()).then_some(Value::Object(answers))
}

fn codebuddy_plan_guidance_payload(session_id: &str, guidance: &str) -> Value {
    json!({
        "sessionId": session_id,
        "messages": [
            {
                "role": "user",
                "content": guidance
            }
        ]
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
        let payload = codebuddy_interruption_resolution_payload(
            "session-1",
            "call_00_abc",
            "allow",
            None,
            None,
        );

        assert_eq!(payload["sessionId"], "session-1");
        assert_eq!(payload["toolCallId"], "call_00_abc");
        assert_eq!(payload["interruptionId"], "ir-call_00_abc");
        assert_eq!(payload["decision"], "allow");
    }

    #[test]
    fn interruption_resolution_payload_does_not_double_prefix_interruption_id() {
        let payload = codebuddy_interruption_resolution_payload(
            "session-1",
            "ir-call_00_abc",
            "deny",
            None,
            None,
        );

        assert_eq!(payload["toolCallId"], "ir-call_00_abc");
        assert_eq!(payload["interruptionId"], "ir-call_00_abc");
    }

    #[test]
    fn interruption_resolution_payload_includes_trimmed_guidance() {
        let payload = codebuddy_interruption_resolution_payload(
            "session-1",
            "call_00_abc",
            "deny",
            Some("  补充风险和验证步骤  "),
            None,
        );

        assert_eq!(payload["decision"], "deny");
        assert_eq!(payload["answers"]["guidance"], "补充风险和验证步骤");
        assert!(payload.get("guidance").is_none());
    }

    #[test]
    fn interruption_resolution_payload_includes_user_input_answers() {
        let mut response = PermissionInputResponse::default();
        response
            .answers
            .insert("approach".into(), vec!["Robust".into()]);
        response
            .answers
            .insert("checks".into(), vec!["Unit".into(), "Build".into()]);

        let payload = codebuddy_interruption_resolution_payload(
            "session-1",
            "call_ask",
            "ask_user_question:0:1",
            None,
            Some(&response),
        );

        assert_eq!(payload["decision"], "ask_user_question:0:1");
        assert_eq!(payload["answers"]["approach"], "Robust");
        assert_eq!(payload["answers"]["checks"], json!(["Unit", "Build"]));
    }
    #[test]
    fn plan_guidance_payload_injects_user_history() {
        let payload = codebuddy_plan_guidance_payload("session-1", "补充风险和验证步骤");

        assert_eq!(payload["sessionId"], "session-1");
        assert_eq!(payload["messages"][0]["role"], "user");
        assert_eq!(payload["messages"][0]["content"], "补充风险和验证步骤");
    }
}
