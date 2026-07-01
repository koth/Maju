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
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::convert::Infallible;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

type ProxyBody = BoxBody<Bytes, Infallible>;

mod provider;
mod sse;

use sse::{
    streaming_anthropic_sse_to_responses_response, streaming_chat_sse_response,
    streaming_chat_sse_to_anthropic_response, streaming_passthrough_response,
    streaming_responses_sse_to_anthropic_response,
};

#[cfg(test)]
use sse::{
    AnthropicSseToResponsesConverter, ChatSseStreamConverter, ResponsesSseToAnthropicConverter,
    TimiaiResponsesSseSanitizer, anthropic_sse_to_responses_sse, chat_sse_to_anthropic_sse,
    chat_sse_to_responses_sse, responses_sse_to_anthropic_sse,
};

use provider::{
    decode_provider_model_id, mapped_proxy_provider_for_model, normalize_proxy_provider,
    normalized_model_key, proxy_provider_for_model, proxy_provider_from_path,
    replace_payload_model, should_bridge_anthropic_messages_to_chat_completions,
    timiai_authorization_log_state, upstream_chat_completion_model, upstream_chat_completions_url,
    upstream_messages_url, upstream_native_anthropic_model, with_timiai_headers,
};
#[cfg(test)]
use provider::{is_claude_family_model, timiai_authorization_header_value};

const TIMIAI_RESPONSES_COMPACT_URL: &str =
    "https://api.timiai.woa.com/ai_api_manage/llmproxy/responses/compact";
const TIMIAI_CHAT_COMPLETIONS_URL: &str =
    "http://api.timiai.woa.com/ai_api_manage/llmproxy/chat/completions";
const TIMIAI_MESSAGES_URL: &str = "http://api.timiai.woa.com/ai_api_manage/llmproxy/v1/messages";
const COMMANDCODE_UPSTREAM_CHAT_COMPLETIONS_URL: &str =
    "https://api.commandcode.ai/provider/v1/chat/completions";
const COMMANDCODE_UPSTREAM_MESSAGES_URL: &str = "https://api.commandcode.ai/provider/v1/messages";
const DEEPSEEK_UPSTREAM_CHAT_COMPLETIONS_URL: &str = "https://api.deepseek.com/v1/chat/completions";
const KIMI_UPSTREAM_CHAT_COMPLETIONS_URL: &str = "https://api.kimi.com/coding/v1/chat/completions";
const KIMI_UPSTREAM_MESSAGES_URL: &str = "https://api.kimi.com/coding/v1/messages";
const MIMO_UPSTREAM_CHAT_COMPLETIONS_URL: &str =
    "https://token-plan-cn.xiaomimimo.com/v1/chat/completions";
const MIMO_UPSTREAM_MESSAGES_URL: &str =
    "https://token-plan-cn.xiaomimimo.com/anthropic/v1/messages";
const CODEX_API_PROXY_PORTS: &[u16] = &[17851, 17852, 17853, 17854, 17855];
const PROVIDER_MODEL_ID_PREFIX: &str = "kodex-provider/";
const SHELL_TOOL_INSTRUCTIONS: &str = "Shell command compatibility rule: Run project commands from the directory that owns the relevant manifest or toolchain file, such as package.json or Cargo.toml. Prefer local project executables and scripts over bare npx or package-manager commands that may download from the network. When piping output through head, tail, grep, or similar filters, preserve the original failing exit status with set -o pipefail or avoid the pipe. Do not repeatedly retry the exact same failing shell command; inspect the error and change strategy first.";
const NON_GPT_EDIT_BRIDGE_INSTRUCTIONS: &str = "Editing tool compatibility rule: Prefer the Edit, MultiEdit, and Write tools for file changes when they are available. Kodex converts those Claude-style editing tools into apply_patch internally, so using them satisfies the apply_patch requirement. Use raw apply_patch only as a fallback when the change cannot be represented with Edit, MultiEdit, or Write. Use shell commands only for inspection or validation.";
const NAMESPACE_TOOL_NAME_SEPARATOR: &str = "__";

fn flattened_namespace_tool_name(namespace: &str, name: &str) -> String {
    format!("{namespace}{NAMESPACE_TOOL_NAME_SEPARATOR}{name}")
}

fn split_flattened_namespace_tool_name(name: &str) -> Option<(&str, &str)> {
    let (namespace, tool_name) = name.rsplit_once(NAMESPACE_TOOL_NAME_SEPARATOR)?;
    if namespace.is_empty() || tool_name.is_empty() {
        return None;
    }
    Some((namespace, tool_name))
}

fn namespaced_function_call_item(id: &str, name: &str, arguments: &str, status: &str) -> Value {
    if let Some((namespace, tool_name)) = split_flattened_namespace_tool_name(name) {
        json!({
            "id": id,
            "type": "function_call",
            "call_id": id,
            "namespace": namespace,
            "name": tool_name,
            "arguments": arguments,
            "status": status
        })
    } else {
        json!({
            "id": id,
            "type": "function_call",
            "call_id": id,
            "name": name,
            "arguments": arguments,
            "status": status
        })
    }
}

#[derive(Debug, Clone)]
struct CodexApiProxyConfig {
    provider: String,
    api_key: String,
    api_keys: BTreeMap<String, String>,
    session_ids: BTreeMap<String, String>,
    model_providers: BTreeMap<String, String>,
    provider_configs: BTreeMap<String, ProxyProviderConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyProviderProtocol {
    ChatCompletions,
    Responses,
    AnthropicMessages,
}

#[derive(Debug, Clone)]
struct ProxyProviderConfig {
    base_url: String,
    protocol: ProxyProviderProtocol,
}

static CODEX_API_PROXY_CONFIG: OnceLock<Arc<RwLock<CodexApiProxyConfig>>> = OnceLock::new();
static CODEX_API_PROXY_RUNNING: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static CODEX_API_PROXY_PORT: OnceLock<Arc<RwLock<u16>>> = OnceLock::new();
static DEEPSEEK_REASONING_HISTORY: OnceLock<Arc<RwLock<Vec<ReasoningHistoryEntry>>>> =
    OnceLock::new();

#[derive(Debug, Clone)]
struct ReasoningHistoryEntry {
    content: String,
    assistant_signature: String,
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
                provider: "timiai".to_string(),
                api_key: String::new(),
                api_keys: BTreeMap::new(),
                session_ids: BTreeMap::new(),
                model_providers: BTreeMap::new(),
                provider_configs: BTreeMap::new(),
            }))
        })
        .clone();
    if let Ok(mut current) = config.write() {
        let provider = normalize_proxy_provider(provider);
        current.provider = provider.to_string();
        current.api_key = api_key.to_string();
        current
            .api_keys
            .insert(provider.to_string(), api_key.to_string());
        current
            .session_ids
            .entry(provider.to_string())
            .or_insert_with(|| uuid::Uuid::new_v4().to_string());
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

pub fn configure_codex_api_proxy_model_provider_map(value: &str) {
    let (model_providers, provider_configs, duplicate_count) = match parse_model_provider_map(value)
    {
        Ok(parsed) => parsed,
        Err(error) => {
            append_codex_api_proxy_log(&format!("model_provider_map_parse_failed error={error}"));
            return;
        }
    };

    let config = CODEX_API_PROXY_CONFIG
        .get_or_init(|| {
            Arc::new(RwLock::new(CodexApiProxyConfig {
                provider: "timiai".to_string(),
                api_key: String::new(),
                api_keys: BTreeMap::new(),
                session_ids: BTreeMap::new(),
                model_providers: BTreeMap::new(),
                provider_configs: BTreeMap::new(),
            }))
        })
        .clone();
    let count = model_providers.len();
    if let Ok(mut current) = config.write() {
        current.model_providers = model_providers;
        current.provider_configs = provider_configs;
    }
    append_codex_api_proxy_log(&format!(
        "model_provider_map_configured entries={count} duplicates={duplicate_count}"
    ));
}

pub fn clear_codex_api_proxy_model_provider_map() {
    let config = CODEX_API_PROXY_CONFIG
        .get_or_init(|| {
            Arc::new(RwLock::new(CodexApiProxyConfig {
                provider: "timiai".to_string(),
                api_key: String::new(),
                api_keys: BTreeMap::new(),
                session_ids: BTreeMap::new(),
                model_providers: BTreeMap::new(),
                provider_configs: BTreeMap::new(),
            }))
        })
        .clone();
    if let Ok(mut current) = config.write() {
        current.model_providers.clear();
        current.provider_configs.clear();
    }
    append_codex_api_proxy_log("model_provider_map_cleared");
}

/// Register an API key for an additional provider without changing the active
/// provider/key.
///
/// Unlike `ensure_codex_api_proxy`, this only inserts into the per-provider
/// `api_keys` (and `session_ids`) map and does **not** mutate
/// `config.provider` / `config.api_key`. This makes it safe to call from a
/// secondary caller (e.g. the `kodex-image` `view_image` tool) mid-session
/// without disrupting the active agent's proxy routing, while still letting a
/// request that pins this provider via the request path
/// (`/providers/{provider}/responses`) resolve the key through
/// `api_key_for_proxy_provider`.
pub fn register_codex_api_proxy_provider_key(provider: &str, api_key: &str) {
    if api_key.trim().is_empty() {
        return;
    }
    let provider = normalize_proxy_provider(provider).to_string();
    let config = CODEX_API_PROXY_CONFIG
        .get_or_init(|| {
            Arc::new(RwLock::new(CodexApiProxyConfig {
                provider: "timiai".to_string(),
                api_key: String::new(),
                api_keys: BTreeMap::new(),
                session_ids: BTreeMap::new(),
                model_providers: BTreeMap::new(),
                provider_configs: BTreeMap::new(),
            }))
        })
        .clone();
    if let Ok(mut current) = config.write() {
        current
            .api_keys
            .entry(provider.clone())
            .or_insert_with(|| api_key.to_string());
        current
            .session_ids
            .entry(provider)
            .or_insert_with(|| uuid::Uuid::new_v4().to_string());
    }
    append_codex_api_proxy_log("provider_key_registered (non-active)");
}

fn parse_model_provider_map(
    value: &str,
) -> anyhow::Result<(
    BTreeMap<String, String>,
    BTreeMap<String, ProxyProviderConfig>,
    usize,
)> {
    let parsed: Value = serde_json::from_str(value)?;
    let Some(entries) = parsed.as_array() else {
        anyhow::bail!("expected_array");
    };
    let mut duplicate_count = 0usize;
    let mut model_providers = BTreeMap::new();
    let mut provider_configs = BTreeMap::new();
    for entry in entries {
        let provider = entry
            .get("provider")
            .and_then(Value::as_str)
            .map(normalize_proxy_provider)
            .unwrap_or_else(|| "timiai".to_string());
        if let (Some(base_url), Some(protocol)) = (
            entry
                .get("base_url")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty()),
            entry
                .get("protocol")
                .and_then(Value::as_str)
                .and_then(parse_proxy_provider_protocol),
        ) {
            provider_configs
                .entry(provider.clone())
                .or_insert_with(|| ProxyProviderConfig {
                    base_url: base_url.to_string(),
                    protocol,
                });
        }
        // Only the fully-qualified `model` slug (which embeds the source
        // provider, e.g. kodex-provider/byok/custom_cline/glm-5.2) is used as a
        // routing key. The bare `display_name` (e.g. "glm-5.2") is intentionally
        // NOT indexed: two different providers can share the same model name,
        // and indexing it would let one provider's entry shadow another's.
        if let Some(model) = entry
            .get("model")
            .and_then(Value::as_str)
            .map(normalized_model_key)
            .filter(|model| !model.is_empty())
        {
            match model_providers.entry(model) {
                Entry::Vacant(entry) => {
                    entry.insert(provider.clone());
                }
                Entry::Occupied(_) => {
                    duplicate_count += 1;
                }
            }
        }
    }
    Ok((model_providers, provider_configs, duplicate_count))
}

