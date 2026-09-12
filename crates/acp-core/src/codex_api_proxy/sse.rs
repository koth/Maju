use super::*;

#[cfg(test)]
use serde_json::Map;

/// Response-item id the converter gives the reasoning summary it synthesizes
/// from the upstream's `reasoning_content` deltas (Codex tracks reasoning by
/// item id; `msg_proxy` is the assistant message).
const REASONING_ITEM_ID: &str = "rs_proxy";

fn remember_stream_reasoning(state: &ChatStreamState) {
    if state.reasoning_content.trim().is_empty() {
        return;
    }
    let tool_calls = state
        .tool_calls
        .iter()
        .filter(|call| call.added)
        .map(stream_tool_call_to_chat_tool_call)
        .collect::<Vec<_>>();
    remember_assistant_reasoning(
        &state.session_id,
        &state.text,
        &tool_calls,
        &state.reasoning_content,
    );
}

fn stream_tool_call_to_chat_tool_call(call: &StreamToolCall) -> Value {
    json!({
        "id": if call.id.is_empty() { "call_proxy" } else { &call.id },
        "type": "function",
        "function": {
            "name": if call.name.is_empty() { "unknown" } else { &call.name },
            "arguments": call.arguments
        }
    })
}

#[derive(Debug, Default)]
struct ChatStreamState {
    response_id: String,
    model: String,
    text: String,
    reasoning_content: String,
    /// Text of the reasoning summary part currently open. The closing `done`
    /// event carries the part's own text, so a second reasoning burst (after a
    /// tool call) does not repeat the first one.
    reasoning_part_text: String,
    /// Whether the reasoning output item was announced to the Responses client.
    /// Codex ignores reasoning deltas that arrive without an active reasoning
    /// item (`ReasoningSummaryDelta without active item`).
    reasoning_item_open: bool,
    message_started: bool,
    text_block_started: bool,
    text_block_index: Option<usize>,
    next_content_block_index: usize,
    stop_reason: Option<String>,
    tool_calls: Vec<StreamToolCall>,
    usage: Value,
    /// Session id this stream belongs to, used to partition the
    /// reasoning-history cache so concurrent sessions cannot cross-talk.
    session_id: String,
}

#[derive(Debug, Default, Clone)]
struct StreamToolCall {
    id: String,
    name: String,
    arguments: String,
    added: bool,
    content_block_index: Option<usize>,
}

#[cfg(test)]
pub(super) fn chat_sse_to_responses_sse(body: &[u8]) -> Vec<u8> {
    let mut converter = ChatSseStreamConverter::new("test-session");
    let mut output = converter.push_chunk(body);
    output.extend(converter.finish());
    output
}

fn process_chat_sse_event(event: &str, output: &mut String, state: &mut ChatStreamState) {
    let Some(data) = sse_data_line(event) else {
        return;
    };
    if data.trim() == "[DONE]" {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
        append_codex_api_proxy_log("failed_to_parse_chat_sse_event");
        return;
    };
    if let Some(id) = value.get("id").and_then(Value::as_str) {
        state.response_id = id.to_string();
    }
    if let Some(model) = value.get("model").and_then(Value::as_str) {
        state.model = model.to_string();
    }
    if let Some(usage) = value.get("usage") {
        state.usage = normalized_chat_usage(Some(usage));
    }

    let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    else {
        return;
    };
    if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str) {
        state.stop_reason = Some(chat_finish_reason_to_anthropic(finish_reason).to_string());
    }
    let delta = choice.get("delta").unwrap_or(&Value::Null);
    if let Some(reasoning_content) = delta.get("reasoning_content").and_then(Value::as_str) {
        state.reasoning_content.push_str(reasoning_content);
        emit_reasoning_delta(output, state, reasoning_content);
    }
    if let Some(content) = delta.get("content").and_then(Value::as_str) {
        emit_text_delta(output, state, content);
    }
    if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            emit_tool_call_delta(output, state, tool_call);
        }
    }
}

fn sse_data_line(event: &str) -> Option<&str> {
    event
        .lines()
        .find_map(|line| line.strip_prefix("data:").map(str::trim_start))
}

/// Forward one upstream `reasoning_content` delta as a **Responses reasoning
/// summary** event.
///
/// Codex only reads reasoning text from the standard Responses event stream:
/// `response.output_item.added` (item type `reasoning`) opens the item,
/// `response.reasoning_summary_text.delta` streams it (`ReasoningSummaryDelta`,
/// which requires an active item and a `summary_index`), and the client
/// surfaces the text as `agent_thought_chunk` over ACP. Chat-completions
/// upstreams (timiai, kimi, deepseek, …) send the model's thinking as
/// `choices[].delta.reasoning_content` instead; this converter used to collect
/// that text and publish it only in the final item's **non-standard**
/// `reasoning_content` field, which Codex ignores — so a codex session showed
/// no thinking block at all while the same model under dsh streamed one.
fn emit_reasoning_delta(output: &mut String, state: &mut ChatStreamState, delta: &str) {
    if delta.is_empty() {
        return;
    }
    if !state.reasoning_item_open {
        state.reasoning_item_open = true;
        state.reasoning_part_text.clear();
        push_sse(
            output,
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "id": REASONING_ITEM_ID,
                    "type": "reasoning",
                    "summary": []
                }
            }),
        );
        push_sse(
            output,
            "response.reasoning_summary_part.added",
            json!({
                "type": "response.reasoning_summary_part.added",
                "item_id": REASONING_ITEM_ID,
                "output_index": 0,
                "summary_index": 0,
                "part": { "type": "summary_text", "text": "" }
            }),
        );
    }
    state.reasoning_part_text.push_str(delta);
    push_sse(
        output,
        "response.reasoning_summary_text.delta",
        json!({
            "type": "response.reasoning_summary_text.delta",
            "item_id": REASONING_ITEM_ID,
            "output_index": 0,
            "summary_index": 0,
            "delta": delta
        }),
    );
}

/// Close the reasoning summary opened by [`emit_reasoning_delta`].
///
/// The `done` text is what Codex consumes when it runs with sequential-cutoff
/// summaries (`reasoning_summary_delivery: SequentialCutoff` skips the live
/// deltas), and the closing item keeps the reasoning in the streamed
/// transcript. Closing is idempotent: reopening happens on the next delta.
fn emit_reasoning_done(output: &mut String, state: &mut ChatStreamState) {
    if !state.reasoning_item_open {
        return;
    }
    state.reasoning_item_open = false;
    let text = std::mem::take(&mut state.reasoning_part_text);
    push_sse(
        output,
        "response.reasoning_summary_text.done",
        json!({
            "type": "response.reasoning_summary_text.done",
            "item_id": REASONING_ITEM_ID,
            "output_index": 0,
            "summary_index": 0,
            "text": text
        }),
    );
    push_sse(
        output,
        "response.output_item.done",
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "id": REASONING_ITEM_ID,
                "type": "reasoning",
                "summary": [{ "type": "summary_text", "text": text }]
            }
        }),
    );
}

fn emit_text_delta(output: &mut String, state: &mut ChatStreamState, delta: &str) {
    if delta.is_empty() {
        return;
    }

    // The reasoning summary precedes the answer in the Responses stream; close
    // it before the assistant message opens so the transcript keeps the order.
    emit_reasoning_done(output, state);

    if !state.message_started {
        state.message_started = true;
        push_sse(
            output,
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "id": "msg_proxy",
                    "type": "message",
                    "role": "assistant",
                    "status": "in_progress",
                    "content": []
                }
            }),
        );
        push_sse(
            output,
            "response.content_part.added",
            json!({
                "type": "response.content_part.added",
                "output_index": 0,
                "content_index": 0,
                "item_id": "msg_proxy",
                "part": { "type": "output_text", "text": "" }
            }),
        );
    }
    state.text.push_str(delta);
    push_sse(
        output,
        "response.output_text.delta",
        json!({
            "type": "response.output_text.delta",
            "output_index": 0,
            "content_index": 0,
            "item_id": "msg_proxy",
            "delta": delta
        }),
    );
}

