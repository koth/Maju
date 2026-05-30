use crate::events::{ClientEvent, SessionConfig};
use crate::mapping::{append_runtime_event_log, format_stop_reason};
use agent_client_protocol::schema::{
    AgentCapabilities, ListSessionsRequest, SessionId, SessionInfo, StopReason,
};
use agent_client_protocol::{Agent, ConnectionTo};
use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;

use super::agent_process::{
    HiddenAgentProcess, codex_woa_default_git_repos_header, codex_woa_git_repos_header,
};
use super::process::{agent_spawn_command, apply_process_cwd_and_pwd, hide_console_window};

const CODEX_WOA_API_KEY_ENVS: &[&str] = &["AUTH_TOKEN", "CODEX_WOA_API_KEY"];
const CODEX_WOA_APP_VERSION_ENVS: &[&str] =
    &["CODEX_INTERNAL_APP_VERSION", "CODEX_WOA_APP_VERSION"];
const CODEX_WOA_USER_AGENT_ENVS: &[&str] = &["CODEX_INTERNAL_USER_AGENT", "CODEX_WOA_USER_AGENT"];
const CODEX_WOA_CONVERSATION_ID_ENVS: &[&str] = &[
    "CODEX_INTERNAL_CONVERSATION_ID",
    "CODEX_WOA_CONVERSATION_ID",
];
const CODEX_WOA_GIT_REPOS_ENVS: &[&str] = &["CODEX_INTERNAL_GIT_REPOS", "CODEX_WOA_GIT_REPOS"];
const CODEX_WOA_KNOT_API_KEY_ENVS: &[&str] = &["CODEBUDDY_API_KEY"];
const CODEX_WOA_YOLO_MODE_ENVS: &[&str] = &["CODEX_INTERNAL_YOLO_MODE"];
const CODEX_WOA_ANYDEV_MODE_ENVS: &[&str] = &["CODEX_INTERNAL_ANYDEV_MODE"];
const CODEX_WOA_SPECIFY_MODEL_ENVS: &[&str] = &["CODEX_INTERNAL_SPECIFY_MODEL"];
const CODEX_WOA_INSTALLATION_ID_ENVS: &[&str] = &[
    "CODEX_INTERNAL_INSTALLATION_ID",
    "CODEX_INSTALLATION_ID",
    "OPENAI_CODEX_INSTALLATION_ID",
];
const CODEX_WOA_WINDOW_ID_ENVS: &[&str] = &["CODEX_INTERNAL_WINDOW_ID", "CODEX_WINDOW_ID"];
const CODEX_WOA_ORIGINATOR_ENVS: &[&str] =
    &["CODEX_INTERNAL_ORIGINATOR_OVERRIDE", "CODEX_WOA_ORIGINATOR"];
const CODEX_WOA_RESPONSES_URL: &str =
    "https://copilot.code.woa.com/server/chat/codebuddy-gateway/codex/responses";
const CODEX_WOA_TITLE_MODEL: &str = "gpt-5.4";
const CODEX_WOA_APP_VERSION: &str = "0.0.9";
const CODEX_WOA_TITLE_HELPER_ENV: &str = "KODEX_CODEX_ACP_TITLE_HELPER";
const CODEX_WOA_ENABLE_SIDE_TITLE_QUERY_ENV: &str = "KODEX_ENABLE_CODEX_WOA_TITLE_SIDE_QUERY";
const CODEX_WOA_TITLE_TIMEOUT_MS: u64 = 20_000;
const CODEX_WOA_TITLE_MAX_PROMPT_CHARS: usize = 4_000;
const TITLE_SYNC_RETRY_DELAYS_MS: [u64; 6] = [120, 400, 900, 2_000, 5_000, 10_000];
const TITLE_SYNC_TIMEOUT_MS: u64 = 2_000;
const SESSION_TITLE_INSTRUCTIONS: &str = r#"Generate a concise title for this coding session.

Rules:
- Return only the title text.
- Return the title in Simplified Chinese, even if the user wrote in another language.
- Use a short Chinese phrase, roughly 6 to 12 Chinese characters when possible.
- Do not quote the title.
- Do not copy the user's request verbatim.
- Keep technical identifiers like API, ACP, session/list, and file names unchanged when clearer."#;

struct CodexWoaGeneratedTitle {
    title: String,
    transport: &'static str,
}

#[derive(Serialize)]
struct CodexWoaTitleHelperRequest<'a> {
    session_id: &'a str,
    prompt_text: &'a str,
    response_text: Option<&'a str>,
}