fn parse_proxy_provider_protocol(protocol: &str) -> Option<ProxyProviderProtocol> {
    match protocol.trim().to_ascii_lowercase().as_str() {
        "chat_completions" | "chat-completions" | "chat" => {
            Some(ProxyProviderProtocol::ChatCompletions)
        }
        "responses" | "response" => Some(ProxyProviderProtocol::Responses),
        "anthropic_messages" | "anthropic-messages" | "messages" => {
            Some(ProxyProviderProtocol::AnthropicMessages)
        }
        _ => None,
    }
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
    if request.method() != Method::POST {
        return Ok(response_with_status(
            StatusCode::NOT_FOUND,
            "not found".to_string(),
            "text/plain; charset=utf-8",
        ));
    }
    let path = request.uri().path().to_string();
    let explicit_provider = proxy_provider_from_path(&path);
    if path.ends_with("/messages") {
        return proxy_anthropic_messages_request(request, config, explicit_provider).await;
    }
    if path.ends_with("/responses/compact") {
        return proxy_native_codex_responses_compact_request(request, config, explicit_provider)
            .await;
    }
    if !path.ends_with("/responses") {
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
            provider: "timiai".to_string(),
            api_key: String::new(),
            api_keys: BTreeMap::new(),
            session_ids: BTreeMap::new(),
            model_providers: BTreeMap::new(),
            provider_configs: BTreeMap::new(),
        });
    let payload: Value = serde_json::from_slice(&body)?;
    let requested_stream = payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let requested_model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let provider_model = decode_provider_model_id(&requested_model);
    let routing_model = provider_model
        .as_ref()
        .map(|model| model.model.as_str())
        .unwrap_or(requested_model.as_str());
    let provider = explicit_provider
        .clone()
        .or_else(|| mapped_proxy_provider_for_model(&requested_model, &config.model_providers))
        .or_else(|| provider_model.as_ref().map(|model| model.provider.clone()))
        .or_else(|| mapped_proxy_provider_for_model(routing_model, &config.model_providers))
        .unwrap_or_else(|| {
            proxy_provider_for_model(routing_model, &config.provider, &config.model_providers)
        });
    let payload = provider_model
        .as_ref()
        .map(|model| replace_payload_model(payload.clone(), &model.model))
        .unwrap_or(payload);
    let payload = prepare_responses_payload_for_provider(payload, &provider);
    log_responses_payload_summary("responses_request", &payload, &provider);
    let api_key = api_key_for_proxy_provider(&config, &provider);
    if api_key.trim().is_empty() {
        return Ok(response_with_status(
            StatusCode::UNAUTHORIZED,
            json!({ "error": { "message": format!("API key is not configured for {provider}") } })
                .to_string(),
            "application/json",
        ));
    }
    if let Some(custom_config) = config.provider_configs.get(&provider).cloned() {
        return proxy_custom_codex_responses_request(
            payload,
            &api_key,
            &provider,
            &custom_config,
            requested_stream,
        )
        .await;
    }
    let chat_payload = match responses_payload_to_chat_payload(payload, &provider) {
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
    let chat_payload = normalize_chat_payload_for_provider(chat_payload, &provider);
    if normalize_proxy_provider(&provider) == "timiai" {
        log_chat_payload_summary("timiai_chat_completions_request", &chat_payload);
        let session_id = session_id_for_proxy_provider(&config, &provider);
        return proxy_chat_completions_codex_responses_request(
            chat_payload,
            &api_key,
            &provider,
            TIMIAI_CHAT_COMPLETIONS_URL,
            requested_stream,
            Some(&session_id),
        )
        .await;
    }
    match normalize_proxy_provider(&provider).as_str() {
        "commandcode" => log_chat_payload_summary("commandcode_request", &chat_payload),
        "deepseek" => log_chat_payload_summary("deepseek_request", &chat_payload),
        "xiaomi_mimo" => log_chat_payload_summary("xiaomi_request", &chat_payload),
        _ => {}
    }
    if provider == "kimi_code" {
        return proxy_kimi_codex_api_request(chat_payload, &api_key, requested_stream).await;
    }
    let upstream_url = upstream_chat_completions_url(&provider);

    proxy_chat_completions_codex_responses_request(
        chat_payload,
        &api_key,
        &provider,
        upstream_url,
        requested_stream,
        None,
    )
    .await
}

async fn proxy_custom_codex_responses_request(
    payload: Value,
    api_key: &str,
    provider: &str,
    custom_config: &ProxyProviderConfig,
    requested_stream: bool,
) -> anyhow::Result<Response<ProxyBody>> {
    match custom_config.protocol {
        ProxyProviderProtocol::Responses => {
            proxy_native_responses_request(payload, api_key, provider, &custom_config.base_url)
                .await
        }
        ProxyProviderProtocol::ChatCompletions => {
            let chat_payload = responses_payload_to_chat_payload(payload, provider)?;
            let chat_payload = normalize_chat_payload_for_provider(chat_payload, provider);
            proxy_chat_completions_codex_responses_request(
                chat_payload,
                api_key,
                provider,
                &custom_config.base_url,
                requested_stream,
                None,
            )
            .await
        }
        ProxyProviderProtocol::AnthropicMessages => {
            let chat_payload = responses_payload_to_chat_payload(payload, provider)?;
            proxy_anthropic_messages_codex_responses_request(
                chat_payload,
                api_key,
                provider,
                &custom_config.base_url,
                requested_stream,
            )
            .await
        }
    }
}

async fn proxy_native_responses_request(
    payload: Value,
    api_key: &str,
    provider: &str,
    upstream_url: &str,
) -> anyhow::Result<Response<ProxyBody>> {
    let client = reqwest::Client::new();
    let upstream = client
        .post(upstream_url)
        .header(CONTENT_TYPE, "application/json")
        .bearer_auth(api_key)
        .body(serde_json::to_vec(&payload)?)
        .send()
        .await?;
    let status = StatusCode::from_u16(upstream.status().as_u16())?;
    let content_type = upstream
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    append_codex_api_proxy_log(&format!(
        "native_responses_upstream_response provider={} url={} status={} content_type={}",
        normalize_proxy_provider(provider),
        upstream_url,
        status.as_u16(),
        content_type,
    ));
    if is_event_stream(&content_type) {
        return Ok(streaming_passthrough_response(
            upstream,
            status,
            &content_type,
        ));
    }
    let body = upstream.bytes().await?;
    let status = normalize_upstream_error_status(status, body.as_ref());
    let mut response = Response::new(full_body(body));
    *response.status_mut() = status;
    if let Ok(value) = content_type.parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    Ok(response)
}

async fn proxy_anthropic_messages_codex_responses_request(
    chat_payload: Value,
    api_key: &str,
    provider: &str,
    upstream_url: &str,
    requested_stream: bool,
) -> anyhow::Result<Response<ProxyBody>> {
    let anthropic_payload =
        chat_payload_to_anthropic_payload(chat_payload, requested_stream);
    log_anthropic_payload_summary(
        &format!("{}_request", normalize_proxy_provider(provider)),
        &anthropic_payload,
    );
    let client = reqwest::Client::new();
    let upstream = client
        .post(upstream_url)
        .header(CONTENT_TYPE, "application/json")
        .header("User-Agent", "claude-code/0.2.0")
        .header("x-api-key", api_key)
        .body(serde_json::to_vec(&anthropic_payload)?)
        .send()
        .await?;
    let status = StatusCode::from_u16(upstream.status().as_u16())?;
    let content_type = upstream
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    if is_event_stream(&content_type) && status.is_success() {
        append_codex_api_proxy_log(&format!(
            "codex_stream_convert provider={} upstream=anthropic_messages downstream=responses",
            normalize_proxy_provider(provider)
        ));
        return Ok(streaming_anthropic_sse_to_responses_response(upstream, status));
    }
    let body = upstream.bytes().await?;
    let mut response_content_type = content_type.clone();
    let body = if status.is_success() {
        let anthropic_response: Value = serde_json::from_slice(body.as_ref())?;
        let response = anthropic_response_to_responses_response(anthropic_response);
        if requested_stream {
            response_content_type = "text/event-stream".to_string();
            responses_response_to_sse(&response)
        } else {
            serde_json::to_vec(&response)?
        }
    } else {
        log_suspicious_upstream_response(status, &content_type, body.as_ref());
        body.to_vec()
    };
    let mut response = Response::new(full_body(Bytes::from(body)));
    *response.status_mut() = status;
    if let Ok(value) = response_content_type.parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    Ok(response)
}
async fn proxy_chat_completions_codex_responses_request(
    chat_payload: Value,
    api_key: &str,
    provider: &str,
    upstream_url: &str,
    requested_stream: bool,
    session_id: Option<&str>,
) -> anyhow::Result<Response<ProxyBody>> {
    let client = reqwest::Client::new();
    let request = client
        .post(upstream_url)
        .header(CONTENT_TYPE, "application/json");
    let request = if normalize_proxy_provider(provider) == "timiai" {
        with_timiai_headers(request, api_key, session_id.unwrap_or_default())
    } else {
        request.bearer_auth(api_key)
    };
    let request_body = serde_json::to_vec(&chat_payload)?;
    let upstream = match request.body(request_body).send().await {
        Ok(upstream) => upstream,
        Err(error) => {
            log_chat_completions_upstream_error(&provider, upstream_url, &chat_payload, &error);
            return Err(error.into());
        }
    };
    let status = StatusCode::from_u16(upstream.status().as_u16())?;
    let content_type = upstream
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    log_chat_completions_upstream_response(
        &provider,
        upstream_url,
        &chat_payload,
        status,
        &content_type,
    );
    if is_event_stream(&content_type) {
        if status.is_success() {
            append_codex_api_proxy_log(&format!(
                "codex_stream_convert provider={provider} upstream=chat_completions downstream=responses"
            ));
            return Ok(streaming_chat_sse_response(upstream, status));
        }
        return Ok(streaming_passthrough_response(
            upstream,
            status,
            &content_type,
        ));
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
        log_suspicious_upstream_response(status, &content_type, body.as_ref());
        body.to_vec()
    };
    log_suspicious_upstream_response(status, &content_type, &body);

    let mut response = Response::new(full_body(Bytes::from(body)));
    *response.status_mut() = status;
    if let Ok(value) = response_content_type.parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    Ok(response)
}

