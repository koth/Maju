use anyhow::{Context, anyhow};
use bytes::Bytes;
use futures::StreamExt;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::{Frame, Incoming};
use hyper::header::CONTENT_TYPE;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::convert::Infallible;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

type ProxyBody = BoxBody<Bytes, Infallible>;

const VENUS_CHAT_COMPLETIONS_URL: &str =
    "https://v2.open.venus.woa.com/llmproxy/v1/chat/completions";
const DEEPSEEK_UPSTREAM_CHAT_COMPLETIONS_URL: &str = "https://api.deepseek.com/v1/chat/completions";
const CODEX_API_PROXY_PORTS: &[u16] = &[17851, 17852, 17853, 17854, 17855];
const DEEPSEEK_REASONING_PLACEHOLDER: &str = "[previous reasoning unavailable]";

#[derive(Debug, Clone)]
struct CodexApiProxyConfig {
    provider: String,
    api_key: String,
}

static CODEX_API_PROXY_CONFIG: OnceLock<Arc<RwLock<CodexApiProxyConfig>>> = OnceLock::new();
static CODEX_API_PROXY_RUNNING: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static CODEX_API_PROXY_PORT: OnceLock<Arc<RwLock<u16>>> = OnceLock::new();
static DEEPSEEK_REASONING_HISTORY: OnceLock<Arc<RwLock<Vec<ReasoningHistoryEntry>>>> =
    OnceLock::new();

#[derive(Debug, Clone)]
struct ReasoningHistoryEntry {
    content: String,
    reasoning_content: String,
}

pub fn codex_api_proxy_base_url() -> String {
    let port = CODEX_API_PROXY_PORT
        .get()
        .and_then(|port| port.read().ok().map(|port| *port))
        .unwrap_or(CODEX_API_PROXY_PORTS[0]);
    format!("http://127.0.0.1:{port}/v1")
}

pub fn ensure_codex_api_proxy(provider: &str, api_key: &str) -> String {
    let config = CODEX_API_PROXY_CONFIG
        .get_or_init(|| {
            Arc::new(RwLock::new(CodexApiProxyConfig {
                provider: "venus".to_string(),
                api_key: String::new(),
            }))
        })
        .clone();
    if let Ok(mut current) = config.write() {
        current.provider = normalize_proxy_provider(provider).to_string();
        current.api_key = api_key.to_string();
    }

    let running = CODEX_API_PROXY_RUNNING
        .get_or_init(|| Arc::new(AtomicBool::new(false)))
        .clone();
    if running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        match bind_codex_api_proxy_listener() {
            Ok((listener, port)) => {
                set_codex_api_proxy_port(port);
                std::thread::spawn(move || {
                    run_codex_api_proxy(config, listener, port);
                    running.store(false, Ordering::SeqCst);
                });
            }
            Err(error) => {
                append_codex_api_proxy_log(&format!("proxy_bind_failed_all error={error}"));
                running.store(false, Ordering::SeqCst);
            }
        }
    }
    codex_api_proxy_base_url()
}

fn set_codex_api_proxy_port(port: u16) {
    let current = CODEX_API_PROXY_PORT
        .get_or_init(|| Arc::new(RwLock::new(CODEX_API_PROXY_PORTS[0])))
        .clone();
    if let Ok(mut current) = current.write() {
        *current = port;
    }
}

fn bind_codex_api_proxy_listener() -> anyhow::Result<(TcpListener, u16)> {
    let mut last_error = None;
    for port in CODEX_API_PROXY_PORTS {
        let addr = SocketAddr::from(([127, 0, 0, 1], *port));
        match TcpListener::bind(addr) {
            Ok(listener) => {
                listener.set_nonblocking(true)?;
                return Ok((listener, *port));
            }
            Err(error) => {
                append_codex_api_proxy_log(&format!("proxy_bind_failed addr={addr} error={error}"));
                last_error = Some(error);
            }
        }
    }
    Err(anyhow!(
        "failed to bind Codex API proxy on ports {:?}: {}",
        CODEX_API_PROXY_PORTS,
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no ports configured".to_string())
    ))
}

fn run_codex_api_proxy(config: Arc<RwLock<CodexApiProxyConfig>>, listener: TcpListener, port: u16) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            append_codex_api_proxy_log(&format!("proxy_runtime_failed error={error}"));
            return;
        }
    };

    runtime.block_on(async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => listener,
            Err(error) => {
                append_codex_api_proxy_log(&format!("proxy_listener_failed error={error}"));
                return;
            }
        };
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        append_codex_api_proxy_log(&format!("proxy_listening addr={addr}"));

        loop {
            let Ok((stream, _)) = listener.accept().await else {
                append_codex_api_proxy_log("proxy_accept_failed");
                continue;
            };
            let config = config.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let service = service_fn(move |request| {
                    let config = config.clone();
                    async move { handle_codex_api_proxy_request(request, config).await }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .await;
            });
        }
    });
}

