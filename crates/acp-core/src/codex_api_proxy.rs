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
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

type ProxyBody = BoxBody<Bytes, Infallible>;

const TIMIAI_RESPONSES_URL: &str = "http://api.timiai.woa.com/ai_api_manage/llmproxy/responses";
const TIMIAI_RESPONSES_COMPACT_URL: &str =
    "https://api.timiai.woa.com/ai_api_manage/llmproxy/responses/compact";
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
const DEEPSEEK_REASONING_PLACEHOLDER: &str = "[previous reasoning unavailable]";

#[derive(Debug, Clone)]
struct CodexApiProxyConfig {
    provider: String,
    api_key: String,
    api_keys: BTreeMap<String, String>,
    session_ids: BTreeMap<String, String>,
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
    let provider = explicit_provider
        .unwrap_or_else(|| proxy_provider_for_model(&requested_model, &config.provider));
    let payload = prepare_responses_payload_for_provider(payload, provider);
    log_responses_payload_summary("responses_request", &payload, provider);
    let api_key = api_key_for_proxy_provider(&config, provider);
    if api_key.trim().is_empty() {
        return Ok(response_with_status(
            StatusCode::UNAUTHORIZED,
            json!({ "error": { "message": format!("API key is not configured for {provider}") } })
                .to_string(),
            "application/json",
        ));
    }
    if normalize_proxy_provider(provider) == "timiai" {
        let session_id = session_id_for_proxy_provider(&config, provider);
        return proxy_native_codex_responses_request(payload, &api_key, provider, &session_id)
            .await;
    }
    let chat_payload = match responses_payload_to_chat_payload(payload, provider) {
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
    let chat_payload = normalize_chat_payload_for_provider(chat_payload, provider);
    match normalize_proxy_provider(provider) {
        "commandcode" => log_chat_payload_summary("commandcode_request", &chat_payload),
        "deepseek" => log_chat_payload_summary("deepseek_request", &chat_payload),
        "xiaomi_mimo" => log_chat_payload_summary("xiaomi_request", &chat_payload),
        _ => {}
    }
    if provider == "kimi_code" {
        return proxy_kimi_codex_api_request(chat_payload, &api_key, requested_stream).await;
    }
    let upstream_url = upstream_chat_completions_url(provider);

    let client = reqwest::Client::new();
    let request = client
        .post(upstream_url)
        .header(CONTENT_TYPE, "application/json");
    let request = request.bearer_auth(api_key);
    let request_body = serde_json::to_vec(&chat_payload)?;
    let upstream = match request.body(request_body).send().await {
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
    log_chat_completions_upstream_response(
        provider,
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

async fn proxy_native_codex_responses_request(
    payload: Value,
    api_key: &str,
    provider: &str,
    session_id: &str,
) -> anyhow::Result<Response<ProxyBody>> {
    let normalized_provider = normalize_proxy_provider(provider);
    let payload = if normalized_provider == "timiai" {
        sanitize_timiai_responses_payload(payload)
    } else {
        payload
    };
    let (auth_header_name, auth_log_state, x_api_key_log_state, session_log_state) =
        if normalized_provider == "timiai" {
            (
                "Authorization",
                timiai_authorization_log_state(api_key),
                if api_key.trim().is_empty() {
                    "empty"
                } else {
                    "present"
                },
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
                if api_key.trim().is_empty() {
                    "empty"
                } else {
                    "present"
                },
                "not_sent",
            )
        };
    append_codex_api_proxy_log(&format!(
        "native_responses_upstream_request provider={} method=POST url={} model={} stream={} input_present={} tools={} auth_header={}:{} x_api_key={} x_session_id={}",
        normalized_provider,
        upstream_responses_url(provider),
        payload
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        payload
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        payload.get("input").is_some(),
        payload
            .get("tools")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0),
        auth_header_name,
        auth_log_state,
        x_api_key_log_state,
        session_log_state,
    ));
    let client = reqwest::Client::new();
    let request = client
        .post(upstream_responses_url(provider))
        .header(CONTENT_TYPE, "application/json");
    let upstream = with_native_responses_headers(request, api_key, session_id)
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
    if is_event_stream(&content_type) {
        if normalized_provider == "timiai" {
            return Ok(streaming_timiai_responses_response(
                upstream,
                status,
                &content_type,
            ));
        }
        return Ok(streaming_passthrough_response(
            upstream,
            status,
            &content_type,
        ));
    }

    let body = upstream.bytes().await?;
    let body = if normalized_provider == "timiai" && status.is_success() {
        sanitize_timiai_responses_response_body(body.as_ref()).unwrap_or_else(|| body.to_vec())
    } else {
        body.to_vec()
    };
    let mut response = Response::new(full_body(Bytes::from(body)));
    *response.status_mut() = status;
    if let Ok(value) = content_type.parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    Ok(response)
}

async fn proxy_native_codex_responses_compact_request(
    request: Request<Incoming>,
    config: Arc<RwLock<CodexApiProxyConfig>>,
    explicit_provider: Option<&'static str>,
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
        });
    let provider = explicit_provider.unwrap_or_else(|| normalize_proxy_provider(&config.provider));
    if provider != "timiai" {
        return Ok(response_with_status(
            StatusCode::NOT_FOUND,
            "not found".to_string(),
            "text/plain; charset=utf-8",
        ));
    }
    let api_key = api_key_for_proxy_provider(&config, provider);
    if api_key.trim().is_empty() {
        return Ok(response_with_status(
            StatusCode::UNAUTHORIZED,
            json!({ "error": { "message": "API key is not configured for timiai" } }).to_string(),
            "application/json",
        ));
    }

    let session_id = session_id_for_proxy_provider(&config, provider);
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
    explicit_provider: Option<&'static str>,
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
        });
    let payload: Value = serde_json::from_slice(&body)?;
    let requested_model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let provider = explicit_provider
        .unwrap_or_else(|| proxy_provider_for_model(&requested_model, &config.provider));
    log_anthropic_payload_summary(
        &format!("anthropic_messages_request provider={provider}"),
        &payload,
    );
    let api_key = api_key_for_proxy_provider(&config, provider);
    if api_key.trim().is_empty() {
        return Ok(response_with_status(
            StatusCode::UNAUTHORIZED,
            json!({ "error": { "message": format!("API key is not configured for {provider}") } })
                .to_string(),
            "application/json",
        ));
    }

    let session_id = session_id_for_proxy_provider(&config, provider);
    if normalize_proxy_provider(provider) == "timiai" && !is_claude_family_model(&requested_model) {
        return proxy_timiai_responses_to_anthropic_messages_request(
            payload,
            &api_key,
            &session_id,
        )
        .await;
    }
    match normalize_proxy_provider(provider) {
        "commandcode" | "kimi_code" | "xiaomi_mimo" | "timiai" => {
            proxy_native_anthropic_messages_request(payload, &api_key, provider, &session_id).await
        }
        _ => proxy_completion_to_anthropic_messages_request(payload, &api_key, provider).await,
    }
}

