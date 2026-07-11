//! Per-session queue of tool calls captured by the proxy's MCP handlers.
//!
//! The proxy registers the client-declared tools as an in-process SDK MCP
//! server whose handlers **capture** the parsed `arguments` and then await a
//! per-call *release signal* rather than resolving unconditionally. For a turn
//! with N `tool_use` blocks the adapter releases **every** capture with a
//! placeholder result ("见下一个<user_query> 中的工具结果") — so every handler
//! resolves (no leaked tasks) and a sequential CLI proceeds to dispatch all N
//! tools (otherwise it stalls on the first and never invokes the rest, leaving
//! their arguments uncaptured). After the N-th capture lands the adapter
//! `interrupt()`s, cancelling the turn before the CLI returns to the model on
//! the placeholders; codex reflows the real results in the next `user_query`,
//! which the placeholder points the model to.
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
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use codebuddy_sdk::mcp::server::SdkMcpToolResult;
use tokio::sync::{mpsc, oneshot, Mutex};

/// One captured tool invocation awaiting an adapter release decision: the
/// tool name, its arguments serialized as a JSON string (the shape OpenAI
/// `tool_call.function.arguments` expects), and the oneshot sender the adapter
/// uses to either **release** the handler (with a placeholder result, so a
/// sequential CLI dispatches the next tool) or **keep** it pending (so the CLI
/// stalls on the last tool until `interrupt()` cancels the turn).
struct CaptureEntry {
    name: String,
    arguments: String,
    release: oneshot::Sender<SdkMcpToolResult>,
}

pub struct PendingQueue {
    // FIFO of captures not yet consumed by the adapter, in arrival order.
    captures: Mutex<VecDeque<CaptureEntry>>,
    // Release senders kept alive (never sent, never dropped mid-session) so
    // their handler futures stay pending until the session closes and the
    // SDK's `close_rx` reaps the spawned `tools/call` tasks. Dropping one
    // here would wake the handler with a `ReceiveError`, which the SDK turns
    // into a stray error `mcp_response` for an already-cancelled tool call —
    // so `clear()` moves unconsumed captures here instead of dropping them.
    pending_releases: Mutex<Vec<oneshot::Sender<SdkMcpToolResult>>>,
    // Signaling channel: `push_capturing` sends a unit, `next_capture` drains
    // it. mpsc (unbuffered-wakeup-safe) avoids the lost-wakeup race a bare
    // `Notify` can hit between condition check and `notified()` registration.
    signal_tx: mpsc::UnboundedSender<()>,
    signal_rx: Mutex<mpsc::UnboundedReceiver<()>>,
}

