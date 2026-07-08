//! Per-session queue of tool calls captured by the proxy's MCP handlers.
//!
//! The proxy registers the client-declared tools as an in-process SDK MCP
//! server whose handlers **capture** the parsed `arguments` and then **never
//! resolve** (mirroring the reference TS `new Promise(() => {})`). The CLI's
//! agentic loop therefore stalls at the `tool_use` until `session.interrupt()`
//! cancels it, leaving no `tool_result` in the CLI's history — so the real
//! result can be fed back as plain text (`[tool_result call_id=…]`) on the next
//! turn instead of as a structured block (which the bundled CLI downgrades to a
//! contentless placeholder, causing the model to loop).
//!
//! Why capture here instead of reading `input` from the assistant `tool_use`
//! block: in the CodeBuddy CLI's stream-json protocol the consolidated
//! `tool_use` block carries `id` + `name` but **not** the full arguments
//! reliably; the arguments arrive only when the CLI invokes the tool via
//! `tools/call`. The adapter pairs the block's `id` with the captured
//! `arguments` (matched by name) to build the OpenAI `tool_call` surfaced to
//! codex.
//!
//! The queue is persistent on the pool entry (bound to the session's MCP server
//! at creation) and cleared at the start of each turn so captures from a prior
//! turn don't leak in.
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

/// One captured tool invocation: the tool name plus its arguments serialized as
/// a JSON string (the shape OpenAI `tool_call.function.arguments` expects).
#[derive(Clone)]
pub struct CapturedToolCall {
    pub name: String,
    pub arguments: String,
}

pub struct PendingQueue {
    handlers: Mutex<Vec<CapturedToolCall>>,
    // Signaling channel: `push` sends a unit, `wait_for_captures` drains it.
    // mpsc (unbuffered-wakeup-safe) avoids the lost-wakeup race a bare `Notify`
    // can hit between condition check and `notified()` registration.
    signal_tx: mpsc::UnboundedSender<()>,
    signal_rx: Mutex<mpsc::UnboundedReceiver<()>>,
}

impl PendingQueue {
    pub fn new() -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel::<()>();
        Arc::new(Self {
            handlers: Mutex::new(Vec::new()),
            signal_tx: tx,
            signal_rx: Mutex::new(rx),
        })
    }

    /// Drop captures and pending signals left from a prior turn. Called at the
    /// start of each `run_streaming`/`run_non_streaming` turn.
    pub async fn clear(&self) {
        self.handlers.lock().await.clear();
        let mut rx = self.signal_rx.lock().await;
        while rx.try_recv().is_ok() {}
    }

    /// Called by an MCP `tools/call` handler: record the arguments and wake the
    /// adapter. The handler then never resolves (see module docs).
    pub async fn push(&self, name: String, arguments: String) {
        self.handlers.lock().await.push(CapturedToolCall { name, arguments });
        let _ = self.signal_tx.send(());
    }

    /// Block until at least `n` captures are present or `timeout` elapses.
    /// Returns the capture count at exit. The adapter uses `n` = number of
    /// `tool_use` blocks seen on the assistant message.
    pub async fn wait_for_captures(&self, n: usize, timeout: Duration) -> usize {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.handlers.lock().await.len() >= n {
                return self.handlers.lock().await.len();
            }
            // Hold the receiver lock across the timed recv. Only the adapter
            // (single in-flight turn per session) waits here, so this doesn't
            // contend with `push` (which only touches `handlers` + `signal_tx`).
            let mut rx = self.signal_rx.lock().await;
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(())) => continue,
                _ => return self.handlers.lock().await.len(),
            }
        }
    }

    /// Last captured arguments for a tool by name (most recent wins, matching
    /// the last `tool_use` block semantics for repeated same-tool calls).
    pub async fn arguments_for(&self, name: &str) -> Option<String> {
        self.handlers
            .lock()
            .await
            .iter()
            .rev()
            .find(|c| c.name == name)
            .map(|c| c.arguments.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn push_unblocks_wait() {
        let q = PendingQueue::new();
        let q2 = q.clone();
        let h = tokio::spawn(async move { q2.wait_for_captures(1, Duration::from_secs(2)).await });
        // Give the waiter a chance to park before pushing.
        tokio::time::sleep(Duration::from_millis(20)).await;
        q.push("shell_command".to_string(), "{\"cmd\":\"ls\"}".to_string()).await;
        let got = h.await.unwrap();
        assert!(got >= 1);
        assert_eq!(
            q.arguments_for("shell_command").await.as_deref(),
            Some("{\"cmd\":\"ls\"}")
        );
    }

    #[tokio::test]
    async fn wait_times_out_when_no_capture() {
        let q = PendingQueue::new();
        let got = q.wait_for_captures(1, Duration::from_millis(50)).await;
        assert_eq!(got, 0);
        assert!(q.arguments_for("none").await.is_none());
    }

    #[tokio::test]
    async fn clear_drops_prior_captures() {
        let q = PendingQueue::new();
        q.push("t".to_string(), "{}".to_string()).await;
        assert!(q.arguments_for("t").await.is_some());
        q.clear().await;
        assert!(q.arguments_for("t").await.is_none());
    }

    #[tokio::test]
    async fn last_match_wins() {
        let q = PendingQueue::new();
        q.push("t".to_string(), "{\"i\":1}".to_string()).await;
        q.push("t".to_string(), "{\"i\":2}".to_string()).await;
        assert_eq!(q.arguments_for("t").await.as_deref(), Some("{\"i\":2}"));
    }
}