async fn proxy_native_codex_responses_compact_request(
    request: Request<Incoming>,
    config: Arc<RwLock<CodexApiProxyConfig>>,
    explicit_provider: Option<String>,
) -> anyhow::Result<Response<ProxyBody>> {
    let body = request.into_body().collect().await?.to_bytes();
    let config = config
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_else(|_| CodexApiProxyConfig {
            provider: "timiai".to_string(),
            api_key: String::new(),
            api_keys: BTreeMap::new(),
            session_ids: BTreeMap::new(),
            model_providers: BTreeMap::new(),
            provider_configs: BTreeMap::new(),
        });
    let provider = explicit_provider
        .clone()
        .unwrap_or_else(|| normalize_proxy_provider(&config.provider));
    if provider != "timiai" {
        return Ok(response_with_status(
            StatusCode::NOT_FOUND,
            "not found".to_string(),
            "text/plain; charset=utf-8",
        ));
    }
    let api_key = api_key_for_proxy_provider(&config, &provider);
    if api_key.trim().is_empty() {
        return Ok(response_with_status(
            StatusCode::UNAUTHORIZED,
            json!({ "error": { "message": "API key is not configured for timiai" } }).to_string(),
            "application/json",
        ));
    }

    let session_id = session_id_for_proxy_provider(&config, &provider);
    let client = reqwest::Client::new();
    let upstream = with_timiai_headers(
        client
            .post(TIMIAI_RESPONSES_COMPACT_URL)
            .header(CONTENT_TYPE, "application/json"),
        &api_key,
        &session_id,
    )
    .body(body)
    .send()
    .await?;
    let status = StatusCode::from_u16(upstream.status().as_u16())?;
    let content_type = upstream
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let body = upstream.bytes().await?;
    let mut response = Response::new(full_body(body));
    *response.status_mut() = status;
    if let Ok(value) = content_type.parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    Ok(response)
}

async fn proxy_anthropic_messages_request(
    request: Request<Incoming>,
    config: Arc<RwLock<CodexApiProxyConfig>>,
    explicit_provider: Option<String>,
) -> anyhow::Result<Response<ProxyBody>> {
    let body = request.into_body().collect().await?.to_bytes();
    let config = config
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_else(|_| CodexApiProxyConfig {
            provider: "timiai".to_string(),
            api_key: String::new(),
            api_keys: BTreeMap::new(),
            session_ids: BTreeMap::new(),
            model_providers: BTreeMap::new(),
            provider_configs: BTreeMap::new(),
        });
    let payload: Value = serde_json::from_slice(&body)?;
    let requested_model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let provider_model = decode_provider_model_id(&requested_model);
    let routing_model = provider_model
        .as_ref()
        .map(|model| model.model.as_str())
        .unwrap_or(requested_model.as_str());
    let provider = explicit_provider
        .clone()
        .or_else(|| mapped_proxy_provider_for_model(&requested_model, &config.model_providers))
        .or_else(|| provider_model.as_ref().map(|model| model.provider.clone()))
        .or_else(|| mapped_proxy_provider_for_model(routing_model, &config.model_providers))
        .unwrap_or_else(|| {
            proxy_provider_for_model(routing_model, &config.provider, &config.model_providers)
        });
    let payload = provider_model
        .as_ref()
        .map(|model| replace_payload_model(payload.clone(), &model.model))
        .unwrap_or(payload);
    log_anthropic_payload_summary(
        &format!("anthropic_messages_request provider={provider}"),
        &payload,
    );
    let api_key = api_key_for_proxy_provider(&config, &provider);
    if api_key.trim().is_empty() {
        return Ok(response_with_status(
            StatusCode::UNAUTHORIZED,
            json!({ "error": { "message": format!("API key is not configured for {provider}") } })
                .to_string(),
            "application/json",
        ));
    }

    let session_id = session_id_for_proxy_provider(&config, &provider);
    if let Some(custom_config) = config.provider_configs.get(&provider).cloned() {
        return proxy_custom_anthropic_messages_request(
            payload,
            &api_key,
            &provider,
            &session_id,
            &custom_config,
        )
        .await;
    }
    if should_bridge_anthropic_messages_to_chat_completions(&provider, routing_model) {
        return proxy_completion_to_anthropic_messages_request(
            payload,
            &api_key,
            &provider,
            &session_id,
        )
        .await;
    }
    match normalize_proxy_provider(&provider).as_str() {
        "commandcode" | "kimi_code" | "xiaomi_mimo" | "timiai" => {
            proxy_native_anthropic_messages_request(payload, &api_key, &provider, &session_id).await
        }
        _ => {
            proxy_completion_to_anthropic_messages_request(
                payload,
                &api_key,
                &provider,
                &session_id,
            )
            .await
        }
    }
}

async fn proxy_custom_anthropic_messages_request(
    payload: Value,
    api_key: &str,
    provider: &str,
    session_id: &str,
    custom_config: &ProxyProviderConfig,
) -> anyhow::Result<Response<ProxyBody>> {
    match custom_config.protocol {
        ProxyProviderProtocol::AnthropicMessages => {
            proxy_native_anthropic_messages_request_with_url(
                payload,
                api_key,
                provider,
                session_id,
                &custom_config.base_url,
            )
            .await
        }
        ProxyProviderProtocol::ChatCompletions => {
            proxy_completion_to_anthropic_messages_request_with_url(
                payload,
                api_key,
                provider,
                session_id,
                &custom_config.base_url,
            )
            .await
        }
        ProxyProviderProtocol::Responses => {
            proxy_responses_to_anthropic_messages_request(
                payload,
                api_key,
                provider,
                &custom_config.base_url,
            )
            .await
        }
    }
}

async fn proxy_responses_to_anthropic_messages_request(
    payload: Value,
    api_key: &str,
    provider: &str,
    upstream_url: &str,
) -> anyhow::Result<Response<ProxyBody>> {
    let requested_stream = payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let responses_payload = anthropic_payload_to_responses_payload(payload);
    let client = reqwest::Client::new();
    let upstream = client
        .post(upstream_url)
        .header(CONTENT_TYPE, "application/json")
        .bearer_auth(api_key)
        .body(serde_json::to_vec(&responses_payload)?)
        .send()
        .await?;
    let status = StatusCode::from_u16(upstream.status().as_u16())?;
    let content_type = upstream
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    if is_event_stream(&content_type) && status.is_success() {
        append_codex_api_proxy_log(&format!(
            "anthropic_stream_convert provider={} upstream=responses downstream=anthropic_messages",
            normalize_proxy_provider(provider)
        ));
        return Ok(streaming_responses_sse_to_anthropic_response(upstream, status));
    }
    let body = upstream.bytes().await?;
    let mut response_content_type = content_type.clone();
    let body = if status.is_success() {
        let responses_response: Value = serde_json::from_slice(body.as_ref())?;
        let anthropic_response = responses_response_to_anthropic_response(responses_response);
        if requested_stream {
            response_content_type = "text/event-stream".to_string();
            anthropic_response_to_sse(&anthropic_response)
        } else {
            serde_json::to_vec(&anthropic_response)?
        }
    } else {
        append_codex_api_proxy_log(&format!(
            "responses_to_anthropic_upstream_error provider={} url={} status={}",
            normalize_proxy_provider(provider),
            upstream_url,
            status.as_u16()
        ));
        body.to_vec()
    };
    let mut response = Response::new(full_body(Bytes::from(body)));
    *response.status_mut() = status;
    if let Ok(value) = response_content_type.parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    Ok(response)
}
async fn proxy_native_anthropic_messages_request(
    payload: Value,
    api_key: &str,
    provider: &str,
    session_id: &str,
) -> anyhow::Result<Response<ProxyBody>> {
    let upstream_url = upstream_messages_url(provider);
    proxy_native_anthropic_messages_request_with_url(
        payload,
        api_key,
        provider,
        session_id,
        upstream_url,
    )
    .await
}

async fn proxy_native_anthropic_messages_request_with_url(
    payload: Value,
    api_key: &str,
    provider: &str,
    session_id: &str,
    upstream_url: &str,
) -> anyhow::Result<Response<ProxyBody>> {
    let requested_stream = payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let client = reqwest::Client::new();
    let normalized_provider = normalize_proxy_provider(provider);
    let payload = if normalized_provider == "timiai" {
        sanitize_timiai_anthropic_messages_payload(normalize_native_anthropic_payload(
            payload, provider,
        ))
    } else {
        normalize_native_anthropic_payload(payload, provider)
    };
    let (auth_header_name, auth_log_state, session_log_state) = if normalized_provider == "timiai" {
        (
            "Authorization",
            timiai_authorization_log_state(api_key),
            "present",
        )
    } else {
        (
            "x-api-key",
            if api_key.trim().is_empty() {
                "empty"
            } else {
                "present"
            },
            "not_sent",
        )
    };
    let x_api_key_log_state = if api_key.trim().is_empty() {
        "empty"
    } else {
        "present"
    };
    append_codex_api_proxy_log(&format!(
        "native_anthropic_upstream_request provider={} method=POST url={} model={} downstream_stream={} upstream_stream={} tools={} auth_header={}:{} x_api_key={} x_session_id={}",
        normalized_provider,
        upstream_url,
        payload
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        requested_stream,
        payload
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        payload
            .get("tools")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0),
        auth_header_name,
        auth_log_state,
        x_api_key_log_state,
        session_log_state
    ));
    let request = client
        .post(upstream_url)
        .header(CONTENT_TYPE, "application/json")
        .header("User-Agent", "claude-code/0.2.0");
    let request = if normalize_proxy_provider(provider) == "timiai" {
        with_timiai_headers(request, api_key, session_id)
    } else {
        request.header("x-api-key", api_key)
    };
    let request_body = serde_json::to_vec(&payload)?;
    let upstream = match request.body(request_body).send().await {
        Ok(upstream) => upstream,
        Err(error) => {
            log_native_anthropic_upstream_send_error(provider, upstream_url, &payload, &error);
            return Err(error.into());
        }
    };
    let status = StatusCode::from_u16(upstream.status().as_u16())?;
    let content_type = upstream
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    log_native_anthropic_upstream_response(provider, upstream_url, &payload, status, &content_type);
    if is_event_stream(&content_type) {
        return Ok(streaming_passthrough_response(
            upstream,
            status,
            &content_type,
        ));
    }
    let body = upstream.bytes().await?;
    let status = normalize_upstream_error_status(status, body.as_ref());
    if !status.is_success() {
        append_codex_api_proxy_log(&format!(
            "native_anthropic_upstream_error provider={} url={} model={} status={}",
            normalize_proxy_provider(provider),
            upstream_url,
            payload
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            status.as_u16()
        ));
        log_suspicious_upstream_response(status, &content_type, body.as_ref());
    }
    let mut response = Response::new(full_body(body));
    *response.status_mut() = status;
    if let Ok(value) = content_type.parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    Ok(response)
}

async fn proxy_completion_to_anthropic_messages_request(
    payload: Value,
    api_key: &str,
    provider: &str,
    session_id: &str,
) -> anyhow::Result<Response<ProxyBody>> {
    let upstream_url = upstream_chat_completions_url(provider);
    proxy_completion_to_anthropic_messages_request_with_url(
        payload,
        api_key,
        provider,
        session_id,
        upstream_url,
    )
    .await
}