fn emit_tool_call_delta(output: &mut String, state: &mut ChatStreamState, tool_call: &Value) {
    // When the model emits a tool_use without any preceding text (common for
    // agentic-first models like glm-5.2-ioa), `emit_text_delta` was never
    // called, so `message_started` is still false. Without a `message` item
    // wrapping the subsequent `function_call` items, the Responses stream is
    // malformed — the consumer (codex-acp) sees function_calls with no
    // assistant message container, resulting in `message_started=false` and
    // no `agent_message_chunk` being emitted to the UI.
    //
    // Fix: ensure the assistant message item exists before emitting any
    // function_call output_item. We emit an empty message item (no content
    // part) because the model produced no text — the tool card alone will be
    // shown in the UI.
    if !state.message_started {
        state.message_started = true;
        // A tool call ends the thinking that preceded it: close the reasoning
        // summary before the message container opens.
        emit_reasoning_done(output, state);
        push_sse(
            output,
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "id": "msg_proxy",
                    "type": "message",
                    "role": "assistant",
                    "status": "in_progress",
                    "content": []
                }
            }),
        );
    }

    let index = tool_call
        .get("index")
        .and_then(Value::as_u64)
        .unwrap_or(state.tool_calls.len() as u64) as usize;
    while state.tool_calls.len() <= index {
        state.tool_calls.push(StreamToolCall::default());
    }
    let call = &mut state.tool_calls[index];
    if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
        call.id = id.to_string();
    }
    if call.id.is_empty() {
        call.id = next_synthetic_tool_call_id();
    }
    let function = tool_call.get("function").unwrap_or(&Value::Null);
    if let Some(name) = function.get("name").and_then(Value::as_str) {
        call.name.push_str(name);
    }
    let output_index = if state.message_started {
        index + 1
    } else {
        index
    };
    if !call.added && !call.name.is_empty() {
        call.added = true;
        if tool_call_outputs_as_apply_patch(&call.name) {
            push_sse(
                output,
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": {
                        "id": call.id,
                        "type": "custom_tool_call",
                        "call_id": call.id,
                        "name": "apply_patch",
                        "input": "",
                        "status": "in_progress"
                    }
                }),
            );
        } else {
            let item = namespaced_function_call_item(&call.id, &call.name, "", "in_progress");
            push_sse(
                output,
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": item
                }),
            );
        }
    }
    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
        call.arguments.push_str(arguments);
        if !tool_call_outputs_as_apply_patch(&call.name) {
            push_sse(
                output,
                "response.function_call_arguments.delta",
                json!({
                    "type": "response.function_call_arguments.delta",
                    "output_index": output_index,
                    "item_id": call.id,
                    "delta": arguments
                }),
            );
        }
    }
}

fn emit_stream_done(output: &mut String, state: &mut ChatStreamState) {
    log_chat_stream_done("responses", state);

    // Closes the reasoning summary for turns that never produce assistant text
    // or tool calls — otherwise Codex's active item never sees its close.
    emit_reasoning_done(output, state);

    let mut final_output = Vec::new();
    if state.message_started {
        push_sse(
            output,
            "response.output_text.done",
            json!({
                "type": "response.output_text.done",
                "output_index": 0,
                "content_index": 0,
                "item_id": "msg_proxy",
                "text": state.text
            }),
        );
        push_sse(
            output,
            "response.content_part.done",
            json!({
                "type": "response.content_part.done",
                "output_index": 0,
                "content_index": 0,
                "item_id": "msg_proxy",
                "part": { "type": "output_text", "text": state.text }
            }),
        );
        let item =
            response_message_item_with_reasoning(&state.text, Some(&state.reasoning_content));
        remember_reasoning_content(&state.session_id, &state.text, &state.reasoning_content);
        push_sse(
            output,
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": item
            }),
        );
        final_output.push(item);
    }

    for (index, call) in state.tool_calls.iter().enumerate() {
        let output_index = if state.message_started {
            index + 1
        } else {
            index
        };
        append_codex_api_proxy_log(&format!(
            "emit_stream_done_tool_call index={index} id={} name={} args_len={} added={}",
            call.id,
            if call.name.is_empty() {
                "<empty>"
            } else {
                &call.name
            },
            call.arguments.len(),
            call.added,
        ));
        if !call.arguments.is_empty() && !tool_call_outputs_as_apply_patch(&call.name) {
            push_sse(
                output,
                "response.function_call_arguments.done",
                json!({
                    "type": "response.function_call_arguments.done",
                    "output_index": output_index,
                    "item_id": call.id,
                    "arguments": call.arguments
                }),
            );
        }
        let item = if tool_call_outputs_as_apply_patch(&call.name) {
            json!({
                "id": call.id,
                "type": "custom_tool_call",
                "call_id": call.id,
                "name": "apply_patch",
                "input": apply_patch_input_for_tool_call(&call.name, &call.arguments),
                "status": "completed"
            })
        } else {
            namespaced_function_call_item(
                &call.id,
                if call.name.is_empty() {
                    "unknown"
                } else {
                    &call.name
                },
                &call.arguments,
                "completed",
            )
        };
        push_sse(
            output,
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": item
            }),
        );
        final_output.push(item);
    }

    push_sse(
        output,
        "response.completed",
        json!({
            "type": "response.completed",
            "response": {
                "id": state.response_id,
                "object": "response",
                "created_at": 0,
                "model": state.model,
                "status": "completed",
                "output": final_output,
                "usage": state.usage
            }
        }),
    );
    output.push_str("data: [DONE]\n\n");
}

pub(super) fn streaming_chat_sse_response(
    upstream: reqwest::Response,
    status: StatusCode,
    session_id: &str,
) -> Response<ProxyBody> {
    let upstream_stream = upstream.bytes_stream();
    let stream = futures::stream::unfold(
        (
            upstream_stream,
            ChatSseStreamConverter::new(session_id),
            false,
        ),
        |(mut upstream_stream, mut converter, done)| async move {
            if done {
                return None;
            }
            loop {
                match upstream_stream.next().await {
                    Some(Ok(chunk)) => {
                        let bytes = converter.push_chunk(&chunk);
                        if bytes.is_empty() {
                            continue;
                        }
                        return Some((
                            Ok(Frame::data(Bytes::from(bytes))),
                            (upstream_stream, converter, false),
                        ));
                    }
                    Some(Err(error)) => {
                        append_codex_api_proxy_log(&format!(
                            "upstream_chat_sse_read_error error={error}"
                        ));
                        let bytes = converter.finish();
                        if bytes.is_empty() {
                            return None;
                        }
                        return Some((
                            Ok(Frame::data(Bytes::from(bytes))),
                            (upstream_stream, converter, true),
                        ));
                    }
                    None => {
                        let bytes = converter.finish();
                        if bytes.is_empty() {
                            return None;
                        }
                        return Some((
                            Ok(Frame::data(Bytes::from(bytes))),
                            (upstream_stream, converter, true),
                        ));
                    }
                }
            }
        },
    );
    let mut response = Response::new(BodyExt::boxed(StreamBody::new(stream)));
    *response.status_mut() = status;
    if let Ok(value) = "text/event-stream".parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::drain_utf8_complete;

    /// 3-byte CJK char split across two chunks must reassemble cleanly
    /// (no U+FFFD `` from a lossy per-chunk decode).
    #[test]
    fn split_multibyte_char_reassembles_without_replacement() {
        let bytes = "中".as_bytes();
        assert_eq!(bytes.len(), 3);
        let mut pending = Vec::new();
        // First chunk: only the leading byte arrives — nothing decodable yet.
        let head = drain_utf8_complete(&mut pending, &bytes[..1]);
        assert_eq!(head, "");
        assert_eq!(pending.len(), 1);
        // Second chunk: remaining bytes complete the char.
        let tail = drain_utf8_complete(&mut pending, &bytes[1..]);
        assert_eq!(tail, "中");
        assert!(pending.is_empty());
    }

    /// 4-byte emoji split 2+2 must also reassemble cleanly.
    #[test]
    fn split_four_byte_char_reassembles_without_replacement() {
        let bytes = "😀".as_bytes();
        assert_eq!(bytes.len(), 4);
        let mut pending = Vec::new();
        let head = drain_utf8_complete(&mut pending, &bytes[..2]);
        assert_eq!(head, "");
        assert_eq!(pending.len(), 2);
        let tail = drain_utf8_complete(&mut pending, &bytes[2..]);
        assert_eq!(tail, "😀");
        assert!(pending.is_empty());
    }

    /// ASCII prefix + truncated multi-byte tail: only the ASCII part decodes,
    /// the tail stays pending for the next chunk.
    #[test]
    fn partial_tail_stays_pending() {
        let mut input = b"hello".to_vec();
        input.extend_from_slice("中".as_bytes());
        let split = input.len() - 1; // cut the last byte of 中
        let mut pending = Vec::new();
        let out = drain_utf8_complete(&mut pending, &input[..split]);
        assert_eq!(out, "hello");
        assert_eq!(pending.len(), 2);
        let out2 = drain_utf8_complete(&mut pending, &input[split..]);
        assert_eq!(out2, "中");
    }
}

