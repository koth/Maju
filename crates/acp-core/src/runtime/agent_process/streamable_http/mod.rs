use super::RemoteAcpTransport;
use super::process_lifecycle::io_other;
use crate::events::SessionConfig;
use crate::mapping::append_runtime_event_log;
use agent_client_protocol::{Client, ConnectTo, Lines, Role};
use futures::channel::mpsc as futures_mpsc;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod sse;
mod wire;

use sse::{feed_streamable_http_payload, spawn_streamable_http_sse_consumer};
pub(super) use wire::message_has_method;
use wire::{
    acp_method_from_message, acp_session_cwd_from_message, acp_session_header,
    acp_session_id_from_message, acp_session_token_header, codebuddy_http_headers,
    remember_acp_connection_id_from_headers, remember_acp_connection_id_from_payload,
    remember_acp_session_token_from_payload,
};

pub(super) const STREAMABLE_HTTP_PATH: &str = "/api/v1/acp";
pub(super) const CODEBUDDY_RESOLVE_INTERRUPTION_METHOD: &str = "_codebuddy.ai/resolveInterruption";
const CODEBUDDY_REQUEST_HEADER: &str = "X-CodeBuddy-Request";
const CODEBUDDY_ACP_CONNECTION_ID_HEADER: &str = "acp-connection-id";
const ACP_CONNECTION_ID_HEADER: &str = "Acp-Connection-Id";
const ACP_SESSION_ID_HEADER: &str = "Acp-Session-Id";
const ACP_SESSION_TOKEN_HEADER: &str = "acp-session-token";
const CODEBUDDY_ACP_CONNECT_ATTEMPTS: u16 = 80;

pub(super) fn connect_streamable_http_endpoint(
    endpoint: String,
    client: impl ConnectTo<<Client as Role>::Counterpart>,
    acp_transport: RemoteAcpTransport,
    log_config: Option<SessionConfig>,
) -> agent_client_protocol::Result<
    impl std::future::Future<Output = agent_client_protocol::Result<()>>,
> {
    let http = reqwest::Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|error| {
            agent_client_protocol::util::internal_error(format!(
                "failed to create streamable-http client: {error}"
            ))
        })?;
    let (incoming_tx, incoming_lines) = futures_mpsc::unbounded::<std::io::Result<String>>();
    let connection_id = Arc::new(Mutex::new(None));
    let session_token = Arc::new(Mutex::new(None));
    let get_task = Arc::new(Mutex::new(None));
    let last_initialize = Arc::new(Mutex::new(None));
    let current_session_id = Arc::new(Mutex::new(None));
    let current_session_cwd = Arc::new(Mutex::new(None));
    let state = StreamableHttpState {
        http: http.clone(),
        endpoint: endpoint.clone(),
        incoming_tx: incoming_tx.clone(),
        connection_id: connection_id.clone(),
        session_token: session_token.clone(),
        get_task: get_task.clone(),
        acp_transport,
        last_initialize: last_initialize.clone(),
        current_session_id: current_session_id.clone(),
        current_session_cwd: current_session_cwd.clone(),
        log_config: log_config.clone(),
    };

    if let Some(config) = log_config.as_ref() {
        let connection_id = connection_id.lock().ok().and_then(|guard| guard.clone());
        let _ = append_runtime_event_log(
            config,
            "agent/streamable_http_connecting",
            &json!({
                "endpoint": endpoint,
                "connection_id": connection_id
            }),
        );
    }

    let outgoing_sink = futures::sink::unfold(state, |state, line: String| async move {
        post_streamable_http_line(&state, line).await?;
        Ok::<_, std::io::Error>(state)
    });

    let protocol = agent_client_protocol::ConnectTo::<Client>::connect_to(
        Lines::new(outgoing_sink, incoming_lines),
        client,
    );

    let connect_http = http.clone();
    let connect_endpoint = endpoint.clone();
    let connect_connection_id = connection_id.clone();
    let connect_session_token = session_token.clone();
    let connect_log_config = log_config.clone();
    Ok(async move {
        match acp_transport {
            RemoteAcpTransport::AcpStreamableHttp => {
                acp_streamable_http_connect(
                    &connect_endpoint,
                    &connect_http,
                    &connect_connection_id,
                    &connect_session_token,
                    connect_log_config.as_ref(),
                )
                .await
                .map_err(|error| {
                    agent_client_protocol::util::internal_error(format!(
                        "ACP streamable-http connect failed: {error}"
                    ))
                })?;
            }
            RemoteAcpTransport::CodeBuddyServeHttp => {
                codebuddy_acp_connect(
                    &connect_endpoint,
                    &connect_http,
                    &connect_connection_id,
                    &connect_session_token,
                    connect_log_config.as_ref(),
                )
                .await
                .map_err(|error| {
                    agent_client_protocol::util::internal_error(format!(
                        "CodeBuddy ACP streamable-http connect failed: {error}"
                    ))
                })?;
            }
            RemoteAcpTransport::Tcp => {}
        }
        let state = StreamableHttpState {
            http,
            endpoint,
            incoming_tx,
            connection_id,
            session_token,
            get_task: get_task.clone(),
            acp_transport,
            last_initialize,
            current_session_id,
            current_session_cwd,
            log_config,
        };
        maybe_spawn_streamable_http_get(&state);
        let result = protocol.await;
        if let Ok(mut guard) = get_task.lock() {
            if let Some(task) = guard.take() {
                task.abort();
            }
        }
        result
    })
}