#[derive(Deserialize)]
struct CodexWoaTitleHelperResponse {
    title: Option<String>,
}

pub(super) async fn emit_turn_finished(
    config: &SessionConfig,
    tx_events: &mpsc::Sender<ClientEvent>,
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
    supports_session_list: bool,
    title_source: Option<&str>,
    reason: StopReason,
) -> anyhow::Result<()> {
    let stop_reason = format_stop_reason(reason);
    append_runtime_event_log(
        config,
        "session/stop_reason",
        &json!({ "stopReason": stop_reason.clone() }),
    )?;

    let _ = tx_events.send(ClientEvent::TurnFinished { stop_reason });
    if command_uses_codex_woa_side_query_titles(config) {
        let synced = if let Some(title_source) =
            title_source.map(str::trim).filter(|text| !text.is_empty())
        {
            sync_codex_woa_title_after_turn(config, tx_events, session_id, title_source).await
        } else {
            false
        };
        if !synced && supports_session_list {
            sync_session_title_from_list_after_turn(config, tx_events, connection, session_id)
                .await;
        }
    } else if supports_session_list {
        sync_session_title_from_list_after_turn(config, tx_events, connection, session_id).await;
    }
    Ok(())
}

pub(super) fn advertised_session_list_capability(agent_capabilities: &AgentCapabilities) -> bool {
    agent_capabilities.session_capabilities.list.is_some()
}

pub(super) fn command_implies_codex_session_list(config: &SessionConfig) -> bool {
    let command = config.agent_command.to_ascii_lowercase();
    command.contains("codex-acp") || command.contains("kodex-acp")
}

pub(super) fn supports_session_list_title_sync(
    config: &SessionConfig,
    advertised_session_list: bool,
) -> bool {
    if command_uses_claude_agent_titles(config) {
        return false;
    }

    advertised_session_list || command_implies_codex_session_list(config)
}

fn command_uses_claude_agent_titles(config: &SessionConfig) -> bool {
    let command = config.agent_command.to_ascii_lowercase();
    command.contains("claude-agent-acp") || command.contains("claude-acp")
}

pub(super) fn command_uses_codex_woa_titles(config: &SessionConfig) -> bool {
    command_implies_codex_session_list(config)
        && config.agent_env.iter().any(|(name, value)| {
            CODEX_WOA_API_KEY_ENVS.contains(&name.as_str()) && !value.trim().is_empty()
        })
}

pub(super) fn command_uses_codex_woa_side_query_titles(config: &SessionConfig) -> bool {
    command_uses_codex_woa_titles(config)
        && config.agent_env.iter().any(|(name, value)| {
            name == CODEX_WOA_ENABLE_SIDE_TITLE_QUERY_ENV
                && matches!(value.trim(), "1" | "true" | "TRUE" | "True")
        })
}

async fn sync_codex_woa_title_after_turn(
    config: &SessionConfig,
    tx_events: &mpsc::Sender<ClientEvent>,
    session_id: &SessionId,
    title_source: &str,
) -> bool {
    match generate_codex_woa_session_title(config, session_id, title_source).await {
        Ok(Some(generated)) => {
            let title = generated.title;
            let _ = append_runtime_event_log(
                config,
                "session/codex_woa_title_query",
                &json!({
                    "sessionId": session_id.0.as_ref(),
                    "transport": generated.transport,
                    "title": title,
                }),
            );
            let _ = tx_events.send(ClientEvent::SessionTitleUpdated { title });
            true
        }
        Ok(None) => {
            let _ = append_runtime_event_log(
                config,
                "session/codex_woa_title_query_empty",
                &json!({ "sessionId": session_id.0.as_ref() }),
            );
            false
        }
        Err(error) => {
            let _ = append_runtime_event_log(
                config,
                "session/codex_woa_title_query_failed",
                &json!({
                    "sessionId": session_id.0.as_ref(),
                    "error": error.to_string(),
                }),
            );
            false
        }
    }
}

