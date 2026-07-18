use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    /// CodeBuddy home dir (`~/.codebuddy` or `CODEBUDDY_HOME`) used to locate
    /// persisted session rollouts. When set, a pool miss resumes an existing
    /// conversation via `--resume <id>` if its rollout file exists on disk;
    /// otherwise a fresh session is created with `--session-id <id>`. `None`
    /// (e.g. in tests) disables resume and always creates fresh — matching
    /// the pre-resume behavior.
    codebuddy_home: Option<PathBuf>,
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
    pub fn new(max_sessions: usize, idle_timeout: Duration, codebuddy_home: Option<PathBuf>) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            max_sessions,
            idle_timeout,
            codebuddy_home,
        }
    }

    pub async fn acquire(
        &self,
        session_id: &str,
        mut opts: SessionOptions,
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
        // Genuine pool miss (not an explicit context reset/epoch recreate):
        // if a persisted CodeBuddy rollout exists for this session id under
        // the request's cwd, resume it so the warm CLI picks up prior context
        // and the proxy can keep sending only the incremental tail. Otherwise
        // start a fresh session pinned to this id. The explicit-reset branches
        // above intentionally bypass this — a reset means "fresh context".
        if let Some(home) = &self.codebuddy_home {
            if let Some(cwd) = opts.cwd.as_ref() {
                if rollout_exists(home, cwd, session_id) {
                    append_codebuddy_proxy_log(&format!(
                        "pool resume_from_rollout session_id={session_id} cwd={}",
                        cwd.display(),
                    ));
                    opts.resume = Some(session_id.to_string());
                    opts.session_id = None;
                }
            }
        }
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

/// Slug a working directory the way the CodeBuddy CLI names its per-project
/// rollout directory: canonicalize (so symlinks like `/tmp`→`/private/tmp`
/// match), drop the leading separator, then replace every remaining separator
/// with `-`. e.g. `/Users/kothchen/code/Kodex` → `Users-kothchen-code-Kodex`.
///
/// On Windows, `std::fs::canonicalize` returns a *verbatim* path prefixed
/// with `\\?\` (e.g. `\\?\C:\Users\...`). Both the verbatim prefix and the
/// drive-letter colon are invalid in directory names, so they are stripped
/// before slugifying — otherwise `fs::create_dir_all` under `projects/<slug>`
/// fails with `ERROR_INVALID_NAME` (code 123).
fn project_dir_slug(cwd: &Path) -> String {
    let resolved = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let s = resolved.to_string_lossy().into_owned();
    let stripped: &str = if cfg!(target_os = "windows") {
        // Drop the `\\?\` verbatim prefix that canonicalize adds on Windows.
        let base = s.strip_prefix(r"\\?\").unwrap_or(&s);
        let bytes = base.as_bytes();
        // Strip the drive-letter prefix such as `C:\` or `C:/`.
        if base.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/') {
            &base[3..]
        } else {
            base
        }
    } else {
        &s
    };
    let stripped = stripped
        .strip_prefix(std::path::MAIN_SEPARATOR)
        .unwrap_or(stripped);
    stripped.replace(std::path::MAIN_SEPARATOR, "-")
}

/// On-disk rollout path the CLI writes for a session: `<home>/projects/<slug>/<id>.jsonl`.
fn rollout_path(home: &Path, cwd: &Path, session_id: &str) -> PathBuf {
    home.join("projects").join(project_dir_slug(cwd)).join(format!("{session_id}.jsonl"))
}

/// Whether a persisted rollout exists for this session id under the given cwd.
/// Used to decide `--resume` (exists) vs `--session-id` (new) on a pool miss.
fn rollout_exists(home: &Path, cwd: &Path, session_id: &str) -> bool {
    rollout_path(home, cwd, session_id).is_file()
}

/// Resolve the CodeBuddy home directory from `CODEBUDDY_HOME` or the user's
/// home `.codebuddy`, mirroring the CLI's default. `None` when neither is
/// available (resume then disabled).
pub fn default_codebuddy_home() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("CODEBUDDY_HOME") {
        return Some(PathBuf::from(v));
    }
    let home_key = if cfg!(target_os = "windows") {
        "USERPROFILE"
    } else {
        "HOME"
    };
    std::env::var_os(home_key)
        .map(PathBuf::from)
        .map(|h| h.join(".codebuddy"))
}

/// Resolve the unified CodeBuddy working directory `~/.kodex/codebuddy` (or
/// `$KODEX_DATA_ROOT/codebuddy` when the env override is set). Unlike the
/// project-root path previously forwarded as `X-Session-Dir` — which could
/// not resolve when the proxy and client ran on different machines/OSes —
/// this is a local directory on the proxy's own machine. The proxy creates
/// it on startup (see `run` in `server.rs`) and spawns every CodeBuddy CLI
/// session here, so its rollout slug is stable regardless of which client
/// connects.
pub fn default_codebuddy_workdir() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("KODEX_DATA_ROOT") {
        if !v.is_empty() {
            return Some(PathBuf::from(v).join("codebuddy"));
        }
    }
    let home_key = if cfg!(target_os = "windows") {
        "USERPROFILE"
    } else {
        "HOME"
    };
    std::env::var_os(home_key)
        .map(PathBuf::from)
        .map(|h| h.join(".kodex").join("codebuddy"))
}

#[cfg(test)]
mod rollout_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn slug_strips_leading_separator_and_replaces_rest() {
        let tmp = tempdir().unwrap();
        let cwd = tmp.path().to_path_buf();
        let slug = project_dir_slug(&cwd);
        assert!(!slug.contains(std::path::MAIN_SEPARATOR), "slug={slug}");
        assert!(!slug.starts_with(std::path::MAIN_SEPARATOR));
        // On Windows the slug must not contain a drive-letter colon — it is
        // invalid in directory names and breaks `fs::create_dir_all` under
        // `projects/<slug>`.
        assert!(!slug.contains(':'), "slug={slug}");
    }

    #[test]
    fn rollout_exists_detects_present_and_absent() {
        let tmp = tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let cwd = tmp.path().join("proj");
        fs::create_dir_all(&cwd).unwrap();
        let slug = project_dir_slug(&cwd);
        let dir = home.join("projects").join(&slug);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("acme-1.jsonl"), "{}").unwrap();
        assert!(rollout_exists(&home, &cwd, "acme-1"));
        assert!(!rollout_exists(&home, &cwd, "acme-2"));
    }

    #[test]
    fn slug_handles_tmp_symlink_canonicalization() {
        // `/tmp` is a symlink to `/private/tmp` on macOS; the CLI stores under
        // the resolved path. canonicalize must match so resume finds it.
        if !cfg!(target_os = "macos") {
            return;
        }
        let slug = project_dir_slug(Path::new("/tmp"));
        assert!(slug.starts_with("private-tmp"), "slug={slug}");
    }
}