async fn handle_codex_api_proxy_request(
    request: Request<Incoming>,
    config: Arc<RwLock<CodexApiProxyConfig>>,
) -> Result<Response<ProxyBody>, Infallible> {
    let response = match proxy_codex_api_request(request, config).await {
        Ok(response) => response,
        Err(error) => response_with_status(
            StatusCode::BAD_GATEWAY,
            json!({ "error": { "message": error.to_string() } }).to_string(),
            "application/json",
        ),
    };
    Ok(response)
}

async fn proxy_codex_api_request(
    request: Request<Incoming>,
    config: Arc<RwLock<CodexApiProxyConfig>>,
) -> anyhow::Result<Response<ProxyBody>> {
    if request.method() != Method::POST || !request.uri().path().ends_with("/responses") {
        return Ok(response_with_status(
            StatusCode::NOT_FOUND,
            "not found".to_string(),
            "text/plain; charset=utf-8",
        ));
    }

    let body = request.into_body().collect().await?.to_bytes();
    let config = config
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_else(|_| CodexApiProxyConfig {
            provider: "venus".to_string(),
            api_key: String::new(),
        });
    let payload: Value = serde_json::from_slice(&body)?;
    let requested_stream = payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    log_responses_payload_summary("responses_request", &payload, &config.provider);
    let chat_payload = match responses_payload_to_chat_payload(payload, &config.provider) {
        Ok(payload) => payload,
        Err(error) => {
            append_codex_api_proxy_log(&format!("responses_to_chat_error error={error}"));
            return Ok(response_with_status(
                StatusCode::BAD_REQUEST,
                json!({ "error": { "message": error.to_string() } }).to_string(),
                "application/json",
            ));
        }
    };
    if config.api_key.trim().is_empty() {
        return Ok(response_with_status(
            StatusCode::UNAUTHORIZED,
            json!({ "error": { "message": "API key is not configured" } }).to_string(),
            "application/json",
        ));
    }
    let chat_payload = normalize_chat_payload_for_provider(chat_payload, &config.provider);
    if normalize_proxy_provider(&config.provider) == "deepseek" {
        log_chat_payload_summary("deepseek_request", &chat_payload);
    }
    let upstream_url = upstream_chat_completions_url(&config.provider);

    let client = reqwest::Client::new();
    let upstream = client
        .post(upstream_url)
        .bearer_auth(config.api_key)
        .header(CONTENT_TYPE, "application/json")
        .body(serde_json::to_vec(&chat_payload)?)
        .send()
        .await?;
    let status = StatusCode::from_u16(upstream.status().as_u16())?;
    let content_type = upstream
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    if is_event_stream(&content_type) {
        return Ok(streaming_chat_sse_response(upstream, status));
    }

    let body = upstream.bytes().await?;
    let mut response_content_type = content_type.clone();
    let body = if status.is_success() {
        let chat_response: Value = serde_json::from_slice(body.as_ref())?;
        let response = chat_response_to_responses_response(chat_response)?;
        if requested_stream {
            response_content_type = "text/event-stream".to_string();
            responses_response_to_sse(&response)
        } else {
            serde_json::to_vec(&response)?
        }
    } else {
        log_suspicious_venus_response(status, &content_type, body.as_ref());
        body.to_vec()
    };
    log_suspicious_venus_response(status, &content_type, &body);

    let mut response = Response::new(full_body(Bytes::from(body)));
    *response.status_mut() = status;
    if let Ok(value) = response_content_type.parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    Ok(response)
}

fn normalize_proxy_provider(provider: &str) -> &'static str {
    match provider.trim().to_ascii_lowercase().as_str() {
        "deepseek" => "deepseek",
        _ => "venus",
    }
}

fn upstream_chat_completions_url(provider: &str) -> &'static str {
    match normalize_proxy_provider(provider) {
        "deepseek" => DEEPSEEK_UPSTREAM_CHAT_COMPLETIONS_URL,
        _ => VENUS_CHAT_COMPLETIONS_URL,
    }
}

fn responses_payload_to_chat_payload(payload: Value, provider: &str) -> anyhow::Result<Value> {
    let mut messages = Vec::new();
    let instructions = payload
        .get("instructions")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned);

    if let Some(text) = instructions {
        messages.push(json!({ "role": "system", "content": text }));
    }

    match payload.get("input") {
        Some(Value::Array(items)) => {
            let mut pending_tool_calls = Vec::new();
            for item in items {
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    pending_tool_calls.push(responses_function_call_to_chat_tool_call(item)?);
                    continue;
                }
                flush_pending_tool_calls(&mut messages, &mut pending_tool_calls);
                if let Some(message) = responses_input_item_to_chat_message(item, provider)? {
                    messages.push(message);
                }
            }
            flush_pending_tool_calls(&mut messages, &mut pending_tool_calls);
        }
        Some(Value::String(text)) => {
            if !text.trim().is_empty() {
                messages.push(json!({ "role": "user", "content": text }));
            }
        }
        Some(other) => {
            messages.push(json!({ "role": "user", "content": response_content_to_text(other) }));
        }
        None => {}
    }

    if messages.is_empty() {
        return Err(anyhow!(
            "Responses request did not contain convertible input"
        ));
    }

    let mut chat = json!({
        "model": payload.get("model").cloned().unwrap_or_else(|| Value::String("glm-5.1".to_string())),
        "messages": messages,
        "stream": payload.get("stream").and_then(Value::as_bool).unwrap_or(false),
    });

    if let Some(tools) = responses_tools_to_chat_tools(payload.get("tools")) {
        chat["tools"] = tools;
    }
    if let Some(tool_choice) = responses_tool_choice_to_chat_tool_choice(payload.get("tool_choice"))
    {
        chat["tool_choice"] = tool_choice;
    } else if chat.get("tools").is_some() {
        chat["tool_choice"] = Value::String("auto".to_string());
    }
    if let Some(temp) = payload.get("temperature") {
        chat["temperature"] = temp.clone();
    }
    if let Some(max_tokens) = payload
        .get("max_output_tokens")
        .or_else(|| payload.get("max_tokens"))
    {
        chat["max_tokens"] = max_tokens.clone();
    }

    Ok(chat)
}