async fn generate_codex_woa_session_title(
    config: &SessionConfig,
    session_id: &SessionId,
    title_source: &str,
) -> anyhow::Result<Option<CodexWoaGeneratedTitle>> {
    match generate_codex_woa_session_title_via_helper(config, session_id, title_source).await {
        Ok(Some(title)) => {
            return Ok(Some(CodexWoaGeneratedTitle {
                title,
                transport: "codex-acp-title-helper",
            }));
        }
        Ok(None) => {}
        Err(error) => {
            let _ = append_runtime_event_log(
                config,
                "session/codex_woa_title_query_helper_failed",
                &json!({
                    "sessionId": session_id.0.as_ref(),
                    "error": error.to_string(),
                }),
            );
        }
    }

    generate_codex_woa_session_title_direct(config, session_id, title_source)
        .await
        .map(|title| {
            title.map(|title| CodexWoaGeneratedTitle {
                title,
                transport: "direct-responses",
            })
        })
}

async fn generate_codex_woa_session_title_via_helper(
    config: &SessionConfig,
    session_id: &SessionId,
    title_source: &str,
) -> anyhow::Result<Option<String>> {
    let Some(_) = codex_woa_env_any(config, CODEX_WOA_API_KEY_ENVS) else {
        return Ok(None);
    };
    let helper_input = serde_json::to_string(&CodexWoaTitleHelperRequest {
        session_id: session_id.0.as_ref(),
        prompt_text: title_source,
        response_text: None,
    })
    .map_err(|err| anyhow!(err.to_string()))?;
    let config = config.clone();
    let output = tokio::task::spawn_blocking(move || {
        run_codex_woa_title_helper_process(&config, &helper_input)
    })
    .await
    .map_err(|err| anyhow!(err.to_string()))??;

    let response: CodexWoaTitleHelperResponse =
        serde_json::from_str(&output).map_err(|err| anyhow!(err.to_string()))?;
    Ok(response
        .title
        .and_then(|title| normalize_codex_woa_title(&title)))
}

fn run_codex_woa_title_helper_process(
    config: &SessionConfig,
    helper_input: &str,
) -> anyhow::Result<String> {
    let parsed = HiddenAgentProcess::from_command(&config.agent_command, &config.workspace_root)?;
    let mut command = agent_spawn_command(&parsed.command, &parsed.args);
    apply_process_cwd_and_pwd(&mut command, &parsed.current_dir);
    for (name, value) in parsed.env.iter().chain(config.agent_env.iter()) {
        command.env(name, value);
    }
    if !env_has_non_empty(&config.agent_env, CODEX_WOA_GIT_REPOS_ENVS) {
        command.env(
            "CODEX_INTERNAL_GIT_REPOS",
            codex_woa_title_git_repos(config),
        );
    }
    command.env(CODEX_WOA_TITLE_HELPER_ENV, "1");
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    hide_console_window(&mut command);

    let mut child = command.spawn().map_err(|err| anyhow!(err.to_string()))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to open codex-acp title helper stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to open codex-acp title helper stderr"))?;
    let stdout_reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        stdout.read_to_end(&mut output).map(|_| output)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        stderr.read_to_end(&mut output).map(|_| output)
    });
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("failed to open codex-acp title helper stdin"))?;
        stdin
            .write_all(helper_input.as_bytes())
            .map_err(|err| anyhow!(err.to_string()))?;
    }

    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_millis(CODEX_WOA_TITLE_TIMEOUT_MS);
    loop {
        if child
            .try_wait()
            .map_err(|err| anyhow!(err.to_string()))?
            .is_some()
        {
            break;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!(
                "codex-acp title helper timed out after {CODEX_WOA_TITLE_TIMEOUT_MS}ms"
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let status = child.wait().map_err(|err| anyhow!(err.to_string()))?;
    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow!("failed to join codex-acp title helper stdout reader"))?
        .map_err(|err| anyhow!(err.to_string()))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow!("failed to join codex-acp title helper stderr reader"))?
        .map_err(|err| anyhow!(err.to_string()))?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
        let message = if stderr.is_empty() {
            format!("codex-acp title helper exited with {status}")
        } else {
            format!("codex-acp title helper exited with {status}: {stderr}")
        };
        return Err(anyhow!(message));
    }
    String::from_utf8(stdout).map_err(|err| anyhow!(err.to_string()))
}