pub(super) fn streaming_chat_sse_to_anthropic_response(
    upstream: reqwest::Response,
    status: StatusCode,
    session_id: &str,
) -> Response<ProxyBody> {
    let upstream_stream = upstream.bytes_stream();
    let stream = futures::stream::unfold(
        (
            upstream_stream,
            ChatAnthropicSseStreamConverter::new(session_id),
            false,
        ),
        |(mut upstream_stream, mut converter, done)| async move {
            if done {
                return None;
            }
            loop {
                match upstream_stream.next().await {
                    Some(Ok(chunk)) => {
                        let bytes = converter.push_chunk(&chunk);
                        if bytes.is_empty() {
                            continue;
                        }
                        return Some((
                            Ok(Frame::data(Bytes::from(bytes))),
                            (upstream_stream, converter, false),
                        ));
                    }
                    Some(Err(error)) => {
                        append_codex_api_proxy_log(&format!(
                            "upstream_anthropic_sse_read_error error={error}"
                        ));
                        let bytes = converter.finish();
                        if bytes.is_empty() {
                            return None;
                        }
                        return Some((
                            Ok(Frame::data(Bytes::from(bytes))),
                            (upstream_stream, converter, true),
                        ));
                    }
                    None => {
                        let bytes = converter.finish();
                        if bytes.is_empty() {
                            return None;
                        }
                        return Some((
                            Ok(Frame::data(Bytes::from(bytes))),
                            (upstream_stream, converter, true),
                        ));
                    }
                }
            }
        },
    );
    let mut response = Response::new(BodyExt::boxed(StreamBody::new(stream)));
    *response.status_mut() = status;
    if let Ok(value) = "text/event-stream".parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    response
}

pub(super) fn streaming_passthrough_response(
    upstream: reqwest::Response,
    status: StatusCode,
    content_type: &str,
) -> Response<ProxyBody> {
    let upstream_stream = upstream.bytes_stream().map(|chunk| {
        Ok(Frame::data(chunk.unwrap_or_else(|error| {
            Bytes::from(format!("event: error\ndata: {error}\n\n"))
        })))
    });
    let body = BodyExt::boxed(StreamBody::new(upstream_stream));
    let mut response = Response::new(body);
    *response.status_mut() = status;
    if let Ok(value) = content_type.parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    response
}

/// Decode as much of `pending` (with `chunk` appended) as is valid UTF-8,
/// leaving any incomplete trailing multi-byte sequence in `pending` for the
/// next call. Upstream SSE bytes can split a multi-byte UTF-8 character
/// across two HTTP chunks; decoding each chunk with `from_utf8_lossy` would
/// replace the half-character with U+FFFD (``) and produce mojibake.
fn drain_utf8_complete(pending: &mut Vec<u8>, chunk: &[u8]) -> String {
    pending.extend_from_slice(chunk);
    let mut split = pending.len();
    if split > 0 {
        while split > 0 && std::str::from_utf8(&pending[..split]).is_err() {
            split -= 1;
            // A truncated tail can be at most 3 bytes for UTF-8 (max 4-byte
            // sequence). If we had to step back further the head is genuinely
            // invalid — fall back to a lossy decode of everything so the
            // converter still makes progress.
            if pending.len() - split > 3 {
                split = pending.len();
                break;
            }
        }
    }
    let head: Vec<u8> = pending.drain(..split).collect();
    String::from_utf8_lossy(&head).into_owned()
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(super) struct TimiaiResponsesSseSanitizer {
    pending: Vec<u8>,
    buffer: String,
    removed_reasoning_events: usize,
}

#[cfg(test)]
impl TimiaiResponsesSseSanitizer {
    pub(super) fn push_chunk(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buffer.push_str(&drain_utf8_complete(&mut self.pending, chunk));
        let mut output = String::new();
        while let Some((event, consumed)) = next_sse_event(&self.buffer) {
            self.buffer.drain(..consumed);
            if let Some(event) = sanitize_timiai_responses_sse_event(&event) {
                output.push_str(&event);
                output.push_str("\n\n");
            } else {
                self.removed_reasoning_events += 1;
            }
        }
        output.into_bytes()
    }

    pub(super) fn finish(&mut self) -> Vec<u8> {
        // Flush any bytes still pending in the UTF-8 boundary buffer.
        let flushed = drain_utf8_complete(&mut self.pending, &[]);
        if !flushed.is_empty() {
            self.buffer.push_str(&flushed);
        }
        let trailing = std::mem::take(&mut self.buffer);
        let mut output = String::new();
        if !trailing.trim().is_empty() {
            if let Some(event) = sanitize_timiai_responses_sse_event(&trailing) {
                output.push_str(&event);
                output.push_str("\n\n");
            } else {
                self.removed_reasoning_events += 1;
            }
        }
        if self.removed_reasoning_events > 0 {
            append_codex_api_proxy_log(&format!(
                "timiai_responses_sse_sanitized removed_reasoning_events={}",
                self.removed_reasoning_events
            ));
        }
        output.into_bytes()
    }
}

#[cfg(test)]
fn sanitize_timiai_responses_sse_event(event: &str) -> Option<String> {
    let event_name = sse_event_name(event);
    let data = sse_data_line(event);

    if event_name.is_some_and(|name| name.contains("reasoning")) {
        return None;
    }
    if data.is_some_and(|value| value.trim() == "[DONE]") {
        return Some(event.to_string());
    }
    let Some(event_name) = event_name else {
        return Some(event.to_string());
    };

    let Some(data) = data else {
        return Some(event.to_string());
    };
    let Ok(mut value) = serde_json::from_str::<Value>(data.trim()) else {
        return Some(event.to_string());
    };

    if responses_event_is_reasoning(&value) {
        return None;
    }
    sanitize_responses_value(&mut value);

    let mut output = String::new();
    push_sse(&mut output, event_name, value);
    Some(output.trim_end().to_string())
}

#[cfg(test)]
fn sse_event_name(event: &str) -> Option<&str> {
    event
        .lines()
        .find_map(|line| line.strip_prefix("event:").map(str::trim))
}

#[cfg(test)]
fn responses_event_is_reasoning(value: &Value) -> bool {
    if value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|event_type| event_type.contains("reasoning"))
    {
        return true;
    }

    value
        .get("item")
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        == Some("reasoning")
}

#[cfg(test)]
fn sanitize_responses_value(value: &mut Value) {
    remove_reasoning_output_items(value);
    normalize_responses_usage_fields(value);
    if let Some(response) = value.get_mut("response") {
        remove_reasoning_output_items(response);
        normalize_responses_usage_fields(response);
    }
}

