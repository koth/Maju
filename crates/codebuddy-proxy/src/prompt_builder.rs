use std::sync::Arc;
use serde_json::{Value, json};
use crate::logging::append_codebuddy_proxy_log;
use crate::openai_types::{OaiChatRequest, OaiMessage, OaiTool};
use crate::pending::PendingQueue;
pub const PROXY_TOOL_SERVER_NAME: &str = "proxy_tools";

/// Per-tool-result reflow cap (in chars). A single oversized result — e.g. a
/// 6254-line / 41KB `apply_patch` diff — makes the CLI end the turn with
/// `finish=stop` and no output (observed at content_len=41750; 19291 is safe).
///
/// Align with Codex's default tool-output truncation policy:
/// `TruncationPolicy::Bytes(10_000)` + middle truncate (head + tail). Codex
/// approximates 4 bytes/token; for reflow we budget by chars (UTF-8 safe) at
/// the same 10k scale, which is stricter than the previous 16k head-only cap
/// that still let CodeBuddy CLI empty-stop.
const MAX_TOOL_RESULT_CHARS: usize = 10_000;
/// Whole reflow body budget across all tool results + trailing user text.
/// Keeps multi-tool turns from stacking several 10k bodies into one CLI user
/// message that historically empty-stopped around ~18–23k content_len.
const MAX_REFLOW_TOTAL_CHARS: usize = 14_000;
/// If the remaining whole-reflow budget can only fit a tiny body after the
/// fixed header, drop the rest of the tool results instead of emitting a
/// near-empty / low-value fragment.
const MIN_USEFUL_TOOL_BODY_CHARS: usize = 512;

/// After compact/session reset the pooled CLI is cold. Instead of sending only
/// the latest user line (which drops the compacted conversation), reseed with
/// the recent post-compact view. Cap by turn count and total chars so a huge
/// history cannot blow up the first post-reset request.
const SEED_MAX_TURNS: usize = 6;
const SEED_MAX_CHARS: usize = 48_000;
const SEED_ASSISTANT_CHARS: usize = 4_000;
const SEED_TOOL_RESULT_CHARS: usize = 4_000;
const SEED_USER_CHARS: usize = 8_000;

/// Truncate a tool-result text to at most `max_chars`, keeping both the head
/// and the tail (Codex-style middle truncation).
///
/// Why head+tail:
/// - head carries status (`Exit code`, errors, file headers, first hunks)
/// - tail often carries the final error / last lines / trailing summary
/// Codex uses the same shape (`…N chars truncated…` between prefix and suffix).
///
/// Splits on Unicode scalar values so multi-byte text is never torn mid-codepoint.
/// Returns the original string unchanged when it fits.
fn truncate_tool_result(text: &str) -> String {
    truncate_middle_chars(text, MAX_TOOL_RESULT_CHARS)
}

fn truncate_middle_chars(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    if len <= max_chars {
        return text.to_string();
    }
    if max_chars == 0 {
        return format!("…{len} chars truncated…");
    }

    // Worst-case marker width for this input (omitted can be up to `len`).
    // Reserve it first so the final string never exceeds `max_chars`.
    let worst_marker = format!("…{len} chars truncated…");
    let marker_budget = worst_marker.chars().count();
    if max_chars <= marker_budget + 2 {
        // Budget too small for any useful head/tail; emit marker only (may still
        // slightly exceed tiny budgets — acceptable for pathological cases).
        return format!("…{len} chars truncated…");
    }

    let keep = max_chars.saturating_sub(marker_budget);
    let head_len = keep / 2;
    let tail_len = keep - head_len;
    let head: String = chars[..head_len].iter().collect();
    let tail: String = chars[len - tail_len..].iter().collect();
    let actual_omitted = len.saturating_sub(head_len).saturating_sub(tail_len);
    let marker = format!("…{actual_omitted} chars truncated…");
    // Because actual_omitted <= len, marker width <= reserved marker_budget,
    // so head+marker+tail fits max_chars (padding with nothing if shorter).
    let mut out = String::with_capacity(max_chars);
    out.push_str(&head);
    out.push_str(&marker);
    out.push_str(&tail);
    // Safety clamp if marker width assumptions ever drift.
    if out.chars().count() > max_chars {
        out.chars().take(max_chars).collect()
    } else {
        out
    }
}

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

