use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex};
use codebuddy_sdk::{Session, SessionOptions};
use crate::logging::append_codebuddy_proxy_log;
use crate::pending::PendingQueue;
use crate::usage::CliUsage;

pub struct PoolEntry {
    pub session: Arc<Session>,
    pub session_id: String,
    pub last_used: Mutex<Instant>,
    pub tool_signature: String,
    pub closed: std::sync::atomic::AtomicBool,
    pub is_new: std::sync::atomic::AtomicBool,
    /// Context generation for this pooled CLI process. Bumped on compact /
    /// explicit reset so a subsequent request with a newer epoch forces a
    /// recreate under the same external session id.
    pub context_epoch: std::sync::atomic::AtomicU64,
    /// Per-session capture queue bound to the session's MCP server at creation.
    /// The adapter clears it each turn and waits on it for tool-call arguments.
    pub pending: Arc<PendingQueue>,
    /// Per-session ack receiver: the SDK sends a unit here after each
    /// `tools/call` result is written to CLI stdin. The adapter awaits `n`
    /// acks (one per captured tool) before `interrupt()` so the placeholder
    /// result reaches the CLI first — otherwise the interrupt wins the stdin
    /// mutex race and the CLI records `undefined` as the tool_result.
    pub ack_rx: Mutex<mpsc::UnboundedReceiver<()>>,
    /// Last observed CLI session-cumulative usage. Used to convert the next
    /// turn's cumulative reading into a per-request OpenAI usage delta.
    /// Cleared when the pool entry is created/evicted (i.e. lives with the CLI
    /// process lifetime).
    pub last_cli_usage: Mutex<Option<CliUsage>>,
}

pub struct SessionPool {
    entries: Mutex<HashMap<String, Arc<PoolEntry>>>,
    max_sessions: usize,
    idle_timeout: Duration,
}

/// Request-side signal that the pooled CLI context should be recreated under
/// the same external session id (e.g. after codex compact).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContextResetRequest {
    /// Hard reset from `X-Context-Reset: 1` / true.
    pub force: bool,
    /// Explicit generation from `X-Context-Epoch`. When present and different
    /// from the pooled entry's epoch, the session is recreated.
    pub requested_epoch: Option<u64>,
}

