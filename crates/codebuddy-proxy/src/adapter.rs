use std::sync::Arc;
use std::time::Duration;
use serde_json::{Value, json};
use codebuddy_sdk::{Session, SessionOptions, SdkMcpServerEntry};
use codebuddy_sdk::mcp::server::{SdkMcpTool, SdkMcpToolResult};
use crate::logging::append_codebuddy_proxy_log;
use crate::openai_types::{OaiChatRequest, OaiChatResponse, OaiChoice, OaiChoiceMessage, OaiUsage};
use crate::pending::PendingQueue;
use crate::prompt_builder::{PROXY_TOOL_SERVER_NAME, build_prompt, build_user_content, demangle_tool_name};
pub struct AdapterOptions {
    pub default_model: String,
    pub cwd: Option<std::path::PathBuf>,
    pub max_turns: Option<u32>,
    /// Resolved CodeBuddy CLI binary path, plumbed through to
    /// `SessionOptions::codebuddy_code_path` so the SDK does not have to rely
    /// on its exe-relative `search_dirs()` (which never contains the
    /// user-installed `codebuddy` CLI inside the desktop app).
    pub cli_path: Option<std::path::PathBuf>,
    /// Environment forwarded to the CLI subprocess (`CODEBUDDY_API_KEY`,
    /// `CODEBUDDY_INTERNET_ENVIRONMENT`, `PATH`, …). Merged into
    /// `SessionOptions::env` so the SDK's `build_child_env` applies it to
    /// the spawned CLI. Without `CODEBUDDY_API_KEY` the headless CLI
    /// cannot authenticate and `initialize` hangs until the 60s control
    /// timeout — see `ProxyConfig::cli_env`.
    pub cli_env: std::collections::BTreeMap<String, String>,
}
pub fn build_session_options(
    req: &OaiChatRequest,
    adapter_opts: &AdapterOptions,
    session_id: &str,
    pending: &Arc<PendingQueue>,
) -> SessionOptions {
    let built = build_prompt(req, &adapter_opts.default_model);
    let mut opts = SessionOptions::default();
    opts.model = Some(built.model);
    opts.session_id = Some(session_id.to_string());
    opts.permission_mode = Some("bypassPermissions".to_string());
    opts.system_prompt = built.system_prompt;
    opts.max_turns = adapter_opts.max_turns;
    opts.cwd = adapter_opts.cwd.clone();
    opts.codebuddy_code_path = adapter_opts.cli_path.clone();
    // Forward backend auth + internet environment + PATH to the CLI
    // subprocess. The SDK's `build_child_env` applies `opts.env` on top of
    // the inherited process env.
    opts.env = adapter_opts.cli_env.clone();
    let proxy_tools = crate::prompt_builder::build_proxy_tools(&req.tools, pending);
    if !proxy_tools.is_empty() {
        opts.mcp_servers.push(SdkMcpServerEntry {
            name: PROXY_TOOL_SERVER_NAME.to_string(),
            tools: proxy_tools,
        });
    }
    opts
}
pub async fn run_non_streaming(
    session: &Session,
    req: &OaiChatRequest,
    adapter_opts: &AdapterOptions,
    pending: &PendingQueue,
    is_new: bool,
) -> anyhow::Result<OaiChatResponse> {
    let built = build_prompt(req, &adapter_opts.default_model);
    let content = build_user_content(&req.messages, is_new);
    append_codebuddy_proxy_log(&format!(
        "non_stream model={} is_new={} content_len={} cli_path={}",
        built.model,
        is_new,
        content.len(),
        adapter_opts
            .cli_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<sdk-resolve>".to_string()),
    ));
    // Clear captures + signals left from a prior turn on this pooled session.
    pending.clear().await;
    session.send(json!(content)).await?;
    let mut assistant_text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut finish_reason = "stop".to_string();
    let mut prompt_tokens = 0u64;
    let mut completion_tokens = 0u64;
    let mut resolved_model = built.model.clone();
    while let Some(msg) = session.stream().await? {
        let ty = msg.msg_type();
        if ty == "assistant" {
            if let Some(model) = msg.model() {
                resolved_model = model.to_string();
            }
            // Collect tool_use blocks (id+name) and text. The full arguments
            // are NOT read from the block (unreliable in stream-json); they
            // arrive via the MCP handler capture, awaited below.
            let mut tool_uses: Vec<(String, String, Value)> = Vec::new();
            if let Some(blocks) = msg.content_blocks() {
                for block in blocks {
                    if let Some(bt) = block.get("type").and_then(Value::as_str) {
                        match bt {
                            "text" => {
                                if let Some(t) = block.get("text").and_then(Value::as_str) {
                                    assistant_text.push_str(t);
                                }
                            }
                            "tool_use" => {
                                let id = block.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                                let name = block.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                                let input_fallback = block.get("input").cloned().unwrap_or(json!({}));
                                tool_uses.push((id, name, input_fallback));
                            }
                            _ => {}
                        }
                    }
                }
            }
            if let Some(reason) = msg.stop_reason() {
                if reason == "max_tokens" {
                    finish_reason = "length".to_string();
                }
            }
            if !tool_uses.is_empty() {
                // Wait for the MCP handlers to capture the real arguments.
                let n = tool_uses.len();
                pending.wait_for_captures(n, Duration::from_secs(5)).await;
                for (id, name, input_fallback) in &tool_uses {
                    let args = pending
                        .arguments_for(name)
                        .await
                        .unwrap_or_else(|| serde_json::to_string(input_fallback).unwrap_or_else(|_| "{}".to_string()));
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": demangle_tool_name(name),
                            "arguments": args,
                        },
                    }));
                }
                append_codebuddy_proxy_log(&format!(
                    "non_stream tool_use detected={} interrupting",
                    tool_calls.len(),
                ));
                let _ = session.interrupt().await;
                drain_turn_tail(session).await;
                finish_reason = "tool_calls".to_string();
                break;
            }
        } else if ty == "result" {
            if let Some(usage) = msg.result_usage() {
                if let Some(v) = usage.get("input_tokens").and_then(Value::as_u64) {
                    prompt_tokens = v;
                }
                if let Some(v) = usage.get("output_tokens").and_then(Value::as_u64) {
                    completion_tokens = v;
                }
            }
            append_codebuddy_proxy_log(&format!(
                "non_stream done model={resolved_model} finish={finish_reason} prompt_tokens={prompt_tokens} completion_tokens={completion_tokens} tool_calls={}",
                tool_calls.len(),
            ));
            break;
        }
    }
    let content_val = if assistant_text.is_empty() {
        Value::Null
    } else {
        Value::String(assistant_text)
    };
    let message = OaiChoiceMessage {
        role: "assistant".to_string(),
        content: Some(content_val),
        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
    };
    Ok(OaiChatResponse {
        id: format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
        object: "chat.completion".to_string(),
        created: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        model: resolved_model,
        choices: vec![OaiChoice {
            index: 0,
            message,
            finish_reason,
            logprobs: None,
        }],
        usage: OaiUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        },
    })
}
pub async fn run_streaming(
    session: &Session,
    req: &OaiChatRequest,
    adapter_opts: &AdapterOptions,
    pending: &PendingQueue,
    is_new: bool,
) -> anyhow::Result<Vec<String>> {
    let built = build_prompt(req, &adapter_opts.default_model);
    let content = build_user_content(&req.messages, is_new);
    append_codebuddy_proxy_log(&format!(
        "stream model={} is_new={} content_len={} cli_path={}",
        built.model,
        is_new,
        content.len(),
        adapter_opts
            .cli_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<sdk-resolve>".to_string()),
    ));
    pending.clear().await;
    session.send(json!(content)).await?;
    let completion_id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut frames = Vec::new();
    let send = |obj: &Value| -> String {
        format!("data: {}\n\n", serde_json::to_string(obj).unwrap_or_default())
    };
    frames.push(send(&json!({
        "id": completion_id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": built.model,
        "choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": null}],
    })));
    let mut finish_reason = "stop".to_string();
    let mut resolved_model = built.model.clone();
    let mut tool_idx = 0u32;
    while let Some(msg) = session.stream().await? {
        let ty = msg.msg_type();
        if ty == "stream_event" {
            if let Some(ev) = msg.event() {
                let etype = ev.get("type").and_then(Value::as_str).unwrap_or("");
                if etype == "message_start" {
                    if let Some(m) = ev.get("message").and_then(|m| m.get("model")).and_then(Value::as_str) {
                        resolved_model = m.to_string();
                    }
                } else if etype == "content_block_delta" {
                    if let Some(d) = ev.get("delta") {
                        if d.get("type").and_then(Value::as_str) == Some("text_delta") {
                            if let Some(text) = d.get("text").and_then(Value::as_str) {
                                frames.push(send(&json!({
                                    "id": completion_id,
                                    "object": "chat.completion.chunk",
                                    "created": created,
                                    "model": resolved_model,
                                    "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}],
                                })));
                            }
                        }
                    }
                }
            }
            continue;
        }
        if ty == "assistant" {
            if let Some(model) = msg.model() {
                resolved_model = model.to_string();
            }
            // Collect tool_use blocks (id+name). Arguments come from the MCP
            // handler capture (the block's input is unreliable in stream-json).
            let mut tool_uses: Vec<(String, String, Value)> = Vec::new();
            if let Some(blocks) = msg.content_blocks() {
                for block in blocks {
                    if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                        let id = block.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                        let input_fallback = block.get("input").cloned().unwrap_or(json!({}));
                        tool_uses.push((id, name, input_fallback));
                    }
                }
            }
            if !tool_uses.is_empty() {
                finish_reason = "tool_calls".to_string();
                // Wait for the MCP handlers to capture the real arguments.
                let n = tool_uses.len();
                pending.wait_for_captures(n, Duration::from_secs(5)).await;
                for (id, name, input_fallback) in &tool_uses {
                    let args = pending
                        .arguments_for(name)
                        .await
                        .unwrap_or_else(|| serde_json::to_string(input_fallback).unwrap_or_else(|_| "{}".to_string()));
                    frames.push(send(&json!({
                        "id": completion_id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": resolved_model,
                        "choices": [{
                            "index": 0,
                            "delta": {
                                "tool_calls": [{
                                    "index": tool_idx,
                                    "id": id,
                                    "type": "function",
                                    "function": {
                                        "name": demangle_tool_name(name),
                                        "arguments": args,
                                    },
                                }],
                            },
                            "finish_reason": null,
                        }],
                    })));
                    tool_idx += 1;
                }
                append_codebuddy_proxy_log(&format!(
                    "stream tool_use interrupting tool_calls={tool_idx}"
                ));
                let _ = session.interrupt().await;
                drain_turn_tail(session).await;
                break;
            }
        } else if ty == "result" {
            break;
        }
    }
    append_codebuddy_proxy_log(&format!(
        "stream done model={resolved_model} finish={finish_reason} tool_calls={tool_idx}"
    ));
    frames.push(send(&json!({
        "id": completion_id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": resolved_model,
        "choices": [{"index": 0, "delta": {}, "finish_reason": finish_reason}],
    })));
    frames.push("data: [DONE]\n\n".to_string());
    Ok(frames)
}

/// After `interrupt()` on a tool_use, drain the session's message channel
/// through the turn-end `result` so the pooled session is clean for the next
/// request. The CLI emits a `result` when the interrupted turn ends; if it is
/// left in the channel, the next `stream()` would receive it and end that turn
/// prematurely (finish=stop with no output). Bounded by a timeout in case the
/// CLI doesn't emit a result.
async fn drain_turn_tail(session: &Session) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let next = match tokio::time::timeout_at(deadline, session.stream()).await {
            Ok(Ok(Some(msg))) => msg,
            _ => return,
        };
        if next.msg_type() == "result" {
            return;
        }
    }
}