async fn proxy_native_anthropic_messages_request(
    payload: Value,
    api_key: &str,
    provider: &str,
    session_id: &str,
) -> anyhow::Result<Response<ProxyBody>> {
    let upstream_url = upstream_messages_url(provider);
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

async fn proxy_timiai_responses_to_anthropic_messages_request(
    payload: Value,
    api_key: &str,
    session_id: &str,
) -> anyhow::Result<Response<ProxyBody>> {
    let requested_stream = payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut responses_payload = anthropic_payload_to_responses_payload(payload);
    if let Some(object) = responses_payload.as_object_mut() {
        object.remove("stream");
    }
    responses_payload = sanitize_timiai_responses_payload(responses_payload);
    append_codex_api_proxy_log(&format!(
        "timiai_responses_anthropic_bridge_request method=POST url={} model={} downstream_stream={} upstream_stream=false tools={} auth_header=Authorization:{} x_api_key=present x_session_id=present",
        TIMIAI_RESPONSES_URL,
        responses_payload
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        requested_stream,
        responses_payload
            .get("tools")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0),
        timiai_authorization_log_state(api_key)
    ));
    let client = reqwest::Client::new();
    let upstream = with_timiai_headers(
        client
            .post(TIMIAI_RESPONSES_URL)
            .header(CONTENT_TYPE, "application/json"),
        api_key,
        session_id,
    )
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
            "timiai_responses_anthropic_bridge_error url={} model={} status={}",
            TIMIAI_RESPONSES_URL,
            responses_payload
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            status.as_u16()
        ));
        log_suspicious_upstream_response(status, &content_type, body.as_ref());
    }
    let mut response_content_type = content_type.clone();
    let body = if requested_stream && status.is_success() {
        let response: Value = serde_json::from_slice(body.as_ref())?;
        response_content_type = "text/event-stream".to_string();
        anthropic_response_to_sse(&responses_response_to_anthropic_response(response))
    } else if status.is_success() {
        let response: Value = serde_json::from_slice(body.as_ref())?;
        serde_json::to_vec(&responses_response_to_anthropic_response(response))?
    } else {
        body.to_vec()
    };
    let mut response = Response::new(full_body(Bytes::from(body)));
    *response.status_mut() = status;
    if let Ok(value) = response_content_type.parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    Ok(response)
}

async fn proxy_completion_to_anthropic_messages_request(
    payload: Value,
    api_key: &str,
    provider: &str,
) -> anyhow::Result<Response<ProxyBody>> {
    let requested_stream = payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let chat_payload = anthropic_payload_to_chat_payload(payload);
    let chat_payload = normalize_chat_payload_for_provider(chat_payload, provider);
    if normalize_proxy_provider(provider) == "deepseek" {
        log_chat_payload_summary("deepseek_anthropic_request", &chat_payload);
    }
    let client = reqwest::Client::new();
    let request = client
        .post(upstream_chat_completions_url(provider))
        .header(CONTENT_TYPE, "application/json");
    let request = request.bearer_auth(api_key);
    let upstream = request
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

fn normalize_proxy_provider(provider: &str) -> &'static str {
    match provider.trim().to_ascii_lowercase().as_str() {
        "timiai" | "timi" | "timi-ai" | "timi_ai" => "timiai",
        "commandcode" | "command-code" | "command_code" => "commandcode",
        "deepseek" => "deepseek",
        "kimi" | "kimi_code" | "kimi-code" => "kimi_code",
        "mimo" | "xiaomi_mimo" | "xiaomi-mimo" => "xiaomi_mimo",
        _ => "timiai",
    }
}