async fn generate_codex_woa_session_title_direct(
    config: &SessionConfig,
    session_id: &SessionId,
    title_source: &str,
) -> anyhow::Result<Option<String>> {
    let Some(token) = codex_woa_env_any(config, CODEX_WOA_API_KEY_ENVS) else {
        return Ok(None);
    };
    let session_id = session_id.0.as_ref();
    let app_version =
        codex_woa_env_any(config, CODEX_WOA_APP_VERSION_ENVS).unwrap_or(CODEX_WOA_APP_VERSION);
    let user_agent = codex_woa_env_any(config, CODEX_WOA_USER_AGENT_ENVS)
        .map(str::to_string)
        .unwrap_or_else(|| format!("Codex-Internal/{CODEX_WOA_APP_VERSION}"));
    let originator =
        codex_woa_env_any(config, CODEX_WOA_ORIGINATOR_ENVS).unwrap_or("Codex Internal");
    let conversation_id = codex_woa_title_conversation_id(config);
    let git_repos = codex_woa_title_git_repos(config);
    let installation_id = codex_woa_title_installation_id(config);
    let window_id = codex_woa_title_window_id(config, session_id);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(CODEX_WOA_TITLE_TIMEOUT_MS))
        .build()
        .map_err(|err| anyhow!(err.to_string()))?;
    let payload = codex_woa_title_payload(config, title_source, session_id, &installation_id);
    let mut request = client
        .post(CODEX_WOA_RESPONSES_URL)
        .header("accept", "text/event-stream")
        .header("content-type", "application/json")
        .header("originator", originator)
        .header("x-app-name", "codex-internal")
        .header("x-request-platform", "codex-internal")
        .header("x-scene-name", "common_chat")
        .header("x-channel", "codex-internal")
        .header("x-app-version", app_version)
        .header("user-agent", user_agent.clone())
        .header("x-conversation-id", conversation_id)
        .header("x-api-key", token)
        .header("x-git-repos", git_repos.clone())
        .header("x-client-request-id", session_id)
        .header("session-id", session_id)
        .header("thread-id", session_id)
        .header("x-codex-installation-id", installation_id.clone())
        .header("x-codex-window-id", window_id)
        .json(&payload);
    request = request_with_optional_env_header(
        config,
        request,
        "x-knot-api-key",
        CODEX_WOA_KNOT_API_KEY_ENVS,
    );
    request =
        request_with_optional_env_header(config, request, "x-yolo-mode", CODEX_WOA_YOLO_MODE_ENVS);
    request = request_with_optional_env_header(
        config,
        request,
        "x-anydev-mode",
        CODEX_WOA_ANYDEV_MODE_ENVS,
    );
    request = request_with_optional_env_header(
        config,
        request,
        "x-specify-model",
        CODEX_WOA_SPECIFY_MODEL_ENVS,
    );
    let response = request
        .send()
        .await
        .map_err(|err| anyhow!(err.to_string()))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| anyhow!(err.to_string()))?;
    if !status.is_success() {
        let context = codex_woa_title_request_diagnostics(
            config,
            app_version,
            &user_agent,
            &git_repos,
            &installation_id,
            session_id,
        );
        return Err(anyhow!(
            "WOA title request failed with HTTP {status}: {body}; context={context}"
        ));
    }
    Ok(extract_codex_woa_title_from_body(&body))
}

fn codex_woa_env<'a>(config: &'a SessionConfig, name: &str) -> Option<&'a str> {
    config
        .agent_env
        .iter()
        .find(|(key, value)| key == name && !value.trim().is_empty())
        .map(|(_, value)| value.as_str())
}

fn codex_woa_env_any<'a>(config: &'a SessionConfig, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|name| codex_woa_env(config, name))
}

fn env_has_non_empty(env: &[(String, String)], names: &[&str]) -> bool {
    names.iter().any(|name| {
        env.iter()
            .any(|(key, value)| key == name && !value.trim().is_empty())
    })
}

fn request_with_optional_env_header(
    config: &SessionConfig,
    request: reqwest::RequestBuilder,
    header: &'static str,
    names: &[&str],
) -> reqwest::RequestBuilder {
    match codex_woa_env_any(config, names) {
        Some(value) => request.header(header, value),
        None => request,
    }
}

