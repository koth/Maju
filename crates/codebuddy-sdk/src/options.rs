use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mcp::server::SdkMcpTool;

/// How the SDK declares the in-process MCP server it hosts, if any.
///
/// We use a simple structural type here (not a trait object) so the SDK
/// can live in `codebuddy-sdk` without circular dependencies on
/// `codebuddy-proxy`. The proxy hands us concrete handler functions via
/// [`SdkMcpTool`].
#[derive(Clone, Debug)]
pub struct SdkMcpServerEntry {
    /// Server name (passed to the CLI as the `name` field of the SDK MCP server).
    pub name: String,
    /// Tools registered on this server.
    pub tools: Vec<SdkMcpTool>,
}

/// Caller-supplied configuration for a CodeBuddy CLI session.
///
/// Mirrors the Python SDK's `CodeBuddyAgentOptions` and the TS SDK's
/// `SessionOptions`, with the subset that the proxy actually uses.
#[derive(Clone, Debug)]
pub struct SessionOptions {
    /// Custom session id to resume or pin; the SDK does NOT generate one.
    pub session_id: Option<String>,
    /// Model name (passed as `--model`).
    pub model: Option<String>,
    /// Working directory for the CLI process. `None` inherits from the SDK process.
    pub cwd: Option<PathBuf>,
    /// Extra environment variables for the child process (in addition to
    /// the SDK's defaults like `DISABLE_AUTOUPDATER=1`).
    pub env: BTreeMap<String, String>,
    /// Permission mode. Defaults to `bypassPermissions` in the proxy.
    pub permission_mode: Option<String>,
    /// Maximum number of agentic turns the CLI may run before returning.
    /// `None` means no limit.
    pub max_turns: Option<u32>,
    /// System prompt passed via the SDK's dedicated channel (the proper
    /// system-prompt wire field), not mixed into the user message.
    pub system_prompt: Option<String>,
    /// In-process SDK MCP servers to register.
    pub mcp_servers: Vec<SdkMcpServerEntry>,
    /// Request timeout for control requests in milliseconds. `None` defaults to 60s.
    pub request_timeout_ms: Option<u64>,
    /// Path to the CLI binary. `None` resolves via [`crate::binary::resolve_cli_path`].
    pub codebuddy_code_path: Option<PathBuf>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            session_id: None,
            model: None,
            cwd: None,
            env: BTreeMap::new(),
            permission_mode: None,
            max_turns: None,
            system_prompt: None,
            mcp_servers: Vec::new(),
            request_timeout_ms: None,
            codebuddy_code_path: None,
        }
    }
}

/// SDK capability flags, advertised to the CLI in the `initialize` control request.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SdkCapabilities {
    /// Always `true` for parity with the Python/TS SDKs.
    #[serde(default)]
    pub ask_user_question: bool,
}

/// Free-form value used to pass through control request payloads.
pub type ControlRequestPayload = Value;