fn proxy_provider_from_path(path: &str) -> Option<&'static str> {
    let (_, rest) = path.split_once("/providers/")?;
    let provider = rest.split('/').next().unwrap_or_default().trim();
    (!provider.is_empty()).then(|| normalize_proxy_provider(provider))
}

fn proxy_provider_for_model<'a>(_model: &str, fallback_provider: &'a str) -> &'a str {
    normalize_proxy_provider(fallback_provider)
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

fn upstream_chat_completions_url(provider: &str) -> &'static str {
    match normalize_proxy_provider(provider) {
        "commandcode" => COMMANDCODE_UPSTREAM_CHAT_COMPLETIONS_URL,
        "deepseek" => DEEPSEEK_UPSTREAM_CHAT_COMPLETIONS_URL,
        "kimi_code" => KIMI_UPSTREAM_CHAT_COMPLETIONS_URL,
        "xiaomi_mimo" => MIMO_UPSTREAM_CHAT_COMPLETIONS_URL,
        _ => DEEPSEEK_UPSTREAM_CHAT_COMPLETIONS_URL,
    }
}

fn upstream_responses_url(provider: &str) -> &'static str {
    match normalize_proxy_provider(provider) {
        "timiai" => TIMIAI_RESPONSES_URL,
        _ => TIMIAI_RESPONSES_URL,
    }
}

fn with_native_responses_headers(
    request: reqwest::RequestBuilder,
    api_key: &str,
    session_id: &str,
) -> reqwest::RequestBuilder {
    with_timiai_headers(request, api_key, session_id)
}

fn with_timiai_headers(
    request: reqwest::RequestBuilder,
    api_key: &str,
    session_id: &str,
) -> reqwest::RequestBuilder {
    let key = timiai_authorization_header_value(api_key);
    request
        .header("Authorization", key.clone())
        .header("x-api-key", key)
        .header("X-Session-Id", session_id)
}

fn timiai_authorization_header_value(api_key: &str) -> String {
    api_key.trim().to_string()
}

fn timiai_authorization_log_state(api_key: &str) -> &'static str {
    let trimmed = api_key.trim();
    if trimmed.is_empty() {
        "empty"
    } else if trimmed
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("bearer "))
    {
        "bearer_value"
    } else {
        "raw_value"
    }
}

fn upstream_messages_url(provider: &str) -> &'static str {
    match normalize_proxy_provider(provider) {
        "timiai" => TIMIAI_MESSAGES_URL,
        "commandcode" => COMMANDCODE_UPSTREAM_MESSAGES_URL,
        "kimi_code" => KIMI_UPSTREAM_MESSAGES_URL,
        "xiaomi_mimo" => MIMO_UPSTREAM_MESSAGES_URL,
        _ => KIMI_UPSTREAM_MESSAGES_URL,
    }
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

fn upstream_native_anthropic_model<'a>(provider: &str, model: &'a str) -> &'a str {
    match (normalize_proxy_provider(provider), model) {
        ("xiaomi_mimo", "MiMo-V2.5-Pro") => "mimo-v2.5-pro",
        ("xiaomi_mimo", "MiMo-V2.5") => "mimo-v2.5",
        _ => model,
    }
}