#[cfg(test)]
fn normalize_responses_usage_fields(value: &mut Value) {
    let Some(usage) = value.get_mut("usage") else {
        return;
    };
    let reported_input = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let total = usage
        .get("total_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(reported_input + output);
    let cached_tokens = usage_cached_input_tokens(usage).unwrap_or(0);
    let reasoning_tokens = usage_reasoning_output_tokens(usage).unwrap_or(0);

    let Some(usage) = usage.as_object_mut() else {
        return;
    };
    // `input_tokens`/`prompt_tokens` here already includes the cache-hit
    // prefix, matching codex's cache-inclusive `TokenUsage.input_tokens`;
    // keep it as-is (see `normalized_chat_usage`).
    usage.insert("input_tokens".to_string(), json!(reported_input));
    usage.insert("output_tokens".to_string(), json!(output));
    usage.insert("total_tokens".to_string(), json!(total));
    upsert_usage_detail_i64(
        usage,
        "input_tokens_details",
        "cached_tokens",
        cached_tokens,
    );
    upsert_usage_detail_i64(
        usage,
        "output_tokens_details",
        "reasoning_tokens",
        reasoning_tokens,
    );
}

#[cfg(test)]
fn upsert_usage_detail_i64(
    usage: &mut Map<String, Value>,
    object_field: &str,
    field: &str,
    value: i64,
) {
    let needs_object = usage
        .get(object_field)
        .is_none_or(|existing| !existing.is_object());
    if needs_object {
        usage.insert(object_field.to_string(), json!({}));
    }
    if let Some(details) = usage.get_mut(object_field).and_then(Value::as_object_mut) {
        details.insert(field.to_string(), json!(value));
    }
}

#[cfg(test)]
fn remove_reasoning_output_items(value: &mut Value) {
    let Some(output) = value.get_mut("output").and_then(Value::as_array_mut) else {
        return;
    };
    output.retain(|item| item.get("type").and_then(Value::as_str) != Some("reasoning"));
}

#[derive(Debug)]
pub(super) struct ChatSseStreamConverter {
    pending: Vec<u8>,
    buffer: String,
    state: ChatStreamState,
}

impl ChatSseStreamConverter {
    pub(super) fn new(session_id: &str) -> Self {
        Self {
            pending: Vec::new(),
            buffer: String::new(),
            state: ChatStreamState {
                response_id: "resp_proxy".to_string(),
                model: CHAT_MODEL_FALLBACK.to_string(),
                usage: normalized_chat_usage(None),
                session_id: session_id.to_string(),
                ..Default::default()
            },
        }
    }

    pub(super) fn push_chunk(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buffer.push_str(&drain_utf8_complete(&mut self.pending, chunk));
        let mut output = String::new();
        while let Some((event, consumed)) = next_sse_event(&self.buffer) {
            self.buffer.drain(..consumed);
            process_chat_sse_event(&event, &mut output, &mut self.state);
        }
        output.into_bytes()
    }

    pub(super) fn finish(&mut self) -> Vec<u8> {
        let flushed = drain_utf8_complete(&mut self.pending, &[]);
        if !flushed.is_empty() {
            self.buffer.push_str(&flushed);
        }
        let trailing = std::mem::take(&mut self.buffer);
        let mut output = String::new();
        if !trailing.trim().is_empty() {
            process_chat_sse_event(&trailing, &mut output, &mut self.state);
        }
        emit_stream_done(&mut output, &mut self.state);
        output.into_bytes()
    }
}

#[cfg(test)]
pub(super) fn chat_sse_to_anthropic_sse(body: &[u8]) -> Vec<u8> {
    let mut converter = ChatAnthropicSseStreamConverter::new("test-session");
    let mut output = converter.push_chunk(body);
    output.extend(converter.finish());
    output
}

#[derive(Debug)]
struct ChatAnthropicSseStreamConverter {
    pending: Vec<u8>,
    buffer: String,
    state: ChatStreamState,
}

impl ChatAnthropicSseStreamConverter {
    pub(super) fn new(session_id: &str) -> Self {
        Self {
            pending: Vec::new(),
            buffer: String::new(),
            state: ChatStreamState {
                response_id: "msg_proxy".to_string(),
                model: CHAT_MODEL_FALLBACK.to_string(),
                usage: normalized_chat_usage(None),
                session_id: session_id.to_string(),
                ..Default::default()
            },
        }
    }

    pub(super) fn push_chunk(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buffer.push_str(&drain_utf8_complete(&mut self.pending, chunk));
        let mut output = String::new();
        while let Some((event, consumed)) = next_sse_event(&self.buffer) {
            self.buffer.drain(..consumed);
            process_chat_sse_anthropic_event(&event, &mut output, &mut self.state);
        }
        output.into_bytes()
    }

    pub(super) fn finish(&mut self) -> Vec<u8> {
        let flushed = drain_utf8_complete(&mut self.pending, &[]);
        if !flushed.is_empty() {
            self.buffer.push_str(&flushed);
        }
        let trailing = std::mem::take(&mut self.buffer);
        let mut output = String::new();
        if !trailing.trim().is_empty() {
            process_chat_sse_anthropic_event(&trailing, &mut output, &mut self.state);
        }
        emit_anthropic_stream_done(&mut output, &mut self.state);
        output.into_bytes()
    }
}

fn process_chat_sse_anthropic_event(event: &str, output: &mut String, state: &mut ChatStreamState) {
    let Some(data) = sse_data_line(event) else {
        return;
    };
    if data.trim() == "[DONE]" {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
        append_codex_api_proxy_log("failed_to_parse_chat_anthropic_sse_event");
        return;
    };
    if let Some(id) = value.get("id").and_then(Value::as_str) {
        state.response_id = id.to_string();
    }
    if let Some(model) = value.get("model").and_then(Value::as_str) {
        state.model = model.to_string();
    }
    if let Some(usage) = value.get("usage") {
        state.usage = normalized_chat_usage(Some(usage));
    }

    let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    else {
        return;
    };
    if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str) {
        state.stop_reason = Some(chat_finish_reason_to_anthropic(finish_reason).to_string());
    }
    let delta = choice.get("delta").unwrap_or(&Value::Null);
    if let Some(reasoning_content) = delta.get("reasoning_content").and_then(Value::as_str) {
        state.reasoning_content.push_str(reasoning_content);
    }
    if let Some(content) = delta.get("content").and_then(Value::as_str) {
        emit_anthropic_text_delta(output, state, content);
    }
    if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            emit_anthropic_tool_call_delta(output, state, tool_call);
        }
    }
}

fn ensure_anthropic_message_started(output: &mut String, state: &mut ChatStreamState) {
    if state.message_started {
        return;
    }
    state.message_started = true;
    push_sse(
        output,
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": state.response_id,
                "type": "message",
                "role": "assistant",
                "model": state.model,
                "content": [],
                "stop_reason": Value::Null,
                "stop_sequence": Value::Null,
                "usage": { "input_tokens": 0, "output_tokens": 0 }
            }
        }),
    );
}

fn emit_anthropic_text_delta(output: &mut String, state: &mut ChatStreamState, delta: &str) {
    if delta.is_empty() {
        return;
    }

    ensure_anthropic_message_started(output, state);
    let index = if let Some(index) = state.text_block_index {
        index
    } else {
        let index = state.next_content_block_index;
        state.next_content_block_index += 1;
        state.text_block_index = Some(index);
        state.text_block_started = true;
        push_sse(
            output,
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": { "type": "text", "text": "" }
            }),
        );
        index
    };
    state.text.push_str(delta);
    push_sse(
        output,
        "content_block_delta",
        json!({
            "type": "content_block_delta",
            "index": index,
            "delta": { "type": "text_delta", "text": delta }
        }),
    );
}