async fn proxy_completion_to_anthropic_messages_request_with_url(
    payload: Value,
    api_key: &str,
    provider: &str,
    session_id: &str,
    upstream_url: &str,
) -> anyhow::Result<Response<ProxyBody>> {
    let requested_stream = payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let chat_payload = anthropic_payload_to_chat_payload(payload);
    let chat_payload = normalize_chat_payload_for_provider(chat_payload, provider);
    log_chat_payload_summary(
        &format!(
            "{}_anthropic_chat_bridge_request",
            normalize_proxy_provider(provider)
        ),
        &chat_payload,
    );
    let client = reqwest::Client::new();
    let request = client
        .post(upstream_url)
        .header(CONTENT_TYPE, "application/json");
    let request = if normalize_proxy_provider(provider) == "timiai" {
        with_timiai_headers(request, api_key, session_id)
    } else {
        request.bearer_auth(api_key)
    };
    let upstream = match request
        .body(serde_json::to_vec(&chat_payload)?)
        .send()
        .await
    {
        Ok(upstream) => upstream,
        Err(error) => {
            log_chat_completions_upstream_error(provider, upstream_url, &chat_payload, &error);
            return Err(error.into());
        }
    };
    let status = StatusCode::from_u16(upstream.status().as_u16())?;
    let content_type = upstream
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    if is_event_stream(&content_type) {
        return Ok(streaming_chat_sse_to_anthropic_response(upstream, status));
    }

    let body = upstream.bytes().await?;
    let status = normalize_upstream_error_status(status, body.as_ref());
    let body = if status.is_success() {
        let chat_response: Value = serde_json::from_slice(body.as_ref())?;
        serde_json::to_vec(&chat_response_to_anthropic_response(chat_response))?
    } else {
        log_suspicious_upstream_response(status, &content_type, body.as_ref());
        body.to_vec()
    };
    let response_content_type = content_type;
    if requested_stream && status.is_success() {
        let response: Value = serde_json::from_slice(&body)?;
        let body = anthropic_response_to_sse(&response);
        let mut response = Response::new(full_body(Bytes::from(body)));
        *response.status_mut() = status;
        response.headers_mut().insert(
            CONTENT_TYPE,
            "text/event-stream".parse().expect("valid content type"),
        );
        return Ok(response);
    }
    let mut response = Response::new(full_body(Bytes::from(body)));
    *response.status_mut() = status;
    if let Ok(value) = response_content_type.parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    Ok(response)
}

fn api_key_for_proxy_provider(config: &CodexApiProxyConfig, provider: &str) -> String {
    config.api_keys.get(provider).cloned().unwrap_or_else(|| {
        (normalize_proxy_provider(&config.provider) == provider)
            .then(|| config.api_key.clone())
            .unwrap_or_default()
    })
}

fn session_id_for_proxy_provider(config: &CodexApiProxyConfig, provider: &str) -> String {
    config
        .session_ids
        .get(provider)
        .cloned()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

fn normalize_native_anthropic_payload(mut payload: Value, provider: &str) -> Value {
    let Some(model) = payload.get("model").and_then(Value::as_str) else {
        return payload;
    };
    let upstream_model = upstream_native_anthropic_model(provider, model).to_string();
    if upstream_model == model {
        return payload;
    }
    append_codex_api_proxy_log(&format!(
        "anthropic_model_rewrite provider={} model={} upstream_model={}",
        normalize_proxy_provider(provider),
        model,
        upstream_model
    ));
    if let Some(object) = payload.as_object_mut() {
        object.insert("model".to_string(), Value::String(upstream_model));
    }
    payload
}

fn prepare_responses_payload_for_provider(payload: Value, provider: &str) -> Value {
    if normalize_proxy_provider(provider) == "timiai" {
        sanitize_timiai_responses_payload(payload)
    } else {
        payload
    }
}

fn sanitize_timiai_responses_payload(mut payload: Value) -> Value {
    let Some(object) = payload.as_object_mut() else {
        return payload;
    };
    let mut removed = Vec::<String>::new();
    for key in ["context_management", "reasoning"] {
        if object.remove(key).is_some() {
            removed.push(key.to_string());
        }
    }
    if let Some(input) = object.get_mut("input") {
        removed.extend(sanitize_timiai_responses_input(input));
    }
    if !removed.is_empty() {
        append_codex_api_proxy_log(&format!(
            "timiai_responses_payload_sanitized removed={}",
            removed.join(",")
        ));
    }
    payload
}

fn sanitize_timiai_responses_input(input: &mut Value) -> Vec<String> {
    let Some(items) = input.as_array_mut() else {
        return Vec::new();
    };

    let mut removed_reasoning = 0usize;
    let mut removed_commentary = 0usize;
    let mut retained = Vec::with_capacity(items.len());
    for item in std::mem::take(items) {
        if item.get("type").and_then(Value::as_str) == Some("reasoning") {
            removed_reasoning += 1;
            continue;
        }
        if item_is_timiai_unsupported_commentary_message(&item) {
            removed_commentary += 1;
            continue;
        }
        retained.push(item);
    }
    *items = retained;

    let mut removed = Vec::new();
    if removed_reasoning > 0 {
        removed.push(format!("input.reasoning:{removed_reasoning}"));
    }
    if removed_commentary > 0 {
        removed.push(format!("input.commentary:{removed_commentary}"));
    }
    removed
}

fn item_is_timiai_unsupported_commentary_message(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("message")
        && item.get("role").and_then(Value::as_str) == Some("assistant")
        && item.get("phase").and_then(Value::as_str) == Some("commentary")
}

fn sanitize_timiai_anthropic_messages_payload(mut payload: Value) -> Value {
    let Some(object) = payload.as_object_mut() else {
        return payload;
    };
    let mut removed = Vec::new();
    for key in ["context_management"] {
        if object.remove(key).is_some() {
            removed.push(key);
        }
    }
    if !removed.is_empty() {
        append_codex_api_proxy_log(&format!(
            "timiai_anthropic_payload_sanitized removed={}",
            removed.join(",")
        ));
    }
    payload
}

fn normalize_upstream_error_status(status: StatusCode, body: &[u8]) -> StatusCode {
    if status != StatusCode::BAD_REQUEST {
        return status;
    }
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return status;
    };
    let Some(error) = value.get("error") else {
        return status;
    };
    let code = error.get("code").and_then(Value::as_str);
    let error_type = error.get("type").and_then(Value::as_str);
    if code == Some("429") || error_type == Some("router_queue_limitation") {
        return StatusCode::TOO_MANY_REQUESTS;
    }
    status
}

async fn proxy_kimi_codex_api_request(
    chat_payload: Value,
    api_key: &str,
    requested_stream: bool,
) -> anyhow::Result<Response<ProxyBody>> {
    let anthropic_payload =
        chat_payload_to_anthropic_payload(chat_payload, requested_stream);
    log_anthropic_payload_summary("kimi_request", &anthropic_payload);

    let client = reqwest::Client::new();
    let upstream = client
        .post(KIMI_UPSTREAM_MESSAGES_URL)
        .header(CONTENT_TYPE, "application/json")
        .header("User-Agent", "claude-code/0.2.0")
        .header("x-api-key", api_key)
        .body(serde_json::to_vec(&anthropic_payload)?)
        .send()
        .await?;
    let status = StatusCode::from_u16(upstream.status().as_u16())?;
    let content_type = upstream
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())


        .unwrap_or("application/json")
        .to_string();
    if is_event_stream(&content_type) && status.is_success() {
        append_codex_api_proxy_log(
            "codex_stream_convert provider=kimi_code upstream=anthropic_messages downstream=responses",
        );
        return Ok(streaming_anthropic_sse_to_responses_response(upstream, status));
    }
    let body = upstream.bytes().await?;
    let mut response_content_type = content_type.clone();
    let body = if status.is_success() {
        let anthropic_response: Value = serde_json::from_slice(body.as_ref())?;
        let response = anthropic_response_to_responses_response(anthropic_response);
        if requested_stream {
            response_content_type = "text/event-stream".to_string();
            responses_response_to_sse(&response)
        } else {
            serde_json::to_vec(&response)?
        }
    } else {
        log_suspicious_upstream_response(status, &content_type, body.as_ref());
        body.to_vec()
    };
    log_suspicious_upstream_response(status, &content_type, &body);

    let mut response = Response::new(full_body(Bytes::from(body)));
    *response.status_mut() = status;
    if let Ok(value) = response_content_type.parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    Ok(response)
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
                if matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("function_call" | "custom_tool_call")
                ) {
                    pending_tool_calls.push(responses_tool_call_to_chat_tool_call(item)?);
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
            if let Some(content) = response_content_to_chat_content(other) {
                messages.push(json!({ "role": "user", "content": content }));
            }
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

    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let add_edit_bridge_instructions =
        should_add_non_gpt_edit_bridge_instructions(payload.get("tools"), model);

    if let Some(tools) = responses_tools_to_chat_tools(payload.get("tools"), model) {
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
    if add_edit_bridge_instructions {
        chat = add_non_gpt_edit_bridge_instructions(chat);
    }

    Ok(chat)
}

fn normalize_chat_payload_for_provider(mut payload: Value, provider: &str) -> Value {
    payload = normalize_chat_completion_model(payload, provider);
    if !chat_payload_needs_deepseek_reasoning_compat(&payload, provider) {
        return payload;
    }
    let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) else {
        return payload;
    };

    let missing_tool_reasoning = messages
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .filter(|message| chat_message_has_tool_calls(message))
        .filter(|message| {
            message
                .get("reasoning_content")
                .and_then(Value::as_str)
                .is_none_or(|value| value.trim().is_empty())
                && reasoning_content_for_assistant_message(message).is_none()
        })
        .count();
    if missing_tool_reasoning > 0 {
        append_codex_api_proxy_log(&format!(
            "deepseek_thinking_disabled_missing_tool_reasoning count={missing_tool_reasoning}"
        ));
        for message in messages {
            if let Some(object) = message.as_object_mut() {
                object.remove("reasoning_content");
            }
        }
        payload["thinking"] = json!({ "type": "disabled" });
        if let Some(object) = payload.as_object_mut() {
            object.remove("reasoning_effort");
        }
        return payload;
    }

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
        if let Some(reasoning_content) = reasoning_content_for_assistant_message(message)
            .or_else(|| reasoning_content_for_text(content))
        {
            message["reasoning_content"] = Value::String(reasoning_content);
        }
    }
    payload
}

fn chat_message_has_tool_calls(message: &Value) -> bool {
    message
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|tool_calls| !tool_calls.is_empty())
}

fn chat_payload_needs_deepseek_reasoning_compat(payload: &Value, provider: &str) -> bool {
    let normalized_provider = normalize_proxy_provider(provider);
    normalized_provider == "deepseek"
        || (normalized_provider == "timiai"
            && payload
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(|model| normalized_model_key(model).contains("deepseek")))
}

