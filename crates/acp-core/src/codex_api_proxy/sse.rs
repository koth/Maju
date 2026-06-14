use super::*;

#[cfg(test)]
use serde_json::Map;

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
    remember_assistant_reasoning(&state.text, &tool_calls, &state.reasoning_content);
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
    message_started: bool,
    text_block_started: bool,
    text_block_index: Option<usize>,
    next_content_block_index: usize,
    stop_reason: Option<String>,
    tool_calls: Vec<StreamToolCall>,
    usage: Value,
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
    let mut converter = ChatSseStreamConverter::new();
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

fn emit_text_delta(output: &mut String, state: &mut ChatStreamState, delta: &str) {
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
        call.id = format!("call_proxy_{index}");
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
        if call.name == "apply_patch" {
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
                        "name": call.name,
                        "input": "",
                        "status": "in_progress"
                    }
                }),
            );
        } else {
            push_sse(
                output,
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": {
                        "id": call.id,
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": "",
                        "status": "in_progress"
                    }
                }),
            );
        }
    }
    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
        call.arguments.push_str(arguments);
        if call.name != "apply_patch" {
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
        remember_reasoning_content(&state.text, &state.reasoning_content);
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
        if !call.arguments.is_empty() && call.name != "apply_patch" {
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
        let item = if call.name == "apply_patch" {
            json!({
                "id": call.id,
                "type": "custom_tool_call",
                "call_id": call.id,
                "name": &call.name,
                "input": apply_patch_input_from_function_arguments(&call.arguments),
                "status": "completed"
            })
        } else {
            json!({
                "id": call.id,
                "type": "function_call",
                "call_id": call.id,
                "name": if call.name.is_empty() { "unknown" } else { &call.name },
                "arguments": call.arguments,
                "status": "completed"
            })
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
) -> Response<ProxyBody> {
    let upstream_stream = upstream.bytes_stream();
    let stream = futures::stream::unfold(
        (upstream_stream, ChatSseStreamConverter::new(), false),
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

pub(super) fn streaming_chat_sse_to_anthropic_response(
    upstream: reqwest::Response,
    status: StatusCode,
) -> Response<ProxyBody> {
    let upstream_stream = upstream.bytes_stream();
    let stream = futures::stream::unfold(
        (
            upstream_stream,
            ChatAnthropicSseStreamConverter::new(),
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

#[cfg(test)]
#[derive(Debug, Default)]
pub(super) struct TimiaiResponsesSseSanitizer {
    buffer: String,
    removed_reasoning_events: usize,
}

#[cfg(test)]
impl TimiaiResponsesSseSanitizer {
    pub(super) fn push_chunk(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
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
    let input = usage
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
        .unwrap_or(input + output);
    let cached_tokens = usage_cached_input_tokens(usage).unwrap_or(0);
    let reasoning_tokens = usage_reasoning_output_tokens(usage).unwrap_or(0);

    let Some(usage) = usage.as_object_mut() else {
        return;
    };
    usage.insert("input_tokens".to_string(), json!(input));
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
    buffer: String,
    state: ChatStreamState,
}

impl ChatSseStreamConverter {
    pub(super) fn new() -> Self {
        Self {
            buffer: String::new(),
            state: ChatStreamState {
                response_id: "resp_proxy".to_string(),
                model: CHAT_MODEL_FALLBACK.to_string(),
                usage: normalized_chat_usage(None),
                ..Default::default()
            },
        }
    }

    pub(super) fn push_chunk(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut output = String::new();
        while let Some((event, consumed)) = next_sse_event(&self.buffer) {
            self.buffer.drain(..consumed);
            process_chat_sse_event(&event, &mut output, &mut self.state);
        }
        output.into_bytes()
    }

    pub(super) fn finish(&mut self) -> Vec<u8> {
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
    let mut converter = ChatAnthropicSseStreamConverter::new();
    let mut output = converter.push_chunk(body);
    output.extend(converter.finish());
    output
}

#[derive(Debug)]
struct ChatAnthropicSseStreamConverter {
    buffer: String,
    state: ChatStreamState,
}

impl ChatAnthropicSseStreamConverter {
    pub(super) fn new() -> Self {
        Self {
            buffer: String::new(),
            state: ChatStreamState {
                response_id: "msg_proxy".to_string(),
                model: CHAT_MODEL_FALLBACK.to_string(),
                usage: normalized_chat_usage(None),
                ..Default::default()
            },
        }
    }

    pub(super) fn push_chunk(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut output = String::new();
        while let Some((event, consumed)) = next_sse_event(&self.buffer) {
            self.buffer.drain(..consumed);
            process_chat_sse_anthropic_event(&event, &mut output, &mut self.state);
        }
        output.into_bytes()
    }

    pub(super) fn finish(&mut self) -> Vec<u8> {
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
        call.id = format!("call_proxy_{index}");
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

fn next_sse_event(buffer: &str) -> Option<(String, usize)> {
    if let Some(index) = buffer.find("\r\n\r\n") {
        return Some((buffer[..index].to_string(), index + 4));
    }
    if let Some(index) = buffer.find("\n\n") {
        return Some((buffer[..index].to_string(), index + 2));
    }
    None
}