fn normalize_chat_payload_for_provider(mut payload: Value, provider: &str) -> Value {
    if normalize_proxy_provider(provider) != "deepseek" {
        return payload;
    }
    let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) else {
        return payload;
    };
    for message in &mut *messages {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if message
            .get("reasoning_content")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
        {
            continue;
        }
        let content = message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let reasoning_content = reasoning_content_for_text(content)
            .unwrap_or_else(|| DEEPSEEK_REASONING_PLACEHOLDER.to_string());
        message["reasoning_content"] = Value::String(reasoning_content);
    }
    payload
}

fn flush_pending_tool_calls(messages: &mut Vec<Value>, pending_tool_calls: &mut Vec<Value>) {
    if pending_tool_calls.is_empty() {
        return;
    }
    messages.push(json!({
        "role": "assistant",
        "content": Value::Null,
        "tool_calls": std::mem::take(pending_tool_calls)
    }));
}

fn responses_input_item_to_chat_message(
    item: &Value,
    provider: &str,
) -> anyhow::Result<Option<Value>> {
    let item_type = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");

    match item_type {
        "message" => {
            let role = match item.get("role").and_then(Value::as_str).unwrap_or("user") {
                "developer" => "system",
                "assistant" => "assistant",
                "system" => "system",
                "tool" => "tool",
                _ => "user",
            };
            let content = response_content_to_text(item.get("content").unwrap_or(&Value::Null));
            let reasoning_content = item
                .get("reasoning_content")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|text| !text.is_empty());
            if content.is_empty() && reasoning_content.is_none() {
                return Ok(None);
            }
            let mut message = json!({ "role": role, "content": content });
            if role == "assistant" {
                if let Some(reasoning_content) = reasoning_content
                    .or_else(|| reasoning_content_for_text(&content))
                    .or_else(|| {
                        (normalize_proxy_provider(provider) == "deepseek")
                            .then(|| DEEPSEEK_REASONING_PLACEHOLDER.to_string())
                    })
                {
                    message["reasoning_content"] = Value::String(reasoning_content);
                }
            }
            Ok(Some(message))
        }
        "function_call" => Ok(None),
        "function_call_output" => {
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .context("function_call_output input item is missing call_id")?;
            let output = response_content_to_text(item.get("output").unwrap_or(&Value::Null));
            Ok(Some(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": output
            })))
        }
        _ => {
            let fallback = fallback_input_item_to_chat_message(item, provider);
            append_codex_api_proxy_log(&format!(
                "unsupported_responses_input_item type={item_type} recovered={}",
                fallback.is_some()
            ));
            Ok(fallback)
        }
    }
}

fn responses_function_call_to_chat_tool_call(item: &Value) -> anyhow::Result<Value> {
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("call_unknown");
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .context("function_call input item is missing name")?;
    let arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}");
    Ok(json!({
        "id": call_id,
        "type": "function",
        "function": { "name": name, "arguments": arguments }
    }))
}

fn fallback_input_item_to_chat_message(item: &Value, provider: &str) -> Option<Value> {
    let role = match item.get("role").and_then(Value::as_str).unwrap_or("user") {
        "developer" => "system",
        "assistant" => "assistant",
        "system" => "system",
        "tool" => "tool",
        _ => "user",
    };
    let content = item
        .get("content")
        .or_else(|| item.get("text"))
        .or_else(|| item.get("output"))
        .or_else(|| item.get("input"))
        .map(response_content_to_text)
        .filter(|text| !text.trim().is_empty())?;
    let mut message = json!({ "role": role, "content": content });
    if role == "assistant" && normalize_proxy_provider(provider) == "deepseek" {
        let reasoning_content = item
            .get("reasoning_content")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|text| !text.trim().is_empty())
            .or_else(|| reasoning_content_for_text(&content))
            .unwrap_or_else(|| DEEPSEEK_REASONING_PLACEHOLDER.to_string());
        message["reasoning_content"] = Value::String(reasoning_content);
    }
    Some(message)
}