pub(super) fn codex_woa_title_conversation_id(config: &SessionConfig) -> String {
    codex_woa_env_any(config, CODEX_WOA_CONVERSATION_ID_ENVS)
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

pub(super) fn codex_woa_title_git_repos(config: &SessionConfig) -> String {
    codex_woa_env_any(config, CODEX_WOA_GIT_REPOS_ENVS)
        .map(str::to_string)
        .or_else(|| codex_woa_git_repos_header(Path::new(&config.workspace_root)))
        .unwrap_or_else(|| codex_woa_default_git_repos_header().to_string())
}

fn codex_woa_title_installation_id(config: &SessionConfig) -> String {
    codex_woa_env_any(config, CODEX_WOA_INSTALLATION_ID_ENVS)
        .map(str::to_string)
        .or_else(|| codex_woa_title_installation_id_from_app_data(config))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

fn codex_woa_title_installation_id_from_app_data(config: &SessionConfig) -> Option<String> {
    let path = Path::new(&config.app_data_root).join("installation_id");
    std::fs::read_to_string(path).ok().and_then(|content| {
        let trimmed = content.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn codex_woa_title_window_id(config: &SessionConfig, conversation_id: &str) -> String {
    codex_woa_env_any(config, CODEX_WOA_WINDOW_ID_ENVS)
        .map(str::to_string)
        .unwrap_or_else(|| conversation_id.to_string())
}

pub(super) fn codex_woa_title_payload(
    config: &SessionConfig,
    title_source: &str,
    prompt_cache_key: &str,
    installation_id: &str,
) -> Value {
    let model = config
        .model
        .trim()
        .is_empty()
        .then_some(CODEX_WOA_TITLE_MODEL)
        .unwrap_or(config.model.trim());
    json!({
        "model": model,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": codex_woa_title_prompt(title_source),
            }],
        }],
        "tools": [],
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "reasoning": null,
        "store": false,
        "stream": true,
        "include": [],
        "prompt_cache_key": prompt_cache_key,
        "client_metadata": {
            "x-codex-installation-id": installation_id,
        },
    })
}

fn codex_woa_title_request_diagnostics(
    config: &SessionConfig,
    app_version: &str,
    user_agent: &str,
    git_repos: &str,
    installation_id: &str,
    session_id: &str,
) -> Value {
    let installation_id_from_env =
        codex_woa_env_any(config, CODEX_WOA_INSTALLATION_ID_ENVS).is_some();
    let installation_id_from_app_data = !installation_id_from_env
        && codex_woa_title_installation_id_from_app_data(config).is_some();
    json!({
        "appVersion": app_version,
        "userAgent": user_agent,
        "gitRepos": git_repos,
        "requestShape": "codex-responses-session-title",
        "usesSessionHeaders": true,
        "sessionId": session_id,
        "installationIdSource": if installation_id_from_env {
            "env"
        } else if installation_id_from_app_data {
            "appData"
        } else {
            "generated"
        },
        "hasKnotApiKey": codex_woa_env_any(config, CODEX_WOA_KNOT_API_KEY_ENVS).is_some(),
        "hasYoloMode": codex_woa_env_any(config, CODEX_WOA_YOLO_MODE_ENVS).is_some(),
        "hasAnydevMode": codex_woa_env_any(config, CODEX_WOA_ANYDEV_MODE_ENVS).is_some(),
        "hasSpecifyModel": codex_woa_env_any(config, CODEX_WOA_SPECIFY_MODEL_ENVS).is_some(),
        "hasInstallationId": !installation_id.trim().is_empty(),
        "hasWindowId": codex_woa_env_any(config, CODEX_WOA_WINDOW_ID_ENVS).is_some(),
    })
}

fn codex_woa_title_prompt(title_source: &str) -> String {
    let trimmed_source = title_source
        .chars()
        .take(CODEX_WOA_TITLE_MAX_PROMPT_CHARS)
        .collect::<String>();
    format!(
        "{SESSION_TITLE_INSTRUCTIONS}\n\n<user_request>\n{trimmed_source}\n</user_request>\n\n<assistant_response>\n</assistant_response>"
    )
}

pub(super) fn extract_codex_woa_title(value: &Value) -> Option<String> {
    value
        .get("output_text")
        .and_then(Value::as_str)
        .and_then(normalize_codex_woa_title)
        .or_else(|| {
            value
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("message"))
                .and_then(|message| message.get("content"))
                .and_then(Value::as_str)
                .and_then(normalize_codex_woa_title)
        })
        .or_else(|| extract_responses_output_text(value).and_then(normalize_codex_woa_title))
        .or_else(|| value.get("response").and_then(extract_codex_woa_title))
}

pub(super) fn extract_codex_woa_title_from_body(body: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        return extract_codex_woa_title(&value);
    }

    let mut streamed_title = String::new();
    let mut completed_title = None;
    for line in body.lines() {
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(delta) = value.get("delta").and_then(Value::as_str) {
            streamed_title.push_str(delta);
        }
        completed_title = extract_codex_woa_title(&value).or(completed_title);
    }

    if !streamed_title.is_empty() {
        normalize_codex_woa_title(&streamed_title)
    } else {
        completed_title
    }
}

