use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use serde_json::Value;
use tokio::sync::Mutex;
use codebuddy_sdk::{Session, SessionOptions, SdkMcpServerEntry};
use crate::logging::append_codebuddy_proxy_log;
use crate::pending::PendingQueue;
use crate::prompt_builder::PROXY_TOOL_SERVER_NAME;
pub struct PoolEntry {
    pub session: Arc<Session>,
    pub session_id: String,
    pub last_used: Mutex<Instant>,
    pub tool_signature: String,
    pub closed: std::sync::atomic::AtomicBool,
    pub is_new: std::sync::atomic::AtomicBool,
    /// Per-session capture queue bound to the session's MCP server at creation.
    /// The adapter clears it each turn and waits on it for tool-call arguments.
    pub pending: Arc<PendingQueue>,
}
pub struct SessionPool {
    entries: Mutex<HashMap<String, Arc<PoolEntry>>>,
    max_sessions: usize,
    idle_timeout: Duration,
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
    ) -> anyhow::Result<Arc<PoolEntry>> {
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.get(session_id) {
            if !entry.closed.load(std::sync::atomic::Ordering::SeqCst) {
                *entry.last_used.lock().await = Instant::now();
                if !tool_sig.is_empty() && entry.tool_signature != tool_sig {
                    append_codebuddy_proxy_log(&format!(
                        "pool evict_reason=tool_sig_mismatch session_id={session_id} old={} new={tool_sig}",
                        entry.tool_signature,
                    ));
                    drop(entries);
                    self.evict(session_id).await;
                    return self.create(session_id, opts, tool_sig, pending).await;
                }
                entry.is_new.store(false, std::sync::atomic::Ordering::SeqCst);
                append_codebuddy_proxy_log(&format!("pool reuse session_id={session_id}"));
                return Ok(entry.clone());
            }
        }
        drop(entries);
        self.create(session_id, opts, tool_sig, pending).await
    }
    async fn create(
        &self,
        session_id: &str,
        opts: SessionOptions,
        tool_sig: &str,
        pending: Arc<PendingQueue>,
    ) -> anyhow::Result<Arc<PoolEntry>> {
        let mut entries = self.entries.lock().await;
        if entries.len() >= self.max_sessions {
            let lru_id = entries
                .iter()
                .filter(|(_, e)| !e.closed.load(std::sync::atomic::Ordering::SeqCst))
                .min_by_key(|(_, e)| {
                    e.last_used.try_lock().map(|t| *t).unwrap_or_else(|_| Instant::now())
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
        let session = Session::new(opts)?;
        session.connect().await?;
        append_codebuddy_proxy_log(&format!(
            "pool create session_id={session_id} tool_sig={}",
            if tool_sig.is_empty() { "<none>" } else { tool_sig },
        ));
        let entry = Arc::new(PoolEntry {
            session: Arc::new(session),
            session_id: session_id.to_string(),
            last_used: Mutex::new(Instant::now()),
            tool_signature: tool_sig.to_string(),
            closed: std::sync::atomic::AtomicBool::new(false),
            is_new: std::sync::atomic::AtomicBool::new(true),
            pending,
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
