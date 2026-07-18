use std::sync::Arc;
use std::time::Duration;
use serde_json::{Value, json};
use codebuddy_sdk::{Session, SessionOptions, SdkMcpServerEntry};
use codebuddy_sdk::mcp::server::{SdkMcpTool, SdkMcpToolResult};
use tokio::sync::{mpsc, Mutex};
use crate::logging::append_codebuddy_proxy_log;
use crate::openai_types::{OaiChatRequest, OaiChatResponse, OaiChoice, OaiChoiceMessage, OaiUsage};
use crate::pending::PendingQueue;
use crate::prompt_builder::{PROXY_TOOL_SERVER_NAME, build_prompt, build_user_content, demangle_tool_name};
use crate::usage::{CliUsage, extract_cli_usage, resolve_reported_usage};
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
    // Disable ALL built-in CLI tools (Bash/Edit/Read/…). Client-declared tools
    // are registered solely as the in-process MCP server below. Without this
    // the CLI may execute tools itself instead of hitting our placeholder MCP
    // handlers — the TS/Python proxies pass `tools: []` for the same reason.
    opts.tools = Some(Vec::new());
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
    ack_rx: &Mutex<mpsc::UnboundedReceiver<()>>,
    is_new: bool,
    last_cli_usage: &Mutex<Option<CliUsage>>,
) -> anyhow::Result<OaiChatResponse> {
    let built = build_prompt(req, &adapter_opts.default_model);
    let content = build_user_content(&req.messages, is_new);
    let content_json = serde_json::to_string(&content).unwrap_or_default();
    if content_json.len() > 10_000 {
        let head: String = content_json.chars().take(400).collect();
        let tail: String = content_json.chars().rev().take(400).collect();
        append_codebuddy_proxy_log(&format!(
            "non_stream tail_dump len={} head={head:?} tail_rev={tail:?}",
            content_json.len(),
        ));
    }
    append_codebuddy_proxy_log(&format!(
        "non_stream model={} is_new={} content_len={} cli_path={}",
        built.model,
        is_new,
        content_json.len(),
        adapter_opts
            .cli_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<sdk-resolve>".to_string()),
    ));
    // Clear captures + signals left from a prior turn on this pooled session.
    // Also drain stale acks (see `drain_pending_acks`): a prior turn whose
    // `await_tool_acks` bailed on its safety deadline can leave late acks
    // buffered in the per-session channel; without draining the next turn
    // would count them as its own and interrupt before its placeholders land.
    pending.clear().await;
    drain_pending_acks(ack_rx).await;
    session.send(content).await?;
    let mut assistant_text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut finish_reason = "stop".to_string();
    let mut cumulative_usage: Option<CliUsage> = None;
    let mut last_model_call: Option<CliUsage> = None;
    let mut resolved_model = built.model.clone();
    while let Some(msg) = session.stream().await? {
        let ty = msg.msg_type();
        if ty == "assistant" {
            if let Some(model) = msg.model() {
                resolved_model = model.to_string();
            }
            // assistant.message.usage is per-model-call (context size for this
            // request), not the session cumulative. Prefer it for OpenAI
            // prompt_tokens / live context occupancy.
            if let Some(u) = extract_cli_usage(msg.usage()) {
                last_model_call = Some(u);
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
                let n = tool_uses.len();
                let mut captures = collect_captures(pending, n).await;
                let released = captures.len();
                for (id, name, input_fallback) in &tool_uses {
                    let args = take_capture_for(&mut captures, name)
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
                    "non_stream tool_use detected={} captured={} interrupting",
                    tool_uses.len(),
                    tool_calls.len(),
                ));
                // Wait until every placeholder result has been written to
                // CLI stdin (one ack per RELEASED capture) BEFORE interrupting,
                // so the CLI records the placeholder text instead of filling
                // `undefined` when the interrupt wins the stdin-write race.
                // Use the released-capture count, not tool_uses.len(): the CLI
                // may dispatch fewer tools than declared, and only released
                // captures produce an ack.
                let got = await_tool_acks(ack_rx, released).await;
                if got == released && released > 0 {
                    settle_cli_before_interrupt().await;
                }
                let _ = session.interrupt().await;
                if let Some(u) = drain_turn_tail(session, &mut last_model_call).await {
                    cumulative_usage = Some(u);
                }
                finish_reason = "tool_calls".to_string();
                break;
            }
        } else if ty == "result" {
            let raw_usage = msg.result_usage();
            append_codebuddy_proxy_log(&format!(
                "result raw usage={}",
                raw_usage
                    .map(|v| serde_json::to_string(v).unwrap_or_default())
                    .unwrap_or_else(|| "null".to_string()),
            ));
            if let Some(u) = extract_cli_usage(raw_usage) {
                cumulative_usage = Some(u);
            }
            break;
        }
    }
    let turn_usage = finalize_turn_usage(
        last_cli_usage,
        cumulative_usage,
        last_model_call,
        "non_stream",
        &resolved_model,
        &finish_reason,
        tool_calls.len() as u32,
    )
    .await;
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
        // Prefer last model-call usage for prompt_tokens / context occupancy.
        usage: turn_usage,
    })
}
pub async fn run_streaming(
    session: &Session,
    req: &OaiChatRequest,
    adapter_opts: &AdapterOptions,
    pending: &PendingQueue,
    ack_rx: &Mutex<mpsc::UnboundedReceiver<()>>,
    is_new: bool,
    last_cli_usage: &Mutex<Option<CliUsage>>,
) -> anyhow::Result<Vec<String>> {
    let built = build_prompt(req, &adapter_opts.default_model);
    let content = build_user_content(&req.messages, is_new);
    let content_json = serde_json::to_string(&content).unwrap_or_default();
    if content_json.len() > 10_000 {
        let head: String = content_json.chars().take(400).collect();
        let tail: String = content_json.chars().rev().take(400).collect();
        append_codebuddy_proxy_log(&format!(
            "stream tail_dump len={} head={head:?} tail_rev={tail:?}",
            content_json.len(),
        ));
    }
    append_codebuddy_proxy_log(&format!(
        "stream model={} is_new={} content_len={} cli_path={}",
        built.model,
        is_new,
        content_json.len(),
        adapter_opts
            .cli_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<sdk-resolve>".to_string()),
    ));
    pending.clear().await;
    drain_pending_acks(ack_rx).await;
    session.send(content).await?;
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
    let mut cumulative_usage: Option<CliUsage> = None;
    let mut last_model_call: Option<CliUsage> = None;
    while let Some(msg) = session.stream().await? {
        let ty = msg.msg_type();
        if ty == "stream_event" {
            if let Some(ev) = msg.event() {
                let etype = ev.get("type").and_then(Value::as_str).unwrap_or("");
                if etype == "message_start" {
                    if let Some(m) = ev.get("message").and_then(|m| m.get("model")).and_then(Value::as_str) {
                        resolved_model = m.to_string();
                    }
                    if let Some(u) =
                        extract_cli_usage(ev.get("message").and_then(|m| m.get("usage")))
                    {
                        last_model_call = Some(u);
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
                } else if etype == "message_delta" {
                    // message_delta.usage is often the running total for this
                    // single model call (input fixed, output growing). Keep the
                    // latest as last_model_call for context reporting.
                    if let Some(u) = extract_cli_usage(ev.get("usage")) {
                        last_model_call = Some(u);
                    }
                }
            }
            continue;
        }
        if ty == "assistant" {
            if let Some(model) = msg.model() {
                resolved_model = model.to_string();
            }
            if let Some(u) = extract_cli_usage(msg.usage()) {
                last_model_call = Some(u);
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
                let n = tool_uses.len();
                let mut captures = collect_captures(pending, n).await;
                let released = captures.len();
                for (id, name, input_fallback) in &tool_uses {
                    let args = take_capture_for(&mut captures, name)
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
                    "stream tool_use detected={} captured={} interrupting",
                    tool_uses.len(),
                    tool_idx,
                ));
                // Wait until every placeholder result has been written to
                // CLI stdin (one ack per RELEASED capture) BEFORE interrupting,
                // so the CLI records the placeholder text instead of filling
                // `undefined` when the interrupt wins the stdin-write race.
                // Use the released-capture count, not tool_uses.len(): the CLI
                // may dispatch fewer tools than declared, and only released
                // captures produce an ack.
                let got = await_tool_acks(ack_rx, released).await;
                if got == released && released > 0 {
                    settle_cli_before_interrupt().await;
                }
                let _ = session.interrupt().await;
                if let Some(u) = drain_turn_tail(session, &mut last_model_call).await {
                    cumulative_usage = Some(u);
                }
                break;
            }
        } else if ty == "result" {
            let raw_usage = msg.result_usage();
            append_codebuddy_proxy_log(&format!(
                "result raw usage={}",
                raw_usage
                    .map(|v| serde_json::to_string(v).unwrap_or_default())
                    .unwrap_or_else(|| "null".to_string()),
            ));
            if let Some(u) = extract_cli_usage(raw_usage) {
                cumulative_usage = Some(u);
            }
            break;
        }
    }
    let turn_usage = finalize_turn_usage(
        last_cli_usage,
        cumulative_usage,
        last_model_call,
        "stream",
        &resolved_model,
        &finish_reason,
        tool_idx,
    )
    .await;
    frames.push(send(&json!({
        "id": completion_id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": resolved_model,
        "choices": [{"index": 0, "delta": {}, "finish_reason": finish_reason}],
    })));
    // Terminal usage chunk carries the per-turn delta (not the CLI session
    // cumulative). `codex_api_proxy` / OpenAI clients pick this up as the
    // request's token accounting.
    frames.push(send(&json!({
        "id": completion_id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": resolved_model,
        "choices": [],
        "usage": turn_usage,
    })));
    frames.push("data: [DONE]\n\n".to_string());
    Ok(frames)
}