fn normalize_chat_completion_model(mut payload: Value, provider: &str) -> Value {
    let Some(model) = payload.get("model").and_then(Value::as_str) else {
        return payload;
    };
    let upstream_model = upstream_chat_completion_model(provider, model).to_string();
    if upstream_model == model {
        return payload;
    }
    append_codex_api_proxy_log(&format!(
        "chat_model_rewrite provider={} model={} upstream_model={}",
        normalize_proxy_provider(provider),
        model,
        upstream_model
    ));
    if let Some(object) = payload.as_object_mut() {
        object.insert("model".to_string(), Value::String(upstream_model));
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
            let content_value = item.get("content").unwrap_or(&Value::Null);
            let content_text = response_content_to_text(content_value);
            let chat_content = response_content_to_chat_content(content_value);
            let reasoning_content = item
                .get("reasoning_content")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|text| !text.is_empty());
            if chat_content.is_none() && reasoning_content.is_none() {
                return Ok(None);
            }
            let mut message = json!({
                "role": role,
                "content": chat_content.unwrap_or_else(|| Value::String(String::new()))
            });
            if role == "assistant" {
                if let Some(reasoning_content) =
                    reasoning_content.or_else(|| reasoning_content_for_text(&content_text))
                {
                    message["reasoning_content"] = Value::String(reasoning_content);
                }
            }
            Ok(Some(message))
        }
        "function_call" | "custom_tool_call" => Ok(None),
        "function_call_output" | "custom_tool_call_output" => {
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

fn responses_tool_call_to_chat_tool_call(item: &Value) -> anyhow::Result<Value> {
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("call_unknown");
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .context("function_call input item is missing name")?;
    let chat_name = item
        .get("namespace")
        .and_then(Value::as_str)
        .map(|namespace| flattened_namespace_tool_name(namespace, name))
        .unwrap_or_else(|| name.to_string());
    let arguments = if item.get("type").and_then(Value::as_str) == Some("custom_tool_call") {
        let input = item.get("input").and_then(Value::as_str).unwrap_or("");
        serde_json::to_string(&json!({ "patch": input })).unwrap_or_else(|_| "{}".to_string())
    } else {
        item.get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("{}")
            .to_string()
    };
    Ok(json!({
        "id": call_id,
        "type": "function",
        "function": { "name": chat_name, "arguments": arguments }
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
        .or_else(|| item.get("input"))?;
    let chat_content = response_content_to_chat_content(content)?;
    let content_text = response_content_to_text(content);
    let mut message = json!({ "role": role, "content": chat_content });
    if role == "assistant" && normalize_proxy_provider(provider) == "deepseek" {
        if let Some(reasoning_content) = item
            .get("reasoning_content")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|text| !text.trim().is_empty())
            .or_else(|| reasoning_content_for_text(&content_text))
        {
            message["reasoning_content"] = Value::String(reasoning_content);
        }
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

fn response_content_to_chat_content(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => (!text.trim().is_empty()).then(|| Value::String(text.clone())),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(response_content_item_to_chat_part)
                .collect::<Vec<_>>();
            chat_content_from_parts(parts)
        }
        Value::Null => None,
        other => {
            let text = other.to_string();
            (!text.trim().is_empty()).then_some(Value::String(text))
        }
    }
}

fn response_content_item_to_chat_part(item: &Value) -> Option<Value> {
    if let Some(text) = item.as_str().filter(|text| !text.trim().is_empty()) {
        return Some(chat_text_part(text));
    }

    if let Some(text) = item
        .get("text")
        .or_else(|| item.get("output_text"))
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        return Some(chat_text_part(text));
    }

    chat_image_url_from_value(item).map(|url| chat_image_url_part(&url))
}

fn chat_content_from_parts(parts: Vec<Value>) -> Option<Value> {
    chat_content_from_parts_with_text_separator(parts, "")
}

fn chat_content_from_parts_with_text_separator(
    parts: Vec<Value>,
    separator: &str,
) -> Option<Value> {
    if parts.is_empty() {
        return None;
    }
    let has_non_text = parts
        .iter()
        .any(|part| part.get("type").and_then(Value::as_str) != Some("text"));
    if has_non_text {
        return Some(Value::Array(parts));
    }

    let text = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(separator);
    (!text.trim().is_empty()).then_some(Value::String(text))
}

fn chat_text_part(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

fn chat_image_url_part(url: &str) -> Value {
    json!({ "type": "image_url", "image_url": { "url": url } })
}

fn chat_image_url_from_value(value: &Value) -> Option<String> {
    value
        .get("image_url")
        .and_then(image_url_from_chat_image_value)
        .or_else(|| value.get("url").and_then(Value::as_str).map(str::to_string))
        .or_else(|| {
            value
                .get("source")
                .and_then(anthropic_source_to_chat_image_url)
        })
}

fn image_url_from_chat_image_value(value: &Value) -> Option<String> {
    match value {
        Value::String(url) => Some(url.clone()),
        Value::Object(_) => value.get("url").and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

fn anthropic_source_to_chat_image_url(source: &Value) -> Option<String> {
    match source.get("type").and_then(Value::as_str) {
        Some("base64") => {
            let data = source.get("data").and_then(Value::as_str)?;
            let media_type = source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            Some(format!("data:{media_type};base64,{data}"))
        }
        Some("url") => source
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => source.as_str().map(str::to_string).or_else(|| {
            source
                .get("url")
                .and_then(Value::as_str)
                .map(str::to_string)
        }),
    }
}

fn chat_content_to_anthropic_blocks(value: &Value) -> Vec<Value> {
    match value {
        Value::String(text) => (!text.trim().is_empty())
            .then(|| json!({ "type": "text", "text": text }))
            .into_iter()
            .collect(),
        Value::Array(items) => items
            .iter()
            .filter_map(chat_content_item_to_anthropic_block)
            .collect(),
        Value::Null => Vec::new(),
        other => vec![json!({ "type": "text", "text": other.to_string() })],
    }
}

fn chat_content_item_to_anthropic_block(item: &Value) -> Option<Value> {
    if let Some(text) = item.as_str().filter(|text| !text.trim().is_empty()) {
        return Some(json!({ "type": "text", "text": text }));
    }

    if let Some(text) = item
        .get("text")
        .or_else(|| item.get("output_text"))
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        return Some(json!({ "type": "text", "text": text }));
    }

    chat_image_url_from_value(item).map(|url| image_url_to_anthropic_block(&url))
}

fn image_url_to_anthropic_block(url: &str) -> Value {
    if let Some((media_type, data)) = parse_base64_data_url(url) {
        return json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": media_type,
                "data": data
            }
        });
    }

    json!({
        "type": "image",
        "source": {
            "type": "url",
            "url": url
        }
    })
}

fn parse_base64_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.trim().strip_prefix("data:")?;
    let (metadata, data) = rest.split_once(',')?;
    let mut metadata_parts = metadata.split(';');
    let media_type = metadata_parts.next()?.trim();
    if media_type.is_empty()
        || !metadata_parts.any(|part| part.eq_ignore_ascii_case("base64"))
        || data.is_empty()
    {
        return None;
    }
    Some((media_type.to_string(), data.to_string()))
}

fn add_system_instruction(mut chat: Value, instruction: &str) -> Value {
    let message = json!({
        "role": "system",
        "content": instruction
    });
    match chat.get_mut("messages").and_then(Value::as_array_mut) {
        Some(messages) => {
            let insert_at = messages
                .iter()
                .position(|message| {
                    !matches!(
                        message.get("role").and_then(Value::as_str),
                        Some("system" | "developer")
                    )
                })
                .unwrap_or(messages.len());
            messages.insert(insert_at, message);
        }
        None => {
            chat["messages"] = Value::Array(vec![message]);
        }
    }
    chat
}

fn add_non_gpt_edit_bridge_instructions(chat: Value) -> Value {
    add_system_instruction(chat, NON_GPT_EDIT_BRIDGE_INSTRUCTIONS)
}

fn chat_payload_to_anthropic_payload(mut chat: Value, stream: bool) -> Value {
    chat["stream"] = Value::Bool(stream);
    let mut system_parts = Vec::new();
    let mut anthropic_messages = Vec::new();

    if let Some(messages) = chat.get("messages").and_then(Value::as_array) {
        for message in messages {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            if role == "system" || role == "developer" {
                let system =
                    response_content_to_text(message.get("content").unwrap_or(&Value::Null));
                if !system.trim().is_empty() {
                    system_parts.push(system);
                }
                continue;
            }
            if role == "tool" {
                let tool_use_id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("call_unknown");
                let content =
                    response_content_to_text(message.get("content").unwrap_or(&Value::Null));
                anthropic_messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content
                    }]
                }));
                continue;
            }

            let mut content =
                chat_content_to_anthropic_blocks(message.get("content").unwrap_or(&Value::Null));
            if role == "assistant" {
                if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for tool_call in tool_calls {
                        let id = tool_call
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("call_unknown");
                        let function = tool_call.get("function").unwrap_or(&Value::Null);
                        let name = function
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        content.push(json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": parse_tool_arguments(function.get("arguments"))
                        }));
                    }
                }
            }
            if content.is_empty() {
                continue;
            }
            anthropic_messages.push(json!({
                "role": if role == "assistant" { "assistant" } else { "user" },
                "content": content
            }));
        }
    }

    let mut payload = json!({
        "model": chat.get("model").cloned().unwrap_or_else(|| Value::String("kimi-for-coding".to_string())),
        "max_tokens": chat.get("max_tokens").cloned().unwrap_or_else(|| Value::from(32768)),
        "messages": anthropic_messages,
    });
    if !system_parts.is_empty() {
        payload["system"] = Value::String(system_parts.join("\n"));
    }
    if let Some(temperature) = chat.get("temperature") {
        payload["temperature"] = temperature.clone();
    }
    if let Some(tools) = chat.get("tools").and_then(Value::as_array) {
        let converted = tools
            .iter()
            .filter_map(|tool| {
                let function = tool.get("function")?;
                let name = function.get("name").and_then(Value::as_str)?;
                Some(json!({
                    "name": name,
                    "description": function.get("description").cloned().unwrap_or(Value::String(String::new())),
                    "input_schema": function.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} }))
                }))
            })
            .collect::<Vec<_>>();
        if !converted.is_empty() {
            payload["tools"] = Value::Array(converted);
        }
    }
    payload
}

fn anthropic_payload_to_responses_payload(anthropic: Value) -> Value {
    let mut input = Vec::new();
    if let Some(messages) = anthropic.get("messages").and_then(Value::as_array) {
        for message in messages {
            append_anthropic_message_to_responses_input(message, &mut input);
        }
    }
    if input.is_empty() {
        input.push(json!({
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": "" }]
        }));
    }

    let mut payload = json!({
        "model": anthropic
            .get("model")
            .cloned()
            .unwrap_or_else(|| Value::String("gpt-5.5".to_string())),
        "input": input,
    });
    if let Some(system) = anthropic.get("system") {
        let instructions = response_content_to_text(system);
        if !instructions.trim().is_empty() {
            payload["instructions"] = Value::String(instructions);
        }
    }
    if let Some(max_tokens) = anthropic.get("max_tokens") {
        payload["max_output_tokens"] = max_tokens.clone();
    }
    if let Some(stream) = anthropic.get("stream") {
        payload["stream"] = stream.clone();
    }
    if let Some(temperature) = anthropic.get("temperature") {
        payload["temperature"] = temperature.clone();
    }
    if let Some(tools) = anthropic_tools_to_responses_tools(anthropic.get("tools")) {
        payload["tools"] = tools;
    }
    if let Some(tool_choice) =
        anthropic_tool_choice_to_responses_tool_choice(anthropic.get("tool_choice"))
    {
        payload["tool_choice"] = tool_choice;
    }
    payload
}

fn append_anthropic_message_to_responses_input(message: &Value, input: &mut Vec<Value>) {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user");
    let Some(content) = message.get("content") else {
        return;
    };
    if !content.is_array() {
        push_responses_text_message(input, role, &response_content_to_text(content));
        return;
    }
    let Some(blocks) = content.as_array() else {
        return;
    };
    let mut pending_text = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        pending_text.push(text.to_string());
                    }
                }
            }
            "tool_result" => {
                flush_responses_text_message(input, role, &mut pending_text);
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or("call_proxy"),
                    "output": block
                        .get("content")
                        .map(response_content_to_text)
                        .unwrap_or_default()
                }));
            }
            "tool_use" => {
                flush_responses_text_message(input, role, &mut pending_text);
                input.push(json!({
                    "type": "function_call",
                    "call_id": block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("call_proxy"),
                    "name": block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                    "arguments": block
                        .get("input")
                        .map(|value| serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()))
                        .unwrap_or_else(|| "{}".to_string())
                }));
            }
            _ => {}
        }
    }
    flush_responses_text_message(input, role, &mut pending_text);
}

