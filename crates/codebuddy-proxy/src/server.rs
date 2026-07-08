use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use crate::adapter::{AdapterOptions, build_session_options, run_non_streaming, run_streaming};
use crate::logging::append_codebuddy_proxy_log;
use crate::openai_types::OaiChatRequest;
use crate::session_pool::{SessionPool, tool_signature_of};
pub struct ProxyConfig {
    pub port: u16,
    pub default_model: String,
    pub cwd: Option<std::path::PathBuf>,
    pub max_turns: Option<u32>,
    pub max_sessions: usize,
    pub idle_timeout: Duration,
    pub api_key: Option<String>,
    /// Resolved CodeBuddy CLI binary path. When `Some`, the SDK spawns this
    /// binary directly (bypassing its own narrow `search_dirs()`); when `None`,
    /// the SDK falls back to `resolve_cli_path()` (env + exe-relative search).
    pub cli_path: Option<std::path::PathBuf>,
    /// Environment forwarded to every spawned CodeBuddy CLI subprocess via
    /// `SessionOptions::env`. The desktop launcher populates this with
    /// `CODEBUDDY_API_KEY` (backend auth — without it the headless CLI
    /// cannot authenticate and `initialize` hangs until the 60s control
    /// timeout), `CODEBUDDY_INTERNET_ENVIRONMENT` (`internal` | `ioa`), and
    /// a `PATH` augmented with `search_paths()` so a node-shim CLI resolves
    /// `node`. Mirrors the env the legacy TS/desktop launcher set on the
    /// CLI child.
    pub cli_env: std::collections::BTreeMap<String, String>,
}
pub async fn run(cfg: ProxyConfig, mut shutdown: tokio::sync::oneshot::Receiver<()>) -> anyhow::Result<()> {
    let pool = Arc::new(SessionPool::new(cfg.max_sessions, cfg.idle_timeout));
    let addr: SocketAddr = format!("127.0.0.1:{}", cfg.port).parse()?;
    let listener = TcpListener::bind(addr).await?;
    append_codebuddy_proxy_log(&format!(
        "listening addr={addr} default_model={} max_sessions={} idle_timeout_secs={} max_turns={:?} has_api_key={} cli_path={} cli_env_keys={:?}",
        cfg.default_model,
        cfg.max_sessions,
        cfg.idle_timeout.as_secs(),
        cfg.max_turns,
        cfg.api_key.is_some(),
        cfg.cli_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<sdk-resolve>".to_string()),
        cfg.cli_env.keys().collect::<Vec<_>>(),
    ));
    eprintln!("[codebuddy-proxy] listening on {addr}");
    let cfg = Arc::new(cfg);
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                let (stream, _) = accept_result?;
                let io = TokioIo::new(stream);
                let pool = pool.clone();
                let cfg = cfg.clone();
                tokio::spawn(async move {
                    let svc = service_fn(move |req: Request<Incoming>| {
                        let pool = pool.clone();
                        let cfg = cfg.clone();
                        async move { handle_request(req, pool, cfg).await }
                    });
                    let _ = http1::Builder::new().serve_connection(io, svc).await;
                });
            }
            _ = &mut shutdown => {
                append_codebuddy_proxy_log("shutting_down");
                eprintln!("[codebuddy-proxy] shutting down");
                break;
            }
        }
    }
    Ok(())
}
async fn handle_request(
    req: Request<Incoming>,
    pool: Arc<SessionPool>,
    cfg: Arc<ProxyConfig>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    if method == Method::GET && path == "/healthz" {
        return Ok(json_response(StatusCode::OK, &json!({"status": "ok"})));
    }
    append_codebuddy_proxy_log(&format!("request {method} {path}"));
    if method == Method::GET && path == "/v1/models" {
        let models = crate::models::list_models();
        return Ok(json_response(
            StatusCode::OK,
            &json!({"object": "list", "data": models}),
        ));
    }
    if method == Method::POST && path == "/v1/chat/completions" {
        return handle_chat(req, pool, cfg).await;
    }
    if method == Method::DELETE && path.starts_with("/v1/sessions/") {
        let id = path.trim_start_matches("/v1/sessions/").to_string();
        pool.evict(&id).await;
        return Ok(json_response(StatusCode::OK, &json!({"ok": true, "sessionId": id})));
    }
    append_codebuddy_proxy_log(&format!("not_found path={path}"));
    Ok(json_response(
        StatusCode::NOT_FOUND,
        &json!({"error": {"message": "not found", "type": "invalid_request_error"}}),
    ))
}
async fn handle_chat(
    req: Request<Incoming>,
    pool: Arc<SessionPool>,
    cfg: Arc<ProxyConfig>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    // Resolve a client-pinned session id BEFORE consuming the body. The
    // `X-Session-Id` header is forwarded by `codex_api_proxy` from the ACP
    // `session-id` header (per-conversation) so a multi-turn conversation
    // reuses one warm CodeBuddy SDK session instead of spawning a CLI per
    // turn. `extra_body.session_id` is the OpenAI-SDK fallback. Only mint a
    // fresh id when the client supplied neither — mirroring the reference
    // TS `resolveSessionId`. The header must be read before `into_body`,
    // which takes `req` by value and ends header access.
    let sid_header = req
        .headers()
        .get("x-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let body = req.into_body().collect().await?.to_bytes();
    let chat_req: OaiChatRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            append_codebuddy_proxy_log(&format!("chat invalid_request error={e}"));
            return Ok(json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error": {"message": format!("invalid request: {e}"), "type": "invalid_request_error"}}),
            ));
        }
    };
    if chat_req.messages.is_empty() {
        append_codebuddy_proxy_log("chat empty_messages");
        return Ok(json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error": {"message": "messages must be a non-empty array", "type": "invalid_request_error"}}),
        ));
    }
    let (session_id, sid_source) = match resolve_session_id(sid_header.as_deref(), &chat_req.extra) {
        Some((id, src)) => (id, src),
        None => (format!("ps-{}", uuid::Uuid::new_v4().simple()), "minted"),
    };
    let tool_sig = tool_signature_of(&chat_req.tools);
    append_codebuddy_proxy_log(&format!(
        "chat session_id={session_id} sid_source={sid_source} model={} stream={} tools={} messages={} tool_sig={}",
        chat_req.model.as_deref().unwrap_or("<default>"),
        chat_req.stream.unwrap_or(false),
        chat_req.tools.as_ref().map(Vec::len).unwrap_or(0),
        chat_req.messages.len(),
        if tool_sig.is_empty() { "<none>" } else { &tool_sig },
    ));
    let adapter_opts = AdapterOptions {
        default_model: cfg.default_model.clone(),
        cwd: cfg.cwd.clone(),
        max_turns: cfg.max_turns,
        cli_path: cfg.cli_path.clone(),
        cli_env: cfg.cli_env.clone(),
    };
    // Per-request pending queue. On a pool miss this becomes the entry's
    // persistent `pending` (bound to the session's MCP server at creation);
    // on a pool hit it is discarded in favor of the existing entry's queue.
    let pending = crate::pending::PendingQueue::new();
    let opts = build_session_options(&chat_req, &adapter_opts, &session_id, &pending);
    let entry = match pool.acquire(&session_id, opts, &tool_sig, pending).await {
        Ok(e) => e,
        Err(e) => {
            append_codebuddy_proxy_log(&format!(
                "chat session_setup_failed session_id={session_id} error={e}"
            ));
            return Ok(json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &json!({"error": {"message": format!("session setup failed: {e}"), "type": "server_error"}}),
            ));
        }
    };
    let is_new = entry.is_new.load(std::sync::atomic::Ordering::SeqCst);
    let stream = chat_req.stream.unwrap_or(false);
    append_codebuddy_proxy_log(&format!(
        "chat_dispatch session_id={session_id} is_new={is_new} stream={stream}"
    ));
    if stream {
        match run_streaming(&entry.session, &chat_req, &adapter_opts, &entry.pending, is_new).await {
            Ok(frames) => {
                let body = frames.join("");
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .header("x-session-id", &session_id)
                    .header("x-accel-buffering", "no")
                    .body(Full::new(Bytes::from(body)))
                    .unwrap())
            }
            Err(e) => {
                append_codebuddy_proxy_log(&format!(
                    "chat stream_failed session_id={session_id} error={e}"
                ));
                Ok(json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &json!({"error": {"message": format!("stream failed: {e}"), "type": "server_error"}}),
                ))
            }
        }
    } else {
        match run_non_streaming(&entry.session, &chat_req, &adapter_opts, &entry.pending, is_new).await {
            Ok(resp) => {
                let body = serde_json::to_vec(&resp).unwrap_or_default();
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/json")
                    .header("x-session-id", &session_id)
                    .body(Full::new(Bytes::from(body)))
                    .unwrap())
            }
            Err(e) => {
                append_codebuddy_proxy_log(&format!(
                    "chat completion_failed session_id={session_id} error={e}"
                ));
                Ok(json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &json!({"error": {"message": format!("completion failed: {e}"), "type": "server_error"}}),
                ))
            }
        }
    }
}
/// Resolve a client-pinned session id from the request, mirroring the
/// reference TS `resolveSessionId`: prefer the `X-Session-Id` header, then
/// fall back to `extra_body.session_id` in the request body. Returns the id
/// plus its origin so callers can log affinity. `None` when the client
/// supplied neither — the caller mints a fresh id.
///
/// Without this the pool keyed every request under a freshly minted UUID, so
/// `acquire` always missed and `is_new` was always true — i.e. each turn
/// spawned a new CodeBuddy CLI and multi-turn reuse never engaged even though
/// `codex_api_proxy` forwards a stable ACP session id via `X-Session-Id`.
fn resolve_session_id(header: Option<&str>, extra: &Value) -> Option<(String, &'static str)> {
    if let Some(h) = header {
        let t = h.trim();
        if !t.is_empty() {
            return Some((t.to_string(), "header"));
        }
    }
    let sid = extra
        .get("extra_body")
        .and_then(|v| v.get("session_id"))
        .and_then(Value::as_str);
    if let Some(sid) = sid {
        let t = sid.trim();
        if !t.is_empty() {
            return Some((t.to_string(), "extra_body"));
        }
    }
    None
}