#[derive(Clone)]
pub(super) struct StreamableHttpState {
    pub(super) http: reqwest::Client,
    pub(super) endpoint: String,
    pub(super) incoming_tx: futures_mpsc::UnboundedSender<std::io::Result<String>>,
    pub(super) connection_id: Arc<Mutex<Option<String>>>,
    pub(super) session_token: Arc<Mutex<Option<String>>>,
    pub(super) get_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    pub(super) acp_transport: RemoteAcpTransport,
    pub(super) last_initialize: Arc<Mutex<Option<serde_json::Value>>>,
    pub(super) current_session_id: Arc<Mutex<Option<String>>>,
    pub(super) current_session_cwd: Arc<Mutex<Option<String>>>,
    pub(super) log_config: Option<SessionConfig>,
}

async fn acp_streamable_http_connect(
    endpoint: &str,
    http: &reqwest::Client,
    connection_id: &Arc<Mutex<Option<String>>>,
    session_token: &Arc<Mutex<Option<String>>>,
    log_config: Option<&SessionConfig>,
) -> std::io::Result<()> {
    let connect_url = format!("{endpoint}/connect");
    let mut last_error = None;
    let mut response = None;
    for attempt in 0..CODEBUDDY_ACP_CONNECT_ATTEMPTS {
        match codebuddy_http_headers(
            http.post(&connect_url).header(ACCEPT, "application/json"),
            connection_id,
        )
        .send()
        .await
        {
            Ok(next_response) => {
                response = Some(next_response);
                break;
            }
            Err(error) => {
                last_error = Some(format!("{error:?}"));
                if let Some(config) = log_config {
                    let _ = append_runtime_event_log(
                        config,
                        "agent/acp_streamable_http_connect_retry",
                        &json!({
                            "url": connect_url,
                            "attempt": attempt + 1,
                            "attempts": CODEBUDDY_ACP_CONNECT_ATTEMPTS,
                            "error": last_error.as_deref()
                        }),
                    );
                }
                if attempt + 1 < CODEBUDDY_ACP_CONNECT_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        }
    }
    let Some(response) = response else {
        return Err(io_other(format!(
            "request failed after {CODEBUDDY_ACP_CONNECT_ATTEMPTS} attempts for {connect_url}: {}",
            last_error.unwrap_or_else(|| "unknown error".to_string())
        )));
    };
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(io_other(format!(
            "ACP streamable-http connect failed with status {status}: {body}"
        )));
    }
    remember_acp_connection_id_from_headers(response.headers(), connection_id);
    let payload = response.text().await.unwrap_or_default();
    remember_acp_connection_id_from_payload(&payload, connection_id);
    remember_acp_session_token_from_payload(&payload, session_token);
    let connection_id = connection_id.lock().ok().and_then(|guard| guard.clone());
    let session_token = session_token.lock().ok().and_then(|guard| guard.clone());
    if connection_id.is_none() {
        return Err(io_other(format!(
            "ACP streamable-http connect response missing connection id: {payload}"
        )));
    }
    if let Some(config) = log_config {
        let _ = append_runtime_event_log(
            config,
            "agent/acp_streamable_http_connected",
            &json!({ "connection_id": connection_id, "session_token_present": session_token.is_some() }),
        );
    }
    Ok(())
}