fn flush_responses_text_message(
    input: &mut Vec<Value>,
    role: &str,
    pending_text: &mut Vec<String>,
) {
    if pending_text.is_empty() {
        return;
    }
    let text = pending_text.join("\n");
    pending_text.clear();
    push_responses_text_message(input, role, &text);
}

fn push_responses_text_message(input: &mut Vec<Value>, role: &str, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    let role = if role == "assistant" {
        "assistant"
    } else {
        "user"
    };
    let content_type = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    input.push(json!({
        "type": "message",
        "role": role,
        "content": [{ "type": content_type, "text": text }]
    }));
}

fn anthropic_tools_to_responses_tools(value: Option<&Value>) -> Option<Value> {
    let tools = value?.as_array()?;
    let converted = tools
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name").and_then(Value::as_str)?;
            Some(json!({
                "type": "function",
                "name": name,
                "description": tool
                    .get("description")
                    .cloned()
                    .unwrap_or_else(|| Value::String(String::new())),
                "parameters": tool
                    .get("input_schema")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} }))
            }))
        })
        .collect::<Vec<_>>();
    (!converted.is_empty()).then(|| Value::Array(converted))
}

fn anthropic_tool_choice_to_responses_tool_choice(value: Option<&Value>) -> Option<Value> {
    let value = value?;
    match value.get("type").and_then(Value::as_str) {
        Some("auto") => Some(Value::String("auto".to_string())),
        Some("any") => Some(Value::String("required".to_string())),
        Some("tool") => value
            .get("name")
            .and_then(Value::as_str)
            .map(|name| json!({ "type": "function", "name": name })),
        _ => None,
    }
}

fn anthropic_payload_to_chat_payload(anthropic: Value) -> Value {
    let mut messages = Vec::new();
    if let Some(system) = anthropic.get("system") {
        let text = response_content_to_text(system);
        if !text.trim().is_empty() {
            messages.push(json!({ "role": "system", "content": text }));
        }
    }
    if let Some(items) = anthropic.get("messages").and_then(Value::as_array) {
        for item in items {
            let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
            match role {
                "assistant" => messages.push(anthropic_assistant_message_to_chat_message(item)),
                _ => messages.extend(anthropic_user_message_to_chat_messages(item)),
            }
        }
    }
    let mut chat = json!({
        "model": anthropic.get("model").cloned().unwrap_or_else(|| Value::String(CHAT_MODEL_FALLBACK.to_string())),
        "messages": messages,
        "stream": anthropic.get("stream").and_then(Value::as_bool).unwrap_or(false)
    });
    if let Some(tools) = anthropic_tools_to_chat_tools(anthropic.get("tools")) {
        chat["tools"] = tools;
    }
    if let Some(tool_choice) =
        anthropic_tool_choice_to_chat_tool_choice(anthropic.get("tool_choice"))
    {
        chat["tool_choice"] = tool_choice;
    }
    if let Some(max_tokens) = anthropic.get("max_tokens") {
        chat["max_tokens"] = max_tokens.clone();
    }
    if let Some(temperature) = anthropic.get("temperature") {
        chat["temperature"] = temperature.clone();
    }
    chat
}

const CHAT_MODEL_FALLBACK: &str = "gpt-5.5";

fn anthropic_assistant_message_to_chat_message(item: &Value) -> Value {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();
    match item.get("content") {
        Some(Value::Array(parts)) => {
            for part in parts {
                match part.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text" => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text_parts.push(text.to_string());
                            }
                        }
                    }
                    "tool_use" => {
                        let id = part
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("call_proxy");
                        let name = part
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        let input = part.get("input").cloned().unwrap_or_else(|| json!({}));
                        tool_calls.push(json!({
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": serde_json::to_string(&input)
                                    .unwrap_or_else(|_| "{}".to_string())
                            }
                        }));
                    }
                    _ => {}
                }
            }
        }
        Some(content) => {
            let text = response_content_to_text(content);
            if !text.is_empty() {
                text_parts.push(text);
            }
        }
        None => {}
    }

    let content = text_parts.join("\n");
    let mut message = json!({
        "role": "assistant",
        "content": if content.is_empty() && !tool_calls.is_empty() {
            Value::Null
        } else {
            Value::String(content)
        }
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    if let Some(reasoning_content) = reasoning_content_for_assistant_message(&message) {
        message["reasoning_content"] = Value::String(reasoning_content);
    }
    message
}

fn anthropic_user_message_to_chat_messages(item: &Value) -> Vec<Value> {
    let Some(content) = item.get("content") else {
        return vec![json!({ "role": "user", "content": "" })];
    };
    let Value::Array(parts) = content else {
        return vec![json!({ "role": "user", "content": response_content_to_text(content) })];
    };

    let mut messages = Vec::new();
    let mut pending_parts = Vec::new();
    for part in parts {
        match part.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        pending_parts.push(chat_text_part(text));
                    }
                }
            }
            "image" => {
                if let Some(url) = part
                    .get("source")
                    .and_then(anthropic_source_to_chat_image_url)
                {
                    pending_parts.push(chat_image_url_part(&url));
                }
            }
            "tool_result" => {
                if !pending_parts.is_empty() {
                    push_chat_user_message_from_parts(&mut messages, &mut pending_parts);
                }
                let tool_call_id = part
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or("call_proxy");
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": part
                        .get("content")
                        .map(response_content_to_text)
                        .unwrap_or_default()
                }));
            }
            _ => {}
        }
    }
    if !pending_parts.is_empty() || messages.is_empty() {
        push_chat_user_message_from_parts(&mut messages, &mut pending_parts);
    }
    messages
}

fn push_chat_user_message_from_parts(messages: &mut Vec<Value>, pending_parts: &mut Vec<Value>) {
    let content = chat_content_from_parts_with_text_separator(std::mem::take(pending_parts), "\n")
        .unwrap_or_else(|| Value::String(String::new()));
    messages.push(json!({ "role": "user", "content": content }));
}

fn anthropic_tools_to_chat_tools(value: Option<&Value>) -> Option<Value> {
    let tools = value?.as_array()?;
    let converted = tools
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name").and_then(Value::as_str)?;
            Some(json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": tool
                        .get("description")
                        .cloned()
                        .unwrap_or_else(|| Value::String(String::new())),
                    "parameters": tool
                        .get("input_schema")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }))
                }
            }))
        })
        .collect::<Vec<_>>();
    (!converted.is_empty()).then(|| Value::Array(converted))
}

fn anthropic_tool_choice_to_chat_tool_choice(value: Option<&Value>) -> Option<Value> {
    let value = value?;
    match value.get("type").and_then(Value::as_str) {
        Some("auto") => Some(Value::String("auto".to_string())),
        Some("any") => Some(Value::String("required".to_string())),
        Some("tool") => value
            .get("name")
            .and_then(Value::as_str)
            .map(|name| json!({ "type": "function", "function": { "name": name } })),
        _ => None,
    }
}

fn chat_response_to_anthropic_response(chat: Value) -> Value {
    let choice = chat
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .cloned()
        .unwrap_or(Value::Null);
    let message = choice.get("message").unwrap_or(&Value::Null);
    let content = response_content_to_text(message.get("content").unwrap_or(&Value::Null));
    let stop_reason = match choice.get("finish_reason").and_then(Value::as_str) {
        Some("tool_calls") => "tool_use",
        Some("length") => "max_tokens",
        _ => "end_turn",
    };
    json!({
        "id": chat.get("id").cloned().unwrap_or_else(|| Value::String("msg_proxy".to_string())),
        "type": "message",
        "role": "assistant",
        "model": chat.get("model").cloned().unwrap_or_else(|| Value::String(CHAT_MODEL_FALLBACK.to_string())),
        "content": if content.is_empty() {
            json!([])
        } else {
            json!([{ "type": "text", "text": content }])
        },
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": normalized_chat_usage(chat.get("usage"))
    })
}

fn anthropic_response_to_sse(response: &Value) -> Vec<u8> {
    let id = response
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("msg_proxy");
    let model = response
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(CHAT_MODEL_FALLBACK);
    let mut output = String::new();
    push_sse(
        &mut output,
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": Value::Null,
                "stop_sequence": Value::Null,
                "usage": { "input_tokens": 0, "output_tokens": 0 }
            }
        }),
    );
    let content = response
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if content.is_empty() {
        emit_anthropic_response_text_sse_block(&mut output, 0, "");
    } else {
        for (index, item) in content.iter().enumerate() {
            match item.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    emit_anthropic_response_tool_use_sse_block(&mut output, index, item);
                }
                _ => {
                    let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
                    emit_anthropic_response_text_sse_block(&mut output, index, text);
                }
            }
        }
    }
    push_sse(
        &mut output,
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": response.get("stop_reason").and_then(Value::as_str).unwrap_or("end_turn"),
                "stop_sequence": Value::Null
            },
            "usage": response.get("usage").cloned().unwrap_or_else(|| json!({ "output_tokens": 0 }))
        }),
    );
    push_sse(
        &mut output,
        "message_stop",
        json!({ "type": "message_stop" }),
    );
    output.into_bytes()
}

fn emit_anthropic_response_text_sse_block(output: &mut String, index: usize, text: &str) {
    push_sse(
        output,
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": index,
            "content_block": { "type": "text", "text": "" }
        }),
    );
    if !text.is_empty() {
        push_sse(
            output,
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "text_delta", "text": text }
            }),
        );
    }
    push_sse(
        output,
        "content_block_stop",
        json!({ "type": "content_block_stop", "index": index }),
    );
}

fn emit_anthropic_response_tool_use_sse_block(output: &mut String, index: usize, item: &Value) {
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("tool_proxy");
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let input = item.get("input").cloned().unwrap_or_else(|| json!({}));
    let input_json = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
    push_sse(
        output,
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": index,
            "content_block": {
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": {}
            }
        }),
    );
    if !input_json.is_empty() {
        push_sse(
            output,
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "input_json_delta", "partial_json": input_json }
            }),
        );
    }
    push_sse(
        output,
        "content_block_stop",
        json!({ "type": "content_block_stop", "index": index }),
    );
}

fn parse_tool_arguments(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(text)) => {
            serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.clone()))
        }
        Some(value) => value.clone(),
        None => json!({}),
    }
}

fn responses_tools_to_chat_tools(value: Option<&Value>, model: &str) -> Option<Value> {
    let tools = value?.as_array()?;
    let prefer_claude_edit_tools = model_prefers_claude_edit_tools(model);
    let mut converted = Vec::new();
    for tool in tools {
        match tool.get("type").and_then(Value::as_str) {
            Some("function") => {
                let Some(name) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let description = tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                converted.push(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": chat_tool_description(name, description),
                        "parameters": tool.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} }))
                    }
                }));
            }
            Some("namespace") => {
                let Some(namespace) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let namespace_description = tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let Some(namespace_tools) = tool.get("tools").and_then(Value::as_array) else {
                    continue;
                };
                for namespace_tool in namespace_tools {
                    if namespace_tool.get("type").and_then(Value::as_str) != Some("function") {
                        continue;
                    }
                    let Some(name) = namespace_tool.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let chat_name = flattened_namespace_tool_name(namespace, name);
                    let description = namespaced_chat_tool_description(
                        namespace,
                        namespace_description,
                        name,
                        namespace_tool
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    );
                    let description = chat_tool_description(&chat_name, &description);
                    converted.push(json!({
                        "type": "function",
                        "function": {
                            "name": chat_name,
                            "description": description,
                            "parameters": namespace_tool.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} }))
                        }
                    }));
                }
            }
            Some("custom") if tool.get("name").and_then(Value::as_str) == Some("apply_patch") => {
                if prefer_claude_edit_tools {
                    converted.extend(claude_edit_chat_tools_for_apply_patch());
                }
                converted.push(apply_patch_chat_tool());
            }
            _ => {}
        }
    }
    (!converted.is_empty()).then_some(Value::Array(converted))
}