/// Placeholder every tool handler returns instead of staying pending: it lets
/// the CLI record a `tool_result` (so a sequential CLI dispatches all N tools
/// in the turn, and every spawned handler task resolves — no leaked tasks)
/// while pointing the model to the real result, which codex reflows in the
/// next `user_query`. After the Nth capture lands, `interrupt()` cancels the
/// turn before the CLI returns to the model on these placeholders.
const PLACEHOLDER_TOOL_RESULT: &str = "见下一个<user_query> 中的工具结果";

/// Collect up to `n` captures for the current turn, releasing each with the
/// placeholder so every handler resolves (no leaked tasks) and a sequential
/// CLI proceeds to dispatch all N tools. After the n-th capture lands, the
/// caller interrupts, cancelling the turn before the CLI returns to the model
/// on the placeholders; codex reflows the real results in the next
/// `user_query`. Returns the captured `(name, arguments)` pairs in arrival
/// order for consume-by-name pairing with the assistant `tool_use` blocks. If
/// the CLI dispatches fewer than `n` tools, `next_capture` times out and the
/// missing blocks fall back to their `input`.
async fn collect_captures(pending: &PendingQueue, n: usize) -> Vec<(String, String)> {
    let mut captures: Vec<(String, String)> = Vec::with_capacity(n);
    for _ in 0..n {
        match pending.next_capture(Duration::from_secs(5)).await {
            Some((name, args, sender)) => {
                let _ = sender.send(SdkMcpToolResult::text(PLACEHOLDER_TOOL_RESULT));
                captures.push((name, args));
            }
            None => break,
        }
    }
    captures
}

