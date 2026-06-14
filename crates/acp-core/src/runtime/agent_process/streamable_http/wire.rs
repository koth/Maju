use reqwest::header::HeaderValue;
use std::sync::{Arc, Mutex};

use super::{
    ACP_CONNECTION_ID_HEADER, ACP_SESSION_ID_HEADER, ACP_SESSION_TOKEN_HEADER,
    CODEBUDDY_ACP_CONNECTION_ID_HEADER, CODEBUDDY_REQUEST_HEADER,
};

pub(super) fn codebuddy_http_headers(
    request: reqwest::RequestBuilder,
    connection_id: &Arc<Mutex<Option<String>>>,
) -> reqwest::RequestBuilder {
    let request = request.header(CODEBUDDY_REQUEST_HEADER, "1");
    let value = connection_id.lock().ok().and_then(|guard| guard.clone());
    let Some(value) = value else {
        return request;
    };
    match HeaderValue::from_str(&value) {
        Ok(value) => request.header(CODEBUDDY_ACP_CONNECTION_ID_HEADER, value),
        Err(_) => request,
    }
}

pub(super) fn acp_session_header(
    request: reqwest::RequestBuilder,
    session_id: Option<&str>,
) -> reqwest::RequestBuilder {
    let Some(session_id) = session_id else {
        return request;
    };
    match HeaderValue::from_str(session_id) {
        Ok(value) => request.header(ACP_SESSION_ID_HEADER, value),
        Err(_) => request,
    }
}

pub(super) fn acp_session_token_header(
    request: reqwest::RequestBuilder,
    session_token: &Arc<Mutex<Option<String>>>,
) -> reqwest::RequestBuilder {
    let token = session_token.lock().ok().and_then(|guard| guard.clone());
    let Some(token) = token else {
        return request;
    };
    match HeaderValue::from_str(&token) {
        Ok(value) => request.header(ACP_SESSION_TOKEN_HEADER, value),
        Err(_) => request,
    }
}

pub(super) fn acp_session_id_from_message(message: &serde_json::Value) -> Option<String> {
    message
        .get("params")
        .and_then(|params| params.get("sessionId").or_else(|| params.get("session_id")))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

pub(super) fn acp_session_cwd_from_message(message: &serde_json::Value) -> Option<String> {
    message
        .get("params")
        .and_then(|params| params.get("cwd"))
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

pub(super) fn acp_method_from_message(message: &serde_json::Value) -> Option<String> {
    message
        .get("method")
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

pub(in crate::runtime::agent_process) fn message_has_method(
    message: &serde_json::Value,
    method: &str,
) -> bool {
    match message {
        serde_json::Value::Array(items) => {
            items.iter().any(|item| message_has_method(item, method))
        }
        _ => message
            .get("method")
            .and_then(|value| value.as_str())
            .is_some_and(|value| value == method),
    }
}

pub(super) fn remember_acp_connection_id_from_headers(
    headers: &reqwest::header::HeaderMap,
    connection_id: &Arc<Mutex<Option<String>>>,
) {
    let Some(value) = headers
        .get(ACP_CONNECTION_ID_HEADER)
        .or_else(|| headers.get(CODEBUDDY_ACP_CONNECTION_ID_HEADER))
        .and_then(|value| value.to_str().ok())
    else {
        return;
    };
    if let Ok(mut guard) = connection_id.lock() {
        *guard = Some(value.to_string());
    }
}

pub(super) fn remember_acp_connection_id_from_payload(
    payload: &str,
    connection_id: &Arc<Mutex<Option<String>>>,
) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return;
    };
    let id = value
        .get("result")
        .and_then(|result| {
            result
                .get("connectionId")
                .or_else(|| result.get("connection_id"))
        })
        .or_else(|| {
            value.get("data").and_then(|data| {
                data.get("connectionId")
                    .or_else(|| data.get("connection_id"))
            })
        })
        .or_else(|| value.get("connectionId"))
        .or_else(|| value.get("connection_id"))
        .and_then(|value| value.as_str());
    let Some(id) = id else {
        return;
    };
    if let Ok(mut guard) = connection_id.lock() {
        *guard = Some(id.to_string());
    }
}

pub(super) fn remember_acp_session_token_from_payload(
    payload: &str,
    session_token: &Arc<Mutex<Option<String>>>,
) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return;
    };
    let token = value
        .get("result")
        .and_then(|result| {
            result
                .get("sessionToken")
                .or_else(|| result.get("session_token"))
        })
        .or_else(|| {
            value.get("data").and_then(|data| {
                data.get("sessionToken")
                    .or_else(|| data.get("session_token"))
            })
        })
        .or_else(|| value.get("sessionToken"))
        .or_else(|| value.get("session_token"))
        .and_then(|value| value.as_str());
    let Some(token) = token else {
        return;
    };
    if let Ok(mut guard) = session_token.lock() {
        *guard = Some(token.to_string());
    }
}
