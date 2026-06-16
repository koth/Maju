use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
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
    cap_string, decode_json_vec, normalize_change_path, normalize_workspace_root, now_iso,
    upsert_loaded_change,
};
use workspace_model::{
    ChangeSetSource, ChangeSetStatus, ChangeSetSummary, ChatMessage, FileChangeRecord,
    FileChangeSummary, FileChangeType, MessageRole, SessionFileChange, SessionListItem,
    TimelineItem, ToolDiffPreview, ToolInvocation, ToolStatus, TurnFileChanges,
};

const MAX_RAW_OUTPUT_BYTES: usize = 32 * 1024;

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

            CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);
            CREATE INDEX IF NOT EXISTS idx_tools_session ON tool_invocations(session_id, seq);
            CREATE INDEX IF NOT EXISTS idx_file_changes_session ON session_file_changes(session_id);
            CREATE INDEX IF NOT EXISTS idx_review_file_changes_session ON session_review_file_changes(session_id);
            CREATE INDEX IF NOT EXISTS idx_turn_file_changes_session_message ON session_turn_file_changes(session_id, message_id);
            CREATE INDEX IF NOT EXISTS idx_change_sets_workspace_source ON change_sets(workspace_root, source, updated_at);
            CREATE INDEX IF NOT EXISTS idx_change_sets_session_source ON change_sets(session_id, source, updated_at);
            CREATE INDEX IF NOT EXISTS idx_change_sets_message ON change_sets(message_id);
            CREATE INDEX IF NOT EXISTS idx_change_set_files_change_set ON change_set_files(change_set_id);
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

        self.conn.execute(
            "UPDATE sessions SET workspace_root = ?1 WHERE workspace_root IS NULL OR workspace_root = ''",
            params![&self.workspace_root],
        )?;

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
            "INSERT INTO messages (id, session_id, role, body, seq, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, session_id, role, body, seq, now_iso()],
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
                "SELECT id, role, body, seq, created_at FROM messages WHERE session_id = ?1 ORDER BY seq",
            )?;
            let rows = stmt.query_map(params![id], |row| {
                let id_str: String = row.get(0)?;
                Ok((
                    row.get::<_, i64>(3)?,
                    Entry::Message {
                        id: Uuid::parse_str(&id_str).unwrap_or_else(|_| Uuid::new_v4()),
                        role: row.get(1)?,
                        body: row.get(2)?,
                        created_at: row.get(4)?,
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
                    ..
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

#[cfg(test)]
mod tests;