/// Wait for `n` acks from the SDK — one per `tools/call` whose placeholder
/// result has been written to CLI stdin — before the caller interrupts.
///
/// The SDK sends a unit on the per-session ack channel **after**
/// `transport.write_json(&reply)` returns for each `tools/call` handler
/// result. Because stdin writes are mutex-serialized, awaiting these acks
/// guarantees the placeholder `control_response` is ahead of the subsequent
/// `interrupt()` write in the stdin stream, so the CLI reads the placeholder
/// first and records its text — not `undefined` (which is what the CLI fills
/// when the interrupt arrives while it is still waiting for the tool result).
///
/// Drain any acks left in the per-session channel from a prior turn.
///
/// A prior turn's `await_tool_acks` can leave late-arriving acks buffered
/// when it bails on the safety deadline (a handler write that overran the
/// budget). Those stale acks belong to the *previous* turn's placeholders;
/// if the next turn's `await_tool_acks` consumed them it would count them
/// toward this turn's releases and return before this turn's placeholders
/// land — so the CLI records `undefined` instead of the placeholder, and the
/// real (late) ack then poisons the turn after that. Draining at turn start
/// (alongside `pending.clear()`) keeps each turn's ack accounting honest.
async fn drain_pending_acks(ack_rx: &Mutex<mpsc::UnboundedReceiver<()>>) {
    let mut rx = ack_rx.lock().await;
    while rx.try_recv().is_ok() {}
}