fn emit_anthropic_tool_call_delta(
    output: &mut String,
    state: &mut ChatStreamState,
    tool_call: &Value,
) {
    ensure_anthropic_message_started(output, state);
    let index = tool_call
        .get("index")
        .and_then(Value::as_u64)
        .unwrap_or(state.tool_calls.len() as u64) as usize;
    while state.tool_calls.len() <= index {
        state.tool_calls.push(StreamToolCall::default());
    }
    let call = &mut state.tool_calls[index];
    if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
        call.id = id.to_string();
    }
    if call.id.is_empty() {
        call.id = next_synthetic_tool_call_id();
    }
    let function = tool_call.get("function").unwrap_or(&Value::Null);
    if let Some(name) = function.get("name").and_then(Value::as_str) {
        call.name.push_str(name);
    }
    if !call.added && !call.name.is_empty() {
        call.added = true;
        let block_index = state.next_content_block_index;
        state.next_content_block_index += 1;
        call.content_block_index = Some(block_index);
        push_sse(
            output,
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": block_index,
                "content_block": {
                    "type": "tool_use",
                    "id": call.id,
                    "name": call.name,
                    "input": {}
                }
            }),
        );
        if !call.arguments.is_empty() {
            push_sse(
                output,
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": block_index,
                    "delta": { "type": "input_json_delta", "partial_json": call.arguments }
                }),
            );
        }
    }
    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
        call.arguments.push_str(arguments);
        if let Some(block_index) = call.content_block_index {
            push_sse(
                output,
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": block_index,
                    "delta": { "type": "input_json_delta", "partial_json": arguments }
                }),
            );
        }
    }
}

fn emit_anthropic_stream_done(output: &mut String, state: &mut ChatStreamState) {
    log_chat_stream_done("anthropic", state);

    ensure_anthropic_message_started(output, state);
    if state.text_block_started {
        if let Some(index) = state.text_block_index {
            push_sse(
                output,
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": index }),
            );
        }
    }
    remember_stream_reasoning(state);
    for call in &state.tool_calls {
        if let Some(index) = call.content_block_index {
            push_sse(
                output,
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": index }),
            );
        }
    }
    push_sse(
        output,
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": state
                    .stop_reason
                    .as_deref()
                    .unwrap_or(if state.tool_calls.iter().any(|call| call.added) {
                        "tool_use"
                    } else {
                        "end_turn"
                    }),
                "stop_sequence": Value::Null
            },
            "usage": state.usage
        }),
    );
    push_sse(output, "message_stop", json!({ "type": "message_stop" }));
}

fn chat_finish_reason_to_anthropic(reason: &str) -> &str {
    match reason {
        "tool_calls" => "tool_use",
        "length" => "max_tokens",
        _ => "end_turn",
    }
}

fn log_chat_stream_done(kind: &str, state: &ChatStreamState) {
    append_codex_api_proxy_log(&format!(
        "chat_sse_stream_done kind={kind} stop_reason={} text_chars={} reasoning_chars={} tool_calls={} message_started={} text_block_started={}",
        state.stop_reason.as_deref().unwrap_or("<missing>"),
        state.text.len(),
        state.reasoning_content.len(),
        state.tool_calls.iter().filter(|call| call.added).count(),
        state.message_started,
        state.text_block_started,
    ));
}

fn next_sse_event(buffer: &str) -> Option<(String, usize)> {
    if let Some(index) = buffer.find("\r\n\r\n") {
        return Some((buffer[..index].to_string(), index + 4));
    }
    if let Some(index) = buffer.find("\n\n") {
        return Some((buffer[..index].to_string(), index + 2));
    }
    None
}

/// Extract the `event:` line value from an SSE event block. Unlike the
/// chat-completions upstream (which only emits `data:` lines), the Anthropic
/// and Responses SSE streams carry the event name on a dedicated `event:` line.
fn sse_event_name_line(event: &str) -> Option<&str> {
    event
        .lines()
        .find_map(|line| line.strip_prefix("event:").map(str::trim))
}

// ---------------------------------------------------------------------------
// Anthropic SSE → Responses SSE (streaming)
// ---------------------------------------------------------------------------

/// Merge a `message_start.message.usage` baseline with a later
/// `message_delta.usage`. True Anthropic streams put `input_tokens` and the
/// cache fields in `message_start` and only `output_tokens` in
/// `message_delta`; proxies that repeat the full usage in `message_delta`
/// must win. Field-level merge: the delta overrides the baseline for any
/// present, non-null key; keys only in the baseline are preserved.
fn merge_anthropic_usage(start: &Value, delta: &Value) -> Value {
    let mut merged = start.clone();
    if let (Some(merged_obj), Some(delta_obj)) = (merged.as_object_mut(), delta.as_object()) {
        for (key, value) in delta_obj {
            if !value.is_null() {
                merged_obj.insert(key.clone(), value.clone());
            }
        }
    }
    merged
}

#[derive(Debug, Default)]
struct AnthropicToResponsesState {
    response_id: String,
    model: String,
    text: String,
    reasoning_content: String,
    message_started: bool,
    text_part_added: bool,
    /// content_block index → our tool call state. Keyed by the Anthropic
    /// `content_block_start` index so deltas map back to the right call.
    tool_calls: BTreeMap<usize, StreamToolCall>,
    /// Order of tool_call content_block indices, to compute Responses
    /// `output_index` relative to the message item.
    tool_call_order: Vec<usize>,
    stop_reason: Option<String>,
    usage: Value,
    /// Raw `message_start.message.usage` baseline. True Anthropic streams put
    /// `input_tokens` / cache fields here and only `output_tokens` in the
    /// later `message_delta`; without preserving it the input would be lost
    /// (the gauge would read 0). Merged field-by-field with `message_delta`.
    start_usage: Option<Value>,
    /// content_block_index of a finalized tool_use block (after content_block_stop).
    finalized_tool_blocks: BTreeSet<usize>,
    /// Session id this stream belongs to, used to partition the
    /// reasoning-history cache so concurrent sessions cannot cross-talk.
    session_id: String,
}

impl AnthropicToResponsesState {
    fn tool_output_index(&self, block_index: usize) -> usize {
        // Message item sits at output_index 0; tool calls follow at +1, +2, ...
        let position = self
            .tool_call_order
            .iter()
            .position(|idx| *idx == block_index)
            .unwrap_or(0);
        if self.message_started {
            position + 1
        } else {
            position
        }
    }
}

#[derive(Debug)]
pub(super) struct AnthropicSseToResponsesConverter {
    pending: Vec<u8>,
    buffer: String,
    state: AnthropicToResponsesState,
}

impl AnthropicSseToResponsesConverter {
    pub(super) fn new(session_id: &str) -> Self {
        Self {
            pending: Vec::new(),
            buffer: String::new(),
            state: AnthropicToResponsesState {
                response_id: "resp_proxy".to_string(),
                model: CHAT_MODEL_FALLBACK.to_string(),
                usage: normalized_chat_usage(None),
                session_id: session_id.to_string(),
                ..Default::default()
            },
        }
    }

    pub(super) fn push_chunk(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buffer.push_str(&drain_utf8_complete(&mut self.pending, chunk));
        let mut output = String::new();
        while let Some((event, consumed)) = next_sse_event(&self.buffer) {
            self.buffer.drain(..consumed);
            process_anthropic_to_responses_event(&event, &mut output, &mut self.state);
        }
        output.into_bytes()
    }

    pub(super) fn finish(&mut self) -> Vec<u8> {
        let flushed = drain_utf8_complete(&mut self.pending, &[]);
        if !flushed.is_empty() {
            self.buffer.push_str(&flushed);
        }
        let trailing = std::mem::take(&mut self.buffer);
        let mut output = String::new();
        if !trailing.trim().is_empty() {
            process_anthropic_to_responses_event(&trailing, &mut output, &mut self.state);
        }
        emit_anthropic_to_responses_done(&mut output, &mut self.state);
        output.into_bytes()
    }
}