fn response_content_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.get("text")
                    .or_else(|| item.get("output_text"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| item.as_str().map(ToOwned::to_owned))
            })
            .collect::<Vec<_>>()
            .join(""),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn responses_tools_to_chat_tools(value: Option<&Value>) -> Option<Value> {
    let tools = value?.as_array()?;
    let converted = tools
        .iter()
        .filter_map(|tool| {
            if tool.get("type").and_then(Value::as_str) != Some("function") {
                return None;
            }
            let name = tool.get("name").and_then(Value::as_str)?;
            Some(json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": tool.get("description").cloned().unwrap_or(Value::String(String::new())),
                    "parameters": tool.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} }))
                }
            }))
        })
        .collect::<Vec<_>>();
    (!converted.is_empty()).then_some(Value::Array(converted))
}

fn responses_tool_choice_to_chat_tool_choice(value: Option<&Value>) -> Option<Value> {
    match value? {
        Value::String(choice) if matches!(choice.as_str(), "auto" | "required" | "none") => {
            Some(Value::String(choice.clone()))
        }
        Value::Object(map) => {
            let name = map
                .get("name")
                .or_else(|| {
                    map.get("function")
                        .and_then(|function| function.get("name"))
                })
                .and_then(Value::as_str)?;
            Some(json!({ "type": "function", "function": { "name": name } }))
        }
        _ => None,
    }
}

fn chat_response_to_responses_response(chat: Value) -> anyhow::Result<Value> {
    let choice = chat
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .context("Venus chat response did not contain choices[0]")?;
    let message = choice
        .get("message")
        .context("Venus chat response did not contain message")?;
    let output = chat_message_to_responses_output(message);
    remember_message_reasoning(message);
    let usage = normalized_chat_usage(chat.get("usage"));
    Ok(json!({
        "id": chat.get("id").cloned().unwrap_or_else(|| Value::String("resp_venus".to_string())),
        "object": "response",
        "created_at": chat.get("created").cloned().unwrap_or_else(|| Value::from(0)),
        "model": chat.get("model").cloned().unwrap_or_else(|| Value::String("glm-5.1".to_string())),
        "status": "completed",
        "output": output,
        "usage": usage,
    }))
}

fn chat_message_to_responses_output(message: &Value) -> Vec<Value> {
    let mut output = Vec::new();
    if let Some(content) = message.get("content").and_then(Value::as_str) {
        if !content.is_empty() {
            output.push(response_message_item_with_reasoning(
                content,
                message.get("reasoning_content").and_then(Value::as_str),
            ));
        }
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            output.push(chat_tool_call_to_responses_item(tool_call));
        }
    }
    output
}

fn response_message_item_with_reasoning(text: &str, reasoning_content: Option<&str>) -> Value {
    let mut item = json!({
        "id": "msg_venus",
        "type": "message",
        "role": "assistant",
        "status": "completed",
        "content": [{ "type": "output_text", "text": text }]
    });
    if let Some(reasoning_content) = reasoning_content.filter(|value| !value.is_empty()) {
        item["reasoning_content"] = Value::String(reasoning_content.to_string());
    }
    item
}

fn chat_tool_call_to_responses_item(tool_call: &Value) -> Value {
    let id = tool_call
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("call_venus");
    let function = tool_call.get("function").unwrap_or(&Value::Null);
    json!({
        "id": id,
        "type": "function_call",
        "call_id": id,
        "name": function.get("name").and_then(Value::as_str).unwrap_or("unknown"),
        "arguments": function.get("arguments").and_then(Value::as_str).unwrap_or("{}"),
        "status": "completed"
    })
}

