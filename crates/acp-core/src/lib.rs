mod client;
mod codex_api_proxy;
mod events;
mod mapping;
pub mod runtime;

pub use agent_client_protocol::schema::McpServer;
pub use client::{PromptTask, SessionHandle};
pub use codex_api_proxy::{
    any_active_proxy_retry_status, clear_codex_api_proxy_model_provider_map,
    codex_api_proxy_base_url, configure_codex_api_proxy_model_provider_map,
    current_proxy_retry_status, ensure_codex_api_proxy, register_codex_api_proxy_provider_key,
    set_codex_api_proxy_project_name,
};
pub use events::{ClientEvent, RemoteSshReverseForward, RemoteSshSessionConfig, SessionConfig};
pub use mapping::diff_to_hunks;
pub use runtime::{
    HarnessApprovalOutcome, HarnessApprovalResult, HarnessBackend, HarnessQuestionAnswer,
    PermissionBroker, RuntimeCommand, ShutdownSignal, set_harness_backend,
};

pub const DEFAULT_AGENT_COMMAND: &str = "codebuddy --acp";

pub fn platform_default_agent_command() -> String {
    DEFAULT_AGENT_COMMAND.to_string()
}

pub fn resolve_agent_command() -> String {
    std::env::var("ACP_AGENT_COMMAND").unwrap_or_else(|_| platform_default_agent_command())
}

pub fn http_mcp_server(
    name: impl Into<String>,
    url: impl Into<String>,
    headers: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
) -> McpServer {
    use agent_client_protocol::schema::{HttpHeader, McpServerHttp};
    McpServer::Http(
        McpServerHttp::new(name, url).headers(
            headers
                .into_iter()
                .map(|(name, value)| HttpHeader::new(name, value))
                .collect(),
        ),
    )
}
