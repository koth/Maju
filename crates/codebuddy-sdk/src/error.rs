//! Error types for the `codebuddy-sdk` crate.

use std::io;
use thiserror::Error;

/// Errors returned by the SDK.
#[derive(Debug, Error)]
pub enum SdkError {
    /// The CLI binary could not be located (env var unset, bundled binary missing,
    /// platform unsupported).
    #[error("CodeBuddy CLI binary not found. Set CODEBUDDY_CODE_PATH or place a bundled binary at the expected path. Searched: {0:?}")]
    CliNotFound(Vec<String>),

    /// Failed to spawn the CLI subprocess.
    #[error("failed to spawn CodeBuddy CLI: {0}")]
    Spawn(#[source] io::Error),

    /// CLI process exited without producing any output (often crash on startup).
    #[error("CLI produced no output and exited with code {exit_code:?}: {stderr}")]
    CliNoOutput {
        exit_code: Option<i32>,
        stderr: String,
    },

    /// CLI process exited non-zero after producing some output.
    #[error("CLI exited with code {exit_code}: {stderr}")]
    CliExited { exit_code: i32, stderr: String },

    /// Stdin pipe was closed (e.g. process exited, transport shut down).
    #[error("stdin pipe closed")]
    StdinClosed,

    /// A control request timed out awaiting its response. `stderr` carries a
    /// snapshot of the CLI's stderr ring buffer at the timeout instant, so the
    /// caller can see why the CLI never answered (e.g. missing auth, a startup
    /// crash printed to stderr). Previously this surfaced as an opaque 60s
    /// timeout with no CLI context.
    #[error("control request {subtype} timed out after {timeout_ms}ms; cli stderr: {stderr}")]
    ControlTimeout {
        subtype: String,
        timeout_ms: u64,
        stderr: String,
    },

    /// The control response indicated an error.
    #[error("control request {subtype} failed: {error}")]
    ControlError { subtype: String, error: String },

    /// The control response did not arrive because the connection was closed.
    #[error("connection closed while waiting for control response (subtype={subtype})")]
    ControlConnectionClosed { subtype: String },

    /// JSON encoding/decoding of wire envelopes failed.
    #[error("invalid JSON on wire: {0}")]
    Json(#[from] serde_json::Error),

    /// Async task join failed.
    #[error("background task panicked: {0}")]
    Join(#[from] tokio::task::JoinError),

    /// Generic I/O error on transport pipes.
    #[error("transport I/O error: {0}")]
    Io(#[from] io::Error),

    /// A user-supplied handler or callback returned an error.
    #[error("handler error: {0}")]
    Handler(String),

    /// Catch-all for unexpected protocol conditions.
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// Convenience alias for results in this crate.
pub type SdkResult<T> = Result<T, SdkError>;
