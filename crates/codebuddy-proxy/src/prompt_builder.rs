use std::sync::Arc;
use serde_json::{Value, json};
use crate::openai_types::{OaiChatRequest, OaiMessage, OaiTool};
use crate::pending::PendingQueue;
pub const PROXY_TOOL_SERVER_NAME: &str = "proxy_tools";
pub fn content_to_text(content: &Value) -> String {
    match content {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(t) = part.get("type").and_then(Value::as_str) {
                    if t == "text" {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            out.push_str(text);
                        }
                    } else if t == "image_url" {
                        out.push_str("[image]");
                    }
                }
            }
            out
        }
        other => other.to_string(),
    }
}
pub struct BuiltPrompt {
    pub system_prompt: Option<String>,
    pub model: String,
}
pub fn build_prompt(req: &OaiChatRequest, default_model: &str) -> BuiltPrompt {
    let mut system_parts = Vec::new();
    for m in &req.messages {
        if m.role == "system" {
            system_parts.push(content_to_text(&m.content));
        }
    }
    let system_prompt = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };
    let model = req
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_model)
        .to_string();
    BuiltPrompt { system_prompt, model }
}
pub fn build_full_prompt(messages: &[OaiMessage]) -> String {
    for m in messages.iter().rev() {
        if m.role == "user" {
            let text = content_to_text(&m.content);
            if !text.is_empty() {
                return text;
            }
        }
    }
    "(continue)".to_string()
}
pub fn build_incremental_tail(messages: &[OaiMessage]) -> String {
    let convo: Vec<&OaiMessage> = messages.iter().filter(|m| m.role != "system").collect();
    let last_assistant_idx = convo.iter().rposition(|m| m.role == "assistant");
    match last_assistant_idx {
        None => {
            let texts: Vec<String> = convo
                .iter()
                .filter(|m| m.role == "user")
                .map(|m| content_to_text(&m.content))
                .filter(|t| !t.is_empty())
                .collect();
            if texts.is_empty() {
                "(continue)".to_string()
            } else {
                texts.join("\n\n")
            }
        }
        Some(idx) => {
            let tail: Vec<&&OaiMessage> = convo[idx + 1..].iter().collect();
            if tail.is_empty() {
                return "(continue)".to_string();
            }
            let tool_results: Vec<&&&OaiMessage> = tail.iter().filter(|m| m.role == "tool").collect();
            let user_texts: Vec<String> = tail
                .iter()
                .filter(|m| m.role == "user")
                .map(|m| content_to_text(&m.content))
                .filter(|t| !t.is_empty())
                .collect();
            if tool_results.is_empty() {
                if user_texts.is_empty() {
                    "(continue)".to_string()
                } else {
                    user_texts.join("\n\n")
                }
            } else {
                let mut parts: Vec<String> = tool_results
                    .iter()
                    .map(|m| {
                        let id = m.tool_call_id.clone().unwrap_or_default();
                        let text = content_to_text(&m.content);
                        format!("[tool_result call_id=\"{id}\"]\n{text}")
                    })
                    .collect();
                if !user_texts.is_empty() {
                    parts.push(user_texts.join("\n\n"));
                }
                parts.join("\n\n")
            }
        }
    }
}
pub fn build_user_content(messages: &[OaiMessage], is_new: bool) -> String {
    if is_new {
        build_full_prompt(messages)
    } else {
        build_incremental_tail(messages)
    }
}
pub fn demangle_tool_name(name: &str) -> String {
    let prefix = format!("mcp__{PROXY_TOOL_SERVER_NAME}__");
    if let Some(stripped) = name.strip_prefix(&prefix) {
        stripped.to_string()
    } else {
        name.to_string()
    }
}
pub fn build_proxy_tools(
    tools: &Option<Vec<OaiTool>>,
    pending: &Arc<PendingQueue>,
) -> Vec<codebuddy_sdk::mcp::server::SdkMcpTool> {
    let mut out = Vec::new();
    if let Some(tools) = tools {
        for t in tools {
            let name = t.function.name.clone();
            if name.is_empty() {
                continue;
            }
            let description = t.function.description.clone().unwrap_or_default();
            let input_schema = t.function.parameters.clone().unwrap_or_else(|| json!({}));
            let pending = pending.clone();
            // Capture+interrupt handler: record the parsed arguments (the
            // assistant `tool_use` block carries id+name but NOT reliable
            // input), then never resolve. The CLI stalls at the `tool_use`
            // until `session.interrupt()` cancels the wait, leaving no
            // `tool_result` in history so the real result can be fed back as
            // plain text next turn. Mirrors the reference TS
            // `buildCapturingHandler` + `new Promise(() => {})`.
            let handler_name = name.clone();
            let handler: codebuddy_sdk::mcp::server::SdkMcpHandler =
                std::sync::Arc::new(move |input: Value| {
                    let name = handler_name.clone();
                    let pending = pending.clone();
                    Box::pin(async move {
                        let args = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                        pending.push(name, args).await;
                        std::future::pending::<
                            Result<codebuddy_sdk::mcp::server::SdkMcpToolResult, codebuddy_sdk::SdkError>,
                        >()
                        .await
                    })
                });
            out.push(codebuddy_sdk::mcp::server::SdkMcpTool {
                name,
                description,
                input_schema,
                handler,
            });
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn user(text: &str) -> OaiMessage {
        OaiMessage { role: "user".to_string(), content: Value::String(text.to_string()), tool_call_id: None, tool_calls: None, name: None }
    }
    fn assistant(text: &str) -> OaiMessage {
        OaiMessage { role: "assistant".to_string(), content: Value::String(text.to_string()), tool_call_id: None, tool_calls: None, name: None }
    }
    fn tool_result(id: &str, text: &str) -> OaiMessage {
        OaiMessage { role: "tool".to_string(), content: Value::String(text.to_string()), tool_call_id: Some(id.to_string()), tool_calls: None, name: None }
    }
    #[test]
    fn cold_session_sends_last_user_message() {
        let msgs = vec![user("first"), assistant("answer"), user("latest")];
        assert_eq!(build_full_prompt(&msgs), "latest");
    }
    #[test]
    fn warm_session_tool_result_is_text_with_content() {
        let msgs = vec![assistant("check"), tool_result("call_1", "sunny, 20C"), user("thanks")];
        let out = build_incremental_tail(&msgs);
        assert_eq!(out, "[tool_result call_id=\"call_1\"]\nsunny, 20C\n\nthanks");
    }
    #[test]
    fn warm_session_tool_result_no_trailing_text() {
        let msgs = vec![assistant("check"), tool_result("call_1", "result")];
        assert_eq!(build_incremental_tail(&msgs), "[tool_result call_id=\"call_1\"]\nresult");
    }
    #[test]
    fn warm_session_no_tool_results_is_plain_text() {
        let msgs = vec![assistant("answer"), user("follow up")];
        assert_eq!(build_incremental_tail(&msgs), "follow up");
    }
    #[test]
    fn system_extracted_into_system_prompt() {
        let req = OaiChatRequest {
            model: Some("m".to_string()),
            messages: vec![
                OaiMessage { role: "system".to_string(), content: Value::String("rule one".to_string()), tool_call_id: None, tool_calls: None, name: None },
                OaiMessage { role: "system".to_string(), content: Value::String("rule two".to_string()), tool_call_id: None, tool_calls: None, name: None },
                user("hi"),
            ],
            stream: None,
            tools: None,
            max_tokens: None,
            temperature: None,
            extra: json!({}),
        };
        let built = build_prompt(&req, "fallback");
        assert_eq!(built.system_prompt.as_deref(), Some("rule one\n\nrule two"));
        assert_eq!(built.model, "m");
    }
    #[test]
    fn demangle_strips_namespace() {
        assert_eq!(demangle_tool_name("mcp__proxy_tools__shell_command"), "shell_command");
        assert_eq!(demangle_tool_name("shell_command"), "shell_command");
    }
}