async fn codebuddy_acp_connect(
    endpoint: &str,
    http: &reqwest::Client,
    connection_id: &Arc<Mutex<Option<String>>>,
    session_token: &Arc<Mutex<Option<String>>>,
    log_config: Option<&SessionConfig>,
) -> std::io::Result<()> {
    let connect_url = format!("{endpoint}/connect");
    let mut last_error = None;
    let mut response = None;
    for attempt in 0..CODEBUDDY_ACP_CONNECT_ATTEMPTS {
        match codebuddy_http_headers(
            http.post(&connect_url).header(ACCEPT, "application/json"),
            connection_id,
        )
        .send()
        .await
        {
            Ok(next_response) => {
                response = Some(next_response);
                break;
            }
            Err(error) => {
                last_error = Some(format!("{error:?}"));
                if let Some(config) = log_config {
                    let _ = append_runtime_event_log(
                        config,
                        "agent/codebuddy_acp_connect_retry",
                        &json!({
                            "url": connect_url,
                            "attempt": attempt + 1,
                            "attempts": CODEBUDDY_ACP_CONNECT_ATTEMPTS,
                            "error": last_error.as_deref()
                        }),
                    );
                }
                if attempt + 1 < CODEBUDDY_ACP_CONNECT_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        }
    }
    let Some(response) = response else {
        return Err(io_other(format!(
            "request failed after {CODEBUDDY_ACP_CONNECT_ATTEMPTS} attempts for {connect_url}: {}",
            last_error.unwrap_or_else(|| "unknown error".to_string())
        )));
    };
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(io_other(format!(
            "CodeBuddy ACP connect failed with status {status}: {body}"
        )));
    }
    let payload = response.text().await.map_err(io_other)?;
    let (id, token) = codebuddy_connect_parts_from_payload(&payload)?;
    if let Ok(mut guard) = connection_id.lock() {
        *guard = Some(id.clone());
    }
    if let Some(token) = token.as_ref() {
        if let Ok(mut guard) = session_token.lock() {
            *guard = Some(token.clone());
        }
    }
    if let Some(config) = log_config {
        let _ = append_runtime_event_log(
            config,
            "agent/codebuddy_acp_connected",
            &json!({ "connection_id": id, "session_token_present": token.is_some() }),
        );
    }
    Ok(())
}