fn normalized_chat_usage(value: Option<&Value>) -> Value {
    let usage = value.unwrap_or(&Value::Null);
    let input = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let output = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let total = usage
        .get("total_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(input + output);
    json!({
        "input_tokens": input,
        "output_tokens": output,
        "total_tokens": total
    })
}

#[derive(Debug, Default)]
struct ChatStreamState {
    response_id: String,
    model: String,
    text: String,
    reasoning_content: String,
    message_started: bool,
    tool_calls: Vec<StreamToolCall>,
    usage: Value,
}

#[derive(Debug, Default, Clone)]
struct StreamToolCall {
    id: String,
    name: String,
    arguments: String,
    added: bool,
}

#[cfg(test)]
fn chat_sse_to_responses_sse(body: &[u8]) -> Vec<u8> {
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
                    "id": "msg_venus",
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
                "item_id": "msg_venus",
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
            "item_id": "msg_venus",
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
        call.id = format!("call_venus_{index}");
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
    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
        call.arguments.push_str(arguments);
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
                "item_id": "msg_venus",
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
                "item_id": "msg_venus",
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
        let item = json!({
            "id": call.id,
            "type": "function_call",
            "call_id": call.id,
            "name": if call.name.is_empty() { "unknown" } else { &call.name },
            "arguments": call.arguments,
            "status": "completed"
        });
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

fn streaming_chat_sse_response(
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
                            "upstream_sse_read_error error={error}"
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

#[derive(Debug)]
struct ChatSseStreamConverter {
    buffer: String,
    state: ChatStreamState,
}

impl ChatSseStreamConverter {
    fn new() -> Self {
        Self {
            buffer: String::new(),
            state: ChatStreamState {
                response_id: "resp_venus".to_string(),
                model: "glm-5.1".to_string(),
                usage: normalized_chat_usage(None),
                ..Default::default()
            },
        }
    }

    fn push_chunk(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut output = String::new();
        while let Some((event, consumed)) = next_sse_event(&self.buffer) {
            self.buffer.drain(..consumed);
            process_chat_sse_event(&event, &mut output, &mut self.state);
        }
        output.into_bytes()
    }

    fn finish(&mut self) -> Vec<u8> {
        let trailing = std::mem::take(&mut self.buffer);
        let mut output = String::new();
        if !trailing.trim().is_empty() {
            process_chat_sse_event(&trailing, &mut output, &mut self.state);
        }
        emit_stream_done(&mut output, &mut self.state);
        output.into_bytes()
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

fn responses_response_to_sse(response: &Value) -> Vec<u8> {
    let mut output = String::new();
    let mut final_output = Vec::new();
    if let Some(items) = response.get("output").and_then(Value::as_array) {
        for (output_index, item) in items.iter().enumerate() {
            match item.get("type").and_then(Value::as_str).unwrap_or("") {
                "message" => {
                    let item_id = item
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("msg_venus");
                    let text = item
                        .get("content")
                        .and_then(Value::as_array)
                        .map(|content| {
                            content
                                .iter()
                                .filter_map(|part| {
                                    part.get("text")
                                        .or_else(|| part.get("output_text"))
                                        .and_then(Value::as_str)
                                })
                                .collect::<Vec<_>>()
                                .join("")
                        })
                        .unwrap_or_default();
                    push_sse(
                        &mut output,
                        "response.output_item.added",
                        json!({
                            "type": "response.output_item.added",
                            "output_index": output_index,
                            "item": {
                                "id": item_id,
                                "type": "message",
                                "role": "assistant",
                                "status": "in_progress",
                                "content": []
                            }
                        }),
                    );
                    push_sse(
                        &mut output,
                        "response.content_part.added",
                        json!({
                            "type": "response.content_part.added",
                            "output_index": output_index,
                            "content_index": 0,
                            "item_id": item_id,
                            "part": { "type": "output_text", "text": "" }
                        }),
                    );
                    if !text.is_empty() {
                        push_sse(
                            &mut output,
                            "response.output_text.delta",
                            json!({
                                "type": "response.output_text.delta",
                                "output_index": output_index,
                                "content_index": 0,
                                "item_id": item_id,
                                "delta": text
                            }),
                        );
                    }
                    push_sse(
                        &mut output,
                        "response.output_text.done",
                        json!({
                            "type": "response.output_text.done",
                            "output_index": output_index,
                            "content_index": 0,
                            "item_id": item_id,
                            "text": text
                        }),
                    );
                    push_sse(
                        &mut output,
                        "response.content_part.done",
                        json!({
                            "type": "response.content_part.done",
                            "output_index": output_index,
                            "content_index": 0,
                            "item_id": item_id,
                            "part": { "type": "output_text", "text": text }
                        }),
                    );
                    let mut done_item = item.clone();
                    done_item["status"] = Value::String("completed".to_string());
                    push_sse(
                        &mut output,
                        "response.output_item.done",
                        json!({
                            "type": "response.output_item.done",
                            "output_index": output_index,
                            "item": done_item
                        }),
                    );
                    final_output.push(item.clone());
                }
                "function_call" => {
                    push_sse(
                        &mut output,
                        "response.output_item.added",
                        json!({
                            "type": "response.output_item.added",
                            "output_index": output_index,
                            "item": item
                        }),
                    );
                    push_sse(
                        &mut output,
                        "response.output_item.done",
                        json!({
                            "type": "response.output_item.done",
                            "output_index": output_index,
                            "item": item
                        }),
                    );
                    final_output.push(item.clone());
                }
                _ => {}
            }
        }
    }
    let mut completed = response.clone();
    completed["output"] = Value::Array(final_output);
    push_sse(
        &mut output,
        "response.completed",
        json!({
            "type": "response.completed",
            "response": completed
        }),
    );
    output.push_str("data: [DONE]\n\n");
    output.into_bytes()
}

fn remember_message_reasoning(message: &Value) {
    let Some(content) = message.get("content").and_then(Value::as_str) else {
        return;
    };
    let Some(reasoning_content) = message.get("reasoning_content").and_then(Value::as_str) else {
        return;
    };
    remember_reasoning_content(content, reasoning_content);
}

fn remember_reasoning_content(content: &str, reasoning_content: &str) {
    if content.trim().is_empty() || reasoning_content.trim().is_empty() {
        return;
    }
    let history = DEEPSEEK_REASONING_HISTORY
        .get_or_init(|| Arc::new(RwLock::new(Vec::new())))
        .clone();
    let Ok(mut entries) = history.write() else {
        return;
    };
    if let Some(existing) = entries.iter_mut().find(|entry| entry.content == content) {
        existing.reasoning_content = reasoning_content.to_string();
        return;
    }
    entries.push(ReasoningHistoryEntry {
        content: content.to_string(),
        reasoning_content: reasoning_content.to_string(),
    });
    const MAX_REASONING_HISTORY_ENTRIES: usize = 128;
    if entries.len() > MAX_REASONING_HISTORY_ENTRIES {
        let overflow = entries.len() - MAX_REASONING_HISTORY_ENTRIES;
        entries.drain(0..overflow);
    }
}

fn reasoning_content_for_text(content: &str) -> Option<String> {
    if content.trim().is_empty() {
        return None;
    }
    let history = DEEPSEEK_REASONING_HISTORY.get()?.clone();
    let entries = history.read().ok()?;
    entries
        .iter()
        .rev()
        .find(|entry| entry.content == content)
        .map(|entry| entry.reasoning_content.clone())
}

fn log_chat_payload_summary(label: &str, payload: &Value) {
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("<missing>");
    let stream = payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let messages = payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .enumerate()
                .map(|(index, message)| {
                    let role = message
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or("<missing>");
                    let content_len = message
                        .get("content")
                        .and_then(Value::as_str)
                        .map(str::len)
                        .unwrap_or(0);
                    let reasoning_len = message
                        .get("reasoning_content")
                        .and_then(Value::as_str)
                        .map(str::len)
                        .unwrap_or(0);
                    let tool_calls = message
                        .get("tool_calls")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    format!(
                        "#{index}:{role}:content={content_len}:reasoning={reasoning_len}:tools={tool_calls}"
                    )
                })
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_else(|| "<no messages>".to_string());
    append_codex_api_proxy_log(&format!(
        "{label} model={model} stream={stream} messages=[{messages}]"
    ));
}

fn log_responses_payload_summary(label: &str, payload: &Value, provider: &str) {
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("<missing>");
    let stream = payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let input = match payload.get("input") {
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let item_type = item
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("message");
                let role = item
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>");
                let content_len = item
                    .get("content")
                    .map(response_content_to_text)
                    .map(|text| text.len())
                    .unwrap_or(0);
                format!("#{index}:{item_type}:{role}:content={content_len}")
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::String(text)) => format!("string:{}", text.len()),
        Some(other) => format!("other:{}", response_content_to_text(other).len()),
        None => "<missing>".to_string(),
    };
    append_codex_api_proxy_log(&format!(
        "{label} provider={} model={model} stream={stream} input=[{input}]",
        normalize_proxy_provider(provider)
    ));
}

fn push_sse(output: &mut String, event: &str, data: Value) {
    output.push_str("event: ");
    output.push_str(event);
    output.push('\n');
    output.push_str("data: ");
    output.push_str(&serde_json::to_string(&data).unwrap_or_else(|_| "{}".to_string()));
    output.push_str("\n\n");
}

fn is_event_stream(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
}

fn log_suspicious_venus_response(status: StatusCode, content_type: &str, body: &[u8]) {
    if is_event_stream(content_type) {
        let Ok(text) = std::str::from_utf8(body) else {
            return;
        };
        if !status.is_success() || !text.contains("response.completed") {
            let preview = text.chars().take(2000).collect::<String>();
            append_codex_api_proxy_log(&format!(
                "upstream_sse_suspicious status={} content_type={content_type} body={preview}",
                status.as_u16()
            ));
        }
        return;
    }

    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        if !status.is_success() {
            append_codex_api_proxy_log(&format!(
                "upstream_non_json status={} content_type={content_type}",
                status.as_u16()
            ));
        }
        return;
    };

    let output_len = value
        .pointer("/output")
        .and_then(Value::as_array)
        .map(Vec::len)
        .or_else(|| {
            value
                .pointer("/response/output")
                .and_then(Value::as_array)
                .map(Vec::len)
        })
        .unwrap_or(0);
    let has_error = value.get("error").is_some();
    if status.is_success() && output_len > 0 && !has_error {
        return;
    }

    let preview = serde_json::to_string(&value).unwrap_or_default();
    let preview = preview.chars().take(2000).collect::<String>();
    append_codex_api_proxy_log(&format!(
        "upstream_suspicious status={} content_type={content_type} output_len={output_len} body={preview}",
        status.as_u16()
    ));
}

fn response_with_status(
    status: StatusCode,
    body: String,
    content_type: &str,
) -> Response<ProxyBody> {
    let mut response = Response::new(full_body(Bytes::from(body)));
    *response.status_mut() = status;
    if let Ok(value) = content_type.parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    response
}

fn full_body(bytes: Bytes) -> ProxyBody {
    Full::new(bytes).map_err(|never| match never {}).boxed()
}

fn append_codex_api_proxy_log(line: &str) {
    let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) else {
        return;
    };
    let path = std::path::PathBuf::from(home)
        .join(".kodex")
        .join("logs")
        .join("codex-api-proxy.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(file, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_responses_request_to_chat_payload() {
        let payload = json!({
            "model": "glm-5.1",
            "instructions": "base instructions",
            "input": [
                {
                    "type": "message",
                    "role": "developer",
                    "content": [{ "type": "input_text", "text": "dev instructions" }]
                },
                {
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "hello" }]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "hi" }],
                    "reasoning_content": "previous thinking"
                },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "list_files",
                    "arguments": "{\"path\":\".\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "file list"
                }
            ],
            "tools": [{
                "type": "function",
                "name": "list_files",
                "description": "List files",
                "parameters": { "type": "object", "properties": {} }
            }],
            "stream": true
        });

        let chat = responses_payload_to_chat_payload(payload, "venus").unwrap();

        assert_eq!(chat["model"], "glm-5.1");
        assert_eq!(chat["stream"], true);
        assert_eq!(chat["messages"][0]["role"], "system");
        assert_eq!(chat["messages"][0]["content"], "base instructions");
        assert_eq!(chat["messages"][1]["role"], "system");
        assert_eq!(chat["messages"][1]["content"], "dev instructions");
        assert_eq!(chat["messages"][2]["role"], "user");
        assert_eq!(chat["messages"][2]["content"], "hello");
        assert_eq!(chat["messages"][3]["role"], "assistant");
        assert_eq!(chat["messages"][3]["content"], "hi");
        assert_eq!(
            chat["messages"][3]["reasoning_content"],
            "previous thinking"
        );
        assert_eq!(chat["messages"][4]["tool_calls"][0]["id"], "call_1");
        assert_eq!(chat["messages"][5]["role"], "tool");
        assert_eq!(chat["messages"][5]["tool_call_id"], "call_1");
        assert_eq!(chat["tools"][0]["function"]["name"], "list_files");
        assert_eq!(chat["tool_choice"], "auto");
    }

    #[test]
    fn deepseek_requests_preserve_upstream_streaming() {
        let payload = json!({
            "model": "deepseek-v4-pro",
            "input": "hello",
            "stream": true
        });

        let chat = responses_payload_to_chat_payload(payload, "deepseek").unwrap();

        assert_eq!(chat["stream"], true);
    }

    #[test]
    fn converts_chat_response_to_responses_response() {
        let chat = json!({
            "id": "chatcmpl_1",
            "created": 123,
            "model": "glm-5.1",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "reasoning_content": "hidden reasoning",
                    "content": "I will inspect files.",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "list_files",
                            "arguments": "{\"path\":\".\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 12,
                "completion_tokens": 3,
                "total_tokens": 15
            }
        });

        let response = chat_response_to_responses_response(chat).unwrap();

        assert_eq!(response["id"], "chatcmpl_1");
        assert_eq!(response["output"][0]["type"], "message");
        assert_eq!(
            response["output"][0]["content"][0]["text"],
            "I will inspect files."
        );
        assert_eq!(
            response["output"][0]["reasoning_content"],
            "hidden reasoning"
        );
        assert_eq!(response["output"][1]["type"], "function_call");
        assert_eq!(response["output"][1]["call_id"], "call_1");
        assert_eq!(response["output"][1]["name"], "list_files");
        assert_eq!(response["usage"]["input_tokens"], 12);
        assert_eq!(response["usage"]["output_tokens"], 3);
        assert_eq!(response["usage"]["total_tokens"], 15);
    }

    #[test]
    fn wraps_non_stream_response_as_responses_sse() {
        let response = json!({
            "id": "resp_1",
            "object": "response",
            "created_at": 123,
            "model": "deepseek-v4-pro",
            "status": "completed",
            "output": [{
                "id": "msg_venus",
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{ "type": "output_text", "text": "done" }]
            }],
            "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2 }
        });

        let sse = responses_response_to_sse(&response);
        let text = String::from_utf8(sse).unwrap();

        assert!(text.contains("event: response.output_item.added"));
        assert!(text.contains("event: response.output_text.delta"));
        assert!(text.contains("\"delta\":\"done\""));
        assert!(text.contains("event: response.completed"));
        assert!(text.contains("data: [DONE]"));
    }

    #[test]
    fn converts_chat_stream_to_responses_stream() {
        let body = concat!(
            "data:{\"id\":\"chatcmpl_1\",\"model\":\"glm-5.1\",\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
            "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"list_files\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
            "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\".\\\"}\"}}]}}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"total_tokens\":15}}\n\n",
            "data: [DONE]\n\n"
        );

        let normalized = chat_sse_to_responses_sse(body.as_bytes());
        let text = String::from_utf8(normalized).unwrap();

        assert!(text.contains("event: response.output_text.delta"));
        assert!(text.contains("event: response.function_call_arguments.delta"));
        assert!(text.contains("event: response.function_call_arguments.done"));
        assert!(text.contains("event: response.completed"));
        assert!(text.contains("\"name\":\"list_files\""));
        assert!(text.contains("\"arguments\":\"{\\\"path\\\":\\\".\\\"}\""));
        assert!(text.contains("\"input_tokens\":12"));
        assert!(text.contains("data: [DONE]"));
    }

    #[test]
    fn converts_chat_stream_incrementally() {
        let mut converter = ChatSseStreamConverter::new();

        let first = converter.push_chunk(
            b"data:{\"id\":\"chatcmpl_1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"delta\":{\"content\":\"hel",
        );
        assert!(first.is_empty());

        let second = converter.push_chunk(b"lo\"}}]}\n\n");
        let second = String::from_utf8(second).unwrap();
        assert!(second.contains("event: response.output_text.delta"));
        assert!(second.contains("\"delta\":\"hello\""));

        let done = String::from_utf8(converter.finish()).unwrap();
        assert!(done.contains("event: response.completed"));
        assert!(done.contains("data: [DONE]"));
    }

    #[test]
    fn preserves_deepseek_stream_reasoning_content() {
        let body = concat!(
            "data:{\"id\":\"chatcmpl_1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"delta\":{\"reasoning_content\":\"think \"}}]}\n\n",
            "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"reasoning_content\":\"more\"}}]}\n\n",
            "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\n",
            "data: [DONE]\n\n"
        );

        let normalized = chat_sse_to_responses_sse(body.as_bytes());
        let text = String::from_utf8(normalized).unwrap();

        assert!(text.contains("\"model\":\"deepseek-v4-pro\""));
        assert!(text.contains("\"reasoning_content\":\"think more\""));
        assert!(text.contains("\"text\":\"answer\""));
    }

    #[test]
    fn injects_remembered_reasoning_content_into_next_chat_request() {
        let body = concat!(
            "data:{\"id\":\"chatcmpl_1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"delta\":{\"reasoning_content\":\"cached thinking\"}}]}\n\n",
            "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"content\":\"unique cached answer\"}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let _ = chat_sse_to_responses_sse(body.as_bytes());
        let payload = json!({
            "input": [{
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "unique cached answer" }]
            }]
        });

        let chat = responses_payload_to_chat_payload(payload, "deepseek").unwrap();

        assert_eq!(chat["messages"][0]["role"], "assistant");
        assert_eq!(chat["messages"][0]["content"], "unique cached answer");
        assert_eq!(chat["messages"][0]["reasoning_content"], "cached thinking");
    }

    #[test]
    fn synthesizes_deepseek_reasoning_for_uncached_assistant_history() {
        let payload = json!({
            "input": [{
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "uncached assistant answer" }]
            }]
        });

        let chat = responses_payload_to_chat_payload(payload, "deepseek").unwrap();

        assert_eq!(chat["messages"][0]["role"], "assistant");
        assert_eq!(chat["messages"][0]["content"], "uncached assistant answer");
        assert_eq!(
            chat["messages"][0]["reasoning_content"],
            DEEPSEEK_REASONING_PLACEHOLDER
        );
    }

    #[test]
    fn normalizes_deepseek_assistant_messages_before_upstream_request() {
        let payload = json!({
            "model": "deepseek-v4-pro",
            "messages": [
                { "role": "system", "content": "base" },
                { "role": "assistant", "content": "older answer" },
                {
                    "role": "assistant",
                    "content": "answer with reasoning",
                    "reasoning_content": "already present"
                }
            ],
            "stream": true
        });

        let normalized = normalize_chat_payload_for_provider(payload, "deepseek");

        assert_eq!(
            normalized["messages"][1]["reasoning_content"],
            DEEPSEEK_REASONING_PLACEHOLDER
        );
        assert_eq!(
            normalized["messages"][2]["reasoning_content"],
            "already present"
        );
    }

    #[test]
    fn groups_consecutive_responses_function_calls_before_outputs() {
        let payload = json!({
            "input": [
                { "role": "user", "content": "run tools" },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "list_files",
                    "arguments": "{}"
                },
                {
                    "type": "function_call",
                    "call_id": "call_2",
                    "name": "read_file",
                    "arguments": "{\"path\":\"README.md\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "files"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_2",
                    "output": "readme"
                }
            ]
        });

        let chat = responses_payload_to_chat_payload(payload, "deepseek").unwrap();

        assert_eq!(chat["messages"][0]["role"], "user");
        assert_eq!(chat["messages"][1]["role"], "assistant");
        assert_eq!(
            chat["messages"][1]["tool_calls"].as_array().unwrap().len(),
            2
        );
        assert_eq!(chat["messages"][1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(chat["messages"][1]["tool_calls"][1]["id"], "call_2");
        assert_eq!(chat["messages"][2]["role"], "tool");
        assert_eq!(chat["messages"][2]["tool_call_id"], "call_1");
        assert_eq!(chat["messages"][3]["role"], "tool");
        assert_eq!(chat["messages"][3]["tool_call_id"], "call_2");
    }

    #[test]
    fn ignores_unsupported_responses_input_item() {
        let payload = json!({
            "input": [
                {
                    "type": "unsupported",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "recovered answer" }]
                },
                {
                    "role": "user",
                    "content": "hello"
                }
            ]
        });

        let chat = responses_payload_to_chat_payload(payload, "deepseek").unwrap();

        assert_eq!(chat["messages"].as_array().unwrap().len(), 2);
        assert_eq!(chat["messages"][0]["role"], "assistant");
        assert_eq!(chat["messages"][0]["content"], "recovered answer");
        assert_eq!(
            chat["messages"][0]["reasoning_content"],
            DEEPSEEK_REASONING_PLACEHOLDER
        );
        assert_eq!(chat["messages"][1]["role"], "user");
        assert_eq!(chat["messages"][1]["content"], "hello");
    }
}