fn process_anthropic_to_responses_event(
    event: &str,
    output: &mut String,
    state: &mut AnthropicToResponsesState,
) {
    let Some(data) = sse_data_line(event) else {
        return;
    };
    if data.trim() == "[DONE]" {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
        append_codex_api_proxy_log("failed_to_parse_anthropic_sse_event");
        return;
    };
    let event_name = sse_event_name_line(event).unwrap_or_else(|| {
        value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    });
    match event_name {
        "message_start" => {
            if let Some(id) = value
                .get("message")
                .and_then(|m| m.get("id"))
                .and_then(Value::as_str)
            {
                state.response_id = id.to_string();
            }
            if let Some(model) = value
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
            {
                state.model = model.to_string();
            }
            // Anthropic streams carry `input_tokens` and the cache fields in
            // `message_start.message.usage`; `message_delta.usage` only adds
            // the final `output_tokens`. Capture the baseline here so the
            // later delta merges onto it instead of replacing it (which would
            // drop input and zero the context-occupancy gauge).
            if let Some(usage) = value.get("message").and_then(|m| m.get("usage")) {
                state.start_usage = Some(usage.clone());
                state.usage = normalized_anthropic_usage(Some(usage));
            }
        }
        "content_block_start" => {
            let Some(index) = value.get("index").and_then(Value::as_u64) else {
                return;
            };
            let index = index as usize;
            let block_type = value
                .get("content_block")
                .and_then(|b| b.get("type"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            match block_type {
                "text" => {
                    if !state.message_started {
                        state.message_started = true;
                        push_sse(
                            output,
                            "response.output_item.added",
                            json!({
                                "type": "response.output_item.added",
                                "output_index": 0,
                                "item": {
                                    "id": "msg_proxy",
                                    "type": "message",
                                    "role": "assistant",
                                    "status": "in_progress",
                                    "content": []
                                }
                            }),
                        );
                        push_sse(
                            output,
                            "response.content_part.added",
                            json!({
                                "type": "response.content_part.added",
                                "output_index": 0,
                                "content_index": 0,
                                "item_id": "msg_proxy",
                                "part": { "type": "output_text", "text": "" }
                            }),
                        );
                        state.text_part_added = true;
                    } else if !state.text_part_added {
                        push_sse(
                            output,
                            "response.content_part.added",
                            json!({
                                "type": "response.content_part.added",
                                "output_index": 0,
                                "content_index": 0,
                                "item_id": "msg_proxy",
                                "part": { "type": "output_text", "text": "" }
                            }),
                        );
                        state.text_part_added = true;
                    }
                }
                "tool_use" => {
                    if !state.message_started {
                        state.message_started = true;
                        push_sse(
                            output,
                            "response.output_item.added",
                            json!({
                                "type": "response.output_item.added",
                                "output_index": 0,
                                "item": {
                                    "id": "msg_proxy",
                                    "type": "message",
                                    "role": "assistant",
                                    "status": "in_progress",
                                    "content": []
                                }
                            }),
                        );
                    }
                    let id = value
                        .get("content_block")
                        .and_then(|b| b.get("id"))
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(next_synthetic_tool_call_id);
                    let name = value
                        .get("content_block")
                        .and_then(|b| b.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string();
                    let call = StreamToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: String::new(),
                        added: true,
                        content_block_index: Some(index),
                    };
                    state.tool_calls.insert(index, call);
                    state.tool_call_order.push(index);
                    let output_index = state.tool_output_index(index);
                    if tool_call_outputs_as_apply_patch(&name) {
                        push_sse(
                            output,
                            "response.output_item.added",
                            json!({
                                "type": "response.output_item.added",
                                "output_index": output_index,
                                "item": {
                                    "id": id,
                                    "type": "custom_tool_call",
                                    "call_id": id,
                                    "name": "apply_patch",
                                    "input": "",
                                    "status": "in_progress"
                                }
                            }),
                        );
                    } else {
                        let item = namespaced_function_call_item(&id, &name, "", "in_progress");
                        push_sse(
                            output,
                            "response.output_item.added",
                            json!({
                                "type": "response.output_item.added",
                                "output_index": output_index,
                                "item": item
                            }),
                        );
                    }
                }
                _ => {}
            }
        }
        "content_block_delta" => {
            let Some(index) = value.get("index").and_then(Value::as_u64) else {
                return;
            };
            let index = index as usize;
            let delta = value.get("delta").unwrap_or(&Value::Null);
            let delta_type = delta
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match delta_type {
                "text_delta" => {
                    if let Some(text) = delta.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            state.text.push_str(text);
                            push_sse(
                                output,
                                "response.output_text.delta",
                                json!({
                                    "type": "response.output_text.delta",
                                    "output_index": 0,
                                    "content_index": 0,
                                    "item_id": "msg_proxy",
                                    "delta": text
                                }),
                            );
                        }
                    }
                }
                "input_json_delta" => {
                    if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                        let output_index = state.tool_output_index(index);
                        if let Some(call) = state.tool_calls.get_mut(&index) {
                            call.arguments.push_str(partial);
                            if !tool_call_outputs_as_apply_patch(&call.name) {
                                push_sse(
                                    output,
                                    "response.function_call_arguments.delta",
                                    json!({
                                        "type": "response.function_call_arguments.delta",
                                        "output_index": output_index,
                                        "item_id": call.id,
                                        "delta": partial
                                    }),
                                );
                            }
                        }
                    }
                }
                "thinking_delta" => {
                    if let Some(thinking) = delta.get("thinking").and_then(Value::as_str) {
                        state.reasoning_content.push_str(thinking);
                    }
                }
                _ => {}
            }
        }
        "content_block_stop" => {
            let Some(index) = value.get("index").and_then(Value::as_u64) else {
                return;
            };
            let index = index as usize;
            // Only finalize tool_use blocks; text block is finalized at stream end.
            if state.tool_calls.contains_key(&index) {
                state.finalized_tool_blocks.insert(index);
                let call = state.tool_calls.get(&index).cloned();
                if let Some(call) = call {
                    let output_index = state.tool_output_index(index);
                    if tool_call_outputs_as_apply_patch(&call.name) {
                        push_sse(
                            output,
                            "response.output_item.done",
                            json!({
                                "type": "response.output_item.done",
                                "output_index": output_index,
                                "item": {
                                    "id": call.id,
                                    "type": "custom_tool_call",
                                    "call_id": call.id,
                                    "name": "apply_patch",
                                    "input": apply_patch_input_for_tool_call(&call.name, &call.arguments),
                                    "status": "completed"
                                }
                            }),
                        );
                    } else if !call.arguments.is_empty() {
                        push_sse(
                            output,
                            "response.function_call_arguments.done",
                            json!({
                                "type": "response.function_call_arguments.done",
                                "output_index": output_index,
                                "item_id": call.id,
                                "arguments": call.arguments
                            }),
                        );
                        let item = namespaced_function_call_item(
                            &call.id,
                            if call.name.is_empty() {
                                "unknown"
                            } else {
                                &call.name
                            },
                            &call.arguments,
                            "completed",
                        );
                        push_sse(
                            output,
                            "response.output_item.done",
                            json!({
                                "type": "response.output_item.done",
                                "output_index": output_index,
                                "item": item
                            }),
                        );
                    } else {
                        let item = namespaced_function_call_item(
                            &call.id,
                            if call.name.is_empty() {
                                "unknown"
                            } else {
                                &call.name
                            },
                            "",
                            "completed",
                        );
                        push_sse(
                            output,
                            "response.output_item.done",
                            json!({
                                "type": "response.output_item.done",
                                "output_index": output_index,
                                "item": item
                            }),
                        );
                    }
                }
            }
        }
        "message_delta" => {
            if let Some(stop_reason) = value
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(Value::as_str)
            {
                state.stop_reason = Some(stop_reason.to_string());
            }
            if let Some(usage) = value.get("usage") {
                // Merge onto the `message_start` baseline (field-level; the
                // delta wins for present non-null keys) so true-Anthropic
                // streams keep their `message_start` input/cache while proxies
                // that repeat the full usage in `message_delta` still override.
                let merged = match &state.start_usage {
                    Some(start) => merge_anthropic_usage(start, usage),
                    None => usage.clone(),
                };
                state.usage = normalized_anthropic_usage(Some(&merged));
            }
        }
        "message_stop" => {
            // Finalization handled in finish()/emit_anthropic_to_responses_done.
        }
        _ => {}
    }
}