/// `n` is the count of *released* captures (== placeholders written), not
/// `tool_uses.len()`: every released capture woke a handler that WILL write
/// and ack — the only escape is session teardown, which drops the ack sender
/// so `recv()` returns `None`. We therefore wait for exactly those `n`,
/// bailing only on channel-close (teardown) or a generous shared safety
/// deadline (a wedged write), never on a tight per-ack timeout that would
/// let a briefly-delayed handler write turn into a `undefined` tool_result.
/// `drain_turn_tail` still cleans up the turn after `interrupt()`.
///
/// Returns the number of acks actually received (`n` on success; fewer when
/// the shared deadline elapsed or the ack channel closed mid-wait). Callers
/// use this to decide whether to settle before interrupting: a full success
/// means every placeholder was written, so a short settle lets the CLI read +
/// commit them before the interrupt cancels the turn.
async fn await_tool_acks(ack_rx: &Mutex<mpsc::UnboundedReceiver<()>>, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let start = tokio::time::Instant::now();
    let mut rx = ack_rx.lock().await;
    // One shared deadline for all n acks: handler writes are mutex-serialized
    // pipe writes, so n acks normally land in milliseconds. The deadline is a
    // backstop against a wedged handler/write — not the common path. `recv()`
    // returning `None` means the ack sender dropped (session teardown): bail.
    let deadline = start + Duration::from_secs(10);
    let mut got = 0usize;
    for _ in 0..n {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(())) => got += 1,
            // channel closed (session torn down) or deadline elapsed: bail.
            _ => break,
        }
    }
    // Diagnose the `undefined` tool_result race: log every tool turn's ack
    // timing so a bail (deadline/channel-close → placeholder never landed →
    // `undefined`) is visible in codebuddy-proxy.log alongside the existing
    // "tool_use detected/captured" lines. No-op in tests.
    append_codebuddy_proxy_log(&format!(
        "await_tool_acks wanted={n} got={got} elapsed_ms={} bailed={}",
        start.elapsed().as_millis(),
        got < n,
    ));
    got
}

/// How long to wait between "every placeholder was written to the CLI stdin"
/// (the ack barrier) and writing the `interrupt()` control_request. The ack
/// only proves the placeholder *bytes* reached the OS pipe buffer; the CLI is
/// a separate process that still has to read them off stdin and commit each
/// `control_response` to its tool-call history. If the interrupt bytes land
/// in the pipe immediately after, a CLI whose stdin reader runs ahead of its
/// tool-result committer can process the cancel first and record `undefined`
/// for the tool. A short settle lets the CLI's event loop read+commit the
/// placeholder(s) ahead of the interrupt. Tunable — bump if `undefined`
/// tool_results persist despite `await_tool_acks got==n bailed=false`.
const SETTLE_BEFORE_INTERRUPT_MS: u64 = 200;

/// Settle pause called after a *full* ack success (`got == released`) and
/// before `interrupt()`. Not called on a bail: if some placeholder never
/// landed, the interrupt will record `undefined` for it regardless, and
/// waiting longer just stalls a turn whose write is already wedged.
async fn settle_cli_before_interrupt() {
    tokio::time::sleep(Duration::from_millis(SETTLE_BEFORE_INTERRUPT_MS)).await;
}

/// Consume the first unconsumed same-named capture's arguments. Returns `None`
/// when no capture matches (the CLI never dispatched that tool — the adapter
/// then falls back to the block `input`). The matched entry is removed so
/// repeated same-name calls pair in arrival order, and arrival order need not
/// match the assistant message's emission order.
fn take_capture_for(captures: &mut Vec<(String, String)>, name: &str) -> Option<String> {
    let idx = captures.iter().position(|(cn, _)| cn == name)?;
    Some(captures.remove(idx).1)
}