fn namespaced_chat_tool_description(
    namespace: &str,
    namespace_description: &str,
    name: &str,
    description: &str,
) -> String {
    let mut parts = Vec::new();
    if !description.trim().is_empty() {
        parts.push(description.trim().to_string());
    }
    if !namespace_description.trim().is_empty() {
        parts.push(format!(
            "Namespace `{namespace}`: {}",
            namespace_description.trim()
        ));
    }
    parts.push(format!(
        "This tool is exposed as `{name}` in namespace `{namespace}`."
    ));
    parts.join("\n\n")
}

fn should_add_non_gpt_edit_bridge_instructions(value: Option<&Value>, model: &str) -> bool {
    model_prefers_claude_edit_tools(model) && responses_tools_include_apply_patch(value)
}

fn chat_tool_description(name: &str, description: &str) -> String {
    if !chat_tool_is_shell(name, description) || description.contains(SHELL_TOOL_INSTRUCTIONS) {
        return description.to_string();
    }
    if description.trim().is_empty() {
        return SHELL_TOOL_INSTRUCTIONS.to_string();
    }
    format!("{}\n\n{}", description.trim_end(), SHELL_TOOL_INSTRUCTIONS)
}

fn chat_tool_is_shell(name: &str, description: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let description = description.to_ascii_lowercase();
    is_shell_tool_name(&name) || is_shell_tool_description(&description)
}

fn is_shell_tool_name(name: &str) -> bool {
    matches!(
        name,
        "bash"
            | "shell"
            | "terminal"
            | "exec"
            | "execute"
            | "exec_command"
            | "run_command"
            | "run_shell_command"
    ) || name.contains("shell")
        || name.contains("bash")
        || name.contains("terminal")
        || name.contains("exec")
}

fn is_shell_tool_description(description: &str) -> bool {
    [
        "shell command",
        "bash command",
        "terminal command",
        "execute command",
        "run command",
        "run commands",
    ]
    .iter()
    .any(|needle| description.contains(needle))
}

fn responses_tools_include_apply_patch(value: Option<&Value>) -> bool {
    value.and_then(Value::as_array).is_some_and(|tools| {
        tools.iter().any(|tool| {
            tool.get("type").and_then(Value::as_str) == Some("custom")
                && tool.get("name").and_then(Value::as_str) == Some("apply_patch")
        })
    })
}

fn model_prefers_claude_edit_tools(model: &str) -> bool {
    !is_gpt_or_codex_model(model)
}

fn is_gpt_or_codex_model(model: &str) -> bool {
    let normalized = normalized_model_key(model);
    let model = normalized
        .strip_prefix(PROVIDER_MODEL_ID_PREFIX)
        .and_then(|rest| rest.split_once('/').map(|(_, model)| model))
        .unwrap_or(normalized.as_str());
    let model = model.strip_prefix("openai/").unwrap_or(model);

    model.starts_with("gpt") || model.starts_with("chatgpt") || model.starts_with("codex")
}

fn apply_patch_chat_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "apply_patch",
            "description": "Low-level fallback for editing files by applying a raw patch. Prefer Edit, MultiEdit, or Write when those tools are available. Put the complete raw patch text in the `patch` string. Do not wrap the patch in shell commands, here-strings, or JSON inside the string.",
            "parameters": {
                "type": "object",
                "properties": {
                    "patch": {
                        "type": "string",
                        "description": "The complete patch text, starting with *** Begin Patch and ending with *** End Patch."
                    }
                },
                "required": ["patch"],
                "additionalProperties": false
            }
        }
    })
}

fn claude_edit_chat_tools_for_apply_patch() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "Edit",
                "description": "Modify an existing text file by replacing old_string with new_string. old_string must match the file exactly. Use MultiEdit for several replacements in one file. Use Write only for new files.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "Path of the file to edit." },
                        "old_string": { "type": "string", "description": "Exact text to replace." },
                        "new_string": { "type": "string", "description": "Replacement text." },
                        "replace_all": { "type": "boolean", "description": "Whether to replace all exact occurrences. Prefer false unless explicitly needed." }
                    },
                    "required": ["file_path", "old_string", "new_string"],
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "MultiEdit",
                "description": "Apply multiple exact string replacements to one existing text file. Edits are applied in order.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "Path of the file to edit." },
                        "edits": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "old_string": { "type": "string", "description": "Exact text to replace." },
                                    "new_string": { "type": "string", "description": "Replacement text." },
                                    "replace_all": { "type": "boolean", "description": "Whether to replace all exact occurrences. Prefer false unless explicitly needed." }
                                },
                                "required": ["old_string", "new_string"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["file_path", "edits"],
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "Write",
                "description": "Create a new text file with the given content. To modify an existing file, use Edit or MultiEdit.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "Path of the new file." },
                        "content": { "type": "string", "description": "Complete file content." }
                    },
                    "required": ["file_path", "content"],
                    "additionalProperties": false
                }
            }
        }),
    ]
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
            let chat_name = map
                .get("namespace")
                .or_else(|| {
                    map.get("function")
                        .and_then(|function| function.get("namespace"))
                })
                .and_then(Value::as_str)
                .map(|namespace| flattened_namespace_tool_name(namespace, name))
                .unwrap_or_else(|| name.to_string());
            Some(json!({ "type": "function", "function": { "name": chat_name } }))
        }
        _ => None,
    }
}

fn chat_response_to_responses_response(chat: Value) -> anyhow::Result<Value> {
    let choice = chat
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .context("chat response did not contain choices[0]")?;
    let message = choice
        .get("message")
        .context("chat response did not contain message")?;
    let output = chat_message_to_responses_output(message);
    remember_message_reasoning(message);
    let usage = normalized_chat_usage(chat.get("usage"));
    Ok(json!({
        "id": chat.get("id").cloned().unwrap_or_else(|| Value::String("resp_proxy".to_string())),
        "object": "response",
        "created_at": chat.get("created").cloned().unwrap_or_else(|| Value::from(0)),
        "model": chat.get("model").cloned().unwrap_or_else(|| Value::String(CHAT_MODEL_FALLBACK.to_string())),
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
        "id": "msg_proxy",
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
        .unwrap_or("call_proxy");
    let function = tool_call.get("function").unwrap_or(&Value::Null);
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let arguments = function
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}");
    if tool_call_outputs_as_apply_patch(name) {
        return json!({
            "id": id,
            "type": "custom_tool_call",
            "call_id": id,
            "name": "apply_patch",
            "input": apply_patch_input_for_tool_call(name, arguments),
            "status": "completed"
        });
    }
    namespaced_function_call_item(id, name, arguments, "completed")
}

fn apply_patch_input_from_function_arguments(arguments: &str) -> String {
    let trimmed = arguments.trim();
    let parsed = serde_json::from_str::<Value>(trimmed);
    if let Ok(value) = parsed {
        if let Some(patch) = value
            .get("patch")
            .or_else(|| value.get("input"))
            .or_else(|| value.get("content"))
            .and_then(Value::as_str)
        {
            return patch.to_string();
        }
        if let Some(text) = value.as_str() {
            return text.to_string();
        }
    }
    trimmed.to_string()
}

fn tool_call_outputs_as_apply_patch(name: &str) -> bool {
    name == "apply_patch" || claude_edit_tool_kind(name).is_some()
}

fn apply_patch_input_for_tool_call(name: &str, arguments: &str) -> String {
    if name == "apply_patch" {
        return apply_patch_input_from_function_arguments(arguments);
    }
    claude_edit_tool_arguments_to_apply_patch(name, arguments)
        .unwrap_or_else(|| invalid_apply_patch_input_for_tool_call(name, arguments))
}

fn claude_edit_tool_arguments_to_apply_patch(name: &str, arguments: &str) -> Option<String> {
    let input = serde_json::from_str::<Value>(arguments).ok()?;
    claude_edit_tool_input_to_apply_patch(name, &input)
}

fn claude_edit_tool_input_to_apply_patch(name: &str, input: &Value) -> Option<String> {
    match claude_edit_tool_kind(name)? {
        "edit" => edit_tool_input_to_apply_patch(input),
        "multiedit" => multi_edit_tool_input_to_apply_patch(input),
        "write" => write_tool_input_to_apply_patch(input),
        _ => None,
    }
}

fn claude_edit_tool_kind(name: &str) -> Option<&'static str> {
    let normalized = name
        .chars()
        .filter(|ch| *ch != '_' && *ch != '-')
        .collect::<String>()
        .to_ascii_lowercase();
    match normalized.as_str() {
        "edit" => Some("edit"),
        "multiedit" => Some("multiedit"),
        "write" => Some("write"),
        _ => None,
    }
}

fn edit_tool_input_to_apply_patch(input: &Value) -> Option<String> {
    let path = string_field(input, &["file_path", "path", "file"])?;
    let old_string = string_field(input, &["old_string", "oldText", "old_text"])?;
    let new_string = string_field(input, &["new_string", "newText", "new_text"])?;
    replacement_patch(path, &[(old_string, new_string)])
}

fn multi_edit_tool_input_to_apply_patch(input: &Value) -> Option<String> {
    let path = string_field(input, &["file_path", "path", "file"])?;
    let edits = input.get("edits")?.as_array()?;
    if edits.is_empty() {
        return None;
    }
    let mut replacements = Vec::new();
    for edit in edits {
        let old_string = string_field(edit, &["old_string", "oldText", "old_text"])?;
        let new_string = string_field(edit, &["new_string", "newText", "new_text"])?;
        replacements.push((old_string, new_string));
    }
    replacement_patch(path, &replacements)
}

fn write_tool_input_to_apply_patch(input: &Value) -> Option<String> {
    let path = string_field(input, &["file_path", "path", "file"])?;
    let content = string_field(
        input,
        &["content", "text", "new_string", "newText", "new_text"],
    )?;
    let mut lines = vec![
        "*** Begin Patch".to_string(),
        format!("*** Add File: {path}"),
    ];
    let mut body = prefixed_patch_lines('+', content);
    if body.is_empty() {
        body.push("+".to_string());
    }
    lines.extend(body);
    lines.push("*** End Patch".to_string());
    Some(lines.join("\n"))
}

fn replacement_patch(path: &str, replacements: &[(&str, &str)]) -> Option<String> {
    if path.trim().is_empty() || replacements.is_empty() {
        return None;
    }
    let mut lines = vec![
        "*** Begin Patch".to_string(),
        format!("*** Update File: {path}"),
    ];
    for (old_string, new_string) in replacements {
        if old_string.is_empty() {
            return None;
        }
        lines.push("@@".to_string());
        lines.extend(prefixed_patch_lines('-', old_string));
        lines.extend(prefixed_patch_lines('+', new_string));
    }
    lines.push("*** End Patch".to_string());
    Some(lines.join("\n"))
}

fn prefixed_patch_lines(prefix: char, text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut parts = text.split('\n').collect::<Vec<_>>();
    if text.ends_with('\n') {
        let _ = parts.pop();
    }
    parts
        .into_iter()
        .map(|line| format!("{prefix}{line}"))
        .collect()
}

fn string_field<'a>(input: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| input.get(*key).and_then(Value::as_str))
}