/// Cold-start seed for a new/recreated CLI session.
///
/// System instructions are already applied via `SessionOptions.system_prompt`
/// (see `build_prompt`); this function only serializes the recent non-system
/// conversation so the fresh CLI can recover post-compact context.
///
/// Strategy:
/// - Keep the last [`SEED_MAX_TURNS`] user-led turns (user + following
///   assistant/tool messages until the next user).
/// - Truncate individual parts and the total seed by char budget.
/// - Fall back to the latest user line when no conversation body exists.
pub fn build_seed_prompt(messages: &[OaiMessage]) -> String {
    let convo: Vec<&OaiMessage> = messages.iter().filter(|m| m.role != "system").collect();
    if convo.is_empty() {
        return "(continue)".to_string();
    }

    // Partition into user-led turns: each turn starts at a user message (or at
    // the first non-user prefix if history begins mid-turn).
    let mut turns: Vec<Vec<&OaiMessage>> = Vec::new();
    let mut current: Vec<&OaiMessage> = Vec::new();
    for msg in &convo {
        if msg.role == "user" && !current.is_empty() {
            // If the current buffer already has a user, start a new turn.
            if current.iter().any(|m| m.role == "user") {
                turns.push(std::mem::take(&mut current));
            }
        }
        current.push(*msg);
    }
    if !current.is_empty() {
        turns.push(current);
    }
    if turns.is_empty() {
        return build_full_prompt(messages);
    }

    let start = turns.len().saturating_sub(SEED_MAX_TURNS);
    let selected = &turns[start..];

    let mut parts: Vec<String> = Vec::new();
    // Walk newest-first for budget, then reverse for chronological output.
    let mut budget_left = SEED_MAX_CHARS;
    let mut kept_rev: Vec<String> = Vec::new();
    for turn in selected.iter().rev() {
        let rendered = render_seed_turn(turn);
        if rendered.is_empty() {
            continue;
        }
        let cost = rendered.chars().count();
        if cost > budget_left {
            if kept_rev.is_empty() {
                // Always keep at least the latest turn, truncated to budget.
                kept_rev.push(truncate_chars(&rendered, budget_left));
            }
            break;
        }
        budget_left = budget_left.saturating_sub(cost.saturating_add(2)); // + blank line
        kept_rev.push(rendered);
    }
    kept_rev.reverse();
    parts.extend(kept_rev);

    if parts.is_empty() {
        return build_full_prompt(messages);
    }

    let body = parts.join("\n\n");
    // Prefix makes the reseed intent explicit for the model and for logs.
    format!("[conversation seed — recent context after session reset]\n\n{body}")
}

