use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, ToSql, params, params_from_iter};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;
use uuid::Uuid;
mod codec;
mod legacy;
mod util;

use codec::{
    change_set_source_from_str, change_set_source_to_str, change_set_status_from_str,
    change_set_status_to_str, diff_quality_from_str, diff_quality_to_str,
    file_change_type_from_str,
};
use legacy::{
    LEGACY_AGENT_CONVERSATION_PREFIX, LEGACY_AGENT_RECENT_PREFIX, LEGACY_AGENT_TURN_PREFIX,
    file_summary_from_record, legacy_agent_conversation_id, legacy_agent_recent_id,
    legacy_agent_turn_id, legacy_records_from_session_changes, summarize_change_records,
};
use util::{
    cap_string, decode_json_vec, epoch_start_of_date_local, instant_to_date_local,
    normalize_change_path, normalize_workspace_root, now_iso, parse_instant_to_epoch_secs,
    upsert_loaded_change,
};
// Re-export the display timestamp helpers so app-core can produce ISO-8601
// without duplicating the calendar algorithm; storage stays epoch-seconds.
pub use util::{epoch_secs_to_iso_utc, instant_to_iso_utc};
use workspace_model::{
    ArchivedSessionListItem, ChangeSetSource, ChangeSetStatus, ChangeSetSummary, ChatMessage,
    FileChangeRecord, FileChangeSummary, FileChangeType, MessageRole, SessionFileChange,
    SessionListItem, SessionUsageSnapshot, TimelineItem, ToolDiffPreview, ToolInvocation,
    ToolStatus, TurnFileChanges, UsageContextSnapshot, UsageDailyBucket, UsageEvent,
    UsageEventScope, UsageModelSummary, UsageSummaryGroupBy, UsageSummaryRequest,
    UsageSummaryRow, UsageTokenBreakdown,
};

const MAX_RAW_OUTPUT_BYTES: usize = 32 * 1024;

/// Agent CLI identifiers that cannot report detailed token usage (input/output/
/// cache/reasoning) because the agent code is third-party and cannot be
/// modified. Their usage events still carry context occupancy (`used`/`size`)
/// from the standard ACP `usage_update`, so single-session snapshots and the
/// live dock continue to work; only the cross-session historical summary
/// filters them out. Keep the literal in sync with
/// `app_core::settings::agent_cli::AgentCliId::Codebuddy` and
/// `update_session_agent_cli` / `append_usage_event`, which write this exact
/// string to `sessions.agent_cli` and `usage_events.agent_cli`.
pub(super) const NON_REPORTING_AGENT_CLI: &str = "codebuddy";

#[derive(Debug, Clone)]
struct StoredUsageEvent {
    session_id: String,
    session_title: Option<String>,
    workspace_root: String,
    agent_cli: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    scope: UsageEventScope,
    tokens: UsageTokenBreakdown,
    context: UsageContextSnapshot,
    created_at: String,
}

pub struct SessionStore {
    conn: Connection,
    workspace_root: String,
}

impl SessionStore {
    /// Open (or create) the global session database at `{app_data_root}/sessions/sessions.db`.
    pub fn open(app_data_root: &Path, workspace_root: &Path) -> Result<Self> {
        let sessions_dir = app_data_root.join("sessions");
        fs::create_dir_all(&sessions_dir)
            .with_context(|| format!("在 {} 创建会话数据目录失败", sessions_dir.display()))?;

        let db_path = sessions_dir.join("sessions.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("在 {} 打开 sessions.db 失败", db_path.display()))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        let store = Self {
            conn,
            workspace_root: normalize_workspace_root(workspace_root),
        };
        store.run_migrations()?;
        store.import_legacy_workspace_db(workspace_root)?;
        Ok(store)
    }

    pub fn open_global(app_data_root: &Path) -> Result<Self> {
        let sessions_dir = app_data_root.join("sessions");
        fs::create_dir_all(&sessions_dir)
            .with_context(|| format!("在 {} 创建会话数据目录失败", sessions_dir.display()))?;

        let db_path = sessions_dir.join("sessions.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("在 {} 打开 sessions.db 失败", db_path.display()))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        let store = Self {
            conn,
            workspace_root: String::new(),
        };
        store.run_migrations()?;
        Ok(store)
    }

    fn run_migrations(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL DEFAULT 'New Session',
                model TEXT NOT NULL,
                model_provider TEXT,
                status TEXT NOT NULL DEFAULT 'Idle',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                archived_at TEXT
            );

            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                role TEXT NOT NULL,
                body TEXT NOT NULL,
                seq INTEGER NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS tool_invocations (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                call_id TEXT NOT NULL,
                parent_call_id TEXT,
                name TEXT NOT NULL,
                kind TEXT NOT NULL DEFAULT '',
                summary TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'Pending',
                raw_input TEXT,
                raw_output TEXT,
                error TEXT,
                diff_paths TEXT,
                diff_previews TEXT,
                seq INTEGER NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS session_file_changes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                path TEXT NOT NULL,
                change_type TEXT NOT NULL,
                base_text TEXT,
                new_text TEXT NOT NULL,
                added_lines INTEGER NOT NULL DEFAULT 0,
                removed_lines INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL,
                UNIQUE(session_id, path)
            );