fn invalid_apply_patch_input_for_tool_call(name: &str, arguments: &str) -> String {
    format!("Invalid {name} tool input; expected Claude-style edit JSON but received: {arguments}")
}

fn responses_response_to_anthropic_response(response: Value) -> Value {
    let mut content = Vec::new();
    let mut stop_reason = "end_turn";
    if let Some(output) = response.get("output").and_then(Value::as_array) {
        for item in output {
            match item.get("type").and_then(Value::as_str).unwrap_or("") {
                "message" => {
                    content.extend(responses_message_item_to_anthropic_content(item));
                }
                "function_call" | "custom_tool_call" => {
                    stop_reason = "tool_use";
                    content.push(responses_tool_call_item_to_anthropic_content(item));
                }
                _ => {}
            }
        }
    }
    json!({
        "id": response
            .get("id")
            .cloned()
            .unwrap_or_else(|| Value::String("msg_timiai".to_string())),
        "type": "message",
        "role": "assistant",
        "model": response
            .get("model")
            .cloned()
            .unwrap_or_else(|| Value::String("gpt-5.5".to_string())),
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": normalized_chat_usage(response.get("usage")),
    })
}

fn responses_message_item_to_anthropic_content(item: &Value) -> Vec<Value> {
    item.get("content")
        .and_then(Value::as_array)
        .map(|content| {
            content
                .iter()
                .filter_map(|part| {
                    let text = part
                        .get("text")
                        .or_else(|| part.get("output_text"))
                        .and_then(Value::as_str)?;
                    (!text.is_empty()).then(|| json!({ "type": "text", "text": text }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn responses_tool_call_item_to_anthropic_content(item: &Value) -> Value {
    let id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("call_timiai");
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let input = item
        .get("arguments")
        .or_else(|| item.get("input"))
        .map(|value| parse_tool_arguments(Some(value)))
        .unwrap_or_else(|| json!({}));
    json!({
        "type": "tool_use",
        "id": id,
        "name": name,
        "input": input
    })
}

fn anthropic_response_to_responses_response(anthropic: Value) -> Value {
    let mut output = Vec::new();
    let mut text = String::new();
    if let Some(content) = anthropic.get("content").and_then(Value::as_array) {
        for block in content {
            match block.get("type").and_then(Value::as_str).unwrap_or("") {
                "text" => {
                    if let Some(value) = block.get("text").and_then(Value::as_str) {
                        text.push_str(value);
                    }
                }
                "tool_use" => {
                    if !text.is_empty() {
                        output.push(response_message_item_with_reasoning(&text, None));
                        text.clear();
                    }
                    let id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("call_kimi");
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    let arguments = block
                        .get("input")
                        .map(|input| input.to_string())
                        .unwrap_or_else(|| "{}".to_string());
                    if tool_call_outputs_as_apply_patch(name) {
                        output.push(json!({
                            "id": id,
                            "type": "custom_tool_call",
                            "call_id": id,
                            "name": "apply_patch",
                            "input": apply_patch_input_for_tool_call(name, &arguments),
                            "status": "completed"
                        }));
                    } else {
                        output.push(namespaced_function_call_item(
                            id,
                            name,
                            &arguments,
                            "completed",
                        ));
                    }
                }
                _ => {}
            }
        }
    }
    if !text.is_empty() {
        output.push(response_message_item_with_reasoning(&text, None));
    }
    let usage = normalized_chat_usage(anthropic.get("usage"));
    json!({
        "id": anthropic.get("id").cloned().unwrap_or_else(|| Value::String("resp_kimi".to_string())),
        "object": "response",
        "created_at": 0,
        "model": anthropic.get("model").cloned().unwrap_or_else(|| Value::String("kimi-for-coding".to_string())),
        "status": "completed",
        "output": output,
        "usage": usage,
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
    let cached_tokens = usage_cached_input_tokens(usage).unwrap_or(0);
    let reasoning_tokens = usage_reasoning_output_tokens(usage).unwrap_or(0);
    json!({
        "input_tokens": input,
        "input_tokens_details": {
            "cached_tokens": cached_tokens
        },
        "output_tokens": output,
        "output_tokens_details": {
            "reasoning_tokens": reasoning_tokens
        },
        "total_tokens": total
    })
}

fn usage_i64_field(usage: &Value, field: &str) -> Option<i64> {
    usage.get(field).and_then(Value::as_i64)
}

fn usage_nested_i64_field(usage: &Value, object_field: &str, field: &str) -> Option<i64> {
    usage
        .get(object_field)
        .and_then(|value| value.get(field))
        .and_then(Value::as_i64)
}

fn usage_cached_input_tokens(usage: &Value) -> Option<i64> {
    let candidates = [
        usage_nested_i64_field(usage, "input_tokens_details", "cached_tokens"),
        usage_nested_i64_field(usage, "prompt_tokens_details", "cached_tokens"),
        usage_i64_field(usage, "cache_read_input_tokens"),
        usage_i64_field(usage, "cached_input_tokens"),
        usage_i64_field(usage, "prompt_cache_hit_tokens"),
        usage_i64_field(usage, "cache_hit_input_tokens"),
        usage_i64_field(usage, "cache_read_tokens"),
        usage_i64_field(usage, "cache_hit_tokens"),
        usage_i64_field(usage, "cached_tokens"),
    ];
    candidates
        .iter()
        .copied()
        .flatten()
        .find(|value| *value > 0)
        .or_else(|| candidates.iter().copied().flatten().next())
}

fn usage_reasoning_output_tokens(usage: &Value) -> Option<i64> {
    usage_nested_i64_field(usage, "output_tokens_details", "reasoning_tokens")
        .or_else(|| usage_nested_i64_field(usage, "completion_tokens_details", "reasoning_tokens"))
        .or_else(|| usage_i64_field(usage, "reasoning_output_tokens"))
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
                        .unwrap_or("msg_proxy");
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
                "function_call" | "custom_tool_call" => {
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
    let tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    remember_assistant_reasoning(content, tool_calls, reasoning_content);
}

fn remember_reasoning_content(content: &str, reasoning_content: &str) {
    remember_assistant_reasoning(content, &[], reasoning_content);
}

fn remember_assistant_reasoning(content: &str, tool_calls: &[Value], reasoning_content: &str) {
    if content.trim().is_empty() || reasoning_content.trim().is_empty() {
        if tool_calls.is_empty() || reasoning_content.trim().is_empty() {
            return;
        }
    }
    let Some(assistant_signature) = assistant_reasoning_signature(content, tool_calls) else {
        return;
    };
    let history = DEEPSEEK_REASONING_HISTORY
        .get_or_init(|| Arc::new(RwLock::new(Vec::new())))
        .clone();
    let Ok(mut entries) = history.write() else {
        return;
    };
    if let Some(existing) = entries
        .iter_mut()
        .find(|entry| entry.assistant_signature == assistant_signature)
    {
        existing.reasoning_content = reasoning_content.to_string();
        return;
    }
    entries.push(ReasoningHistoryEntry {
        content: content.to_string(),
        assistant_signature,
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

fn reasoning_content_for_assistant_message(message: &Value) -> Option<String> {
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let signature = assistant_reasoning_signature(content, tool_calls)?;
    let history = DEEPSEEK_REASONING_HISTORY.get()?.clone();
    let entries = history.read().ok()?;
    entries
        .iter()
        .rev()
        .find(|entry| entry.assistant_signature == signature)
        .map(|entry| entry.reasoning_content.clone())
}

fn assistant_reasoning_signature(content: &str, tool_calls: &[Value]) -> Option<String> {
    if content.trim().is_empty() && tool_calls.is_empty() {
        return None;
    }
    let tool_signature = tool_calls
        .iter()
        .map(normalized_tool_call_signature)
        .collect::<Vec<_>>();
    Some(format!(
        "{}\n{}",
        content,
        serde_json::to_string(&tool_signature).unwrap_or_default()
    ))
}

fn normalized_tool_call_signature(tool_call: &Value) -> Value {
    let function = tool_call.get("function").unwrap_or(&Value::Null);
    json!({
        "id": tool_call.get("id").and_then(Value::as_str).unwrap_or_default(),
        "name": function.get("name").and_then(Value::as_str).unwrap_or_default(),
        "arguments": function
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or_default()
    })
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
    let tools = payload
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let tool_choice = payload
        .get("tool_choice")
        .map(response_content_to_text)
        .unwrap_or_default();
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
        "{label} model={model} stream={stream} tools={tools} tool_choice={tool_choice} messages=[{messages}]"
    ));
}

fn log_chat_completions_upstream_error(
    provider: &str,
    url: &str,
    payload: &Value,
    error: &reqwest::Error,
) {
    append_codex_api_proxy_log(&format!(
        "chat_completions_upstream_error provider={} url={} model={} stream={} tools={} error={} debug={:?}",
        normalize_proxy_provider(provider),
        url,
        payload
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("<missing>"),
        payload
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        payload
            .get("tools")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0),
        error,
        error
    ));
}

fn log_chat_completions_upstream_response(
    provider: &str,
    url: &str,
    payload: &Value,
    status: StatusCode,
    content_type: &str,
) {
    append_codex_api_proxy_log(&format!(
        "chat_completions_upstream_response provider={} url={} model={} stream={} tools={} status={} content_type={}",
        normalize_proxy_provider(provider),
        url,
        payload
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("<missing>"),
        payload
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        payload
            .get("tools")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0),
        status.as_u16(),
        content_type
    ));
}

fn log_native_anthropic_upstream_send_error(
    provider: &str,
    url: &str,
    payload: &Value,
    error: &reqwest::Error,
) {
    append_codex_api_proxy_log(&format!(
        "native_anthropic_upstream_send_error provider={} url={} model={} stream={} tools={} error={} debug={:?}",
        normalize_proxy_provider(provider),
        url,
        payload
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("<missing>"),
        payload
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        payload
            .get("tools")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0),
        error,
        error
    ));
}

fn log_native_anthropic_upstream_response(
    provider: &str,
    url: &str,
    payload: &Value,
    status: StatusCode,
    content_type: &str,
) {
    append_codex_api_proxy_log(&format!(
        "native_anthropic_upstream_response provider={} url={} model={} stream={} tools={} status={} content_type={}",
        normalize_proxy_provider(provider),
        url,
        payload
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("<missing>"),
        payload
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        payload
            .get("tools")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0),
        status.as_u16(),
        content_type
    ));
}

fn log_anthropic_payload_summary(label: &str, payload: &Value) {
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("<missing>");
    let system_len = payload
        .get("system")
        .and_then(Value::as_str)
        .map(str::len)
        .unwrap_or(0);
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
                    let content = message
                        .get("content")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    format!("#{index}:{role}:blocks={content}")
                })
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_else(|| "<no messages>".to_string());
    let tools = payload
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    append_codex_api_proxy_log(&format!(
        "{label} model={model} system={system_len} messages=[{messages}] tools={tools}"
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

fn log_suspicious_upstream_response(status: StatusCode, content_type: &str, body: &[u8]) {
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

#[cfg(not(test))]
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
fn append_codex_api_proxy_log(_line: &str) {}

#[cfg(test)]
mod tests;