/// After `interrupt()` on a tool_use, drain the session's message channel
/// through the turn-end `result` so the pooled session is clean for the next
/// request. The CLI emits a `result` when the interrupted turn ends; if it is
/// left in the channel, the next `stream()` would receive it and end that turn
/// prematurely (finish=stop with no output). Bounded by a timeout in case the
/// CLI doesn't emit a result.
///
/// While draining, any late-arriving `assistant` or `stream_event` messages
/// are consumed; their per-call usage (if present) is captured into
/// `last_model_call` so it is never lost even when the `result` message never
/// arrives (timeout) or carries no usage. Without this, the turn would report
/// zero usage when `last_model_call` was never set in the main loop AND
/// `drain_turn_tail` times out — the under-counting gap for agentic-first
/// models like glm-5.2-ioa that emit tool_use before reporting usage.
async fn drain_turn_tail(
    session: &Session,
    last_model_call: &mut Option<CliUsage>,
) -> Option<CliUsage> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let next = match tokio::time::timeout_at(deadline, session.stream()).await {
            Ok(Ok(Some(msg))) => msg,
            _ => return None,
        };
        match next.msg_type() {
            "result" => {
                let raw_usage = next.result_usage();
                // Diagnostic: log the CLI's raw `result.usage` JSON so the
                // per-turn source numbers can be reconciled against the
                // CodeBuddy backend totals (which are aggregate, not
                // per-session). Without this we cannot tell whether the
                // CLI-reported `input_tokens` / `cache_read_*` are already
                // half of the backend's real consumption at the source.
                append_codebuddy_proxy_log(&format!(
                    "result raw usage={}",
                    raw_usage
                        .map(|v| serde_json::to_string(v).unwrap_or_default())
                        .unwrap_or_else(|| "null".to_string()),
                ));
                let cumulative = extract_cli_usage(raw_usage);
                // Also capture per-call usage from the result message itself
                // as a last-resort fallback when the CLI reports it there
                // but not in any earlier assistant/stream_event message.
                if cumulative.is_some() && last_model_call.is_none() {
                    *last_model_call = cumulative;
                }
                return cumulative;
            }
            "assistant" => {
                if let Some(u) = extract_cli_usage(next.usage()) {
                    *last_model_call = Some(u);
                }
            }
            "stream_event" => {
                if let Some(ev) = next.event() {
                    if let Some(u) = extract_cli_usage(ev.get("usage")) {
                        *last_model_call = Some(u);
                    }
                    if let Some(u) =
                        extract_cli_usage(ev.get("message").and_then(|m| m.get("usage")))
                    {
                        *last_model_call = Some(u);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Convert the latest CLI cumulative reading into a per-turn OpenAI usage,
/// update the pool-entry baseline, and log both the cumulative and delta.
async fn finalize_turn_usage(
    last_cli_usage: &Mutex<Option<CliUsage>>,
    current: Option<CliUsage>,
    last_model_call: Option<CliUsage>,
    mode: &str,
    model: &str,
    finish_reason: &str,
    tool_calls: u32,
) -> OaiUsage {
    // Cumulative baseline → per-turn OpenAI usage delta (reported to codex).
    // `result.usage.input_tokens` is a cumulative input accumulator, so the
    // delta is the per-turn input cost. The proxy does NOT auto-reset the
    // pooled session on bloat heuristics — codex CLI is told (via the model
    // catalog) a ~1B context window for the CodeBuddy provider so it never
    // auto-compacts, and the CodeBuddy agent manages its own compaction
    // transparently. Reset only happens on an explicit `X-Context-Reset`/
    // `X-Context-Epoch` request from the client.
    let mut baseline = last_cli_usage.lock().await;
    let prev = *baseline;
    let (reported, next_baseline, delta) =
        resolve_reported_usage(prev, current, last_model_call);
    *baseline = next_baseline;
    drop(baseline);
    let cum = current.unwrap_or_default();
    let call = last_model_call.unwrap_or_default();
    append_codebuddy_proxy_log(&format!(
        "{mode} done model={model} finish={finish_reason} tool_calls={tool_calls} \
         cli_cumulative_prompt_tokens={} cli_cumulative_completion_tokens={} \
         cli_cumulative_cache_read={} cli_cumulative_cache_write={} \
         turn_delta_prompt_tokens={} turn_delta_completion_tokens={} \
         last_call_prompt_tokens={} last_call_completion_tokens={} \
         reported_prompt_tokens={} reported_completion_tokens={} reported_total_tokens={} \
         reported_cache_read={} reported_cache_write={}",
        cum.input_tokens,
        cum.output_tokens,
        cum.cache_read_input_tokens,
        cum.cache_creation_input_tokens,
        delta.input_tokens,
        delta.output_tokens,
        call.input_tokens,
        call.output_tokens,
        reported.prompt_tokens,
        reported.completion_tokens,
        reported.total_tokens,
        reported.cache_read_input_tokens.unwrap_or(0),
        reported.cache_creation_input_tokens.unwrap_or(0),
    ));
    if reported.total_tokens == 0 {
        append_codebuddy_proxy_log(&format!(
            "{mode} zero_usage_warning model={model} finish={finish_reason} \
             had_cumulative={} had_last_call={} baseline_prompt={} baseline_completion={}",
            current.is_some(),
            last_model_call.is_some(),
            cum.input_tokens,
            cum.output_tokens,
        ));
    }
    reported
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn take_capture_for_pairs_by_name_in_arrival_order() {
        let mut caps: Vec<(String, String)> = vec![
            ("a".into(), "a1".into()),
            ("b".into(), "b1".into()),
            ("a".into(), "a2".into()),
        ];
        // first 'a' block consumes the first 'a' capture (a1)
        assert_eq!(take_capture_for(&mut caps, "a").as_deref(), Some("a1"));
        // 'b' pairs regardless of arrival position
        assert_eq!(take_capture_for(&mut caps, "b").as_deref(), Some("b1"));
        // second 'a' block consumes the remaining 'a' capture (a2)
        assert_eq!(take_capture_for(&mut caps, "a").as_deref(), Some("a2"));
        // no capture left for a third 'a' → None (adapter falls back to input)
        assert!(take_capture_for(&mut caps, "a").is_none());
        assert!(caps.is_empty());
    }

    #[test]
    fn take_capture_for_missing_returns_none_for_fallback() {
        let mut caps: Vec<(String, String)> = vec![("a".into(), "a1".into())];
        assert_eq!(take_capture_for(&mut caps, "a").as_deref(), Some("a1"));
        assert!(take_capture_for(&mut caps, "b").is_none());
    }

    #[tokio::test]
    async fn collect_captures_releases_every_handler_with_placeholder() {
        use crate::pending::PendingQueue;
        use codebuddy_sdk::mcp::server::SdkMcpToolContent;
        use std::time::Duration;
        use tokio::time::timeout;

        let q = PendingQueue::new();
        let rx1 = q.push_capturing("a".to_string(), "{}".to_string()).await;
        let rx2 = q.push_capturing("b".to_string(), "{}".to_string()).await;
        let rx3 = q.push_capturing("a".to_string(), "{}".to_string()).await;
        let caps = collect_captures(&q, 3).await;
        assert_eq!(caps.len(), 3);
        // every handler must resolve with the placeholder — none stay pending
        for rx in [rx1, rx2, rx3] {
            let r = timeout(Duration::from_secs(1), rx)
                .await
                .expect("handler resolved (not kept pending)")
                .expect("sender not dropped");
            match &r.content[0] {
                SdkMcpToolContent::Text { text, .. } => assert_eq!(text, PLACEHOLDER_TOOL_RESULT),
                other => panic!("expected text, got {other:?}"),
            }
        }
    }

    #[test]
    fn pairing_survives_arrival_order_mismatch() {
        // emission order: a, b ; arrival order: b, a — must still pair by name.
        let blocks: Vec<(String, String, Value)> = vec![
            ("id1".into(), "a".into(), json!({"fb": "fa"})),
            ("id2".into(), "b".into(), json!({"fb": "fb"})),
        ];
        let mut caps: Vec<(String, String)> = vec![
            ("b".into(), "realb".into()),
            ("a".into(), "reala".into()),
        ];
        let out: Vec<(String, String)> = blocks
            .iter()
            .map(|(id, name, fb)| {
                let args = take_capture_for(&mut caps, name)
                    .unwrap_or_else(|| serde_json::to_string(fb).unwrap_or_else(|_| "{}".into()));
                (id.clone(), args)
            })
            .collect();
        assert_eq!(out[0], ("id1".to_string(), "reala".to_string()));
        assert_eq!(out[1], ("id2".to_string(), "realb".to_string()));
    }

    #[tokio::test]
    async fn drain_pending_acks_clears_stale_acks() {
        let (tx, rx) = mpsc::unbounded_channel::<()>();
        let rx = Mutex::new(rx);
        // Two stale acks left by a prior turn whose await_tool_acks bailed.
        let _ = tx.send(());
        let _ = tx.send(());
        drain_pending_acks(&rx).await;
        // Channel must be empty now.
        assert!(rx.lock().await.try_recv().is_err());
    }

    #[tokio::test]
    async fn await_tool_acks_waits_for_exact_real_acks() {
        let (tx, rx) = mpsc::unbounded_channel::<()>();
        let rx = Mutex::new(rx);
        // Keep `tx` alive so the channel stays open (recv blocks, not None).
        let tx2 = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let _ = tx2.send(());
            let _ = tx2.send(());
        });
        // Awaits exactly 2 real acks; returns promptly once both land (does
        // not hang, does not bail early on a missing/stale ack).
        let got = tokio::time::timeout(Duration::from_secs(3), async {
            await_tool_acks(&rx, 2).await
        })
        .await;
        assert!(got.is_ok(), "await_tool_acks returned after both real acks");
        assert_eq!(got.unwrap(), 2, "received exactly 2 real acks");
    }

    #[tokio::test]
    async fn drain_prevents_stale_ack_poisoning_next_turn() {
        // Regression: a prior turn left a stale ack (its placeholder write
        // overran the deadline). Without draining at turn start, the next
        // turn's await_tool_acks(1) would consume that stale ack instantly
        // and interrupt before this turn's placeholder lands → `undefined`.
        let (tx, rx) = mpsc::unbounded_channel::<()>();
        let rx = Mutex::new(rx);
        // prior turn residue
        let _ = tx.send(());
        // new turn: drain first, then await 1 real (delayed) ack
        drain_pending_acks(&rx).await;
        let tx2 = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            let _ = tx2.send(());
        });
        let started = tokio::time::Instant::now();
        let got = await_tool_acks(&rx, 1).await;
        assert_eq!(got, 1, "awaited exactly the one real (delayed) ack");
        assert!(
            started.elapsed() >= Duration::from_millis(30),
            "awaited the real delayed ack, not the drained stale one: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn settle_cli_before_interrupt_sleeps_at_least_settle_budget() {
        let started = tokio::time::Instant::now();
        settle_cli_before_interrupt().await;
        assert!(
            started.elapsed() >= Duration::from_millis(SETTLE_BEFORE_INTERRUPT_MS),
            "settle must pause ~{}ms to let the CLI commit placeholders before interrupt",
            SETTLE_BEFORE_INTERRUPT_MS,
        );
    }

    #[tokio::test]
    async fn await_tool_acks_bails_on_closed_channel_returning_zero() {
        // A closed ack channel (session teardown drops the sender) makes
        // `recv()` return `None` immediately → bail with 0 received, so the
        // caller skips the settle and interrupts straight away.
        let (tx, rx) = mpsc::unbounded_channel::<()>();
        let rx = Mutex::new(rx);
        drop(tx);
        let got = tokio::time::timeout(Duration::from_secs(2), async {
            await_tool_acks(&rx, 2).await
        })
        .await
        .expect("bails immediately on closed channel");
        assert_eq!(got, 0, "closed channel → 0 acks received, bailed");
    }

    #[test]
    fn pairing_falls_back_to_input_for_missing_capture() {
        // two blocks, only one captured → the uncaptured block uses its input.
        let blocks: Vec<(String, String, Value)> = vec![
            ("id1".into(), "a".into(), json!({"fb": "fa"})),
            ("id2".into(), "b".into(), json!({"fb": "fb"})),
        ];
        let mut caps: Vec<(String, String)> = vec![("a".into(), "reala".into())];
        let out: Vec<(String, String)> = blocks
            .iter()
            .map(|(id, name, fb)| {
                let args = take_capture_for(&mut caps, name)
                    .unwrap_or_else(|| serde_json::to_string(fb).unwrap_or_else(|_| "{}".into()));
                (id.clone(), args)
            })
            .collect();
        assert_eq!(out[0], ("id1".to_string(), "reala".to_string()));
        assert_eq!(out[1], ("id2".to_string(), "{\"fb\":\"fb\"}".to_string()));
    }

}