impl SessionPool {
    pub fn new(max_sessions: usize, idle_timeout: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            max_sessions,
            idle_timeout,
        }
    }

    pub async fn acquire(
        &self,
        session_id: &str,
        opts: SessionOptions,
        tool_sig: &str,
        pending: Arc<PendingQueue>,
        reset: ContextResetRequest,
    ) -> anyhow::Result<Arc<PoolEntry>> {
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.get(session_id) {
            if !entry.closed.load(std::sync::atomic::Ordering::SeqCst) {
                *entry.last_used.lock().await = Instant::now();
                let current_epoch = entry
                    .context_epoch
                    .load(std::sync::atomic::Ordering::SeqCst);
                let epoch_mismatch = reset
                    .requested_epoch
                    .is_some_and(|epoch| epoch != current_epoch);
                // The proxy does NOT auto-reset sessions on usage heuristics
                // anymore — codex CLI is told a ~1B context window for the
                // CodeBuddy provider (so it never auto-compacts) and the
                // CodeBuddy agent manages its own compaction transparently.
                // A pooled session is recreated only on an explicit client
                // request: `X-Context-Reset` (reset.force) or a mismatched
                // `X-Context-Epoch`.
                if reset.force || epoch_mismatch {
                    let reason = if reset.force {
                        "context_reset"
                    } else {
                        "epoch_mismatch"
                    };
                    let next_epoch = reset
                        .requested_epoch
                        .unwrap_or_else(|| current_epoch.saturating_add(1));
                    append_codebuddy_proxy_log(&format!(
                        "pool evict_reason={reason} session_id={session_id} old_epoch={current_epoch} new_epoch={next_epoch}"
                    ));
                    drop(entries);
                    self.evict(session_id).await;
                    return self
                        .create(session_id, opts, tool_sig, pending, next_epoch)
                        .await;
                }
                if !tool_sig.is_empty() && entry.tool_signature != tool_sig {
                    append_codebuddy_proxy_log(&format!(
                        "pool evict_reason=tool_sig_mismatch session_id={session_id} old={} new={tool_sig}",
                        entry.tool_signature,
                    ));
                    drop(entries);
                    self.evict(session_id).await;
                    return self
                        .create(session_id, opts, tool_sig, pending, current_epoch)
                        .await;
                }
                entry.is_new.store(false, std::sync::atomic::Ordering::SeqCst);
                append_codebuddy_proxy_log(&format!(
                    "pool reuse session_id={session_id} epoch={current_epoch}"
                ));
                return Ok(entry.clone());
            }
        }
        drop(entries);
        let initial_epoch = reset.requested_epoch.unwrap_or(0);
        self.create(session_id, opts, tool_sig, pending, initial_epoch)
            .await
    }

    async fn create(
        &self,
        session_id: &str,
        mut opts: SessionOptions,
        tool_sig: &str,
        pending: Arc<PendingQueue>,
        context_epoch: u64,
    ) -> anyhow::Result<Arc<PoolEntry>> {
        let mut entries = self.entries.lock().await;
        if entries.len() >= self.max_sessions {
            let lru_id = entries
                .iter()
                .filter(|(_, e)| !e.closed.load(std::sync::atomic::Ordering::SeqCst))
                .min_by_key(|(_, e)| {
                    e.last_used
                        .try_lock()
                        .map(|t| *t)
                        .unwrap_or_else(|_| Instant::now())
                })
                .map(|(k, _)| k.clone());
            if let Some(id) = lru_id {
                append_codebuddy_proxy_log(&format!(
                    "pool evict_reason=lru session_id={session_id} victim={id} size={}",
                    entries.len(),
                ));
                drop(entries);
                self.evict(&id).await;
                entries = self.entries.lock().await;
            }
        }
        // Per-session ack channel: the SDK writes a unit after each
        // `tools/call` result hits CLI stdin. Bound here (not in
        // `build_session_options`) because the mpsc pair is owned by the pool
        // entry for the session's lifetime; `build_session_options` only sets
        // the sender side via `opts.tool_call_ack`.
        let (ack_tx, ack_rx) = mpsc::unbounded_channel::<()>();
        opts.tool_call_ack = Some(ack_tx);
        let session = Session::new(opts)?;
        session.connect().await?;
        append_codebuddy_proxy_log(&format!(
            "pool create session_id={session_id} epoch={context_epoch} tool_sig={}",
            if tool_sig.is_empty() {
                "<none>"
            } else {
                tool_sig
            },
        ));
        let entry = Arc::new(PoolEntry {
            session: Arc::new(session),
            session_id: session_id.to_string(),
            last_used: Mutex::new(Instant::now()),
            tool_signature: tool_sig.to_string(),
            closed: std::sync::atomic::AtomicBool::new(false),
            is_new: std::sync::atomic::AtomicBool::new(true),
            context_epoch: std::sync::atomic::AtomicU64::new(context_epoch),
            pending,
            ack_rx: Mutex::new(ack_rx),
            last_cli_usage: Mutex::new(None),
        });
        entries.insert(session_id.to_string(), entry.clone());
        Ok(entry)
    }

    pub async fn evict(&self, session_id: &str) {
        let entry = self.entries.lock().await.remove(session_id);
        if let Some(entry) = entry {
            append_codebuddy_proxy_log(&format!("pool evict session_id={session_id}"));
            entry.closed.store(true, std::sync::atomic::Ordering::SeqCst);
            entry.session.close().await;
        }
    }

    pub async fn size(&self) -> usize {
        self.entries.lock().await.len()
    }
}

pub fn tool_signature_of(tools: &Option<Vec<crate::openai_types::OaiTool>>) -> String {
    match tools {
        None => String::new(),
        Some(arr) if arr.is_empty() => String::new(),
        Some(arr) => {
            let mut names: Vec<String> = arr.iter().map(|t| t.function.name.clone()).collect();
            names.sort();
            serde_json::to_string(&names).unwrap_or_default()
        }
    }
}