pub(super) fn codebuddy_connect_parts_from_payload(
    payload: &str,
) -> std::io::Result<(String, Option<String>)> {
    let value: serde_json::Value = serde_json::from_str(payload)
        .map_err(|error| io_other(format!("invalid CodeBuddy ACP connect response: {error}")))?;
    let data = value.get("data").unwrap_or(&value);
    let id = data
        .get("connectionId")
        .or_else(|| data.get("connection_id"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| io_other("CodeBuddy ACP connect response missing connectionId"))?;
    let token = data
        .get("sessionToken")
        .or_else(|| data.get("session_token"))
        .and_then(|value| value.as_str())
        .map(ToString::to_string);
    Ok((id.to_string(), token))
}

pub(super) async fn post_streamable_http_line(
    state: &StreamableHttpState,
    line: String,
) -> std::io::Result<()> {
    let body = serde_json::from_str::<serde_json::Value>(&line)
        .map_err(|error| io_other(format!("invalid ACP JSON-RPC message: {error}")))?;
    let expects_response_body = streamable_http_message_expects_response_body(&body);
    for attempt in 0..2 {
        match send_streamable_http_request_with_retry(state, &body).await {
            Ok(response) => {
                if !expects_response_body {
                    let result = handle_streamable_http_ack_response(
                        response,
                        &state.connection_id,
                        state.log_config.as_ref(),
                    )
                    .await;
                    match result {
                        Ok(()) => {
                            maybe_spawn_streamable_http_get(state);
                            return Ok(());
                        }
                        Err(error)
                            if attempt == 0 && streamable_connection_not_found_error(&error) =>
                        {
                            reconnect_streamable_http_for_retry(state, &body).await?;
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                }
                let result = handle_streamable_http_response(
                    response,
                    &state.connection_id,
                    &state.incoming_tx,
                    state.log_config.as_ref(),
                )
                .await;
                match result {
                    Ok(()) => {
                        maybe_spawn_streamable_http_get(state);
                        return Ok(());
                    }
                    Err(error) if attempt == 0 && streamable_connection_not_found_error(&error) => {
                        reconnect_streamable_http_for_retry(state, &body).await?;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) if attempt == 0 && streamable_connection_not_found_error(&error) => {
                reconnect_streamable_http_for_retry(state, &body).await?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub(super) async fn send_streamable_http_request_with_retry(
    state: &StreamableHttpState,
    body: &serde_json::Value,
) -> std::io::Result<reqwest::Response> {
    remember_streamable_http_outgoing_request(state, body);
    let session_id = streamable_http_session_id_for_message(state, body);
    let mut last_error = None;
    for attempt in 0..50 {
        let request = state
            .http
            .post(&state.endpoint)
            .header(ACCEPT, "text/event-stream, application/json");
        let request = codebuddy_http_headers(request, &state.connection_id);
        let request = acp_session_token_header(request, &state.session_token);
        let request = acp_session_header(request, session_id.as_deref());
        match request.json(body).send().await {
            Ok(response) => {
                if response.status().as_u16() == 409 {
                    let status = response.status();
                    let response_body = response.text().await.unwrap_or_default();
                    if codebuddy_connection_not_found_response(&response_body) {
                        reconnect_streamable_http_for_retry(state, &body).await?;
                        if attempt + 1 < 50 {
                            continue;
                        }
                    }
                    let connection_id = state
                        .connection_id
                        .lock()
                        .ok()
                        .and_then(|guard| guard.clone());
                    return Err(io_other(format!(
                        "streamable-http ACP request failed with status {status} using connection_id={connection_id:?}: {response_body}"
                    )));
                }
                return Ok(response);
            }
            Err(error) => {
                last_error = Some(format!("{error:?}"));
                if attempt + 1 < 50 {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    }
    Err(io_other(format!(
        "streamable-http ACP request failed after retries: {}",
        last_error.unwrap_or_else(|| "unknown error".to_string())
    )))
}

fn remember_streamable_http_outgoing_request(
    state: &StreamableHttpState,
    body: &serde_json::Value,
) {
    if message_has_method(body, "initialize") {
        if let Ok(mut guard) = state.last_initialize.lock() {
            *guard = Some(body.clone());
        }
    }

    if let Some(session_id) = acp_session_id_from_message(body) {
        if let Ok(mut guard) = state.current_session_id.lock() {
            *guard = Some(session_id);
        }
    }

    if let Some(cwd) = acp_session_cwd_from_message(body) {
        if let Ok(mut guard) = state.current_session_cwd.lock() {
            *guard = Some(cwd);
        }
    }
}

pub(super) fn streamable_http_session_id_for_message(
    state: &StreamableHttpState,
    body: &serde_json::Value,
) -> Option<String> {
    acp_session_id_from_message(body).or_else(|| {
        state
            .current_session_id
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    })
}

pub(super) fn streamable_http_message_expects_response_body(body: &serde_json::Value) -> bool {
    !streamable_http_message_is_response(body)
        && !message_has_method(body, CODEBUDDY_RESOLVE_INTERRUPTION_METHOD)
}

fn streamable_http_message_is_response(body: &serde_json::Value) -> bool {
    match body {
        serde_json::Value::Array(items) => {
            !items.is_empty() && items.iter().all(streamable_http_message_is_response)
        }
        serde_json::Value::Object(object) => {
            object.get("method").is_none()
                && object.get("id").is_some()
                && (object.get("result").is_some() || object.get("error").is_some())
        }
        _ => false,
    }
}

async fn reconnect_streamable_http_for_retry(
    state: &StreamableHttpState,
    retry_body: &serde_json::Value,
) -> std::io::Result<()> {
    if let Ok(mut guard) = state.get_task.lock() {
        if let Some(task) = guard.take() {
            task.abort();
        }
    }
    if let Ok(mut guard) = state.connection_id.lock() {
        guard.take();
    }
    if let Ok(mut guard) = state.session_token.lock() {
        guard.take();
    }

    reconnect_streamable_http_transport(state).await?;

    if let Some(config) = state.log_config.as_ref() {
        let _ = append_runtime_event_log(
            config,
            "agent/streamable_http_reconnected",
            &json!({
                "retry_method": acp_method_from_message(retry_body)
            }),
        );
    }

    maybe_spawn_streamable_http_get(state);

    if message_has_method(retry_body, "initialize") {
        return Ok(());
    }

    let initialize = state
        .last_initialize
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .ok_or_else(|| io_other("CodeBuddy ACP reconnect cannot replay initialize request"))?;
    send_streamable_http_control_request(state, initialize, "initialize").await?;

    if message_has_method(retry_body, "session/new")
        || message_has_method(retry_body, "session/load")
    {
        return Ok(());
    }

    let session_id = acp_session_id_from_message(retry_body).or_else(|| {
        state
            .current_session_id
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    });
    let Some(session_id) = session_id else {
        return Ok(());
    };
    let cwd = state
        .current_session_cwd
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_else(|| ".".to_string());
    send_streamable_http_control_request(
        state,
        codebuddy_session_load_request(&session_id, &cwd),
        "session/load",
    )
    .await
}

async fn reconnect_streamable_http_transport(state: &StreamableHttpState) -> std::io::Result<()> {
    match state.acp_transport {
        RemoteAcpTransport::AcpStreamableHttp => {
            acp_streamable_http_connect(
                &state.endpoint,
                &state.http,
                &state.connection_id,
                &state.session_token,
                state.log_config.as_ref(),
            )
            .await
        }
        RemoteAcpTransport::CodeBuddyServeHttp => {
            codebuddy_acp_connect(
                &state.endpoint,
                &state.http,
                &state.connection_id,
                &state.session_token,
                state.log_config.as_ref(),
            )
            .await
        }
        RemoteAcpTransport::Tcp => Err(io_other(
            "cannot reconnect TCP transport as streamable-http",
        )),
    }
}

async fn send_streamable_http_control_request(
    state: &StreamableHttpState,
    body: serde_json::Value,
    label: &str,
) -> std::io::Result<()> {
    let session_id = acp_session_id_from_message(&body);
    let request = state
        .http
        .post(&state.endpoint)
        .header(ACCEPT, "text/event-stream, application/json");
    let request = codebuddy_http_headers(request, &state.connection_id);
    let request = acp_session_token_header(request, &state.session_token);
    let request = acp_session_header(request, session_id.as_deref());
    let response = request.json(&body).send().await.map_err(io_other)?;
    if !response.status().is_success() {
        let status = response.status();
        let response_body = response.text().await.unwrap_or_default();
        return Err(io_other(format!(
            "CodeBuddy ACP reconnect {label} failed with status {status}: {response_body}"
        )));
    }
    remember_acp_connection_id_from_headers(response.headers(), &state.connection_id);
    let response_body = response.text().await.unwrap_or_default();
    remember_acp_connection_id_from_payload(&response_body, &state.connection_id);
    remember_acp_session_token_from_payload(&response_body, &state.session_token);
    Ok(())
}

pub(super) fn codebuddy_session_load_request(session_id: &str, cwd: &str) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": "kodex-codebuddy-reconnect-session-load",
        "method": "session/load",
        "params": {
            "sessionId": session_id,
            "cwd": cwd,
            "mcpServers": []
        }
    })
}

pub(super) fn codebuddy_connection_not_found_response(body: &str) -> bool {
    body.to_ascii_lowercase().contains("connection not found")
}

fn streamable_connection_not_found_error(error: &std::io::Error) -> bool {
    codebuddy_connection_not_found_response(&error.to_string())
}

fn maybe_spawn_streamable_http_get(state: &StreamableHttpState) {
    if state
        .connection_id
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .is_none()
    {
        return;
    }
    let should_spawn = state
        .get_task
        .lock()
        .map(|guard| guard.is_none())
        .unwrap_or(false);
    if !should_spawn {
        return;
    }

    let http = state.http.clone();
    let endpoint = state.endpoint.clone();
    let incoming_tx = state.incoming_tx.clone();
    let connection_id = state.connection_id.clone();
    let session_token = state.session_token.clone();
    let log_config = state.log_config.clone();
    let get_task_store = state.get_task.clone();
    let task = tokio::spawn(async move {
        if let Err(error) = run_streamable_http_get(
            http,
            endpoint,
            incoming_tx,
            connection_id,
            session_token,
            log_config,
        )
        .await
        {
            let _ = error;
        }
        if let Ok(mut guard) = get_task_store.lock() {
            guard.take();
        }
    });
    if let Ok(mut guard) = state.get_task.lock() {
        if guard.is_none() {
            *guard = Some(task);
        } else {
            task.abort();
        }
    } else {
        task.abort();
    }
}

async fn run_streamable_http_get(
    http: reqwest::Client,
    endpoint: String,
    incoming_tx: futures_mpsc::UnboundedSender<std::io::Result<String>>,
    connection_id: Arc<Mutex<Option<String>>>,
    session_token: Arc<Mutex<Option<String>>>,
    log_config: Option<SessionConfig>,
) -> std::io::Result<()> {
    let request = http.get(&endpoint).header(ACCEPT, "text/event-stream");
    let request = codebuddy_http_headers(request, &connection_id);
    let request = acp_session_token_header(request, &session_token);
    let response = request.send().await.map_err(io_other)?;
    if !response.status().is_success() {
        if let Some(config) = log_config.as_ref() {
            let _ = append_runtime_event_log(
                config,
                "agent/streamable_http_get_ignored",
                &json!({ "status": response.status().as_u16() }),
            );
        }
        return Ok(());
    }
    handle_streamable_http_response(response, &connection_id, &incoming_tx, log_config.as_ref())
        .await
}

async fn handle_streamable_http_ack_response(
    response: reqwest::Response,
    connection_id: &Arc<Mutex<Option<String>>>,
    log_config: Option<&SessionConfig>,
) -> std::io::Result<()> {
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let connection_id = connection_id.lock().ok().and_then(|guard| guard.clone());
        return Err(io_other(format!(
            "streamable-http ACP request failed with status {status} using connection_id={connection_id:?}: {body}"
        )));
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    remember_acp_connection_id_from_headers(response.headers(), connection_id);
    if !content_type.contains("text/event-stream") {
        let body = response.text().await.unwrap_or_default();
        remember_acp_connection_id_from_payload(&body, connection_id);
    }

    if let Some(config) = log_config {
        let _ = append_runtime_event_log(
            config,
            "agent/streamable_http_response_ack",
            &json!({ "contentType": content_type }),
        );
    }
    Ok(())
}

async fn handle_streamable_http_response(
    response: reqwest::Response,
    connection_id: &Arc<Mutex<Option<String>>>,
    incoming_tx: &futures_mpsc::UnboundedSender<std::io::Result<String>>,
    log_config: Option<&SessionConfig>,
) -> std::io::Result<()> {
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let connection_id = connection_id.lock().ok().and_then(|guard| guard.clone());
        return Err(io_other(format!(
            "streamable-http ACP request failed with status {status} using connection_id={connection_id:?}: {body}"
        )));
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    remember_acp_connection_id_from_headers(response.headers(), connection_id);
    if content_type.contains("text/event-stream") {
        spawn_streamable_http_sse_consumer(response, incoming_tx.clone(), log_config.cloned());
    } else {
        let body = response.text().await.map_err(io_other)?;
        remember_acp_connection_id_from_payload(&body, connection_id);
        feed_streamable_http_payload(&body, incoming_tx)?;
    }

    if let Some(config) = log_config {
        let _ = append_runtime_event_log(
            config,
            "agent/streamable_http_response",
            &json!({ "contentType": content_type }),
        );
    }
    Ok(())
}
