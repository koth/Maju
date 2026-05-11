mod client;
mod codex_api_proxy;
mod events;
mod mapping;
mod runtime;

pub use client::{PromptTask, SessionHandle};
pub use codex_api_proxy::codex_api_proxy_base_url;
pub use events::{ClientEvent, SessionConfig};
pub use mapping::diff_to_hunks;

pub const DEFAULT_AGENT_COMMAND: &str = "codebuddy --acp";

pub fn platform_default_agent_command() -> String {
    DEFAULT_AGENT_COMMAND.to_string()
}

pub fn resolve_agent_command() -> String {
    std::env::var("ACP_AGENT_COMMAND").unwrap_or_else(|_| platform_default_agent_command())
}