fn emit_anthropic_to_responses_done(output: &mut String, state: &mut AnthropicToResponsesState) {
    append_codex_api_proxy_log(&format!(
        "anthropic_to_responses_stream_done stop_reason={} text_chars={} tool_calls={}",
        state.stop_reason.as_deref().unwrap_or("<missing>"),
        state.text.len(),
        state.tool_calls.len(),
    ));

    let mut final_output = Vec::new();
    if state.message_started {
        push_sse(
            output,
            "response.output_text.done",
            json!({
                "type": "response.output_text.done",
                "output_index": 0,
                "content_index": 0,
                "item_id": "msg_proxy",
                "text": state.text
            }),
        );
        push_sse(
            output,
            "response.content_part.done",
            json!({
                "type": "response.content_part.done",
                "output_index": 0,
                "content_index": 0,
                "item_id": "msg_proxy",
                "part": { "type": "output_text", "text": state.text }
            }),
        );
        let item =
            response_message_item_with_reasoning(&state.text, Some(&state.reasoning_content));
        remember_reasoning_content(&state.session_id, &state.text, &state.reasoning_content);
        push_sse(
            output,
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": item
            }),
        );
        final_output.push(item);
    }

    // Tool calls that never received a content_block_stop (defensive).
    for &block_index in &state.tool_call_order.clone() {
        if state.finalized_tool_blocks.contains(&block_index) {
            continue;
        }
        let Some(call) = state.tool_calls.get(&block_index).cloned() else {
            continue;
        };
        let output_index = state.tool_output_index(block_index);
        if tool_call_outputs_as_apply_patch(&call.name) {
            push_sse(
                output,
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "output_index": output_index,
                    "item": {
                        "id": call.id,
                        "type": "custom_tool_call",
                        "call_id": call.id,
                        "name": "apply_patch",
                        "input": apply_patch_input_for_tool_call(&call.name, &call.arguments),
                        "status": "completed"
                    }
                }),
            );
            final_output.push(json!({
                "id": call.id,
                "type": "custom_tool_call",
                "call_id": call.id,
                "name": "apply_patch",
                "input": apply_patch_input_for_tool_call(&call.name, &call.arguments),
                "status": "completed"
            }));
        } else {
            if !call.arguments.is_empty() {
                push_sse(
                    output,
                    "response.function_call_arguments.done",
                    json!({
                        "type": "response.function_call_arguments.done",
                        "output_index": output_index,
                        "item_id": call.id,
                        "arguments": call.arguments
                    }),
                );
            }
            let item = namespaced_function_call_item(
                &call.id,
                if call.name.is_empty() {
                    "unknown"
                } else {
                    &call.name
                },
                &call.arguments,
                "completed",
            );
            push_sse(
                output,
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "output_index": output_index,
                    "item": item
                }),
            );
            final_output.push(item);
        }
    }

    push_sse(
        output,
        "response.completed",
        json!({
            "type": "response.completed",
            "response": {
                "id": state.response_id,
                "object": "response",
                "created_at": 0,
                "model": state.model,
                "status": "completed",
                "output": final_output,
                "usage": state.usage
            }
        }),
    );
    output.push_str("data: [DONE]\n\n");
}

#[cfg(test)]
pub(super) fn anthropic_sse_to_responses_sse(body: &[u8]) -> Vec<u8> {
    let mut converter = AnthropicSseToResponsesConverter::new("test-session");
    let mut output = converter.push_chunk(body);
    output.extend(converter.finish());
    output
}

pub(super) fn streaming_anthropic_sse_to_responses_response(
    upstream: reqwest::Response,
    status: StatusCode,
    session_id: &str,
) -> Response<ProxyBody> {
    let upstream_stream = upstream.bytes_stream();
    let stream = futures::stream::unfold(
        (
            upstream_stream,
            AnthropicSseToResponsesConverter::new(session_id),
            false,
        ),
        |(mut upstream_stream, mut converter, done)| async move {
            if done {
                return None;
            }
            loop {
                match upstream_stream.next().await {
                    Some(Ok(chunk)) => {
                        let bytes = converter.push_chunk(&chunk);
                        if bytes.is_empty() {
                            continue;
                        }
                        return Some((
                            Ok(Frame::data(Bytes::from(bytes))),
                            (upstream_stream, converter, false),
                        ));
                    }
                    Some(Err(error)) => {
                        append_codex_api_proxy_log(&format!(
                            "upstream_anthropic_to_responses_sse_read_error error={error}"
                        ));
                        let bytes = converter.finish();
                        if bytes.is_empty() {
                            return None;
                        }
                        return Some((
                            Ok(Frame::data(Bytes::from(bytes))),
                            (upstream_stream, converter, true),
                        ));
                    }
                    None => {
                        let bytes = converter.finish();
                        if bytes.is_empty() {
                            return None;
                        }
                        return Some((
                            Ok(Frame::data(Bytes::from(bytes))),
                            (upstream_stream, converter, true),
                        ));
                    }
                }
            }
        },
    );
    let mut response = Response::new(BodyExt::boxed(StreamBody::new(stream)));
    *response.status_mut() = status;
    if let Ok(value) = "text/event-stream".parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    response
}

// ---------------------------------------------------------------------------
// Responses SSE → Anthropic SSE (streaming)
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct ResponsesToAnthropicState {
    message_id: String,
    model: String,
    text: String,
    reasoning_content: String,
    message_started: bool,
    text_block_started: bool,
    text_block_index: Option<usize>,
    next_content_block_index: usize,
    /// output_index → tool call state.
    tool_calls: BTreeMap<usize, StreamToolCall>,
    stop_reason: Option<String>,
    usage: Value,
    /// Session id this stream belongs to, used to partition the
    /// reasoning-history cache so concurrent sessions cannot cross-talk.
    session_id: String,
}

#[derive(Debug)]
pub(super) struct ResponsesSseToAnthropicConverter {
    pending: Vec<u8>,
    buffer: String,
    state: ResponsesToAnthropicState,
}

impl ResponsesSseToAnthropicConverter {
    pub(super) fn new(session_id: &str) -> Self {
        Self {
            pending: Vec::new(),
            buffer: String::new(),
            state: ResponsesToAnthropicState {
                message_id: "msg_proxy".to_string(),
                model: CHAT_MODEL_FALLBACK.to_string(),
                usage: normalized_chat_usage(None),
                session_id: session_id.to_string(),
                ..Default::default()
            },
        }
    }

    pub(super) fn push_chunk(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buffer.push_str(&drain_utf8_complete(&mut self.pending, chunk));
        let mut output = String::new();
        while let Some((event, consumed)) = next_sse_event(&self.buffer) {
            self.buffer.drain(..consumed);
            process_responses_to_anthropic_event(&event, &mut output, &mut self.state);
        }
        output.into_bytes()
    }

    pub(super) fn finish(&mut self) -> Vec<u8> {
        let flushed = drain_utf8_complete(&mut self.pending, &[]);
        if !flushed.is_empty() {
            self.buffer.push_str(&flushed);
        }
        let trailing = std::mem::take(&mut self.buffer);
        let mut output = String::new();
        if !trailing.trim().is_empty() {
            process_responses_to_anthropic_event(&trailing, &mut output, &mut self.state);
        }
        emit_responses_to_anthropic_done(&mut output, &mut self.state);
        output.into_bytes()
    }
}

fn ensure_responses_to_anthropic_message_started(
    output: &mut String,
    state: &mut ResponsesToAnthropicState,
) {
    if state.message_started {
        return;
    }
    state.message_started = true;
    push_sse(
        output,
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": state.message_id,
                "type": "message",
                "role": "assistant",
                "model": state.model,
                "content": [],
                "stop_reason": Value::Null,
                "stop_sequence": Value::Null,
                "usage": { "input_tokens": 0, "output_tokens": 0 }
            }
        }),
    );
}

