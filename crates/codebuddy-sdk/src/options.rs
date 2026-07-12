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
    /// Custom session id to pin for a NEW conversation; the SDK does NOT
    /// generate one. Mutually exclusive with [`SessionOptions::resume`]:
    /// setting both is a programming error — `cli_args` lets `resume` win.
    /// On a fresh CLI process this becomes `--session-id <id>`, which labels
    /// a new session (it does NOT load any prior history).
    pub session_id: Option<String>,
    /// Resume an existing conversation by id, loading its rollout history
    /// into the CLI's in-memory state so subsequent incremental turns carry
    /// prior context. Becomes `--resume <id>` on the CLI argv. Mutually
    /// exclusive with [`SessionOptions::session_id`]. Use this when the
    /// proxy's pool misses but a persisted rollout exists on disk; use
    /// `session_id` for a genuinely new conversation.
    pub resume: Option<String>,
    /// Model name (passed as `--model`).
    pub model: Option<String>,
    /// Working directory for the CLI process. `None` inherits from the SDK process.
    pub cwd: Option<PathBuf>,
    /// Extra environment variables for the child process (in addition to
    /// the SDK's defaults like `DISABLE_AUTOUPDATER=1`).
    pub env: BTreeMap<String, String>,
    /// Permission mode. Defaults to `bypassPermissions` in the proxy.
    pub permission_mode: Option<String>,
    /// Built-in tool allow-list passed as `--tools`.
    ///
    /// - `None` — omit the flag (CLI keeps its default built-ins).
    /// - `Some([])` — pass `--tools ""`, which **disables all built-in tools**
    ///   (Bash/Edit/Read/…). Required by the reverse proxy so only the
    ///   client-declared MCP tools (registered via `mcp_servers`) are available;
    ///   otherwise the CLI can execute tools itself instead of the proxy
    ///   placeholder path.
    /// - `Some(names)` — restrict to the listed built-in tool names.
    pub tools: Option<Vec<String>>,
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
    /// Optional ack channel: the SDK sends a unit on it **after** a
    /// `tools/call` handler's result has been written back to the CLI stdin
    /// (`transport.write_json`). The proxy uses this to wait until the
    /// placeholder `tool_result` has actually reached the CLI before issuing
    /// `interrupt()` — otherwise the interrupt (also a stdin write) can win
    /// the mutex race and the CLI fills the tool_result with `undefined`.
    /// `None` disables ack (no wait). Per-session; the proxy owns the receiver.
    pub tool_call_ack: Option<tokio::sync::mpsc::UnboundedSender<()>>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            session_id: None,
            resume: None,
            model: None,
            cwd: None,
            env: BTreeMap::new(),
            permission_mode: None,
            tools: None,
            max_turns: None,
            system_prompt: None,
            mcp_servers: Vec::new(),
            request_timeout_ms: None,
            codebuddy_code_path: None,
            tool_call_ack: None,
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