fn render_seed_turn(turn: &[&OaiMessage]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for msg in turn {
        match msg.role.as_str() {
            "user" => {
                let text = truncate_chars(&content_to_text(&msg.content), SEED_USER_CHARS);
                if !text.is_empty() {
                    lines.push(format!("user:\n{text}"));
                }
            }
            "assistant" => {
                let text = truncate_chars(&content_to_text(&msg.content), SEED_ASSISTANT_CHARS);
                if !text.is_empty() {
                    lines.push(format!("assistant:\n{text}"));
                }
                if let Some(calls) = msg.tool_calls.as_ref() {
                    for call in calls {
                        let name = call
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .or_else(|| call.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("tool");
                        let args = call
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .or_else(|| call.get("arguments"))
                            .map(|v| match v {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .unwrap_or_default();
                        let args = truncate_chars(&args, 500);
                        let id = call
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if id.is_empty() {
                            lines.push(format!("assistant tool_call: {name}({args})"));
                        } else {
                            lines.push(format!("assistant tool_call id={id}: {name}({args})"));
                        }
                    }
                }
            }
            "tool" => {
                let id = msg.tool_call_id.clone().unwrap_or_default();
                let text = truncate_chars(&content_to_text(&msg.content), SEED_TOOL_RESULT_CHARS);
                if id.is_empty() {
                    lines.push(format!("tool result:\n{text}"));
                } else {
                    lines.push(format!("tool result id={id}:\n{text}"));
                }
            }
            _ => {}
        }
    }
    lines.join("\n\n")
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    // Leave room for an ellipsis marker when possible.
    let keep = max_chars.saturating_sub(1);
    let head: String = chars[..keep].iter().collect();
    format!("{head}…")
}
/// Build the plain-text content to send to the CLI for this turn.
///
/// Tool results are emitted as plain text:
/// ```text
/// 下面是工具调用结果，收到请继续
/// ID: <tool_call_id>
/// <result body>
/// ```
/// Multiple results (and any trailing user text) are separated by a blank
/// line. Plain text keeps the reflow format simple and sidesteps structured-
/// block handling in the CLI; the model correlates a result to its `tool_use`
/// via the `ID:` line. The result payload is just text after the header line,
/// so a 40KB diff or a literal marker inside it cannot corrupt the `ID:`
/// pairing.
const TOOL_RESULT_CONTINUE_HINT: &str = "下面是工具调用结果，收到请继续";
pub fn build_incremental_tail(messages: &[OaiMessage]) -> Value {
    let convo: Vec<&OaiMessage> = messages.iter().filter(|m| m.role != "system").collect();
    let last_assistant_idx = convo.iter().rposition(|m| m.role == "assistant");
    let plain = |t: String| Value::String(t);
    match last_assistant_idx {
        None => {
            let texts: Vec<String> = convo
                .iter()
                .filter(|m| m.role == "user")
                .map(|m| content_to_text(&m.content))
                .filter(|t| !t.is_empty())
                .collect();
            plain(if texts.is_empty() { "(continue)".to_string() } else { texts.join("\n\n") })
        }
        Some(idx) => {
            let tail: Vec<&&OaiMessage> = convo[idx + 1..].iter().collect();
            if tail.is_empty() {
                return plain("(continue)".to_string());
            }
            let tool_results: Vec<&&&OaiMessage> = tail.iter().filter(|m| m.role == "tool").collect();
            let user_texts: Vec<String> = tail
                .iter()
                .filter(|m| m.role == "user")
                .map(|m| content_to_text(&m.content))
                .filter(|t| !t.is_empty())
                .collect();
            if tool_results.is_empty() {
                plain(if user_texts.is_empty() { "(continue)".to_string() } else { user_texts.join("\n\n") })
            } else {
                // Plain text: continue hint + "ID: <call_id>" + body.
                // Cap each result (Codex-like 10k middle truncate) and the whole
                // reflow body so multi-tool turns cannot stack into an empty-stop.
                let mut used_total = 0usize;
                let mut parts: Vec<String> = Vec::new();
                for (idx, m) in tool_results.iter().enumerate() {
                    // Fixed framing around every tool body. Count it against the
                    // total budget up-front so a second result cannot squeeze in
                    // after a near-full first body.
                    let id = m.tool_call_id.clone().unwrap_or_default();
                    let header = format!("{TOOL_RESULT_CONTINUE_HINT}\nID: {id}\n");
                    let header_chars = header.chars().count();
                    let sep = if parts.is_empty() { 0 } else { 2 }; // "\n\n" between parts
                    let remaining_budget = MAX_REFLOW_TOTAL_CHARS
                        .saturating_sub(used_total)
                        .saturating_sub(sep);
                    let body_budget = remaining_budget
                        .saturating_sub(header_chars)
                        .min(MAX_TOOL_RESULT_CHARS);
                    if remaining_budget <= header_chars || body_budget < MIN_USEFUL_TOOL_BODY_CHARS {
                        let remaining = tool_results.len().saturating_sub(idx);
                        parts.push(format!(
                            "…[omitted {remaining} more tool result(s); reflow total budget {MAX_REFLOW_TOTAL_CHARS} chars]…"
                        ));
                        break;
                    }
                    let text = content_to_text(&m.content);
                    let original_chars = text.chars().count();
                    let text = truncate_middle_chars(&text, body_budget);
                    let kept_chars = text.chars().count();
                    if kept_chars < original_chars {
                        append_codebuddy_proxy_log(&format!(
                            "reflow truncated tool_result id={id} original_chars={original_chars} kept_chars={kept_chars} budget={body_budget}"
                        ));
                    }
                    let part = format!("{header}{text}");
                    used_total = used_total
                        .saturating_add(sep)
                        .saturating_add(part.chars().count());
                    parts.push(part);
                }
                if !user_texts.is_empty() {
                    let mut user_joined = user_texts.join("\n\n");
                    let remaining_budget = MAX_REFLOW_TOTAL_CHARS.saturating_sub(used_total);
                    if user_joined.chars().count() > remaining_budget {
                        user_joined = truncate_middle_chars(&user_joined, remaining_budget);
                    }
                    if !user_joined.is_empty() {
                        parts.push(user_joined);
                    }
                }
                plain(parts.join("\n\n"))
            }
        }
    }
}
pub fn build_user_content(messages: &[OaiMessage], is_new: bool) -> Value {
    if is_new {
        // Cold / post-reset: reseed with recent post-compact conversation view
        // (system already lives in SessionOptions.system_prompt).
        Value::String(build_seed_prompt(messages))
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
            // Capture+release handler: record the parsed arguments (the
            // assistant `tool_use` block carries id+name but NOT reliable
            // input), then await the adapter's per-call release signal. For a
            // turn with N tool_use blocks the adapter releases every capture
            // with a placeholder ("见下一个<user_query> 中的工具结果") so a
            // sequential CLI dispatches all N tools and every handler resolves
            // (no leaked tasks); after the N-th capture it interrupts, and
            // codex reflows the real results in the next `user_query`.
            let handler_name = name.clone();
            let handler: codebuddy_sdk::mcp::server::SdkMcpHandler =
                std::sync::Arc::new(move |input: Value| {
                    let name = handler_name.clone();
                    let pending = pending.clone();
                    Box::pin(async move {
                        let args = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                        // Hand back a release receiver; the adapter releases
                        // every capture with a placeholder pointing the model
                        // to the next user_query, then interrupts after the
                        // Nth. A ReceiveError here means the sender was
                        // dropped — only possible at session teardown, where
                        // the SDK's `close_rx` has already reaped this task, so
                        // the error is never actually observed by the CLI.
                        let rx = pending.push_capturing(name, args).await;
                        match rx.await {
                            Ok(result) => Ok(result),
                            Err(_) => Err(codebuddy_sdk::SdkError::Handler(
                                "tool capture release dropped".to_string(),
                            )),
                        }
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
        // Legacy helper still returns the latest user only.
        assert_eq!(build_full_prompt(&msgs), "latest");
        // Cold-start seed includes recent turns, not only the last user line.
        let seed = build_seed_prompt(&msgs);
        assert!(seed.contains("conversation seed"), "seed header present");
        assert!(seed.contains("user:\nfirst"), "earlier user kept within budget");
        assert!(seed.contains("assistant:\nanswer"));
        assert!(seed.contains("user:\nlatest"));
    }

    #[test]
    fn seed_keeps_only_recent_turns() {
        let mut msgs = Vec::new();
        for i in 1..=10 {
            msgs.push(user(&format!("user-turn-{i}")));
            msgs.push(assistant(&format!("assistant-turn-{i}")));
        }
        msgs.push(user("user-turn-final"));
        let seed = build_seed_prompt(&msgs);
        // Last SEED_MAX_TURNS user-led turns kept; oldest dropped.
        assert!(
            !seed.contains("user:\nuser-turn-1\n") && !seed.contains("user:\nuser-turn-1\r"),
            "old turn dropped: {seed}"
        );
        assert!(
            !seed.contains("user:\nuser-turn-2\n"),
            "old turn dropped: {seed}"
        );
        assert!(seed.contains("user:\nuser-turn-final"), "latest user kept: {seed}");
        assert!(
            seed.contains("user:\nuser-turn-10\n") || seed.contains("assistant:\nassistant-turn-10"),
            "recent turn kept: {seed}"
        );
    }

    #[test]
    fn seed_includes_tool_calls_and_results() {
        let assistant_with_tools = OaiMessage {
            role: "assistant".to_string(),
            content: Value::String("checking".to_string()),
            tool_call_id: None,
            tool_calls: Some(vec![json!({
                "id": "call_1",
                "type": "function",
                "function": { "name": "shell_command", "arguments": "{\"command\":\"ls\"}" }
            })]),
            name: None,
        };
        let msgs = vec![
            user("list files"),
            assistant_with_tools,
            tool_result("call_1", "a.rs\nb.rs"),
            user("thanks"),
        ];
        let seed = build_seed_prompt(&msgs);
        assert!(seed.contains("assistant tool_call id=call_1: shell_command"));
        assert!(seed.contains("tool result id=call_1:"));
        assert!(seed.contains("a.rs\nb.rs"));
        assert!(seed.contains("user:\nthanks"));
    }

    #[test]
    fn seed_falls_back_when_only_latest_user() {
        let msgs = vec![user("only")];
        let seed = build_seed_prompt(&msgs);
        assert!(seed.contains("user:\nonly") || seed == "only" || seed.contains("only"));
    }

    #[test]
    fn build_user_content_cold_uses_seed() {
        let msgs = vec![user("first"), assistant("answer"), user("latest")];
        let out = build_user_content(&msgs, true);
        let s = out.as_str().expect("string");
        assert!(s.contains("conversation seed"));
        assert!(s.contains("latest"));
    }

    #[test]
    fn build_user_content_warm_still_incremental() {
        let msgs = vec![assistant("answer"), user("follow up")];
        let out = build_user_content(&msgs, false);
        assert_eq!(out, Value::String("follow up".to_string()));
    }
    #[test]
    fn warm_session_tool_result_is_text_with_content() {
        let msgs = vec![assistant("check"), tool_result("call_1", "sunny, 20C"), user("thanks")];
        let out = build_incremental_tail(&msgs);
        assert_eq!(
            out,
            Value::String(format!(
                "{TOOL_RESULT_CONTINUE_HINT}\nID: call_1\nsunny, 20C\n\nthanks"
            ))
        );
    }
    #[test]
    fn warm_session_tool_result_no_trailing_text() {
        let msgs = vec![assistant("check"), tool_result("call_1", "result")];
        let out = build_incremental_tail(&msgs);
        assert_eq!(
            out,
            Value::String(format!("{TOOL_RESULT_CONTINUE_HINT}\nID: call_1\nresult"))
        );
    }
    #[test]
    fn warm_session_no_tool_results_is_plain_text() {
        let msgs = vec![assistant("answer"), user("follow up")];
        let out = build_incremental_tail(&msgs);
        assert_eq!(out, Value::String("follow up".to_string()));
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

    #[test]
    fn truncate_short_result_unchanged() {
        assert_eq!(truncate_tool_result("ok"), "ok");
        assert_eq!(truncate_tool_result(""), "");
    }

    #[test]
    fn truncate_long_result_keeps_head_and_tail() {
        let big: String = "a".repeat(40_000);
        let out = truncate_tool_result(&big);
        // Codex-style middle truncate: head + marker + tail.
        assert!(out.starts_with('a'), "head kept");
        assert!(out.ends_with('a'), "tail kept");
        assert!(out.contains("truncated"), "marker present");
        assert!(out.chars().count() <= MAX_TOOL_RESULT_CHARS, "within budget");
        // Marker should report how many chars were removed from the middle.
        assert!(out.contains("chars truncated"), "omission count present: {out}");
        // Ensure both ends survived (not head-only).
        let marker_at = out.find('…').expect("marker");
        assert!(marker_at > 0, "prefix before marker");
        assert!(marker_at < out.len() - 1, "suffix after marker");
    }

    #[test]
    fn truncate_preserves_id_header_in_reflow() {
        // A 30K result reflows capped; the continue hint + `ID:` header stay
        // and the body is middle-truncated.
        let big = "x".repeat(30_000);
        let msgs = vec![assistant("check"), tool_result("call_1", &big)];
        let out = build_incremental_tail(&msgs);
        let s = out.as_str().expect("string");
        assert!(
            s.starts_with(&format!("{TOOL_RESULT_CONTINUE_HINT}\nID: call_1\n")),
            "continue hint + header preserved: {s}"
        );
        assert!(s.contains("truncated"), "body truncated");
        // Body should keep a tail after the marker, not drop everything after head.
        let body = s.split_once("ID: call_1\n").expect("id header").1;
        assert!(body.contains('…'), "middle marker in body");
        assert!(body.ends_with('x'), "tail retained: {body}");
    }

    #[test]
    fn truncate_safe_on_multibyte() {
        // 20_000 CJK chars: must cap by char count without splitting a codepoint,
        // and retain both ends.
        let big: String = "中".repeat(20_000);
        let out = truncate_tool_result(&big);
        assert!(out.starts_with("中"));
        assert!(out.ends_with("中"), "tail retained");
        assert!(out.contains("truncated"));
        assert!(out.chars().count() <= MAX_TOOL_RESULT_CHARS);
    }

    #[test]
    fn reflow_total_budget_omits_later_tool_results() {
        // Two max-size tool bodies would exceed MAX_REFLOW_TOTAL_CHARS once
        // framing is included. The second result must be omitted, or kept only
        // as a residual fragment well under the per-result cap.
        let big = "y".repeat(MAX_TOOL_RESULT_CHARS + 5_000);
        let msgs = vec![
            assistant("check"),
            tool_result("call_1", &big),
            tool_result("call_2", &big),
        ];
        let out = build_incremental_tail(&msgs);
        let s = out.as_str().expect("string");
        assert!(s.contains("ID: call_1\n"), "first result kept");
        if let Some(second) = s.split("ID: call_2\n").nth(1) {
            // Residual second body must be far smaller than a full per-result cap.
            assert!(
                second.chars().count() < MAX_TOOL_RESULT_CHARS / 2,
                "second body should be residual, got {}",
                second.chars().count()
            );
        } else {
            assert!(s.contains("omitted"), "or explicit omit marker");
        }
        assert!(
            s.chars().count() <= MAX_REFLOW_TOTAL_CHARS + 120,
            "near total budget, got {}",
            s.chars().count()
        );
    }
}