fn process_responses_to_anthropic_event(
    event: &str,
    output: &mut String,
    state: &mut ResponsesToAnthropicState,
) {
    let Some(data) = sse_data_line(event) else {
        return;
    };
    if data.trim() == "[DONE]" {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
        append_codex_api_proxy_log("failed_to_parse_responses_sse_event");
        return;
    };
    let event_name = sse_event_name_line(event).unwrap_or_else(|| {
        value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    });
    match event_name {
        "response.created" => {
            if let Some(id) = value
                .get("response")
                .and_then(|r| r.get("id"))
                .and_then(Value::as_str)
            {
                state.message_id = id.to_string();
            }
            if let Some(model) = value
                .get("response")
                .and_then(|r| r.get("model"))
                .and_then(Value::as_str)
            {
                state.model = model.to_string();
            }
        }
        "response.output_item.added" => {
            let Some(item) = value.get("item") else {
                return;
            };
            let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
            match item_type {
                "message" => {
                    // Text deltas will follow via response.output_text.delta; no
                    // content_block_start emitted yet (lazy until first delta).
                }
                "function_call" | "custom_tool_call" => {
                    ensure_responses_to_anthropic_message_started(output, state);
                    let output_index = value
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as usize;
                    let id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(next_synthetic_tool_call_id);
                    let name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string();
                    let call = StreamToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: String::new(),
                        added: true,
                        content_block_index: None,
                    };
                    state.tool_calls.insert(output_index, call);
                    let block_index = state.next_content_block_index;
                    state.next_content_block_index += 1;
                    if let Some(call) = state.tool_calls.get_mut(&output_index) {
                        call.content_block_index = Some(block_index);
                    }
                    let display_name = if tool_call_outputs_as_apply_patch(&name) {
                        "apply_patch"
                    } else {
                        name.as_str()
                    };
                    push_sse(
                        output,
                        "content_block_start",
                        json!({
                            "type": "content_block_start",
                            "index": block_index,
                            "content_block": {
                                "type": "tool_use",
                                "id": id,
                                "name": display_name,
                                "input": {}
                            }
                        }),
                    );
                }
                _ => {}
            }
        }
        "response.content_part.added" => {
            // Lazy: message_start is emitted on first text delta.
        }
        "response.output_text.delta" => {
            ensure_responses_to_anthropic_message_started(output, state);
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                if !delta.is_empty() {
                    let index = if let Some(index) = state.text_block_index {
                        index
                    } else {
                        let index = state.next_content_block_index;
                        state.next_content_block_index += 1;
                        state.text_block_index = Some(index);
                        state.text_block_started = true;
                        push_sse(
                            output,
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": index,
                                "content_block": { "type": "text", "text": "" }
                            }),
                        );
                        index
                    };
                    state.text.push_str(delta);
                    push_sse(
                        output,
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "text_delta", "text": delta }
                        }),
                    );
                }
            }
        }
        "response.function_call_arguments.delta" => {
            let Some(output_index) = value.get("output_index").and_then(Value::as_u64) else {
                return;
            };
            let output_index = output_index as usize;
            let Some(partial) = value.get("delta").and_then(Value::as_str) else {
                return;
            };
            if let Some(call) = state.tool_calls.get_mut(&output_index) {
                call.arguments.push_str(partial);
                if let Some(block_index) = call.content_block_index {
                    if !tool_call_outputs_as_apply_patch(&call.name) {
                        push_sse(
                            output,
                            "content_block_delta",
                            json!({
                                "type": "content_block_delta",
                                "index": block_index,
                                "delta": { "type": "input_json_delta", "partial_json": partial }
                            }),
                        );
                    }
                }
            }
        }
        "response.output_item.done" => {
            let Some(output_index) = value.get("output_index").and_then(Value::as_u64) else {
                return;
            };
            let output_index = output_index as usize;
            let Some(item) = value.get("item") else {
                return;
            };
            let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
            if item_type == "function_call" || item_type == "custom_tool_call" {
                if let Some(call) = state.tool_calls.get_mut(&output_index) {
                    // Capture final arguments if not already accumulated.
                    if let Some(args) = item
                        .get("arguments")
                        .or_else(|| item.get("input"))
                        .and_then(Value::as_str)
                    {
                        if call.arguments.is_empty() {
                            call.arguments = args.to_string();
                        }
                    }
                }
                if let Some(block_index) = state
                    .tool_calls
                    .get(&output_index)
                    .and_then(|c| c.content_block_index)
                {
                    push_sse(
                        output,
                        "content_block_stop",
                        json!({ "type": "content_block_stop", "index": block_index }),
                    );
                }
            }
        }
        "response.output_text.done" => {
            // Text block finalization handled at stream end.
            if let Some(text) = value.get("text").and_then(Value::as_str) {
                if state.text.is_empty() && !text.is_empty() {
                    state.text = text.to_string();
                }
            }
        }
        "response.reasoning_text.delta" => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                state.reasoning_content.push_str(delta);
            }
        }
        "response.completed" => {
            if let Some(response) = value.get("response") {
                if let Some(usage) = response.get("usage") {
                    state.usage = normalized_chat_usage(Some(usage));
                }
                if let Some(model) = response.get("model").and_then(Value::as_str) {
                    if !model.is_empty() {
                        state.model = model.to_string();
                    }
                }
                if let Some(id) = response.get("id").and_then(Value::as_str) {
                    if !id.is_empty() {
                        state.message_id = id.to_string();
                    }
                }
            }
        }
        _ => {}
    }
}

fn emit_responses_to_anthropic_done(output: &mut String, state: &mut ResponsesToAnthropicState) {
    append_codex_api_proxy_log(&format!(
        "responses_to_anthropic_stream_done stop_reason={} text_chars={} tool_calls={}",
        state.stop_reason.as_deref().unwrap_or("<missing>"),
        state.text.len(),
        state.tool_calls.len(),
    ));

    ensure_responses_to_anthropic_message_started(output, state);
    if state.text_block_started {
        if let Some(index) = state.text_block_index {
            push_sse(
                output,
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": index }),
            );
        }
    }
    remember_reasoning_content(&state.session_id, &state.text, &state.reasoning_content);
    // Finalize any tool blocks that did not receive response.output_item.done.
    for (_, call) in state.tool_calls.iter() {
        if let Some(block_index) = call.content_block_index {
            push_sse(
                output,
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": block_index }),
            );
        }
    }
    push_sse(
        output,
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": state
                    .stop_reason
                    .as_deref()
                    .unwrap_or(if state.tool_calls.iter().any(|(_, call)| call.added) {
                        "tool_use"
                    } else {
                        "end_turn"
                    }),
                "stop_sequence": Value::Null
            },
            "usage": state.usage
        }),
    );
    push_sse(output, "message_stop", json!({ "type": "message_stop" }));
}

#[cfg(test)]
pub(super) fn responses_sse_to_anthropic_sse(body: &[u8]) -> Vec<u8> {
    let mut converter = ResponsesSseToAnthropicConverter::new("test-session");
    let mut output = converter.push_chunk(body);
    output.extend(converter.finish());
    output
}

pub(super) fn streaming_responses_sse_to_anthropic_response(
    upstream: reqwest::Response,
    status: StatusCode,
    session_id: &str,
) -> Response<ProxyBody> {
    let upstream_stream = upstream.bytes_stream();
    let stream = futures::stream::unfold(
        (
            upstream_stream,
            ResponsesSseToAnthropicConverter::new(session_id),
            false,
        ),
        |(mut upstream_stream, mut converter, done)| async move {
            if done {
                return None;
            }
            loop {
                match upstream_stream.next().await {
                    Some(Ok(chunk)) => {
                        let bytes = converter.push_chunk(&chunk);
                        if bytes.is_empty() {
                            continue;
                        }
                        return Some((
                            Ok(Frame::data(Bytes::from(bytes))),
                            (upstream_stream, converter, false),
                        ));
                    }
                    Some(Err(error)) => {
                        append_codex_api_proxy_log(&format!(
                            "upstream_responses_to_anthropic_sse_read_error error={error}"
                        ));
                        let bytes = converter.finish();
                        if bytes.is_empty() {
                            return None;
                        }
                        return Some((
                            Ok(Frame::data(Bytes::from(bytes))),
                            (upstream_stream, converter, true),
                        ));
                    }
                    None => {
                        let bytes = converter.finish();
                        if bytes.is_empty() {
                            return None;
                        }
                        return Some((
                            Ok(Frame::data(Bytes::from(bytes))),
                            (upstream_stream, converter, true),
                        ));
                    }
                }
            }
        },
    );
    let mut response = Response::new(BodyExt::boxed(StreamBody::new(stream)));
    *response.status_mut() = status;
    if let Ok(value) = "text/event-stream".parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    response
}