fn json_response(status: StatusCode, body: &Value) -> Response<Full<Bytes>> {
    let bytes = serde_json::to_vec(body).unwrap_or_default();
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(bytes)))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::resolve_session_id;
    use serde_json::json;

    #[test]
    fn resolves_header_first() {
        let got = resolve_session_id(Some("  ps-abc  "), &json!({}));
        assert_eq!(got, Some(("ps-abc".to_string(), "header")));
    }

    #[test]
    fn falls_back_to_extra_body_when_no_header() {
        let extra = json!({ "extra_body": { "session_id": "ps-xyz" } });
        let got = resolve_session_id(None, &extra);
        assert_eq!(got, Some(("ps-xyz".to_string(), "extra_body")));
    }

    #[test]
    fn header_wins_over_extra_body() {
        let extra = json!({ "extra_body": { "session_id": "ps-body" } });
        let got = resolve_session_id(Some("ps-header"), &extra);
        assert_eq!(got, Some(("ps-header".to_string(), "header")));
    }

    #[test]
    fn none_when_absent_or_blank() {
        assert_eq!(resolve_session_id(None, &json!({})), None);
        // whitespace-only header is treated as absent so we still mint.
        assert_eq!(resolve_session_id(Some("   "), &json!({})), None);
        // whitespace-only extra_body.session_id likewise.
        assert_eq!(
            resolve_session_id(None, &json!({ "extra_body": { "session_id": "  " } })),
            None
        );
        // non-string session_id is ignored.
        assert_eq!(
            resolve_session_id(None, &json!({ "extra_body": { "session_id": 42 } })),
            None
        );
    }
}