fn is_claude_family_model(model: &str) -> bool {
    model.trim().to_ascii_lowercase().starts_with("claude-")
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
    let anthropic_payload = chat_payload_to_anthropic_payload(chat_payload);
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
        let reasoning_content = reasoning_content_for_assistant_message(message)
            .or_else(|| reasoning_content_for_text(content))
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

fn chat_payload_to_anthropic_payload(mut chat: Value) -> Value {
    chat["stream"] = Value::Bool(false);
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

            let mut content = Vec::new();
            let text = response_content_to_text(message.get("content").unwrap_or(&Value::Null));
            if !text.is_empty() {
                content.push(json!({ "type": "text", "text": text }));
            }
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
    let mut text_parts = Vec::new();
    for part in parts {
        match part.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        text_parts.push(text.to_string());
                    }
                }
            }
            "tool_result" => {
                if !text_parts.is_empty() {
                    messages.push(json!({ "role": "user", "content": text_parts.join("\n") }));
                    text_parts.clear();
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
    if !text_parts.is_empty() || messages.is_empty() {
        messages.push(json!({ "role": "user", "content": text_parts.join("\n") }));
    }
    messages
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

fn responses_tools_to_chat_tools(value: Option<&Value>) -> Option<Value> {
    let tools = value?.as_array()?;
    let converted = tools
        .iter()
        .filter_map(|tool| {
            match tool.get("type").and_then(Value::as_str) {
                Some("function") => {
                    let name = tool.get("name").and_then(Value::as_str)?;
                    Some(json!({
                        "type": "function",
                        "function": {
                            "name": name,
                            "description": tool.get("description").cloned().unwrap_or(Value::String(String::new())),
                            "parameters": tool.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} }))
                        }
                    }))
                }
                Some("custom") if tool.get("name").and_then(Value::as_str) == Some("apply_patch") => {
                    Some(json!({
                        "type": "function",
                        "function": {
                            "name": "apply_patch",
                            "description": "Edit files by applying a patch. Put the complete raw patch text in the `patch` string. Do not wrap the patch in shell commands, here-strings, or JSON inside the string.",
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
                    }))
                }
                _ => None,
            }
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
    if name == "apply_patch" {
        return json!({
            "id": id,
            "type": "custom_tool_call",
            "call_id": id,
            "name": name,
            "input": apply_patch_input_from_function_arguments(arguments),
            "status": "completed"
        });
    }
    json!({
        "id": id,
        "type": "function_call",
        "call_id": id,
        "name": name,
        "arguments": arguments,
        "status": "completed"
    })
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
                    if name == "apply_patch" {
                        output.push(json!({
                            "id": id,
                            "type": "custom_tool_call",
                            "call_id": id,
                            "name": name,
                            "input": apply_patch_input_from_function_arguments(&arguments),
                            "status": "completed"
                        }));
                    } else {
                        output.push(json!({
                            "id": id,
                            "type": "function_call",
                            "call_id": id,
                            "name": name,
                            "arguments": arguments,
                            "status": "completed"
                        }));
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

fn streaming_chat_sse_to_anthropic_response(
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

fn streaming_passthrough_response(
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

fn streaming_timiai_responses_response(
    upstream: reqwest::Response,
    status: StatusCode,
    content_type: &str,
) -> Response<ProxyBody> {
    let upstream_stream = upstream.bytes_stream();
    let stream = futures::stream::unfold(
        (
            upstream_stream,
            TimiaiResponsesSseSanitizer::default(),
            false,
        ),
        |(mut upstream_stream, mut sanitizer, done)| async move {
            if done {
                return None;
            }
            loop {
                match upstream_stream.next().await {
                    Some(Ok(chunk)) => {
                        let bytes = sanitizer.push_chunk(&chunk);
                        if bytes.is_empty() {
                            continue;
                        }
                        return Some((
                            Ok(Frame::data(Bytes::from(bytes))),
                            (upstream_stream, sanitizer, false),
                        ));
                    }
                    Some(Err(error)) => {
                        append_codex_api_proxy_log(&format!(
                            "timiai_responses_sse_read_error error={error}"
                        ));
                        let bytes = sanitizer.finish();
                        if bytes.is_empty() {
                            return None;
                        }
                        return Some((
                            Ok(Frame::data(Bytes::from(bytes))),
                            (upstream_stream, sanitizer, true),
                        ));
                    }
                    None => {
                        let bytes = sanitizer.finish();
                        if bytes.is_empty() {
                            return None;
                        }
                        return Some((
                            Ok(Frame::data(Bytes::from(bytes))),
                            (upstream_stream, sanitizer, true),
                        ));
                    }
                }
            }
        },
    );
    let mut response = Response::new(BodyExt::boxed(StreamBody::new(stream)));
    *response.status_mut() = status;
    if let Ok(value) = content_type.parse() {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    response
}

#[derive(Debug, Default)]
struct TimiaiResponsesSseSanitizer {
    buffer: String,
    removed_reasoning_events: usize,
}

impl TimiaiResponsesSseSanitizer {
    fn push_chunk(&mut self, chunk: &[u8]) -> Vec<u8> {
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

    fn finish(&mut self) -> Vec<u8> {
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

fn sse_event_name(event: &str) -> Option<&str> {
    event
        .lines()
        .find_map(|line| line.strip_prefix("event:").map(str::trim))
}

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

fn sanitize_timiai_responses_response_body(body: &[u8]) -> Option<Vec<u8>> {
    let mut value = serde_json::from_slice::<Value>(body).ok()?;
    sanitize_responses_value(&mut value);
    serde_json::to_vec(&value).ok()
}

fn sanitize_responses_value(value: &mut Value) {
    remove_reasoning_output_items(value);
    if let Some(response) = value.get_mut("response") {
        remove_reasoning_output_items(response);
    }
}

fn remove_reasoning_output_items(value: &mut Value) {
    let Some(output) = value.get_mut("output").and_then(Value::as_array_mut) else {
        return;
    };
    output.retain(|item| item.get("type").and_then(Value::as_str) != Some("reasoning"));
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
                response_id: "resp_proxy".to_string(),
                model: CHAT_MODEL_FALLBACK.to_string(),
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

#[cfg(test)]
fn chat_sse_to_anthropic_sse(body: &[u8]) -> Vec<u8> {
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
    fn new() -> Self {
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

    fn push_chunk(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut output = String::new();
        while let Some((event, consumed)) = next_sse_event(&self.buffer) {
            self.buffer.drain(..consumed);
            process_chat_sse_anthropic_event(&event, &mut output, &mut self.state);
        }
        output.into_bytes()
    }

    fn finish(&mut self) -> Vec<u8> {
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

        let chat = responses_payload_to_chat_payload(payload, "timiai").unwrap();

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
    fn converts_apply_patch_custom_tool_to_chat_function_tool() {
        let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch";
        let payload = json!({
            "model": "glm-5.1",
            "input": [
                { "role": "user", "content": "edit" },
                {
                    "type": "custom_tool_call",
                    "call_id": "call_patch",
                    "name": "apply_patch",
                    "input": patch
                },
                {
                    "type": "custom_tool_call_output",
                    "call_id": "call_patch",
                    "output": "Done"
                }
            ],
            "tools": [{
                "type": "custom",
                "name": "apply_patch",
                "description": "Use the `apply_patch` tool to edit files.",
                "format": { "type": "grammar", "syntax": "lark", "definition": "start: begin_patch" }
            }]
        });

        let chat = responses_payload_to_chat_payload(payload, "timiai").unwrap();

        assert_eq!(
            chat["tools"][0]["function"]["parameters"]["properties"]["patch"]["type"],
            "string"
        );
        assert_eq!(
            chat["messages"][1]["tool_calls"][0]["function"]["name"],
            "apply_patch"
        );
        let arguments = chat["messages"][1]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap();
        let arguments: Value = serde_json::from_str(arguments).unwrap();
        assert_eq!(arguments["patch"], patch);
        assert_eq!(chat["messages"][2]["role"], "tool");
        assert_eq!(chat["messages"][2]["tool_call_id"], "call_patch");
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
    fn unsupported_provider_aliases_fall_back_to_timiai() {
        assert_eq!(normalize_proxy_provider("unsupported"), "timiai");
        assert_eq!(normalize_proxy_provider("legacy-gateway"), "timiai");
    }

    #[test]
    fn timiai_provider_uses_native_responses_and_messages() {
        assert_eq!(normalize_proxy_provider("timi-ai"), "timiai");
        assert_eq!(upstream_responses_url("timiai"), TIMIAI_RESPONSES_URL);
        assert_eq!(upstream_messages_url("timiai"), TIMIAI_MESSAGES_URL);
        assert!(is_claude_family_model("claude-sonnet-4.6"));
        assert!(!is_claude_family_model("gpt-5.5"));
        assert_eq!(
            proxy_provider_for_model("deepseek-v4-pro", "timiai"),
            "timiai"
        );
    }

    #[test]
    fn converts_non_claude_anthropic_request_to_timiai_responses_payload() {
        let anthropic = json!({
            "model": "gpt-5.5",
            "max_tokens": 1024,
            "stream": true,
            "system": [{
                "type": "text",
                "text": "You are helpful",
                "cache_control": { "type": "ephemeral" }
            }],
            "messages": [
                { "role": "user", "content": "hello" },
                {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "call_1",
                        "name": "read_file",
                        "input": { "path": "README.md" }
                    }]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "call_1",
                        "content": "file body"
                    }]
                }
            ],
            "tools": [{
                "name": "read_file",
                "description": "Read a file",
                "input_schema": {
                    "type": "object",
                    "properties": { "path": { "type": "string" } }
                }
            }],
            "tool_choice": { "type": "auto" }
        });

        let responses = anthropic_payload_to_responses_payload(anthropic);

        assert_eq!(responses["model"], "gpt-5.5");
        assert_eq!(responses["max_output_tokens"], 1024);
        assert_eq!(responses["stream"], true);
        assert_eq!(responses["instructions"], "You are helpful");
        assert_eq!(responses["input"][0]["role"], "user");
        assert_eq!(responses["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(responses["input"][1]["type"], "function_call");
        assert_eq!(responses["input"][1]["name"], "read_file");
        assert_eq!(responses["input"][2]["type"], "function_call_output");
        assert_eq!(responses["tools"][0]["type"], "function");
        assert_eq!(responses["tools"][0]["name"], "read_file");
        assert_eq!(responses["tool_choice"], "auto");
    }

    #[test]
    fn converts_timiai_responses_response_to_anthropic_message() {
        let responses = json!({
            "id": "resp_1",
            "model": "gpt-5.5",
            "output": [
                {
                    "type": "message",
                    "content": [{ "type": "output_text", "text": "checking" }]
                },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"README.md\"}"
                }
            ],
            "usage": { "input_tokens": 12, "output_tokens": 4 }
        });

        let anthropic = responses_response_to_anthropic_response(responses);

        assert_eq!(anthropic["id"], "resp_1");
        assert_eq!(anthropic["model"], "gpt-5.5");
        assert_eq!(anthropic["content"][0]["type"], "text");
        assert_eq!(anthropic["content"][0]["text"], "checking");
        assert_eq!(anthropic["content"][1]["type"], "tool_use");
        assert_eq!(anthropic["content"][1]["id"], "call_1");
        assert_eq!(anthropic["content"][1]["input"]["path"], "README.md");
        assert_eq!(anthropic["stop_reason"], "tool_use");
    }

    #[test]
    fn sanitizes_timiai_responses_payload_extensions() {
        let payload = json!({
            "model": "gpt-5.5",
            "input": [
                {
                    "type": "reasoning",
                    "summary": [],
                    "content": null
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": "hello"
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "phase": "commentary",
                    "content": [{ "type": "output_text", "text": "interim note" }]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "phase": "final_answer",
                    "content": [{ "type": "output_text", "text": "final" }]
                }
            ],
            "context_management": { "strategy": "auto" },
            "reasoning": { "effort": "medium" },
            "stream": true
        });

        let sanitized = sanitize_timiai_responses_payload(payload);

        assert!(sanitized.get("context_management").is_none());
        assert!(sanitized.get("reasoning").is_none());
        assert_eq!(sanitized["model"], "gpt-5.5");
        assert_eq!(sanitized["input"].as_array().unwrap().len(), 2);
        assert_eq!(sanitized["input"][0]["type"], "message");
        assert_eq!(sanitized["input"][0]["content"], "hello");
        assert_eq!(sanitized["input"][1]["phase"], "final_answer");
        assert_eq!(sanitized["stream"], true);
    }

    #[test]
    fn timiai_responses_payload_is_prepared_before_upstream_logging() {
        let payload = json!({
            "model": "gpt-5.5",
            "input": [
                { "type": "reasoning", "summary": [] },
                {
                    "type": "message",
                    "role": "assistant",
                    "phase": "commentary",
                    "content": [{ "type": "output_text", "text": "planning" }]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "phase": "final_answer",
                    "content": [{ "type": "output_text", "text": "done" }]
                }
            ],
            "reasoning": { "effort": "medium" }
        });

        let prepared = prepare_responses_payload_for_provider(payload, "timiai");

        assert!(prepared.get("reasoning").is_none());
        assert_eq!(prepared["input"].as_array().unwrap().len(), 1);
        assert_eq!(prepared["input"][0]["phase"], "final_answer");
        assert_eq!(prepared["input"][0]["content"][0]["text"], "done");
    }

    #[test]
    fn sanitizes_timiai_responses_sse_reasoning_items() {
        let body = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"summary\":[]}}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"id\":\"msg_1\",\"type\":\"message\",\"content\":[]}}\n\n",
            "event: response.reasoning_text.delta\n",
            "data: {\"type\":\"response.reasoning_text.delta\",\"delta\":\"hidden\"}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"visible\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"reasoning\",\"summary\":[]},{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"visible\"}]}]}}\n\n",
            "data: [DONE]\n\n",
        );

        let mut sanitizer = TimiaiResponsesSseSanitizer::default();
        let mut sanitized = sanitizer.push_chunk(body.as_bytes());
        sanitized.extend(sanitizer.finish());
        let text = String::from_utf8(sanitized).unwrap();

        assert!(!text.contains("response.reasoning_text.delta"));
        assert!(!text.contains("\"type\":\"reasoning\""));
        assert!(text.contains("response.output_text.delta"));
        assert!(text.contains("visible"));
        assert!(text.contains("[DONE]"));
    }

    #[test]
    fn sanitizes_timiai_anthropic_messages_payload_extensions() {
        let payload = json!({
            "model": "claude-opus-4.8",
            "context_management": { "strategy": "auto" },
            "messages": [{ "role": "user", "content": "hello" }],
            "tools": [{ "name": "read_file" }]
        });

        let sanitized = sanitize_timiai_anthropic_messages_payload(payload);

        assert!(sanitized.get("context_management").is_none());
        assert_eq!(sanitized["model"], "claude-opus-4.8");
        assert_eq!(sanitized["messages"][0]["role"], "user");
        assert_eq!(sanitized["tools"][0]["name"], "read_file");
    }

    #[test]
    fn timiai_session_id_is_reused_from_proxy_config() {
        let mut session_ids = BTreeMap::new();
        session_ids.insert("timiai".to_string(), "session-1".to_string());
        let config = CodexApiProxyConfig {
            provider: "timiai".to_string(),
            api_key: "secret".to_string(),
            api_keys: BTreeMap::new(),
            session_ids,
        };

        assert_eq!(
            session_id_for_proxy_provider(&config, "timiai"),
            "session-1"
        );
    }

    #[test]
    fn timiai_authorization_header_uses_saved_key_without_bearer_injection() {
        assert_eq!(
            timiai_authorization_header_value("timiai-secret"),
            "timiai-secret"
        );
        assert_eq!(
            timiai_authorization_header_value("  timiai-secret  "),
            "timiai-secret"
        );
        assert_eq!(
            timiai_authorization_header_value("Bearer timiai-secret"),
            "Bearer timiai-secret"
        );
        assert_eq!(timiai_authorization_log_state("timiai-secret"), "raw_value");
        assert_eq!(
            timiai_authorization_log_state("Bearer timiai-secret"),
            "bearer_value"
        );
    }

    #[test]
    fn timiai_upstream_headers_include_x_api_key() {
        let request = with_timiai_headers(
            reqwest::Client::new().post("http://example.com"),
            " timiai-secret ",
            "session-1",
        )
        .build()
        .unwrap();

        assert_eq!(
            request
                .headers()
                .get("Authorization")
                .and_then(|value| value.to_str().ok()),
            Some("timiai-secret")
        );
        assert_eq!(
            request
                .headers()
                .get("x-api-key")
                .and_then(|value| value.to_str().ok()),
            Some("timiai-secret")
        );
        assert_eq!(
            request
                .headers()
                .get("X-Session-Id")
                .and_then(|value| value.to_str().ok()),
            Some("session-1")
        );
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
    fn converts_apply_patch_chat_function_call_to_custom_tool_call() {
        let patch = "*** Begin Patch\n*** Add File: probe.txt\n+ok\n*** End Patch";
        let chat = json!({
            "id": "chatcmpl_1",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_patch",
                        "type": "function",
                        "function": {
                            "name": "apply_patch",
                            "arguments": serde_json::to_string(&json!({ "patch": patch })).unwrap()
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let response = chat_response_to_responses_response(chat).unwrap();

        assert_eq!(response["output"][0]["type"], "custom_tool_call");
        assert_eq!(response["output"][0]["call_id"], "call_patch");
        assert_eq!(response["output"][0]["name"], "apply_patch");
        assert_eq!(response["output"][0]["input"], patch);
    }

    #[test]
    fn converts_chat_payload_to_kimi_anthropic_messages() {
        let chat = json!({
            "model": "kimi-for-coding",
            "stream": true,
            "max_tokens": 4096,
            "temperature": 0.2,
            "messages": [
                { "role": "system", "content": "base" },
                { "role": "user", "content": "hello" },
                {
                    "role": "assistant",
                    "content": "checking",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "read_file", "arguments": "{\"path\":\"main.rs\"}" }
                    }]
                },
                { "role": "tool", "tool_call_id": "call_1", "content": "file body" }
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "Read file",
                    "parameters": { "type": "object", "properties": { "path": { "type": "string" } } }
                }
            }]
        });

        let anthropic = chat_payload_to_anthropic_payload(chat);

        assert_eq!(anthropic["model"], "kimi-for-coding");
        assert_eq!(anthropic["max_tokens"], 4096);
        assert_eq!(anthropic["system"], "base");
        assert_eq!(anthropic["messages"][0]["role"], "user");
        assert_eq!(anthropic["messages"][0]["content"][0]["text"], "hello");
        assert_eq!(anthropic["messages"][1]["role"], "assistant");
        assert_eq!(anthropic["messages"][1]["content"][0]["text"], "checking");
        assert_eq!(anthropic["messages"][1]["content"][1]["type"], "tool_use");
        assert_eq!(anthropic["messages"][1]["content"][1]["name"], "read_file");
        assert_eq!(
            anthropic["messages"][1]["content"][1]["input"]["path"],
            "main.rs"
        );
        assert_eq!(anthropic["messages"][2]["role"], "user");
        assert_eq!(
            anthropic["messages"][2]["content"][0]["type"],
            "tool_result"
        );
        assert_eq!(
            anthropic["messages"][2]["content"][0]["tool_use_id"],
            "call_1"
        );
        assert_eq!(anthropic["tools"][0]["name"], "read_file");
        assert_eq!(
            anthropic["tools"][0]["input_schema"]["properties"]["path"]["type"],
            "string"
        );
        assert!(anthropic.get("stream").is_none());
    }

    #[test]
    fn converts_anthropic_tools_to_chat_completion_tools() {
        let anthropic = json!({
            "model": "deepseek-v4-pro",
            "stream": true,
            "max_tokens": 4096,
            "messages": [
                { "role": "user", "content": [{ "type": "text", "text": "inspect" }] }
            ],
            "tools": [{
                "name": "Read",
                "description": "Read a file",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string" }
                    },
                    "required": ["file_path"]
                }
            }],
            "tool_choice": { "type": "auto" }
        });

        let chat = anthropic_payload_to_chat_payload(anthropic);

        assert_eq!(chat["model"], "deepseek-v4-pro");
        assert_eq!(chat["stream"], true);
        assert_eq!(chat["messages"][0]["role"], "user");
        assert_eq!(chat["messages"][0]["content"], "inspect");
        assert_eq!(chat["tools"][0]["type"], "function");
        assert_eq!(chat["tools"][0]["function"]["name"], "Read");
        assert_eq!(
            chat["tools"][0]["function"]["parameters"]["properties"]["file_path"]["type"],
            "string"
        );
        assert_eq!(chat["tool_choice"], "auto");
    }

    #[test]
    fn converts_anthropic_tool_history_to_chat_completion_messages() {
        let anthropic = json!({
            "model": "deepseek-v4-pro",
            "messages": [
                {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "call_1",
                        "name": "Read",
                        "input": { "file_path": "README.md" }
                    }]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "call_1",
                        "content": "file body"
                    }]
                }
            ]
        });

        let chat = anthropic_payload_to_chat_payload(anthropic);

        assert_eq!(chat["messages"][0]["role"], "assistant");
        assert!(chat["messages"][0]["content"].is_null());
        assert_eq!(chat["messages"][0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(
            chat["messages"][0]["tool_calls"][0]["function"]["name"],
            "Read"
        );
        assert_eq!(
            chat["messages"][0]["tool_calls"][0]["function"]["arguments"],
            "{\"file_path\":\"README.md\"}"
        );
        assert_eq!(chat["messages"][1]["role"], "tool");
        assert_eq!(chat["messages"][1]["tool_call_id"], "call_1");
        assert_eq!(chat["messages"][1]["content"], "file body");
    }

    #[test]
    fn converts_kimi_anthropic_response_to_responses_response() {
        let anthropic = json!({
            "id": "msg_1",
            "model": "kimi-for-coding",
            "content": [
                { "type": "text", "text": "I will read it." },
                {
                    "type": "tool_use",
                    "id": "call_1",
                    "name": "read_file",
                    "input": { "path": "main.rs" }
                }
            ],
            "usage": { "input_tokens": 12, "output_tokens": 5 }
        });

        let response = anthropic_response_to_responses_response(anthropic);

        assert_eq!(response["id"], "msg_1");
        assert_eq!(response["model"], "kimi-for-coding");
        assert_eq!(response["output"][0]["type"], "message");
        assert_eq!(
            response["output"][0]["content"][0]["text"],
            "I will read it."
        );
        assert_eq!(response["output"][1]["type"], "function_call");
        assert_eq!(response["output"][1]["call_id"], "call_1");
        assert_eq!(response["output"][1]["name"], "read_file");
        assert_eq!(response["output"][1]["arguments"], "{\"path\":\"main.rs\"}");
        assert_eq!(response["usage"]["input_tokens"], 12);
        assert_eq!(response["usage"]["output_tokens"], 5);
        assert_eq!(response["usage"]["total_tokens"], 17);
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
                "id": "msg_proxy",
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
    fn converts_chat_stream_to_anthropic_stream() {
        let body = concat!(
            "data:{\"id\":\"chatcmpl_1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
            "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"total_tokens\":15}}\n\n",
            "data: [DONE]\n\n"
        );

        let normalized = chat_sse_to_anthropic_sse(body.as_bytes());
        let text = String::from_utf8(normalized).unwrap();

        assert!(text.contains("event: message_start"));
        assert!(text.contains("event: content_block_start"));
        assert!(text.contains("event: content_block_delta"));
        assert!(text.contains("\"type\":\"text_delta\""));
        assert!(text.contains("\"text\":\"hello\""));
        assert!(text.contains("event: content_block_stop"));
        assert!(text.contains("event: message_delta"));
        assert!(text.contains("\"stop_reason\":\"end_turn\""));
        assert!(text.contains("\"input_tokens\":12"));
        assert!(text.contains("event: message_stop"));
    }

    #[test]
    fn preserves_markdown_newlines_in_anthropic_stream() {
        let body = concat!(
            "data:{\"id\":\"chatcmpl_1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"delta\":{\"content\":\"核心功能是：\\n\\n1. 第一项\\n\"}}]}\n\n",
            "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"content\":\"2. 第二项\"}}]}\n\n",
            "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n",
            "data: [DONE]\n\n"
        );

        let normalized = chat_sse_to_anthropic_sse(body.as_bytes());
        let text = String::from_utf8(normalized).unwrap();

        assert!(text.contains("核心功能是：\\n\\n1. 第一项\\n"));
        assert!(text.contains("2. 第二项"));
    }

    #[test]
    fn converts_chat_tool_stream_to_anthropic_tool_use() {
        let body = concat!(
            "data:{\"id\":\"chatcmpl_1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"list_files\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
            "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\".\\\"}\"}}]}}]}\n\n",
            "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"finish_reason\":\"tool_calls\",\"delta\":{}}]}\n\n",
            "data: [DONE]\n\n"
        );

        let normalized = chat_sse_to_anthropic_sse(body.as_bytes());
        let text = String::from_utf8(normalized).unwrap();

        assert!(text.contains("\"type\":\"tool_use\""));
        assert!(text.contains("\"id\":\"call_1\""));
        assert!(text.contains("\"name\":\"list_files\""));
        assert!(text.contains("\"type\":\"input_json_delta\""));
        assert!(text.contains("\"partial_json\":\"{\\\"path\\\":\""));
        assert!(text.contains("\"partial_json\":\"\\\".\\\"}\""));
        assert!(text.contains("\"stop_reason\":\"tool_use\""));
    }

    #[test]
    fn restores_deepseek_reasoning_for_anthropic_tool_history() {
        let body = concat!(
            "data:{\"id\":\"chatcmpl_1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"delta\":{\"reasoning_content\":\"tool thinking\"}}]}\n\n",
            "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_cached_tool\",\"type\":\"function\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"file_path\\\":\\\"README.md\\\"}\"}}]}}]}\n\n",
            "data:{\"id\":\"chatcmpl_1\",\"choices\":[{\"finish_reason\":\"tool_calls\",\"delta\":{}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let _ = chat_sse_to_anthropic_sse(body.as_bytes());

        let anthropic = json!({
            "model": "deepseek-v4-pro",
            "messages": [{
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "call_cached_tool",
                    "name": "Read",
                    "input": { "file_path": "README.md" }
                }]
            }]
        });

        let chat = anthropic_payload_to_chat_payload(anthropic);

        assert_eq!(chat["messages"][0]["reasoning_content"], "tool thinking");
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
    fn rewrites_xiaomi_anthropic_display_model_to_upstream_slug() {
        let payload = json!({
            "model": "MiMo-V2.5-Pro",
            "messages": [{ "role": "user", "content": "hello" }],
            "stream": true
        });

        let normalized = normalize_native_anthropic_payload(payload, "xiaomi_mimo");

        assert_eq!(normalized["model"], "mimo-v2.5-pro");
    }

    #[test]
    fn leaves_non_xiaomi_anthropic_model_names_unchanged() {
        let payload = json!({
            "model": "kimi-for-coding",
            "messages": [{ "role": "user", "content": "hello" }]
        });

        let normalized = normalize_native_anthropic_payload(payload, "kimi_code");

        assert_eq!(normalized["model"], "kimi-for-coding");
    }

    #[test]
    fn maps_xiaomi_router_queue_limitation_to_http_429() {
        let body = br#"{
            "error": {
                "code": "429",
                "message": "Cluster rate limit exceeded, request queued but not admitted",
                "type": "router_queue_limitation"
            }
        }"#;

        let status = normalize_upstream_error_status(StatusCode::BAD_REQUEST, body);

        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
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