fn extract_responses_output_text(value: &Value) -> Option<&str> {
    value
        .get("output")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flat_map(|content| content.iter())
        .find_map(|content| {
            content
                .get("text")
                .or_else(|| content.get("content"))
                .and_then(Value::as_str)
        })
}

fn normalize_codex_woa_title(raw: &str) -> Option<String> {
    let mut title = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?
        .trim_matches(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '"' | '\''
                        | '`'
                        | '#'
                        | '*'
                        | '-'
                        | ':'
                        | '.'
                        | ','
                        | '!'
                        | '?'
                        | ';'
                        | '：'
                        | '。'
                        | '，'
                        | '！'
                        | '？'
                        | '；'
                        | '“'
                        | '”'
                        | '‘'
                        | '’'
                )
        })
        .trim();
    for prefix in ["Title:", "title:", "Session title:", "Conversation title:"] {
        if let Some(rest) = title.strip_prefix(prefix) {
            title = rest.trim();
            break;
        }
    }
    let title = title
        .trim_matches(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '`'))
        .chars()
        .take(60)
        .collect::<String>();
    if title.trim().is_empty() {
        None
    } else {
        Some(title)
    }
}

pub(super) async fn sync_session_title_from_list(
    config: &SessionConfig,
    tx_events: &mpsc::Sender<ClientEvent>,
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
) -> anyhow::Result<bool> {
    append_runtime_event_log(
        config,
        "session/list_title_sync_start",
        &json!({
            "sessionId": session_id.0.as_ref(),
            "cwd": config.workspace_root,
            "timeoutMs": TITLE_SYNC_TIMEOUT_MS,
        }),
    )?;
    let request = ListSessionsRequest::new().cwd(PathBuf::from(&config.workspace_root));
    let response = tokio::time::timeout(
        std::time::Duration::from_millis(TITLE_SYNC_TIMEOUT_MS),
        connection.send_request_to(Agent, request).block_task(),
    )
    .await
    .map_err(|_| anyhow!("session/list timed out after {TITLE_SYNC_TIMEOUT_MS}ms"))?
    .map_err(|err| anyhow!(err.to_string()))?;
    let session_count = response.sessions.len();

    let title = select_session_title_for_sync(&response.sessions, session_id);
    let matched = response
        .sessions
        .iter()
        .find(|session| session.session_id == *session_id);

    if let Some((title, matched_by)) = title {
        append_runtime_event_log(
            config,
            "session/list_title_sync",
            &json!({
                "sessionId": session_id.0.as_ref(),
                "title": title,
                "matchedBy": matched_by,
            }),
        )?;
        let _ = tx_events.send(ClientEvent::SessionTitleUpdated { title });
        return Ok(true);
    }

    append_runtime_event_log(
        config,
        "session/list_title_sync_empty",
        &json!({
            "sessionId": session_id.0.as_ref(),
            "sessionCount": session_count,
            "matched": matched.is_some(),
            "hasTitle": matched.and_then(|session| session.title.as_ref()).is_some(),
        }),
    )?;

    Ok(false)
}

pub(super) async fn sync_session_title_from_list_after_turn(
    config: &SessionConfig,
    tx_events: &mpsc::Sender<ClientEvent>,
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
) {
    match sync_session_title_from_list(config, tx_events, connection, session_id).await {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            let _ = append_runtime_event_log(
                config,
                "session/list_title_sync_failed",
                &json!({ "error": error.to_string() }),
            );
            return;
        }
    }

    for delay_ms in TITLE_SYNC_RETRY_DELAYS_MS {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        match sync_session_title_from_list(config, tx_events, connection, session_id).await {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                let _ = append_runtime_event_log(
                    config,
                    "session/list_title_sync_retry_failed",
                    &json!({ "delayMs": delay_ms, "error": error.to_string() }),
                );
                return;
            }
        }
    }
}

pub(super) fn select_session_title_for_sync(
    sessions: &[SessionInfo],
    session_id: &SessionId,
) -> Option<(String, &'static str)> {
    sessions
        .iter()
        .find(|session| session.session_id == *session_id)
        .and_then(trimmed_session_title)
        .map(|title| (title, "sessionId"))
}

fn trimmed_session_title(session: &SessionInfo) -> Option<String> {
    let title = session.title.as_deref()?.trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}