            CREATE TABLE IF NOT EXISTS session_review_file_changes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                path TEXT NOT NULL,
                change_type TEXT NOT NULL,
                base_text TEXT,
                new_text TEXT NOT NULL,
                added_lines INTEGER NOT NULL DEFAULT 0,
                removed_lines INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL,
                UNIQUE(session_id, path)
            );

            CREATE TABLE IF NOT EXISTS session_turn_file_changes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
                path TEXT NOT NULL,
                change_type TEXT NOT NULL,
                base_text TEXT,
                new_text TEXT NOT NULL,
                added_lines INTEGER NOT NULL DEFAULT 0,
                removed_lines INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL,
                UNIQUE(session_id, message_id, path)
            );

            CREATE TABLE IF NOT EXISTS change_sets (
                id TEXT PRIMARY KEY,
                session_id TEXT REFERENCES sessions(id) ON DELETE CASCADE,
                workspace_root TEXT NOT NULL,
                source TEXT NOT NULL,
                message_id TEXT,
                tool_call_id TEXT,
                owner_key TEXT,
                label TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS change_set_files (
                change_set_id TEXT NOT NULL REFERENCES change_sets(id) ON DELETE CASCADE,
                path TEXT NOT NULL,
                change_type TEXT NOT NULL,
                base_text TEXT,
                target_text TEXT,
                added_lines INTEGER NOT NULL DEFAULT 0,
                removed_lines INTEGER NOT NULL DEFAULT 0,
                quality TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (change_set_id, path)
            );

            CREATE TABLE IF NOT EXISTS usage_events (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                workspace_root TEXT NOT NULL,
                agent_cli TEXT,
                provider TEXT,
                model TEXT,
                scope TEXT NOT NULL,
                input_tokens INTEGER,
                output_tokens INTEGER,
                cache_read_tokens INTEGER,
                cache_write_tokens INTEGER,
                reasoning_tokens INTEGER,
                total_tokens INTEGER,
                context_used_tokens INTEGER,
                context_window_tokens INTEGER,
                raw_json TEXT,
                created_at TEXT NOT NULL,
                latency_ms INTEGER,
                ttft_ms INTEGER,
                tokens_per_second REAL
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);
            CREATE INDEX IF NOT EXISTS idx_tools_session ON tool_invocations(session_id, seq);
            CREATE INDEX IF NOT EXISTS idx_file_changes_session ON session_file_changes(session_id);
            CREATE INDEX IF NOT EXISTS idx_review_file_changes_session ON session_review_file_changes(session_id);
            CREATE INDEX IF NOT EXISTS idx_turn_file_changes_session_message ON session_turn_file_changes(session_id, message_id);
            CREATE INDEX IF NOT EXISTS idx_change_sets_workspace_source ON change_sets(workspace_root, source, updated_at);
            CREATE INDEX IF NOT EXISTS idx_change_sets_session_source ON change_sets(session_id, source, updated_at);
            CREATE INDEX IF NOT EXISTS idx_change_sets_message ON change_sets(message_id);
            CREATE INDEX IF NOT EXISTS idx_change_set_files_change_set ON change_set_files(change_set_id);
            CREATE INDEX IF NOT EXISTS idx_usage_events_session ON usage_events(session_id, created_at);
            CREATE INDEX IF NOT EXISTS idx_usage_events_workspace ON usage_events(workspace_root, created_at);
            ",
        )?;

        // Migration: add acp_session_id column if it doesn't exist
        let has_acp_col: bool = self
            .conn
            .prepare(
                "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'acp_session_id'",
            )?
            .query_row([], |row| row.get(0))?;
        if !has_acp_col {
            self.conn
                .execute_batch("ALTER TABLE sessions ADD COLUMN acp_session_id TEXT;")?;
        }

        let has_mode_col: bool = self
            .conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'mode'")?
            .query_row([], |row| row.get(0))?;
        if !has_mode_col {
            self.conn
                .execute_batch("ALTER TABLE sessions ADD COLUMN mode TEXT;")?;
        }

        let has_model_provider_col: bool = self
            .conn
            .prepare(
                "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'model_provider'",
            )?
            .query_row([], |row| row.get(0))?;
        if !has_model_provider_col {
            self.conn
                .execute_batch("ALTER TABLE sessions ADD COLUMN model_provider TEXT;")?;
        }

        let has_workspace_col: bool = self
            .conn
            .prepare(
                "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'workspace_root'",
            )?
            .query_row([], |row| row.get(0))?;
        if !has_workspace_col {
            self.conn
                .execute_batch("ALTER TABLE sessions ADD COLUMN workspace_root TEXT;")?;
        }

        let has_tool_diff_paths_col: bool = self
            .conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('tool_invocations') WHERE name = 'diff_paths'")?
            .query_row([], |row| row.get(0))?;
        if !has_tool_diff_paths_col {
            self.conn
                .execute_batch("ALTER TABLE tool_invocations ADD COLUMN diff_paths TEXT;")?;
        }

        let has_tool_diff_previews_col: bool = self
            .conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('tool_invocations') WHERE name = 'diff_previews'")?
            .query_row([], |row| row.get(0))?;
        if !has_tool_diff_previews_col {
            self.conn
                .execute_batch("ALTER TABLE tool_invocations ADD COLUMN diff_previews TEXT;")?;
        }

        if !self.workspace_root.is_empty() {
            self.conn.execute(
                "UPDATE sessions SET workspace_root = ?1 WHERE workspace_root IS NULL OR workspace_root = ''",
                params![&self.workspace_root],
            )?;
        }

        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_sessions_workspace_updated ON sessions(workspace_root, updated_at);",
        )?;

        // Migration: add agent_cli column
        let has_agent_col: bool = self
            .conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'agent_cli'")?
            .query_row([], |row| row.get(0))?;
        if !has_agent_col {
            self.conn
                .execute_batch("ALTER TABLE sessions ADD COLUMN agent_cli TEXT;")?;
        }

        let has_codex_provider_col: bool = self
            .conn
            .prepare(
                "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'codex_provider'",
            )?
            .query_row([], |row| row.get(0))?;
        if !has_codex_provider_col {
            self.conn
                .execute_batch("ALTER TABLE sessions ADD COLUMN codex_provider TEXT;")?;
        }

        let has_archived_at_col: bool = self
            .conn
            .prepare(
                "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'archived_at'",
            )?
            .query_row([], |row| row.get(0))?;
        if !has_archived_at_col {
            self.conn
                .execute_batch("ALTER TABLE sessions ADD COLUMN archived_at TEXT;")?;
        }

        // Migration: add is_steer column to messages so the frontend can
        // distinguish steer (追加指令) messages from regular turn-starting
        // User messages and skip the premature turn-boundary fold.
        let has_is_steer_col: bool = self
            .conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'is_steer'")?
            .query_row([], |row| row.get(0))?;
        if !has_is_steer_col {
            self.conn
                .execute_batch("ALTER TABLE messages ADD COLUMN is_steer INTEGER NOT NULL DEFAULT 0;")?;
        }

        let has_latency_col: bool = self
            .conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('usage_events') WHERE name = 'latency_ms'")?
            .query_row([], |row| row.get(0))?;
        if !has_latency_col {
            self.conn.execute_batch(
                "ALTER TABLE usage_events ADD COLUMN latency_ms INTEGER;
                 ALTER TABLE usage_events ADD COLUMN ttft_ms INTEGER;
                 ALTER TABLE usage_events ADD COLUMN tokens_per_second REAL;",
            )?;
        }

        Ok(())
    }

    pub fn db_path(app_data_root: &Path) -> std::path::PathBuf {
        app_data_root.join("sessions").join("sessions.db")
    }

    pub fn workspace_root(&self) -> &str {
        &self.workspace_root
    }

    fn import_legacy_workspace_db(&self, workspace_root: &Path) -> Result<()> {
        let legacy_db = workspace_root.join(".kodex").join("sessions.db");
        let new_db = Self::db_path(workspace_root);
        if !legacy_db.is_file() || legacy_db == new_db {
            return Ok(());
        }

        let legacy_db_path = legacy_db.to_string_lossy().to_string();
        self.conn
            .execute("ATTACH DATABASE ?1 AS legacy", params![legacy_db_path])
            .with_context(|| {
                format!("failed to attach legacy session DB {}", legacy_db.display())
            })?;

        let result = self.import_attached_legacy_db();
        let detach_result = self.conn.execute_batch("DETACH DATABASE legacy;");
        result?;
        detach_result?;
        Ok(())
    }

    fn import_attached_legacy_db(&self) -> Result<()> {
        let has_sessions: bool = self.conn.query_row(
            "SELECT COUNT(*) FROM legacy.sqlite_master WHERE type = 'table' AND name = 'sessions'",
            [],
            |row| row.get::<_, i64>(0),
        )? > 0;
        if !has_sessions {
            return Ok(());
        }

        let acp_session_id = if self.legacy_column_exists("sessions", "acp_session_id")? {
            "acp_session_id"
        } else {
            "NULL"
        };
        let mode = if self.legacy_column_exists("sessions", "mode")? {
            "mode"
        } else {
            "NULL"
        };
        let session_sql = format!(
            "INSERT OR IGNORE INTO sessions (id, title, model, status, created_at, updated_at, acp_session_id, mode, workspace_root)
             SELECT id, title, model, status, created_at, updated_at, {acp_session_id}, {mode}, ?1 FROM legacy.sessions"
        );
        self.conn
            .execute(&session_sql, params![&self.workspace_root])?;

        if self.legacy_table_exists("messages")? {
            self.conn.execute(
                "INSERT OR IGNORE INTO messages (id, session_id, role, body, seq, created_at)
                 SELECT m.id, m.session_id, m.role, m.body, m.seq, m.created_at
                 FROM legacy.messages m
                 JOIN sessions s ON s.id = m.session_id AND s.workspace_root = ?1;",
                params![&self.workspace_root],
            )?;
        }

        if self.legacy_table_exists("tool_invocations")? {
            let diff_paths = if self.legacy_column_exists("tool_invocations", "diff_paths")? {
                "diff_paths"
            } else {
                "NULL"
            };
            let diff_previews = if self.legacy_column_exists("tool_invocations", "diff_previews")? {
                "diff_previews"
            } else {
                "NULL"
            };
            let tools_sql = format!(
                "INSERT OR IGNORE INTO tool_invocations (id, session_id, call_id, parent_call_id, name, kind, summary, status, raw_input, raw_output, error, diff_paths, diff_previews, seq, created_at)
                 SELECT t.id, t.session_id, t.call_id, t.parent_call_id, t.name, t.kind, t.summary, t.status, t.raw_input, t.raw_output, t.error, {diff_paths}, {diff_previews}, t.seq, t.created_at
                 FROM legacy.tool_invocations t
                 JOIN sessions s ON s.id = t.session_id AND s.workspace_root = ?1"
            );
            self.conn
                .execute(&tools_sql, params![&self.workspace_root])?;
        }

        if self.legacy_table_exists("session_file_changes")? {
            self.conn.execute(
                "INSERT OR IGNORE INTO session_file_changes (session_id, path, change_type, base_text, new_text, added_lines, removed_lines, updated_at)
                 SELECT f.session_id, f.path, f.change_type, f.base_text, f.new_text, f.added_lines, f.removed_lines, f.updated_at
                 FROM legacy.session_file_changes f
                 JOIN sessions s ON s.id = f.session_id AND s.workspace_root = ?1;",
                params![&self.workspace_root],
            )?;
        }

        Ok(())
    }

    fn legacy_table_exists(&self, table: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM legacy.sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |row| row.get::<_, i64>(0),
        )? > 0)
    }

    fn legacy_column_exists(&self, table: &str, column: &str) -> Result<bool> {
        let sql = format!("PRAGMA legacy.table_info({table})");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        for row in rows {
            if row? == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    // ── Session CRUD ──

    pub fn create_session(&self, id: &str, model: &str) -> Result<()> {
        let now = now_iso();
        self.conn.execute(
            "INSERT INTO sessions (id, title, model, status, created_at, updated_at, workspace_root) VALUES (?1, '新会话', ?2, 'Idle', ?3, ?4, ?5)",
            params![id, model, now, now, &self.workspace_root],
        )?;
        Ok(())
    }

    pub fn update_session_title(&self, id: &str, title: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn update_session_status(&self, id: &str, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn update_acp_session_id(&self, id: &str, acp_session_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET acp_session_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![acp_session_id, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn clear_acp_session_id(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET acp_session_id = NULL, updated_at = ?1 WHERE id = ?2",
            params![now_iso(), id],
        )?;
        Ok(())
    }

    pub fn session_has_activity(&self, id: &str) -> Result<bool> {
        let message_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        if message_count > 0 {
            return Ok(true);
        }

        let tool_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tool_invocations WHERE session_id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        Ok(tool_count > 0)
    }

    pub fn update_session_model_mode(
        &self,
        id: &str,
        model: &str,
        mode: Option<&str>,
    ) -> Result<()> {
        self.update_session_model_mode_provider(id, model, None, mode)
    }

    pub fn update_session_model_mode_provider(
        &self,
        id: &str,
        model: &str,
        model_provider: Option<&str>,
        mode: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET model = ?1, model_provider = ?2, mode = ?3, updated_at = ?4 WHERE id = ?5",
            params![model, model_provider, mode, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn get_session_model_mode(&self, id: &str) -> Result<Option<(String, Option<String>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT model, mode FROM sessions WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some((row.get(0)?, row.get(1)?)))
        } else {
            Ok(None)
        }
    }

    pub fn get_session_model_provider_mode(
        &self,
        id: &str,
    ) -> Result<Option<(String, Option<String>, Option<String>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT model, model_provider, mode FROM sessions WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some((row.get(0)?, row.get(1)?, row.get(2)?)))
        } else {
            Ok(None)
        }
    }

    pub fn get_acp_session_id(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT acp_session_id FROM sessions WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .ok()
            .flatten())
    }

    pub fn update_session_agent_cli(&self, id: &str, agent_cli: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET agent_cli = ?1, updated_at = ?2 WHERE id = ?3",
            params![agent_cli, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn get_session_agent_cli(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT agent_cli FROM sessions WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .ok()
            .flatten())
    }

    pub fn update_session_codex_provider(&self, id: &str, provider: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET codex_provider = ?1, updated_at = ?2 WHERE id = ?3",
            params![provider, now_iso(), id],
        )?;
        Ok(())
    }

    pub fn get_session_codex_provider(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT codex_provider FROM sessions WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .ok()
            .flatten())
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionListItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.title, s.status, s.created_at, s.updated_at,
                    COUNT(m.id) as msg_count,
                    s.acp_session_id, s.agent_cli
             FROM sessions s
             LEFT JOIN messages m ON m.session_id = s.id
             WHERE s.workspace_root = ?1 AND s.archived_at IS NULL
             GROUP BY s.id
             ORDER BY s.updated_at DESC",
        )?;

        let rows = stmt.query_map(params![&self.workspace_root], |row| {
            Ok(SessionListItem {
                id: row.get(0)?,
                title: row.get(1)?,
                status: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
                message_count: row.get(5)?,
                acp_session_id: row.get(6)?,
                agent_cli: row.get(7)?,
                runtime_status: Default::default(),
                attention_state: Default::default(),
            })
        })?;

        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    pub fn list_session_summaries(&self) -> Result<Vec<SessionListItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, status, created_at, updated_at, acp_session_id, agent_cli
             FROM sessions
             WHERE workspace_root = ?1 AND archived_at IS NULL
             ORDER BY updated_at DESC",
        )?;

        let rows = stmt.query_map(params![&self.workspace_root], |row| {
            Ok(SessionListItem {
                id: row.get(0)?,
                title: row.get(1)?,
                status: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
                message_count: 0,
                acp_session_id: row.get(5)?,
                agent_cli: row.get(6)?,
                runtime_status: Default::default(),
                attention_state: Default::default(),
            })
        })?;

        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    pub fn list_archived_sessions(&self) -> Result<Vec<ArchivedSessionListItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.title, s.status, s.created_at, s.updated_at,
                    COALESCE(s.archived_at, ''),
                    COUNT(m.id) as msg_count,
                    s.acp_session_id, s.agent_cli, COALESCE(s.workspace_root, '')
             FROM sessions s
             LEFT JOIN messages m ON m.session_id = s.id
             WHERE s.archived_at IS NOT NULL
             GROUP BY s.id
             ORDER BY s.archived_at DESC, s.updated_at DESC",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(ArchivedSessionListItem {
                id: row.get(0)?,
                title: row.get(1)?,
                status: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
                archived_at: row.get(5)?,
                message_count: row.get(6)?,
                acp_session_id: row.get(7)?,
                agent_cli: row.get(8)?,
                workspace_root: row.get(9)?,
            })
        })?;

        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn archive_session(&self, id: &str) -> Result<()> {
        let now = now_iso();
        self.conn.execute(
            "UPDATE sessions SET archived_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }

    pub fn unarchive_session(&self, id: &str) -> Result<()> {
        let now = now_iso();
        self.conn.execute(
            "UPDATE sessions SET archived_at = NULL, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }

    pub fn delete_archived_session(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM sessions WHERE id = ?1 AND archived_at IS NOT NULL",
            params![id],
        )?;
        Ok(())
    }

    pub fn delete_all_archived_sessions(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM sessions WHERE archived_at IS NOT NULL", [])?;
        Ok(())
    }

    pub fn archive_workspace_sessions(&self) -> Result<()> {
        let now = now_iso();
        self.conn.execute(
            "UPDATE sessions
             SET archived_at = ?1, updated_at = ?1
             WHERE workspace_root = ?2 AND archived_at IS NULL",
            params![now, &self.workspace_root],
        )?;
        Ok(())
    }

    // ── Message persistence ──

    pub fn insert_message(
        &self,
        session_id: &str,
        id: &str,
        role: &str,
        body: &str,
        seq: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO messages (id, session_id, role, body, seq, created_at, is_steer) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
            params![id, session_id, role, body, seq, now_iso()],
        )?;
        self.touch_session(session_id)?;
        Ok(())
    }

    /// Insert a steer (追加指令) message. Same as [`insert_message`] but with
    /// `is_steer = 1` so the frontend collapse logic can identify it and skip
    /// the premature turn-boundary fold.
    pub fn insert_steer_message(
        &self,
        session_id: &str,
        id: &str,
        body: &str,
        seq: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO messages (id, session_id, role, body, seq, created_at, is_steer) VALUES (?1, ?2, 'User', ?3, ?4, ?5, 1)",
            params![id, session_id, body, seq, now_iso()],
        )?;
        self.touch_session(session_id)?;
        Ok(())
    }

    pub fn update_message_body(&self, id: &str, body: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET body = ?1 WHERE id = ?2",
            params![body, id],
        )?;
        Ok(())
    }

    pub fn delete_messages(&self, ids: &[String]) -> Result<()> {
        for id in ids {
            self.conn
                .execute("DELETE FROM messages WHERE id = ?1", params![id])?;
        }
        Ok(())
    }

    // ── Tool persistence ──

    pub fn insert_tool(&self, session_id: &str, tool: &ToolInvocation, seq: i64) -> Result<()> {
        let raw_output = tool
            .raw_output
            .as_deref()
            .map(|s| cap_string(s, MAX_RAW_OUTPUT_BYTES));
        let diff_paths = serde_json::to_string(&tool.diff_paths)?;
        let diff_previews = serde_json::to_string(&tool.diff_previews)?;
        self.conn.execute(
            "INSERT INTO tool_invocations (id, session_id, call_id, parent_call_id, name, kind, summary, status, raw_input, raw_output, error, diff_paths, diff_previews, seq, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(id) DO UPDATE SET
                call_id = excluded.call_id,
                parent_call_id = excluded.parent_call_id,
                name = excluded.name,
                kind = excluded.kind,
                summary = excluded.summary,
                status = excluded.status,
                raw_input = excluded.raw_input,
                raw_output = excluded.raw_output,
                error = excluded.error,
                diff_paths = excluded.diff_paths,
                diff_previews = excluded.diff_previews",
            params![
                tool.id.to_string(),
                session_id,
                tool.call_id,
                tool.parent_call_id,
                tool.name,
                tool.kind,
                tool.summary,
                format!("{:?}", tool.status),
                tool.raw_input,
                raw_output,
                tool.error,
                diff_paths,
                diff_previews,
                seq,
                now_iso(),
            ],
        )?;
        self.touch_session(session_id)?;
        Ok(())
    }

    pub fn update_tool(
        &self,
        id: &str,
        status: &str,
        raw_output: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let capped_output = raw_output.map(|s| cap_string(s, MAX_RAW_OUTPUT_BYTES));
        self.conn.execute(
            "UPDATE tool_invocations SET status = ?1, raw_output = ?2, error = ?3 WHERE id = ?4",
            params![status, capped_output, error, id],
        )?;
        Ok(())
    }

    // ── Usage events ──

    pub fn append_usage_event(
        &self,
        session_id: &str,
        event: &UsageEvent,
        fallback_model: Option<&str>,
        fallback_agent_cli: Option<&str>,
    ) -> Result<()> {
        let workspace_root = if self.workspace_root.is_empty() {
            self.conn
                .query_row(
                    "SELECT workspace_root FROM sessions WHERE id = ?1",
                    params![session_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten()
                .unwrap_or_default()
        } else {
            self.workspace_root.clone()
        };
        // Store `created_at` as epoch-seconds (the storage format) even when
        // `event.timestamp` is ISO-8601 (as the reducer now produces for
        // display). Parsing to epoch seconds keeps `CAST(created_at AS INTEGER)`
        // date filters and `ORDER BY created_at` working without a data
        // migration; epoch-seconds timestamps pass through unchanged.
        let created_at = event
            .timestamp
            .as_deref()
            .and_then(parse_instant_to_epoch_secs)
            .map(|secs| secs.to_string())
            .unwrap_or_else(now_iso);
        let model = event
            .model
            .as_deref()
            .or(fallback_model)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let agent_cli = event
            .agent_cli
            .as_deref()
            .or(fallback_agent_cli)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let provider = event
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        self.conn.execute(
            "INSERT INTO usage_events (
                id, session_id, workspace_root, agent_cli, provider, model, scope,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                reasoning_tokens, total_tokens, context_used_tokens, context_window_tokens,
                raw_json, created_at, latency_ms, ttft_ms, tokens_per_second
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                Uuid::new_v4().to_string(),
                session_id,
                workspace_root,
                agent_cli,
                provider,
                model,
                usage_scope_to_str(&event.scope),
                opt_i64(event.tokens.input_tokens),
                opt_i64(event.tokens.output_tokens),
                opt_i64(event.tokens.cache_read_tokens),
                opt_i64(event.tokens.cache_write_tokens),
                opt_i64(event.tokens.reasoning_tokens),
                opt_i64(event.tokens.total_tokens),
                opt_i64(event.context.used_tokens),
                opt_i64(event.context.window_tokens),
                event.raw_json,
                created_at,
                opt_i64(event.tokens.latency_ms),
                opt_i64(event.tokens.ttft_ms),
                event.tokens.tokens_per_second,
            ],
        )?;
        self.touch_session(session_id)?;
        Ok(())
    }

    pub fn load_session_usage_snapshot(&self, session_id: &str) -> Result<SessionUsageSnapshot> {
        let events = self.load_usage_events_for_session(session_id)?;
        Ok(session_usage_snapshot_from_events(&events))
    }

    pub fn query_usage_summary(
        &self,
        request: UsageSummaryRequest,
    ) -> Result<Vec<UsageSummaryRow>> {
        let from_epoch = request
            .from
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .and_then(parse_instant_to_epoch_secs);
        let mut events = self.load_usage_events_for_summary(&request)?;
        // Merge the carry-over baseline: the last SessionTotal per
        // (session, model) BEFORE the `from` boundary. The aggregate subtracts
        // this baseline from the in-range final SessionTotal so the result is
        // an increment, not a cumulative total folded from prior days.
        if let Some(from) = from_epoch {
            let baseline = self.load_usage_baseline_before(&request, from)?;
            events = merge_baseline_events(events, baseline);
        }
        Ok(usage_summary_from_events(&events, request.group_by, from_epoch))
    }

    /// P2: real daily usage series for the settings "每日用量" chart. Loads the
    /// same filtered event set as [`query_usage_summary`] (so workspace,
    /// archived, date-range and non-reporting-agent filters all apply), then
    /// buckets events by calendar day in the request's timezone (local time
    /// when `utc_offset_minutes` is set, UTC otherwise). Each day reports the
    /// **incremental** usage (last SessionTotal of the day minus the last
    /// SessionTotal before the day), so cumulative carry-over from prior
    /// days does not inflate the daily figure.
    pub fn query_usage_daily_series(
        &self,
        request: UsageSummaryRequest,
    ) -> Result<Vec<UsageDailyBucket>> {
        // Load with a widened lower bound (no `from`) so we can compute
        // per-day baselines; the daily series function itself splits into
        // per-day increments. The `to` bound still applies.
        let utc_offset_minutes = request.utc_offset_minutes;
        let baseline_request = UsageSummaryRequest {
            from: None,
            ..request.clone()
        };
        let events = self.load_usage_events_for_summary(&baseline_request)?;
        Ok(usage_daily_series_from_events(
            &events,
            request.from.as_deref(),
            utc_offset_minutes,
        ))
    }

    /// Count token-reporting usage events (`TurnDelta` + `SessionTotal`) in
    /// the request's date range. Unlike [`query_usage_summary`], this does NOT
    /// merge carry-over baseline events, so the count reflects only in-range
    /// requests. Used by the settings "24H REQ" card, which needs an accurate
    /// rolling-24h request count undiluted by pre-range `SessionTotal`
    /// baselines (those would otherwise be folded in by
    /// [`merge_baseline_events`] and counted by [`update_usage_summary_row`]).
    pub fn query_usage_request_count(
        &self,
        request: UsageSummaryRequest,
    ) -> Result<u64> {
        let events = self.load_usage_events_for_summary(&request)?;
        let count = events
            .iter()
            .filter(|event| {
                matches!(
                    event.scope,
                    UsageEventScope::TurnDelta | UsageEventScope::SessionTotal
                )
            })
            .count() as u64;
        Ok(count)
    }

    fn load_usage_events_for_session(&self, session_id: &str) -> Result<Vec<StoredUsageEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT u.session_id, s.title, u.workspace_root, u.agent_cli, u.provider, u.model, u.scope,
                    u.input_tokens, u.output_tokens, u.cache_read_tokens, u.cache_write_tokens,
                    u.reasoning_tokens, u.total_tokens, u.context_used_tokens, u.context_window_tokens,
                    u.created_at, u.latency_ms, u.ttft_ms, u.tokens_per_second
             FROM usage_events u
             LEFT JOIN sessions s ON s.id = u.session_id
             WHERE u.session_id = ?1
             ORDER BY u.created_at ASC",
        )?;
        let rows = stmt.query_map(params![session_id], stored_usage_event_from_row)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    fn load_usage_events_for_summary(
        &self,
        request: &UsageSummaryRequest,
    ) -> Result<Vec<StoredUsageEvent>> {
        let mut sql = String::from(
            "SELECT u.session_id, s.title, u.workspace_root, u.agent_cli, u.provider, u.model, u.scope,
                    u.input_tokens, u.output_tokens, u.cache_read_tokens, u.cache_write_tokens,
                    u.reasoning_tokens, u.total_tokens, u.context_used_tokens, u.context_window_tokens,
                    u.created_at, u.latency_ms, u.ttft_ms, u.tokens_per_second
             FROM usage_events u
             LEFT JOIN sessions s ON s.id = u.session_id
             WHERE 1 = 1",
        );
        let mut params_vec = Vec::<String>::new();
        // Exclude non-reporting third-party agents (e.g. CodeBuddy) from the
        // cross-session historical summary. They only ever emit empty
        // `ContextSnapshot` rows (no token breakdown) and would otherwise
        // surface as empty groups under "按智能体" or pollute "按模型". The
        // single-session snapshot path (`load_usage_events_for_session`) is
        // intentionally untouched, so live context occupancy in the dock
        // still works for those sessions. We filter on the joined session's
        // `agent_cli` first, falling back to the `usage_events` row's own
        // `agent_cli` (which `append_usage_event` populates from the same
        // source) so a missing session row is still excluded.
        sql.push_str(" AND COALESCE(s.agent_cli, u.agent_cli, '') != ?");
        params_vec.push(NON_REPORTING_AGENT_CLI.to_string());
        if !request.all_workspaces {
            let workspace_root = request
                .workspace_root
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
                .or_else(|| {
                    if self.workspace_root.is_empty() {
                        None
                    } else {
                        Some(self.workspace_root.clone())
                    }
                });
            if let Some(workspace_root) = workspace_root {
                sql.push_str(" AND u.workspace_root = ?");
                params_vec.push(workspace_root);
            }
        }
        if !request.include_archived {
            sql.push_str(" AND s.archived_at IS NULL");
        }
        if let Some(session_id) = request
            .session_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            sql.push_str(" AND u.session_id = ?");
            params_vec.push(session_id.to_string());
        }
        // Date bounds are compared as epoch seconds (numeric) rather than as
        // raw text. The desktop UI sends ISO-8601 UTC bounds (e.g. "2026-06-30T00:00:00.000Z")
        // while `usage_events.created_at` is stored as a decimal-seconds string
        // (e.g. "1780185600"); textual comparison breaks across formats. We
        // parse the bound with `parse_instant_to_epoch_secs` (which also accepts
        // raw decimal seconds for backward compatibility) and cast the stored
        // column to INTEGER so SQLite compares both sides as numbers.
        let from_epoch = request
            .from
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .and_then(parse_instant_to_epoch_secs);
        if let Some(from) = from_epoch {
            sql.push_str(" AND CAST(u.created_at AS INTEGER) >= ?");
            params_vec.push(from.to_string());
        }
        if let Some(to) = request
            .to
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .and_then(parse_instant_to_epoch_secs)
        {
            sql.push_str(" AND CAST(u.created_at AS INTEGER) <= ?");
            params_vec.push(to.to_string());
        }
        // Stable tiebreaker by primary key so events sharing the same
        // epoch-second timestamp (e.g. a SessionTotal + TurnDelta pair
        // emitted from one ACP frame) always come back in insertion order,
        // making aggregation deterministic across queries.
        sql.push_str(" ORDER BY u.created_at ASC, u.id ASC");

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs = params_vec.iter().map(|value| value as &dyn ToSql);
        let rows = stmt.query_map(params_from_iter(param_refs), stored_usage_event_from_row)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    /// Load the last `SessionTotal` event per `(session_id, model, provider,
    /// agent_cli)` strictly before the `from` epoch boundary. These are the
    /// carry-over baselines that [`compute_per_session_model_totals`] subtracts
    /// from the in-range final SessionTotal so the reported figure is an
    /// increment (today's consumption) rather than a cumulative total (which
    /// would fold in yesterday's usage). Applies the same workspace / archived /
    /// non-reporting-agent / session filters as the in-range query.
    fn load_usage_baseline_before(
        &self,
        request: &UsageSummaryRequest,
        from_epoch: i64,
    ) -> Result<Vec<StoredUsageEvent>> {
        // For each (session, model, provider, agent_cli), pick the latest
        // SessionTotal row before `from`. We rely on a correlated subquery to
        // select the single newest baseline row per group.
        let mut sql = String::from(
            "SELECT u.session_id, s.title, u.workspace_root, u.agent_cli, u.provider, u.model, u.scope,
                    u.input_tokens, u.output_tokens, u.cache_read_tokens, u.cache_write_tokens,
                    u.reasoning_tokens, u.total_tokens, u.context_used_tokens, u.context_window_tokens,
                    u.created_at, u.latency_ms, u.ttft_ms, u.tokens_per_second
             FROM usage_events u
             LEFT JOIN sessions s ON s.id = u.session_id
             WHERE u.scope = 'session_total'
               AND CAST(u.created_at AS INTEGER) < ?
               AND COALESCE(s.agent_cli, u.agent_cli, '') != ?",
        );
        let mut params_vec = vec![from_epoch.to_string(), NON_REPORTING_AGENT_CLI.to_string()];
        if !request.all_workspaces {
            if let Some(workspace_root) = request
                .workspace_root
                .as_deref()
                .filter(|v| !v.trim().is_empty())
                .map(str::to_string)
                .or_else(|| {
                    if self.workspace_root.is_empty() {
                        None
                    } else {
                        Some(self.workspace_root.clone())
                    }
                })
            {
                sql.push_str(" AND u.workspace_root = ?");
                params_vec.push(workspace_root);
            }
        }
        if !request.include_archived {
            sql.push_str(" AND s.archived_at IS NULL");
        }
        if let Some(session_id) = request
            .session_id
            .as_deref()
            .filter(|v| !v.trim().is_empty())
        {
            sql.push_str(" AND u.session_id = ?");
            params_vec.push(session_id.to_string());
        }
        // Keep only the newest baseline per (session, model, provider,
        // agent_cli) via a correlated subquery on `created_at, id`.
        sql.push_str(
            " AND NOT EXISTS (
                 SELECT 1 FROM usage_events u2
                 WHERE u2.session_id = u.session_id
                   AND COALESCE(u2.model, '') = COALESCE(u.model, '')
                   AND COALESCE(u2.provider, '') = COALESCE(u.provider, '')
                   AND COALESCE(u2.agent_cli, '') = COALESCE(u.agent_cli, '')
                   AND u2.scope = 'session_total'
                   AND CAST(u2.created_at AS INTEGER) < ?
                   AND (CAST(u2.created_at AS INTEGER), u2.id) > (CAST(u.created_at AS INTEGER), u.id)
             ) ORDER BY u.created_at ASC, u.id ASC",
        );
        params_vec.push(from_epoch.to_string());
        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs = params_vec.iter().map(|v| v as &dyn ToSql);
        let rows = stmt.query_map(params_from_iter(param_refs), stored_usage_event_from_row)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    // ── Session loading ──

    pub fn load_session(
        &self,
        id: &str,
    ) -> Result<(Vec<ChatMessage>, Vec<ToolInvocation>, Vec<TimelineItem>)> {
        let mut messages = Vec::new();
        let mut tools = Vec::new();

        // Load all timeline entries (messages + tools) ordered by seq
        #[derive(Debug)]
        enum Entry {
            Message {
                id: Uuid,
                role: String,
                body: String,
                created_at: String,
                is_steer: bool,
            },
            Tool {
                id: Uuid,
                call_id: String,
                parent_call_id: Option<String>,
                name: String,
                kind: String,
                summary: String,
                status: String,
                raw_input: Option<String>,
                raw_output: Option<String>,
                error: Option<String>,
                diff_paths: Vec<std::path::PathBuf>,
                diff_previews: Vec<ToolDiffPreview>,
            },
        }

        let mut entries: Vec<(i64, Entry)> = Vec::new();

        // Load messages
        {
            let mut stmt = self.conn.prepare(
                "SELECT id, role, body, seq, created_at, is_steer FROM messages WHERE session_id = ?1 ORDER BY seq",
            )?;
            let rows = stmt.query_map(params![id], |row| {
                let id_str: String = row.get(0)?;
                let is_steer: bool = row.get::<_, i64>(5)? != 0;
                Ok((
                    row.get::<_, i64>(3)?,
                    Entry::Message {
                        id: Uuid::parse_str(&id_str).unwrap_or_else(|_| Uuid::new_v4()),
                        role: row.get(1)?,
                        body: row.get(2)?,
                        created_at: row.get(4)?,
                        is_steer,
                    },
                ))
            })?;
            for row in rows {
                entries.push(row?);
            }
        }

        // Load tools
        {
            let mut stmt = self.conn.prepare(
                "SELECT id, call_id, parent_call_id, name, kind, summary, status, raw_input, raw_output, error, diff_paths, diff_previews, seq
                 FROM tool_invocations WHERE session_id = ?1 ORDER BY seq",
            )?;
            let rows = stmt.query_map(params![id], |row| {
                let id_str: String = row.get(0)?;
                let diff_paths_json: Option<String> = row.get(10)?;
                let diff_previews_json: Option<String> = row.get(11)?;
                Ok((
                    row.get::<_, i64>(12)?,
                    Entry::Tool {
                        id: Uuid::parse_str(&id_str).unwrap_or_else(|_| Uuid::new_v4()),
                        call_id: row.get(1)?,
                        parent_call_id: row.get(2)?,
                        name: row.get(3)?,
                        kind: row.get(4)?,
                        summary: row.get(5)?,
                        status: row.get(6)?,
                        raw_input: row.get(7)?,
                        raw_output: row.get(8)?,
                        error: row.get(9)?,
                        diff_paths: decode_json_vec(diff_paths_json.as_deref()),
                        diff_previews: decode_json_vec(diff_previews_json.as_deref()),
                    },
                ))
            })?;
            for row in rows {
                entries.push(row?);
            }
        }

        // Sort by seq to reconstruct timeline order
        entries.sort_by_key(|(seq, _)| *seq);

        let mut timeline = Vec::new();
        for (_seq, entry) in entries {
            match entry {
                Entry::Message {
                    id,
                    role,
                    body,
                    created_at,
                    is_steer,
                } => {
                    let role = match role.as_str() {
                        "User" => MessageRole::User,
                        "Assistant" => MessageRole::Assistant,
                        _ => MessageRole::System,
                    };
                    messages.push(ChatMessage {
                        id,
                        role,
                        body,
                        created_at,
                        is_steer,
                    });
                    timeline.push(TimelineItem::Message(id));
                }
                Entry::Tool {
                    id,
                    call_id,
                    parent_call_id,
                    name,
                    kind,
                    summary,
                    status,
                    raw_input,
                    raw_output,
                    error,
                    diff_paths,
                    diff_previews,
                    ..
                } => {
                    let status = match status.as_str() {
                        "Pending" => ToolStatus::Pending,
                        "Running" => ToolStatus::Running,
                        "Succeeded" => ToolStatus::Succeeded,
                        "Failed" => ToolStatus::Failed,
                        "Interrupted" => ToolStatus::Interrupted,
                        _ => ToolStatus::Succeeded,
                    };
                    tools.push(ToolInvocation {
                        id,
                        call_id,
                        parent_call_id,
                        name,
                        kind,
                        summary,
                        status,
                        is_subagent: false,
                        detail_text: String::new(),
                        logs: Vec::new(),
                        diff_paths,
                        diff_previews,
                        raw_input,
                        raw_output,
                        terminal_output: None,
                        error,
                        permission_options: Vec::new(),
                        permission_input: None,
                        permission_decision: None,
                        can_stop: false,
                        stop_kind: None,
                        stop_status: None,
                    });
                    timeline.push(TimelineItem::Tool(id));
                }
            }
        }

        Ok((messages, tools, timeline))
    }

    // ── Helpers ──

    fn touch_session(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![now_iso(), id],
        )?;
        Ok(())
    }

    /// Get the next sequence number for a session's timeline
    pub fn next_seq(&self, session_id: &str) -> Result<i64> {
        let msg_max: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        let tool_max: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM tool_invocations WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(msg_max.max(tool_max) + 1)
    }

    fn existing_file_change_path(
        &self,
        session_id: &str,
        normalized_path: &str,
    ) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM session_file_changes WHERE session_id = ?1")?;
        let rows = stmt.query_map(params![session_id], |row| row.get::<_, String>(0))?;
        for row in rows {
            let path = row?;
            if normalize_change_path(&path) == normalized_path {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    // ── File change persistence ──

    /// Upsert a file change record. Preserves existing `base_text` if the new value is None.
    pub fn upsert_file_change(
        &self,
        session_id: &str,
        path: &str,
        change_type: &str,
        base_text: Option<&str>,
        new_text: &str,
        added_lines: usize,
        removed_lines: usize,
    ) -> Result<()> {
        let normalized_path = normalize_change_path(path);
        let effective_path = self
            .existing_file_change_path(session_id, &normalized_path)?
            .unwrap_or_else(|| normalized_path.clone());

        // First try to fetch existing base_text so we don't overwrite it
        let existing_base: Option<String> = self
            .conn
            .query_row(
                "SELECT base_text FROM session_file_changes WHERE session_id = ?1 AND path = ?2",
                params![session_id, effective_path],
                |row| row.get(0),
            )
            .ok();

        // Preserve existing base_text: only use the new value if there's no existing one
        let effective_base = existing_base.as_deref().or(base_text);

        self.conn.execute(
            "INSERT INTO session_file_changes (session_id, path, change_type, base_text, new_text, added_lines, removed_lines, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(session_id, path) DO UPDATE SET
                 change_type = excluded.change_type,
                 base_text = COALESCE(session_file_changes.base_text, excluded.base_text),
                 new_text = excluded.new_text,
                 added_lines = excluded.added_lines,
                 removed_lines = excluded.removed_lines,
                 updated_at = excluded.updated_at",
            params![
                session_id,
                effective_path,
                change_type,
                effective_base,
                new_text,
                added_lines as i64,
                removed_lines as i64,
                now_iso(),
            ],
        )?;
        Ok(())
    }

    /// Replace all file changes for a session with the current in-memory snapshot.
    /// This keeps SQLite from resurrecting stale changes after a file is reverted.
    pub fn replace_file_changes(
        &self,
        session_id: &str,
        changes: &[SessionFileChange],
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM session_file_changes WHERE session_id = ?1",
            params![session_id],
        )?;

        for change in changes {
            let change_type = format!("{:?}", change.change_type);
            self.upsert_file_change(
                session_id,
                &change.path,
                &change_type,
                change.old_text.as_deref(),
                &change.new_text,
                change.added_lines,
                change.removed_lines,
            )?;
        }

        Ok(())
    }

    pub fn replace_review_file_changes(
        &self,
        session_id: &str,
        changes: &[SessionFileChange],
    ) -> Result<()> {
        self.replace_changes_in_table("session_review_file_changes", session_id, changes)
    }

    /// Load all file changes for a session, ordered by path.
    pub fn load_file_changes(&self, session_id: &str) -> Result<Vec<SessionFileChange>> {
        self.load_changes_from_table("session_file_changes", session_id)
    }

    pub fn load_review_file_changes(&self, session_id: &str) -> Result<Vec<SessionFileChange>> {
        self.load_changes_from_table("session_review_file_changes", session_id)
    }

    pub fn replace_turn_file_changes(
        &self,
        session_id: &str,
        message_id: &Uuid,
        changes: &[SessionFileChange],
    ) -> Result<()> {
        let message_id = message_id.to_string();
        self.conn.execute(
            "DELETE FROM session_turn_file_changes WHERE session_id = ?1 AND message_id = ?2",
            params![session_id, &message_id],
        )?;

        for change in changes {
            self.insert_turn_file_change(session_id, &message_id, change)?;
        }

        Ok(())
    }

    pub fn replace_all_turn_file_changes(
        &self,
        session_id: &str,
        turn_changes: &[TurnFileChanges],
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM session_turn_file_changes WHERE session_id = ?1",
            params![session_id],
        )?;

        for entry in turn_changes {
            let message_id = entry.message_id.to_string();
            for change in &entry.changes {
                self.insert_turn_file_change(session_id, &message_id, change)?;
            }
        }

        Ok(())
    }

    pub fn load_turn_file_changes(&self, session_id: &str) -> Result<Vec<TurnFileChanges>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.message_id, c.path, c.change_type, c.base_text, c.new_text, c.added_lines, c.removed_lines, c.updated_at
             FROM session_turn_file_changes c
             LEFT JOIN messages m ON m.id = c.message_id AND m.session_id = c.session_id
             WHERE c.session_id = ?1
             ORDER BY COALESCE(m.seq, 9223372036854775807), c.message_id, c.path",
        )?;

        let rows = stmt.query_map(params![session_id], |row| {
            let change_type_str: String = row.get(2)?;
            let change_type = match change_type_str.as_str() {
                "Created" => FileChangeType::Created,
                "Deleted" => FileChangeType::Deleted,
                _ => FileChangeType::Modified,
            };
            Ok((
                row.get::<_, String>(0)?,
                SessionFileChange {
                    path: row.get(1)?,
                    change_type,
                    old_text: row.get(3)?,
                    new_text: row.get(4)?,
                    added_lines: row.get::<_, i64>(5)? as usize,
                    removed_lines: row.get::<_, i64>(6)? as usize,
                    timestamp: row.get(7)?,
                },
            ))
        })?;

        let mut items: Vec<TurnFileChanges> = Vec::new();
        for row in rows {
            let (message_id, mut change) = row?;
            let Ok(message_id) = Uuid::parse_str(&message_id) else {
                continue;
            };
            change.path = normalize_change_path(&change.path);
            if let Some(entry) = items
                .iter_mut()
                .find(|entry| entry.message_id == message_id)
            {
                upsert_loaded_change(&mut entry.changes, change);
            } else {
                items.push(TurnFileChanges {
                    message_id,
                    changes: vec![change],
                });
            }
        }

        Ok(items)
    }

    fn insert_turn_file_change(
        &self,
        session_id: &str,
        message_id: &str,
        change: &SessionFileChange,
    ) -> Result<()> {
        let change_type = format!("{:?}", change.change_type);
        let normalized_path = normalize_change_path(&change.path);
        self.conn.execute(
            "INSERT INTO session_turn_file_changes (session_id, message_id, path, change_type, base_text, new_text, added_lines, removed_lines, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(session_id, message_id, path) DO UPDATE SET
                 change_type = excluded.change_type,
                 base_text = excluded.base_text,
                 new_text = excluded.new_text,
                 added_lines = excluded.added_lines,
                 removed_lines = excluded.removed_lines,
                 updated_at = excluded.updated_at",
            params![
                session_id,
                message_id,
                normalized_path,
                change_type,
                change.old_text.as_deref(),
                &change.new_text,
                change.added_lines as i64,
                change.removed_lines as i64,
                now_iso(),
            ],
        )?;
        Ok(())
    }

    pub fn upsert_change_set(&self, summary: &ChangeSetSummary) -> Result<()> {
        let session_id = summary.session_id.map(|id| id.to_string());
        let message_id = summary.message_id.map(|id| id.to_string());
        let source = change_set_source_to_str(&summary.source);
        let status = change_set_status_to_str(&summary.status);
        let now = now_iso();
        let updated_at = if summary.updated_at.is_empty() {
            now.clone()
        } else {
            summary.updated_at.clone()
        };

        self.conn.execute(
            "INSERT INTO change_sets
             (id, session_id, workspace_root, source, message_id, tool_call_id, owner_key, label, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                 session_id = excluded.session_id,
                 workspace_root = excluded.workspace_root,
                 source = excluded.source,
                 message_id = excluded.message_id,
                 tool_call_id = excluded.tool_call_id,
                 owner_key = excluded.owner_key,
                 label = excluded.label,
                 status = excluded.status,
                 updated_at = excluded.updated_at",
            params![
                &summary.id,
                session_id.as_deref(),
                &summary.workspace_root,
                source,
                message_id.as_deref(),
                summary.tool_call_id.as_deref(),
                summary.owner_key.as_deref(),
                &summary.label,
                status,
                now,
                updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn replace_change_set(
        &self,
        summary: &ChangeSetSummary,
        files: &[FileChangeRecord],
    ) -> Result<()> {
        if files.is_empty() {
            self.conn.execute(
                "DELETE FROM change_set_files WHERE change_set_id = ?1",
                params![&summary.id],
            )?;
            self.conn.execute(
                "DELETE FROM change_sets WHERE id = ?1",
                params![&summary.id],
            )?;
            return Ok(());
        }

        self.upsert_change_set(summary)?;
        self.conn.execute(
            "DELETE FROM change_set_files WHERE change_set_id = ?1",
            params![&summary.id],
        )?;
        for file in files {
            self.upsert_change_set_file(file)?;
        }
        Ok(())
    }

    pub fn upsert_change_set_file(&self, file: &FileChangeRecord) -> Result<()> {
        let change_type = format!("{:?}", file.change_type);
        let quality = diff_quality_to_str(&file.quality);
        let path = normalize_change_path(&file.path);
        let updated_at = if file.updated_at.is_empty() {
            now_iso()
        } else {
            file.updated_at.clone()
        };
        self.conn.execute(
            "INSERT INTO change_set_files
             (change_set_id, path, change_type, base_text, target_text, added_lines, removed_lines, quality, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(change_set_id, path) DO UPDATE SET
                 change_type = excluded.change_type,
                 base_text = excluded.base_text,
                 target_text = excluded.target_text,
                 added_lines = excluded.added_lines,
                 removed_lines = excluded.removed_lines,
                 quality = excluded.quality,
                 updated_at = excluded.updated_at",
            params![
                &file.change_set_id,
                path,
                change_type,
                file.old_text.as_deref(),
                file.new_text.as_deref(),
                file.added_lines as i64,
                file.removed_lines as i64,
                quality,
                updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_change_sets(
        &self,
        session_id: Option<&str>,
        source: Option<ChangeSetSource>,
    ) -> Result<Vec<ChangeSetSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT
                 cs.id,
                 cs.source,
                 cs.session_id,
                 cs.workspace_root,
                 cs.message_id,
                 cs.tool_call_id,
                 cs.owner_key,
                 cs.label,
                 COALESCE(SUM(f.added_lines), 0) AS added_lines,
                 COALESCE(SUM(f.removed_lines), 0) AS removed_lines,
                 COUNT(f.path) AS file_count,
                 cs.updated_at,
                 cs.status
             FROM change_sets cs
             LEFT JOIN change_set_files f ON f.change_set_id = cs.id
             WHERE cs.workspace_root = ?1
             GROUP BY cs.id
             ORDER BY cs.updated_at DESC",
        )?;

        let rows = stmt.query_map(params![&self.workspace_root], |row| {
            let source_str: String = row.get(1)?;
            let status_str: String = row.get(12)?;
            let session_id_str: Option<String> = row.get(2)?;
            let message_id_str: Option<String> = row.get(4)?;
            Ok(ChangeSetSummary {
                id: row.get(0)?,
                source: change_set_source_from_str(&source_str),
                session_id: session_id_str
                    .as_deref()
                    .and_then(|value| Uuid::parse_str(value).ok()),
                workspace_root: row.get(3)?,
                message_id: message_id_str
                    .as_deref()
                    .and_then(|value| Uuid::parse_str(value).ok()),
                tool_call_id: row.get(5)?,
                owner_key: row.get(6)?,
                label: row.get(7)?,
                added_lines: row.get::<_, i64>(8)? as usize,
                removed_lines: row.get::<_, i64>(9)? as usize,
                file_count: row.get::<_, i64>(10)? as usize,
                updated_at: row.get(11)?,
                status: change_set_status_from_str(&status_str),
            })
        })?;

        let mut summaries = Vec::new();
        for row in rows {
            let summary = row?;
            let session_matches = session_id.is_none_or(|expected| {
                summary.session_id.map(|id| id.to_string()).as_deref() == Some(expected)
                    || self
                        .change_set_session_id(&summary.id)
                        .ok()
                        .flatten()
                        .as_deref()
                        == Some(expected)
            });
            let source_matches = source
                .as_ref()
                .is_none_or(|expected| &summary.source == expected);
            if session_matches && source_matches {
                summaries.push(summary);
            }
        }
        Ok(summaries)
    }

    pub fn list_change_sets_with_legacy(
        &self,
        session_id: &str,
        source: Option<ChangeSetSource>,
    ) -> Result<Vec<ChangeSetSummary>> {
        let mut summaries = self.list_change_sets(Some(session_id), source.clone())?;
        let mut legacy = self.load_legacy_change_set_summaries(session_id)?;
        if let Some(source) = source {
            legacy.retain(|summary| summary.source == source);
        }
        summaries.extend(legacy);
        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
        Ok(summaries)
    }

    pub fn list_change_set_files(&self, change_set_id: &str) -> Result<Vec<FileChangeSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT change_set_id, path, change_type, added_lines, removed_lines, quality, updated_at
             FROM change_set_files
             WHERE change_set_id = ?1
             ORDER BY path",
        )?;
        let rows = stmt.query_map(params![change_set_id], |row| {
            let change_type_str: String = row.get(2)?;
            let quality_str: String = row.get(5)?;
            Ok(FileChangeSummary {
                change_set_id: row.get(0)?,
                path: normalize_change_path(&row.get::<_, String>(1)?),
                change_type: file_change_type_from_str(&change_type_str),
                added_lines: row.get::<_, i64>(3)? as usize,
                removed_lines: row.get::<_, i64>(4)? as usize,
                quality: diff_quality_from_str(&quality_str),
                updated_at: row.get(6)?,
            })
        })?;

        let mut files = Vec::new();
        for row in rows {
            files.push(row?);
        }
        Ok(files)
    }

    pub fn list_change_set_files_with_legacy(
        &self,
        change_set_id: &str,
    ) -> Result<Vec<FileChangeSummary>> {
        if let Some(records) = self.load_legacy_change_set_records(change_set_id)? {
            return Ok(records.iter().map(file_summary_from_record).collect());
        }
        self.list_change_set_files(change_set_id)
    }

    pub fn load_change_set_file_diff(
        &self,
        change_set_id: &str,
        path: &str,
    ) -> Result<Option<FileChangeRecord>> {
        let normalized_path = normalize_change_path(path);
        self.conn
            .query_row(
                "SELECT change_set_id, path, change_type, base_text, target_text, added_lines, removed_lines, quality, updated_at
                 FROM change_set_files
                 WHERE change_set_id = ?1 AND path = ?2",
                params![change_set_id, normalized_path],
                |row| {
                    let change_type_str: String = row.get(2)?;
                    let quality_str: String = row.get(7)?;
                    Ok(FileChangeRecord {
                        change_set_id: row.get(0)?,
                        path: normalize_change_path(&row.get::<_, String>(1)?),
                        change_type: file_change_type_from_str(&change_type_str),
                        old_text: row.get(3)?,
                        new_text: row.get(4)?,
                        added_lines: row.get::<_, i64>(5)? as usize,
                        removed_lines: row.get::<_, i64>(6)? as usize,
                        quality: diff_quality_from_str(&quality_str),
                        updated_at: row.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn load_change_set_file_diff_with_legacy(
        &self,
        change_set_id: &str,
        path: &str,
    ) -> Result<Option<FileChangeRecord>> {
        if let Some(records) = self.load_legacy_change_set_records(change_set_id)? {
            let normalized = normalize_change_path(path);
            return Ok(records
                .into_iter()
                .find(|record| normalize_change_path(&record.path) == normalized));
        }
        self.load_change_set_file_diff(change_set_id, path)
    }

    fn change_set_session_id(&self, change_set_id: &str) -> Result<Option<String>> {
        let value = self
            .conn
            .query_row(
                "SELECT session_id FROM change_sets WHERE id = ?1",
                params![change_set_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.flatten())
    }

    fn load_legacy_change_set_summaries(&self, session_id: &str) -> Result<Vec<ChangeSetSummary>> {
        let mut summaries = Vec::new();

        let conversation = self.load_file_changes(session_id)?;
        if !conversation.is_empty() {
            let id = legacy_agent_conversation_id(session_id);
            let records = legacy_records_from_session_changes(&id, conversation);
            summaries.push(summarize_change_records(
                id,
                ChangeSetSource::AgentConversation,
                session_id,
                None,
                "整体对话（旧数据）",
                ChangeSetStatus::LegacyIncomplete,
                &self.workspace_root,
                &records,
            ));
        }

        let recent = self.load_review_file_changes(session_id)?;
        if !recent.is_empty() {
            let id = legacy_agent_recent_id(session_id);
            let records = legacy_records_from_session_changes(&id, recent);
            summaries.push(summarize_change_records(
                id,
                ChangeSetSource::AgentTurn,
                session_id,
                None,
                "最近对话（旧数据）",
                ChangeSetStatus::LegacyIncomplete,
                &self.workspace_root,
                &records,
            ));
        }

        for entry in self.load_turn_file_changes(session_id)? {
            if entry.changes.is_empty() {
                continue;
            }
            let id = legacy_agent_turn_id(session_id, &entry.message_id);
            let records = legacy_records_from_session_changes(&id, entry.changes);
            summaries.push(summarize_change_records(
                id,
                ChangeSetSource::AgentTurn,
                session_id,
                Some(entry.message_id),
                "历史对话（旧数据）",
                ChangeSetStatus::LegacyIncomplete,
                &self.workspace_root,
                &records,
            ));
        }

        Ok(summaries)
    }

    fn load_legacy_change_set_records(
        &self,
        change_set_id: &str,
    ) -> Result<Option<Vec<FileChangeRecord>>> {
        if let Some(session_id) = change_set_id.strip_prefix(LEGACY_AGENT_CONVERSATION_PREFIX) {
            let records = legacy_records_from_session_changes(
                change_set_id,
                self.load_file_changes(session_id)?,
            );
            return Ok(Some(records));
        }
        if let Some(session_id) = change_set_id.strip_prefix(LEGACY_AGENT_RECENT_PREFIX) {
            let records = legacy_records_from_session_changes(
                change_set_id,
                self.load_review_file_changes(session_id)?,
            );
            return Ok(Some(records));
        }
        if let Some(rest) = change_set_id.strip_prefix(LEGACY_AGENT_TURN_PREFIX)
            && let Some((session_id, message_id)) = rest.split_once(':')
            && let Ok(message_id) = Uuid::parse_str(message_id)
        {
            let records = self
                .load_turn_file_changes(session_id)?
                .into_iter()
                .find(|entry| entry.message_id == message_id)
                .map(|entry| legacy_records_from_session_changes(change_set_id, entry.changes))
                .unwrap_or_default();
            return Ok(Some(records));
        }
        Ok(None)
    }

    fn replace_changes_in_table(
        &self,
        table: &str,
        session_id: &str,
        changes: &[SessionFileChange],
    ) -> Result<()> {
        let delete_sql = format!("DELETE FROM {table} WHERE session_id = ?1");
        self.conn.execute(&delete_sql, params![session_id])?;

        for change in changes {
            let change_type = format!("{:?}", change.change_type);
            self.upsert_change_in_table(
                table,
                session_id,
                &change.path,
                &change_type,
                change.old_text.as_deref(),
                &change.new_text,
                change.added_lines,
                change.removed_lines,
            )?;
        }

        Ok(())
    }

    fn upsert_change_in_table(
        &self,
        table: &str,
        session_id: &str,
        path: &str,
        change_type: &str,
        base_text: Option<&str>,
        new_text: &str,
        added_lines: usize,
        removed_lines: usize,
    ) -> Result<()> {
        let normalized_path = normalize_change_path(path);
        let existing_sql =
            format!("SELECT base_text FROM {table} WHERE session_id = ?1 AND path = ?2");
        let existing_base: Option<String> = self
            .conn
            .query_row(
                &existing_sql,
                params![session_id, &normalized_path],
                |row| row.get(0),
            )
            .ok();
        let effective_base = existing_base.as_deref().or(base_text);
        let insert_sql = format!(
            "INSERT INTO {table} (session_id, path, change_type, base_text, new_text, added_lines, removed_lines, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(session_id, path) DO UPDATE SET
                 change_type = excluded.change_type,
                 base_text = COALESCE({table}.base_text, excluded.base_text),
                 new_text = excluded.new_text,
                 added_lines = excluded.added_lines,
                 removed_lines = excluded.removed_lines,
                 updated_at = excluded.updated_at"
        );
        self.conn.execute(
            &insert_sql,
            params![
                session_id,
                normalized_path,
                change_type,
                effective_base,
                new_text,
                added_lines as i64,
                removed_lines as i64,
                now_iso(),
            ],
        )?;
        Ok(())
    }

    fn load_changes_from_table(
        &self,
        table: &str,
        session_id: &str,
    ) -> Result<Vec<SessionFileChange>> {
        let sql = format!(
            "SELECT path, change_type, base_text, new_text, added_lines, removed_lines, updated_at
             FROM {table} WHERE session_id = ?1 ORDER BY path"
        );
        let mut stmt = self.conn.prepare(&sql)?;

        let rows = stmt.query_map(params![session_id], |row| {
            let change_type_str: String = row.get(1)?;
            let change_type = match change_type_str.as_str() {
                "Created" => FileChangeType::Created,
                "Deleted" => FileChangeType::Deleted,
                _ => FileChangeType::Modified,
            };
            Ok(SessionFileChange {
                path: row.get(0)?,
                change_type,
                old_text: row.get(2)?,
                new_text: row.get(3)?,
                added_lines: row.get::<_, i64>(4)? as usize,
                removed_lines: row.get::<_, i64>(5)? as usize,
                timestamp: row.get(6)?,
            })
        })?;

        let mut items: Vec<SessionFileChange> = Vec::new();
        for row in rows {
            let mut item = row?;
            item.path = normalize_change_path(&item.path);
            let normalized = normalize_change_path(&item.path);
            if let Some(existing) = items
                .iter_mut()
                .find(|change| normalize_change_path(&change.path) == normalized)
            {
                if item.new_text.len() >= existing.new_text.len()
                    || item.timestamp >= existing.timestamp
                {
                    *existing = item;
                }
            } else {
                items.push(item);
            }
        }
        Ok(items)
    }
}

fn stored_usage_event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredUsageEvent> {
    let scope: String = row.get(6)?;
    let created_at: String = row.get(15)?;
    Ok(StoredUsageEvent {
        session_id: row.get(0)?,
        session_title: row.get(1)?,
        workspace_root: row.get(2)?,
        agent_cli: row.get(3)?,
        provider: row.get(4)?,
        model: row.get(5)?,
        scope: usage_scope_from_str(&scope),
        tokens: UsageTokenBreakdown {
            input_tokens: opt_u64(row.get::<_, Option<i64>>(7)?),
            output_tokens: opt_u64(row.get::<_, Option<i64>>(8)?),
            cache_read_tokens: opt_u64(row.get::<_, Option<i64>>(9)?),
            cache_write_tokens: opt_u64(row.get::<_, Option<i64>>(10)?),
            reasoning_tokens: opt_u64(row.get::<_, Option<i64>>(11)?),
            total_tokens: opt_u64(row.get::<_, Option<i64>>(12)?),
            latency_ms: opt_u64(row.get::<_, Option<i64>>(16)?),
            ttft_ms: opt_u64(row.get::<_, Option<i64>>(17)?),
            tokens_per_second: row.get::<_, Option<f64>>(18)?,
        },
        context: UsageContextSnapshot {
            used_tokens: opt_u64(row.get::<_, Option<i64>>(13)?),
            window_tokens: opt_u64(row.get::<_, Option<i64>>(14)?),
            // Display boundary: normalize the epoch-seconds storage value to
            // ISO-8601 UTC so the dock shows a readable time. Live events
            // already carry ISO via the reducer's `now_iso_utc`.
            updated_at: Some(instant_to_iso_utc(&created_at)),
        },
        created_at,
    })
}

fn session_usage_snapshot_from_events(events: &[StoredUsageEvent]) -> SessionUsageSnapshot {
    let mut snapshot = SessionUsageSnapshot::default();
    let mut saw_session_total = false;
    let mut saw_turn_delta = false;
    for event in events {
        if event.context.used_tokens.is_some() {
            snapshot.context.used_tokens = event.context.used_tokens;
        }
        if event.context.window_tokens.is_some() {
            snapshot.context.window_tokens = event.context.window_tokens;
        }
        if event.context.updated_at.is_some() {
            snapshot.context.updated_at = event.context.updated_at.clone();
        }

        match event.scope {
            UsageEventScope::TurnDelta => {
                snapshot.current_turn = event.tokens.clone();
                // Only accumulate into `session_total` when no SessionTotal
                // event has been observed for this session. When a
                // SessionTotal row is present it is authoritative and the
                // per-turn delta is already folded into it.
                if !saw_session_total && has_usage_tokens(&event.tokens) {
                    add_usage_tokens(&mut snapshot.session_total, &event.tokens);
                }
                saw_turn_delta = true;
            }
            UsageEventScope::SessionTotal => {
                if has_usage_tokens(&event.tokens) {
                    snapshot.session_total = event.tokens.clone();
                    saw_session_total = true;
                }
            }
            // `ContextSnapshot` only carries context-window occupancy; its
            // token breakdown is ignored so occupancy is never confused with
            // consumption. Context peak is already updated by
            // `update_usage_summary_row`.
            UsageEventScope::ContextSnapshot => {}
        }
        // P7: pass an explicit model key instead of relying on the
        // `explicit_key = None` fallback, so the single-session snapshot and
        // the cross-session summary build the key through the same helper.
        let model_key = usage_model_key_from_fields(
            event.model.as_deref(),
            event.provider.as_deref(),
            event.agent_cli.as_deref(),
        );
        update_usage_summary_row(
            &mut snapshot.by_model,
            Some(&model_key),
            event.model.clone().or_else(|| Some("Unknown model".into())),
            event.model.clone(),
            event.provider.clone(),
            event.agent_cli.clone(),
            None,
            None,
            event,
            true,
        );
    }

    // Backward-compat: rows persisted by older Kodex builds (where codex-acp
    // mislabelled its single-total payload as `context_snapshot`) carry only
    // `total_tokens`. When no SessionTotal or TurnDelta events were persisted
    // for this session, surface that total as a best-effort session total so
    // historical sessions still display a non-zero figure on reload. New
    // sessions always produce SessionTotal / TurnDelta events and bypass
    // this path.
    if !saw_session_total
        && !saw_turn_delta
        && let Some(total) = events
            .iter()
            .rev()
            .find_map(|e| matches!(e.scope, UsageEventScope::ContextSnapshot)
                .then(|| e.tokens.total_tokens)
                .flatten())
    {
        // P6: this branch only fires for sessions persisted by older Kodex
        // builds that mislabelled a single-total payload as
        // `context_snapshot`. The surfaced total may be stale; emit a debug
        // line so the fallback is observable while those legacy rows still
        // exist (a one-time migration to relabel them would let us drop this
        // branch entirely).
        let sid = events
            .first()
            .map(|event| event.session_id.as_str())
            .unwrap_or("?");
        eprintln!(
            "[kodex/usage] session {sid} has only a legacy context_snapshot total ({total}); \
             no SessionTotal/TurnDelta events were persisted — figure may be stale"
        );
        snapshot.session_total.total_tokens = Some(total);
    }

    snapshot.has_session_total = saw_session_total;

    snapshot
}

/// Per-(session, model) accumulator used by [`compute_per_session_model_totals`].
/// `has_session_total` and `session_total` are tracked **per session** here,
/// not per group row — this is the key difference from the old per-group
/// `UsageModelSummary::has_session_total` flag which broke when multiple
/// sessions shared a model group.
///
/// When a `from` boundary is supplied, `baseline` holds the last SessionTotal
/// seen BEFORE that boundary (the cumulative consumption carried over from
/// prior days). The effective in-range total is then
/// `final - baseline` (an increment), not the cumulative `final`.
#[derive(Default)]
struct SessionModelTotalState {
    has_session_total: bool,
    session_total: UsageTokenBreakdown,
    turn_delta_sum: UsageTokenBreakdown,
    saw_turn_delta: bool,
    /// Legacy fallback: sessions persisted by older Kodex builds may only have
    /// `ContextSnapshot` rows carrying `total_tokens`. Surfaced when no
    /// `SessionTotal` / `TurnDelta` events exist for the (session, model) pair.
    legacy_total: Option<u64>,
    /// Last `SessionTotal` seen strictly before the `from` boundary. Used to
    /// subtract carry-over consumption when reporting an in-range increment.
    baseline: Option<UsageTokenBreakdown>,
    /// True once any event at-or-after the `from` boundary was observed.
    /// When false the (session, model) pair had no in-range activity and must
    /// not contribute tokens, even if a baseline exists.
    saw_in_range: bool,
}

/// Pre-compute the effective token total for each `(session_id, model_key)`
/// pair. For each pair:
/// - If any in-range `TurnDelta` events exist, they are **summed** and preferred.
///   `TurnDelta` is request-scoped and model-accurate (stamped with the model
///   active when that request completed).
/// - Else if any in-range `SessionTotal` event exists, the **last** one's
///   tokens are used as a fallback, minus the last pre-range `SessionTotal`
///   baseline so the result is an increment. `SessionTotal` is cumulative for
///   the whole session and may be mis-attributed after a mid-session model
///   switch, so it is only used when no `TurnDelta` rows exist.
/// - If neither exists but `ContextSnapshot` rows carry a legacy
///   `total_tokens`, that is surfaced as a best-effort fallback.
///
/// When `from_epoch` is supplied, the result is an **increment**: the last
/// Merge carry-over baseline events into the in-range event stream. Baselines
/// (loaded by [`SessionStore::load_usage_baseline_before`]) carry timestamps
/// strictly before `from`, so when [`compute_per_session_model_totals`] splits
/// events at the `from` boundary the baselines populate the pre-range
/// `baseline` field while in-range events populate the final SessionTotal.
/// The merged vec stays sorted ascending by `(created_at, id)`.
fn merge_baseline_events(
    mut events: Vec<StoredUsageEvent>,
    baseline: Vec<StoredUsageEvent>,
) -> Vec<StoredUsageEvent> {
    if baseline.is_empty() {
        return events;
    }
    if events.is_empty() {
        return baseline;
    }
    events.extend(baseline);
    events.sort_by(|a, b| {
        let a_secs = parse_instant_to_epoch_secs(&a.created_at).unwrap_or(0);
        let b_secs = parse_instant_to_epoch_secs(&b.created_at).unwrap_or(0);
        a_secs
            .cmp(&b_secs)
            .then_with(|| a.session_id.cmp(&b.session_id))
            .then_with(|| a.model.cmp(&b.model))
    });
    events
}

/// in-range SessionTotal minus the last pre-range SessionTotal (the baseline
/// carried over from prior days). This prevents yesterday's cumulative
/// consumption from being folded into "today". Events are assumed sorted
/// ascending by `created_at` so "last" is well-defined.
fn compute_per_session_model_totals<'a, I: IntoIterator<Item = &'a StoredUsageEvent>>(
    events: I,
    from_epoch: Option<i64>,
) -> HashMap<(String, String), UsageTokenBreakdown> {
    let mut state: HashMap<(String, String), SessionModelTotalState> = HashMap::new();
    for event in events {
        let model_key = usage_model_key_from_fields(
            event.model.as_deref(),
            event.provider.as_deref(),
            event.agent_cli.as_deref(),
        );
        let event_secs = parse_instant_to_epoch_secs(&event.created_at).unwrap_or(0);
        let in_range = from_epoch.map_or(true, |from| event_secs >= from);
        let entry = state
            .entry((event.session_id.clone(), model_key))
            .or_default();
        match event.scope {
            UsageEventScope::SessionTotal if has_usage_tokens(&event.tokens) => {
                if !in_range {
                    // Track the carry-over baseline (pre-range cumulative).
                    entry.baseline = Some(event.tokens.clone());
                } else {
                    entry.has_session_total = true;
                    entry.session_total = event.tokens.clone();
                    entry.saw_in_range = true;
                }
            }
            // Always accumulate TurnDeltas (even after a SessionTotal has been
            // seen). Historical summaries prefer the TurnDelta sum because it
            // is request-scoped and model-accurate; SessionTotal is only used
            // as a fallback when no TurnDelta rows exist for the pair.
            UsageEventScope::TurnDelta => {
                if in_range {
                    entry.saw_turn_delta = true;
                    add_usage_tokens(&mut entry.turn_delta_sum, &event.tokens);
                    entry.saw_in_range = true;
                }
            }
            UsageEventScope::ContextSnapshot => {
                if let Some(total) = event.tokens.total_tokens {
                    entry.legacy_total = Some(total);
                }
            }
            _ => {}
        }
    }
    state
        .into_iter()
        .filter(|(_, st)| st.saw_in_range)
        .map(|(pair, st)| {
            // Prefer request-scoped TurnDeltas when available. SessionTotal is
            // only a fallback for older/partial streams that never emitted
            // TurnDelta rows (and may mis-attribute after a model switch).
            let total = if st.saw_turn_delta {
                st.turn_delta_sum
            } else if st.has_session_total {
                // Subtract the carry-over baseline so the result is the
                // in-range increment, not the cumulative total.
                if let Some(baseline) = &st.baseline {
                    subtract_usage_tokens(&st.session_total, baseline)
                } else {
                    st.session_total
                }
            } else if let Some(legacy) = st.legacy_total {
                UsageTokenBreakdown {
                    total_tokens: Some(legacy),
                    ..Default::default()
                }
            } else {
                UsageTokenBreakdown::default()
            };
            (pair, total)
        })
        .collect()
}

fn usage_summary_from_events(
    events: &[StoredUsageEvent],
    group_by: UsageSummaryGroupBy,
    from_epoch: Option<i64>,
) -> Vec<UsageSummaryRow> {
    // Pre-compute per-(session, model) effective token totals. The
    // SessionTotal-overwrites / TurnDelta-accumulates rule applies per
    // (session, model), NOT per group — tracking it per group would break
    // when multiple sessions share a model (one session's SessionTotal would
    // suppress another session's TurnDelta, and SessionTotal would overwrite
    // instead of sum across sessions).
    let per_session_model = compute_per_session_model_totals(events, from_epoch);
    let mut contributed: HashSet<(String, String)> = HashSet::new();

    let mut rows = Vec::<UsageSummaryRow>::new();
    let mut sessions_by_key = HashMap::<String, HashSet<String>>::new();
    for event in events {
        // Carry-over baselines loaded by `merge_baseline_events` keep
        // timestamps strictly before `from`. They exist only so
        // `compute_per_session_model_totals` can subtract prior cumulative
        // SessionTotals. They must NOT create summary rows, inflate
        // request/event counts, or surface models that had no in-range
        // activity (e.g. a model only used yesterday should not appear
        // under "今天" with request_count=1).
        let event_secs = parse_instant_to_epoch_secs(&event.created_at).unwrap_or(0);
        if from_epoch.is_some_and(|from| event_secs < from) {
            continue;
        }

        let (key, label, model, provider, agent_cli, session_id, workspace_root) = match group_by {
            UsageSummaryGroupBy::Model => {
                let label = event
                    .model
                    .clone()
                    .or_else(|| event.agent_cli.clone())
                    .unwrap_or_else(|| "Unknown model".into());
                (
                    format!(
                        "model:{}:{}:{}",
                        event.model.as_deref().unwrap_or(""),
                        event.provider.as_deref().unwrap_or(""),
                        event.agent_cli.as_deref().unwrap_or("")
                    ),
                    label.clone(),
                    event.model.clone(),
                    event.provider.clone(),
                    event.agent_cli.clone(),
                    None,
                    None,
                )
            }
            UsageSummaryGroupBy::Agent => {
                let label = event
                    .agent_cli
                    .clone()
                    .unwrap_or_else(|| "Unknown agent".into());
                (
                    format!("agent:{}", event.agent_cli.as_deref().unwrap_or("")),
                    label,
                    None,
                    event.provider.clone(),
                    event.agent_cli.clone(),
                    None,
                    None,
                )
            }
            UsageSummaryGroupBy::Workspace => {
                let label = event.workspace_root.clone();
                (
                    format!("workspace:{}", event.workspace_root),
                    label.clone(),
                    None,
                    None,
                    None,
                    None,
                    Some(label),
                )
            }
            UsageSummaryGroupBy::Session => {
                let label = event
                    .session_title
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| event.session_id.clone());
                (
                    format!("session:{}", event.session_id),
                    label,
                    event.model.clone(),
                    event.provider.clone(),
                    event.agent_cli.clone(),
                    Some(event.session_id.clone()),
                    Some(event.workspace_root.clone()),
                )
            }
        };
        sessions_by_key
            .entry(key.clone())
            .or_default()
            .insert(event.session_id.clone());
        update_usage_summary_row(
            &mut rows,
            Some(&key),
            Some(label),
            model,
            provider,
            agent_cli,
            session_id,
            workspace_root,
            event,
            false,
        );

        // Add this (session, model) pair's pre-computed effective total to
        // the group row exactly once. Subsequent events for the same pair
        // only update metadata (event_count, request_count, latest_at,
        // context_peak) handled above.
        let event_model_key = usage_model_key_from_fields(
            event.model.as_deref(),
            event.provider.as_deref(),
            event.agent_cli.as_deref(),
        );
        let sm_pair = (event.session_id.clone(), event_model_key);
        if contributed.insert(sm_pair.clone()) {
            if let Some(total) = per_session_model.get(&sm_pair) {
                if let Some(index) = rows.iter().position(|row| usage_row_matches_key(row, &key)) {
                    add_usage_tokens(&mut rows[index].tokens, total);
                }
            }
        }
    }

    for row in &mut rows {
        let key = usage_summary_key(row, &group_by);
        row.session_count = sessions_by_key
            .get(&key)
            .map(|sessions| sessions.len() as u64)
            .unwrap_or(row.session_count);
    }
    rows.sort_by(|a, b| {
        usage_total_tokens(&b.tokens)
            .cmp(&usage_total_tokens(&a.tokens))
            .then_with(|| b.latest_at.cmp(&a.latest_at))
            .then_with(|| a.label.cmp(&b.label))
    });
    rows
}

/// P2: bucket usage events into calendar days (local time when
/// `utc_offset_minutes` is set, UTC otherwise). Each bucket carries a
/// per-model breakdown (reusing [`update_usage_summary_row`] so the
/// `SessionTotal`-overwrites / `TurnDelta`-accumulates rules apply per day)
/// and a day-total breakdown whose `total_tokens` is the sum of each
/// per-model row's effective total. Days with no parseable timestamp are
/// skipped. `BTreeMap` keeps buckets sorted by date ascending.
fn usage_daily_series_from_events(
    events: &[StoredUsageEvent],
    from: Option<&str>,
    utc_offset_minutes: Option<i32>,
) -> Vec<UsageDailyBucket> {
    // Parse the `from` bound so days strictly before it are skipped entirely.
    let from_epoch = from
        .filter(|v| !v.trim().is_empty())
        .and_then(parse_instant_to_epoch_secs);
    let mut events_by_date: BTreeMap<String, Vec<&StoredUsageEvent>> = BTreeMap::new();
    for event in events {
        let Some(date) = instant_to_date_local(&event.created_at, utc_offset_minutes) else {
            continue;
        };
        events_by_date.entry(date).or_default().push(event);
    }
    // Compute the last SessionTotal per (session, model) seen before the first
    // in-range day. This is the running baseline that each day's increment
    // subtracts from. We process days ascending and advance the baseline as we
    // encounter new SessionTotals, so each day subtracts the carry-over from
    // all prior days (not just the immediately preceding one).
    let mut running_baseline: HashMap<(String, String), UsageTokenBreakdown> = HashMap::new();
    let mut buckets = Vec::with_capacity(events_by_date.len());
    for (date, day_events) in &events_by_date {
        // Compute the epoch boundary for the start of this day so
        // `compute_per_session_model_totals` can split baseline vs in-day.
        let day_start_epoch =
            epoch_start_of_date_local(date, utc_offset_minutes).unwrap_or(0);
        // Skip days entirely before the `from` bound (they only served to
        // build the running baseline).
        if let Some(from) = from_epoch {
            let day_end_epoch = day_start_epoch.saturating_add(86_399);
            if day_end_epoch < from {
                advance_baseline(&mut running_baseline, day_events);
                continue;
            }
        }
        // Pre-compute per-(session, model) effective totals for this day,
        // subtracting the running baseline (last SessionTotal before this day).
        let per_session_model = compute_daily_day_totals(
            day_events,
            &running_baseline,
            day_start_epoch,
        );
        // Advance the baseline for the NEXT day with this day's SessionTotals.
        advance_baseline(&mut running_baseline, day_events);
        let mut contributed: HashSet<(String, String)> = HashSet::new();

        let mut by_model = Vec::<UsageModelSummary>::new();
        for event in day_events {
            let label = event
                .model
                .clone()
                .or_else(|| event.agent_cli.clone())
                .unwrap_or_else(|| "Unknown model".into());
            // `explicit_key = None` + `session_id = None` keys rows by
            // `model:provider:agent_cli` (see `usage_summary_key_by_values`),
            // collapsing same-model events within the day.
            update_usage_summary_row(
                &mut by_model,
                None,
                Some(label),
                event.model.clone(),
                event.provider.clone(),
                event.agent_cli.clone(),
                None,
                None,
                event,
                false,
            );
            // Add each (session, model) pair's effective total once.
            let event_model_key = usage_model_key_from_fields(
                event.model.as_deref(),
                event.provider.as_deref(),
                event.agent_cli.as_deref(),
            );
            let sm_pair = (event.session_id.clone(), event_model_key);
            if contributed.insert(sm_pair.clone()) {
                if let Some(total) = per_session_model.get(&sm_pair) {
                    let key = usage_model_key_from_fields(
                        event.model.as_deref(),
                        event.provider.as_deref(),
                        event.agent_cli.as_deref(),
                    );
                    if let Some(index) = by_model.iter().position(|row| usage_row_matches_key(row, &key)) {
                        add_usage_tokens(&mut by_model[index].tokens, total);
                    }
                }
            }
        }
        let mut tokens = UsageTokenBreakdown::default();
        let mut day_total: u64 = 0;
        for row in &by_model {
            add_usage_tokens(&mut tokens, &row.tokens);
            day_total = day_total.saturating_add(usage_total_tokens(&row.tokens));
        }
        // Override the summed `total_tokens` with the sum of each row's
        // *effective* total so rows that only carried component tokens (no
        // authoritative `total_tokens`) still count toward the day total.
        tokens.total_tokens = Some(day_total);
        by_model.sort_by(|a, b| {
            usage_total_tokens(&b.tokens)
                .cmp(&usage_total_tokens(&a.tokens))
                .then_with(|| a.label.cmp(&b.label))
        });
        buckets.push(UsageDailyBucket {
            date: date.clone(),
            tokens,
            by_model,
        });
    }
    buckets
}

/// Advance the running baseline with a day's SessionTotal events: for each
/// `(session, model)` pair, the last SessionTotal of the day becomes the new
/// baseline that the *next* day subtracts from.
fn advance_baseline(
    baseline: &mut HashMap<(String, String), UsageTokenBreakdown>,
    day_events: &[&StoredUsageEvent],
) {
    for event in day_events {
        if matches!(event.scope, UsageEventScope::SessionTotal) && has_usage_tokens(&event.tokens) {
            let key = usage_model_key_from_fields(
                event.model.as_deref(),
                event.provider.as_deref(),
                event.agent_cli.as_deref(),
            );
            baseline.insert((event.session_id.clone(), key), event.tokens.clone());
        }
    }
}

/// Per-(session, model) incremental total for a single day: last in-day
/// SessionTotal minus the running baseline (last SessionTotal before the
/// day). Falls back to TurnDelta accumulation when no SessionTotal exists.
fn compute_daily_day_totals(
    day_events: &[&StoredUsageEvent],
    running_baseline: &HashMap<(String, String), UsageTokenBreakdown>,
    day_start_epoch: i64,
) -> HashMap<(String, String), UsageTokenBreakdown> {
    #[derive(Default)]
    struct DayTotalState {
        session_total: Option<UsageTokenBreakdown>,
        turn_delta_sum: UsageTokenBreakdown,
        saw_turn_delta: bool,
    }
    let mut state: HashMap<(String, String), DayTotalState> = HashMap::new();
    for event in day_events {
        let key = usage_model_key_from_fields(
            event.model.as_deref(),
            event.provider.as_deref(),
            event.agent_cli.as_deref(),
        );
        let pair = (event.session_id.clone(), key);
        let entry = state.entry(pair).or_default();
        match event.scope {
            UsageEventScope::SessionTotal if has_usage_tokens(&event.tokens) => {
                let model_key = usage_model_key_from_fields(
                    event.model.as_deref(),
                    event.provider.as_deref(),
                    event.agent_cli.as_deref(),
                );
                let baseline_key = (event.session_id.clone(), model_key);
                let total = if let Some(baseline) = running_baseline.get(&baseline_key) {
                    subtract_usage_tokens(&event.tokens, baseline)
                } else {
                    event.tokens.clone()
                };
                entry.session_total = Some(total);
            }
            UsageEventScope::TurnDelta => {
                entry.saw_turn_delta = true;
                add_usage_tokens(&mut entry.turn_delta_sum, &event.tokens);
            }
            _ => {}
        }
    }
    let totals = state
        .into_iter()
        .filter_map(|(pair, st)| {
            if st.saw_turn_delta {
                Some((pair, st.turn_delta_sum))
            } else {
                st.session_total.map(|total| (pair, total))
            }
        })
        .collect();
    let _ = day_start_epoch;
    totals
}

fn update_usage_summary_row(
    rows: &mut Vec<UsageModelSummary>,
    explicit_key: Option<&str>,
    label: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    agent_cli: Option<String>,
    session_id: Option<String>,
    workspace_root: Option<String>,
    event: &StoredUsageEvent,
    track_tokens: bool,
) {
    let key = explicit_key.map(str::to_string).unwrap_or_else(|| {
        usage_model_key_from_fields(model.as_deref(), provider.as_deref(), agent_cli.as_deref())
    });
    let index = rows.iter().position(|row| usage_row_matches_key(row, &key));
    let row = if let Some(index) = index {
        &mut rows[index]
    } else {
        rows.push(UsageModelSummary {
            label: label.unwrap_or_else(|| "Unknown model".into()),
            model,
            provider,
            agent_cli,
            session_id,
            workspace_root,
            event_count: 0,
            request_count: 0,
            session_count: 1,
            tokens: UsageTokenBreakdown::default(),
            context_peak_tokens: None,
            latest_at: None,
            avg_latency_ms: None,
            avg_ttft_ms: None,
            avg_tokens_per_second: None,
            latency_count: 0,
            ttft_count: 0,
            tps_count: 0,
            has_session_total: false,
        });
        rows.last_mut().expect("usage row just inserted")
    };

    row.event_count += 1;
    // P5: only TurnDelta / SessionTotal represent an actual token-reporting
    // request; ContextSnapshot is occupancy-only telemetry.
    if matches!(
        event.scope,
        UsageEventScope::TurnDelta | UsageEventScope::SessionTotal
    ) {
        row.request_count += 1;
    }
    // `latest_at` is stored as canonical ISO-8601 UTC (via `instant_to_iso_utc`)
    // so the cross-session sort in `usage_summary_from_events` can compare
    // strings safely. The "latest" pick itself compares numeric epoch seconds
    // so it stays correct even while a session's rows transition from legacy
    // epoch-seconds storage to ISO.
    let event_at = instant_to_iso_utc(&event.created_at);
    row.latest_at = Some(match row.latest_at.as_ref() {
        Some(latest) => {
            let latest_secs = parse_instant_to_epoch_secs(latest).unwrap_or(i64::MIN);
            let event_secs = parse_instant_to_epoch_secs(&event.created_at).unwrap_or(i64::MIN);
            if event_secs >= latest_secs {
                event_at
            } else {
                latest.clone()
            }
        }
        None => event_at,
    });
    if let Some(used) = event.context.used_tokens {
        row.context_peak_tokens = Some(row.context_peak_tokens.unwrap_or(0).max(used));
    }
    // Timing metrics: per-field rolling average. Each field uses its own
    // counter because a timed event may carry `latency_ms` without
    // `ttft_ms`/`tokens_per_second` (e.g. a model call that produced no
    // output tokens), so a shared counter would divide by an inflated n
    // and understate the absent fields' averages. Independent of the
    // token SessionTotal/TurnDelta accounting below.
    if let Some(v) = event.tokens.latency_ms {
        row.latency_count += 1;
        row.avg_latency_ms =
            Some(rolling_avg(row.avg_latency_ms, v as f64, row.latency_count as f64));
    }
    if let Some(v) = event.tokens.ttft_ms {
        row.ttft_count += 1;
        row.avg_ttft_ms =
            Some(rolling_avg(row.avg_ttft_ms, v as f64, row.ttft_count as f64));
    }
    if let Some(v) = event.tokens.tokens_per_second {
        row.tps_count += 1;
        row.avg_tokens_per_second =
            Some(rolling_avg(row.avg_tokens_per_second, v, row.tps_count as f64));
    }
    // Token handling is only active for the single-session snapshot path
    // (`track_tokens = true`), where `has_session_total` is effectively
    // per-session because only one session's events are in scope. The
    // cross-session summary and daily series pass `track_tokens = false`
    // and compute token totals from [`compute_per_session_model_totals`]
    // instead, because the per-group `has_session_total` flag would break
    // when multiple sessions share a model group.
    if track_tokens {
        match event.scope {
            UsageEventScope::TurnDelta => {
                if !row.has_session_total {
                    add_usage_tokens(&mut row.tokens, &event.tokens);
                }
            }
            UsageEventScope::SessionTotal if has_usage_tokens(&event.tokens) => {
                row.tokens = event.tokens.clone();
                row.has_session_total = true;
            }
            // `ContextSnapshot` does not contribute to per-model token
            // totals; context peak is already recorded above.
            UsageEventScope::ContextSnapshot => {}
            _ => {}
        }
    }
}

fn rolling_avg(prev: Option<f64>, new: f64, n: f64) -> f64 {
    match prev {
        Some(p) => p + (new - p) / n,
        None => new,
    }
}

fn usage_row_matches_key(row: &UsageSummaryRow, key: &str) -> bool {
    if let Some(session_id) = key.strip_prefix("session:") {
        return row.session_id.as_deref() == Some(session_id);
    }
    if let Some(agent_cli) = key.strip_prefix("agent:") {
        return row.agent_cli.as_deref().unwrap_or("") == agent_cli;
    }
    if let Some(workspace_root) = key.strip_prefix("workspace:") {
        return row.workspace_root.as_deref().unwrap_or("") == workspace_root;
    }
    usage_summary_key_by_values(row) == key
}

fn usage_summary_key(row: &UsageSummaryRow, group_by: &UsageSummaryGroupBy) -> String {
    match group_by {
        UsageSummaryGroupBy::Model => usage_summary_key_by_values(row),
        UsageSummaryGroupBy::Agent => format!("agent:{}", row.agent_cli.as_deref().unwrap_or("")),
        UsageSummaryGroupBy::Workspace => {
            format!("workspace:{}", row.workspace_root.as_deref().unwrap_or(""))
        }
        UsageSummaryGroupBy::Session => {
            format!("session:{}", row.session_id.as_deref().unwrap_or(""))
        }
    }
}

/// Build the model-keyed aggregation key (`model:provider:agent_cli`) from
/// individual fields. This is the single source of truth for that key shape:
/// [`update_usage_summary_row`]'s `explicit_key = None` fallback and the
/// single-session snapshot path both route through here so the two paths
/// cannot drift.
fn usage_model_key_from_fields(
    model: Option<&str>,
    provider: Option<&str>,
    agent_cli: Option<&str>,
) -> String {
    format!(
        "model:{}:{}:{}",
        model.unwrap_or(""),
        provider.unwrap_or(""),
        agent_cli.unwrap_or("")
    )
}

fn usage_summary_key_by_values(row: &UsageSummaryRow) -> String {
    // Defensive fallback: if a row already carries a session id, key it by
    // session so it is never silently merged into a model group. In practice
    // every call site that matches via a `model:` key creates rows with
    // `session_id = None`, so this branch is only hit by legacy/defensive
    // paths; the authoritative per-session key is built explicitly in
    // `usage_summary_from_events` (`session:{id}`).
    if row.session_id.is_some() {
        return format!("session:{}", row.session_id.as_deref().unwrap_or(""));
    }
    usage_model_key_from_fields(
        row.model.as_deref(),
        row.provider.as_deref(),
        row.agent_cli.as_deref(),
    )
}

fn usage_scope_to_str(scope: &UsageEventScope) -> &'static str {
    match scope {
        UsageEventScope::ContextSnapshot => "context_snapshot",
        UsageEventScope::TurnDelta => "turn_delta",
        UsageEventScope::SessionTotal => "session_total",
    }
}

fn usage_scope_from_str(value: &str) -> UsageEventScope {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "turn_delta" => UsageEventScope::TurnDelta,
        "session_total" => UsageEventScope::SessionTotal,
        _ => UsageEventScope::ContextSnapshot,
    }
}

fn has_usage_tokens(tokens: &UsageTokenBreakdown) -> bool {
    tokens.input_tokens.is_some()
        || tokens.output_tokens.is_some()
        || tokens.cache_read_tokens.is_some()
        || tokens.cache_write_tokens.is_some()
        || tokens.reasoning_tokens.is_some()
        || tokens.total_tokens.is_some()
}

fn add_usage_tokens(target: &mut UsageTokenBreakdown, delta: &UsageTokenBreakdown) {
    add_optional_u64(&mut target.input_tokens, delta.input_tokens);
    add_optional_u64(&mut target.output_tokens, delta.output_tokens);
    add_optional_u64(&mut target.cache_read_tokens, delta.cache_read_tokens);
    add_optional_u64(&mut target.cache_write_tokens, delta.cache_write_tokens);
    add_optional_u64(&mut target.reasoning_tokens, delta.reasoning_tokens);
    add_optional_u64(&mut target.total_tokens, delta.total_tokens);
}

fn add_optional_u64(target: &mut Option<u64>, delta: Option<u64>) {
    if let Some(delta) = delta {
        *target = Some(target.unwrap_or(0).saturating_add(delta));
    }
}

/// Subtract one token breakdown from another (clamped at 0). Used to turn a
/// cumulative `SessionTotal` into an in-range increment by removing the
/// carry-over baseline. Missing fields on either side are treated as 0.
fn subtract_usage_tokens(
    value: &UsageTokenBreakdown,
    baseline: &UsageTokenBreakdown,
) -> UsageTokenBreakdown {
    UsageTokenBreakdown {
        input_tokens: sub_optional_u64(value.input_tokens, baseline.input_tokens),
        output_tokens: sub_optional_u64(value.output_tokens, baseline.output_tokens),
        cache_read_tokens: sub_optional_u64(value.cache_read_tokens, baseline.cache_read_tokens),
        cache_write_tokens: sub_optional_u64(value.cache_write_tokens, baseline.cache_write_tokens),
        reasoning_tokens: sub_optional_u64(value.reasoning_tokens, baseline.reasoning_tokens),
        total_tokens: sub_optional_u64(value.total_tokens, baseline.total_tokens),
        latency_ms: None,
        ttft_ms: None,
        tokens_per_second: None,
    }
}

fn sub_optional_u64(value: Option<u64>, sub: Option<u64>) -> Option<u64> {
    Some(value.unwrap_or(0).saturating_sub(sub.unwrap_or(0)))
}

fn usage_total_tokens(tokens: &UsageTokenBreakdown) -> u64 {
    // `cache_read_tokens` is a subset of `input_tokens` (cache hits are billed
    // as discounted input), and `cache_write_tokens` is null for codex-acp.
    // Adding either to the fallback would double-count the same input tokens.
    // `cache_read_tokens` / `cache_write_tokens` remain as display-only
    // breakdown fields; the authoritative `total_tokens` is preferred when
    // present, otherwise fall back to input + output + reasoning.
    tokens.total_tokens.unwrap_or_else(|| {
        tokens.input_tokens.unwrap_or(0)
            + tokens.output_tokens.unwrap_or(0)
            + tokens.reasoning_tokens.unwrap_or(0)
    })
}

fn opt_i64(value: Option<u64>) -> Option<i64> {
    value.map(|value| value.min(i64::MAX as u64) as i64)
}

fn opt_u64(value: Option<i64>) -> Option<u64> {
    value.and_then(|value| u64::try_from(value).ok())
}

#[cfg(test)]
mod tests;