impl PendingQueue {
    pub fn new() -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel::<()>();
        Arc::new(Self {
            captures: Mutex::new(VecDeque::new()),
            pending_releases: Mutex::new(Vec::new()),
            signal_tx: tx,
            signal_rx: Mutex::new(rx),
        })
    }

    /// Reset for a new turn: drain unconsumed captures (moving their release
    /// senders into `pending_releases` so their handlers stay pending rather
    /// than erroring) and clear buffered signals. Does NOT touch
    /// `pending_releases` from prior turns — those handlers remain parked
    /// until session close, matching the single-tool "never resolves" leak the
    /// capture+interrupt strategy already accepts. Called at the start of each
    /// `run_streaming`/`run_non_streaming` turn.
    pub async fn clear(&self) {
        let leftover: Vec<CaptureEntry> = self.captures.lock().await.drain(..).collect();
        if !leftover.is_empty() {
            let mut releases = self.pending_releases.lock().await;
            for entry in leftover {
                releases.push(entry.release);
            }
        }
        let mut rx = self.signal_rx.lock().await;
        while rx.try_recv().is_ok() {}
    }

    /// Called by an MCP `tools/call` handler: record the arguments and wake the
    /// adapter, returning a release receiver the handler then awaits. The
    /// adapter decides per-capture whether to release (send a result on the
    /// sender) or keep (store the sender via `keep_release` so the receiver
    /// stays pending until session close).
    pub async fn push_capturing(
        &self,
        name: String,
        arguments: String,
    ) -> oneshot::Receiver<SdkMcpToolResult> {
        let (tx, rx) = oneshot::channel::<SdkMcpToolResult>();
        self.captures.lock().await.push_back(CaptureEntry {
            name,
            arguments,
            release: tx,
        });
        let _ = self.signal_tx.send(());
        rx
    }

    /// Pop the next capture in arrival order, waiting up to `timeout` for one
    /// to arrive. Returns the name, arguments, and the release sender the
    /// adapter uses to release-or-keep this capture. `None` on timeout (the
    /// CLI dispatched fewer tools than the assistant message declared; the
    /// adapter falls back to the block `input` for the missing calls).
    pub async fn next_capture(
        &self,
        timeout: Duration,
    ) -> Option<(String, String, oneshot::Sender<SdkMcpToolResult>)> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(entry) = self.captures.lock().await.pop_front() {
                return Some((entry.name, entry.arguments, entry.release));
            }
            // Hold the receiver lock across the timed recv. Only the adapter
            // (single in-flight turn per session) waits here, so this doesn't
            // contend with `push_capturing` (which only touches `captures` +
            // `signal_tx`). A push between the pop above and locking `rx` sends
            // a unit buffered in the unbounded channel, so `recv` won't miss it.
            let mut rx = self.signal_rx.lock().await;
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(())) => continue,
                _ => return None,
            }
        }
    }

    /// Keep a capture's release sender pending for the rest of the session:
    /// the handler never resolves, the CLI stalls on this tool, and
    /// `interrupt()` cancels the turn. The sender is held here (never sent,
    /// never dropped) and reaped when the SDK closes the session.
    pub async fn keep_release(&self, sender: oneshot::Sender<SdkMcpToolResult>) {
        self.pending_releases.lock().await.push(sender);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codebuddy_sdk::mcp::server::{SdkMcpToolContent, SdkMcpToolResult};
    use std::time::Duration;
    use tokio::time::timeout;

    const SKIP: &str = "（暂时跳过，待下轮回灌真实结果）";

    #[tokio::test]
    async fn push_unblocks_next_capture() {
        let q = PendingQueue::new();
        let q2 = q.clone();
        let h = tokio::spawn(async move { q2.next_capture(Duration::from_secs(2)).await });
        // Give the waiter a chance to park before pushing.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _rx = q
            .push_capturing("shell_command".to_string(), "{\"cmd\":\"ls\"}".to_string())
            .await;
        let got = h.await.unwrap().expect("capture present");
        assert_eq!(got.0, "shell_command");
        assert_eq!(got.1, "{\"cmd\":\"ls\"}");
    }

    #[tokio::test]
    async fn release_resolves_handler_with_skip() {
        let q = PendingQueue::new();
        let rx = q.push_capturing("t".to_string(), "{}".to_string()).await;
        let (_, _, sender) = q.next_capture(Duration::from_secs(2)).await.unwrap();
        let _ = sender.send(SdkMcpToolResult::text(SKIP));
        let result = timeout(Duration::from_secs(2), rx)
            .await
            .expect("handler resolves after release")
            .expect("release sender not dropped");
        match &result.content[0] {
            SdkMcpToolContent::Text { text, .. } => assert_eq!(text, SKIP),
            other => panic!("expected text content, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn keep_release_keeps_handler_pending() {
        let q = PendingQueue::new();
        let rx = q.push_capturing("t".to_string(), "{}".to_string()).await;
        let (_, _, sender) = q.next_capture(Duration::from_secs(2)).await.unwrap();
        q.keep_release(sender).await;
        // The handler must NOT resolve: the kept sender is held, never sent.
        let outcome = timeout(Duration::from_millis(80), rx).await;
        assert!(outcome.is_err(), "handler should stay pending after keep_release");
    }

    #[tokio::test]
    async fn clear_keeps_kept_release_pending() {
        // Invariant: clear() must not drop a kept release sender — dropping it
        // would wake the handler with a ReceiveError, which the SDK turns into
        // a stray error mcp_response for an already-cancelled tool call.
        let q = PendingQueue::new();
        let rx = q.push_capturing("t".to_string(), "{}".to_string()).await;
        let (_, _, sender) = q.next_capture(Duration::from_secs(2)).await.unwrap();
        q.keep_release(sender).await;
        q.clear().await;
        let outcome = timeout(Duration::from_millis(80), rx).await;
        assert!(outcome.is_err(), "kept release must survive clear()");
    }

    #[tokio::test]
    async fn clear_keeps_unconsumed_captures_pending() {
        // Leftover (un-consumed) captures from an aborted turn must also stay
        // pending across clear() — no stray Err mcp_response.
        let q = PendingQueue::new();
        let rx1 = q.push_capturing("a".to_string(), "{}".to_string()).await;
        let rx2 = q.push_capturing("b".to_string(), "{}".to_string()).await;
        q.clear().await;
        for rx in [rx1, rx2] {
            assert!(
                timeout(Duration::from_millis(80), rx).await.is_err(),
                "unconsumed capture must stay pending after clear()"
            );
        }
    }

    #[tokio::test]
    async fn next_capture_times_out_when_empty() {
        let q = PendingQueue::new();
        assert!(q.next_capture(Duration::from_millis(50)).await.is_none());
    }

    #[tokio::test]
    async fn next_capture_returns_in_arrival_order() {
        let q = PendingQueue::new();
        let _ = q.push_capturing("a".to_string(), "{\"i\":1}".to_string()).await;
        let _ = q.push_capturing("a".to_string(), "{\"i\":2}".to_string()).await;
        let _ = q.push_capturing("b".to_string(), "{\"i\":3}".to_string()).await;
        assert_eq!(q.next_capture(Duration::from_secs(1)).await.unwrap().1, "{\"i\":1}");
        assert_eq!(q.next_capture(Duration::from_secs(1)).await.unwrap().1, "{\"i\":2}");
        assert_eq!(q.next_capture(Duration::from_secs(1)).await.unwrap().1, "{\"i\":3}");
    }
}
