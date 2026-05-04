use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::fs;
use std::path::Path;
use uuid::Uuid;
use workspace_model::{
    ChatMessage, FileChangeType, MessageRole, SessionFileChange, SessionListItem, TimelineItem,
    ToolDiffPreview, ToolInvocation, ToolStatus,
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
        fs::create_dir_all(&sessions_dir).with_context(|| {
            format!(
                "failed to create session data dir at {}",
                sessions_dir.display()
            )
        })?;

        let db_path = sessions_dir.join("sessions.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open sessions.db at {}", db_path.display()))?;

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
                status TEXT NOT NULL DEFAULT 'Idle',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
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

            CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);
            CREATE INDEX IF NOT EXISTS idx_tools_session ON tool_invocations(session_id, seq);
            CREATE INDEX IF NOT EXISTS idx_file_changes_session ON session_file_changes(session_id);
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
            "INSERT INTO sessions (id, title, model, status, created_at, updated_at, workspace_root) VALUES (?1, 'New Session', ?2, 'Idle', ?3, ?4, ?5)",
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

    pub fn update_session_model_mode(
        &self,
        id: &str,
        model: &str,
        mode: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET model = ?1, mode = ?2, updated_at = ?3 WHERE id = ?4",
            params![model, mode, now_iso(), id],
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

    pub fn list_sessions(&self) -> Result<Vec<SessionListItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.title, s.status, s.created_at, s.updated_at,
                    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) as msg_count,
                    s.acp_session_id
             FROM sessions s WHERE s.workspace_root = ?1 ORDER BY s.updated_at DESC",
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
                seq: i64,
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
                seq: i64,
            },
        }

        let mut entries: Vec<(i64, Entry)> = Vec::new();

        // Load messages
        {
            let mut stmt = self.conn.prepare(
                "SELECT id, role, body, seq FROM messages WHERE session_id = ?1 ORDER BY seq",
            )?;
            let rows = stmt.query_map(params![id], |row| {
                let id_str: String = row.get(0)?;
                Ok((
                    row.get::<_, i64>(3)?,
                    Entry::Message {
                        id: Uuid::parse_str(&id_str).unwrap_or_else(|_| Uuid::new_v4()),
                        role: row.get(1)?,
                        body: row.get(2)?,
                        seq: row.get(3)?,
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
                        seq: row.get(12)?,
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
                Entry::Message { id, role, body, .. } => {
                    let role = match role.as_str() {
                        "User" => MessageRole::User,
                        "Assistant" => MessageRole::Assistant,
                        _ => MessageRole::System,
                    };
                    messages.push(ChatMessage { id, role, body });
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

    /// Load all file changes for a session, ordered by path.
    pub fn load_file_changes(&self, session_id: &str) -> Result<Vec<SessionFileChange>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, change_type, base_text, new_text, added_lines, removed_lines, updated_at
             FROM session_file_changes WHERE session_id = ?1 ORDER BY path",
        )?;

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

fn normalize_change_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let normalized = normalized
        .strip_prefix("//?/")
        .or_else(|| normalized.strip_prefix("//./"))
        .unwrap_or(&normalized)
        .to_string();
    if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        let mut chars: Vec<char> = normalized.chars().collect();
        chars[0] = chars[0].to_ascii_lowercase();
        chars.into_iter().collect()
    } else {
        normalized
    }
}

fn normalize_workspace_root(path: &Path) -> String {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    normalize_change_path(&path.to_string_lossy())
}

fn now_iso() -> String {
    // Simple UTC timestamp without chrono dependency
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = since_epoch.as_secs();
    // Return as epoch seconds string (good enough for ordering)
    format!("{secs}")
}

fn cap_string(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        s.to_string()
    } else {
        s[..max_bytes].to_string()
    }
}

fn decode_json_vec<T>(json: Option<&str>) -> Vec<T>
where
    T: serde::de::DeserializeOwned,
{
    json.and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_list_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path(), dir.path()).unwrap();

        store.create_session("s1", "gpt-4").unwrap();
        store.create_session("s2", "claude-3").unwrap();

        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].title, "New Session");
    }

    #[test]
    fn test_update_session_title() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path(), dir.path()).unwrap();

        store.create_session("s1", "gpt-4").unwrap();
        store.update_session_title("s1", "Fix login bug").unwrap();

        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions[0].title, "Fix login bug");
    }

    #[test]
    fn test_insert_and_load_messages() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path(), dir.path()).unwrap();

        store.create_session("s1", "gpt-4").unwrap();
        store
            .insert_message("s1", "m1", "User", "hello", 1)
            .unwrap();
        store
            .insert_message("s1", "m2", "Assistant", "hi there", 2)
            .unwrap();

        let (messages, _tools, timeline) = store.load_session("s1").unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].body, "hello");
        assert_eq!(messages[1].body, "hi there");
        assert_eq!(timeline.len(), 2);
    }

    #[test]
    fn test_insert_and_load_tool_diff_preview() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path(), dir.path()).unwrap();
        store.create_session("s1", "gpt-4").unwrap();

        let tool_id = Uuid::new_v4();
        let path = std::path::PathBuf::from("d:/work/kodex/AGENTS.md");
        let tool = ToolInvocation {
            id: tool_id,
            call_id: "edit-1".into(),
            parent_call_id: None,
            name: "Edit".into(),
            kind: "edit".into(),
            summary: "Editing AGENTS.md".into(),
            status: ToolStatus::Succeeded,
            is_subagent: false,
            detail_text: String::new(),
            logs: Vec::new(),
            diff_paths: vec![path.clone()],
            diff_previews: vec![ToolDiffPreview {
                path: path.clone(),
                hunks: vec![workspace_model::DiffHunk {
                    heading: "ACP diff".into(),
                    lines: vec![workspace_model::DiffLine {
                        kind: workspace_model::DiffLineKind::Added,
                        content: "new line".into(),
                    }],
                }],
            }],
            raw_input: None,
            raw_output: None,
            terminal_output: None,
            error: None,
            permission_options: Vec::new(),
            permission_decision: None,
        };

        store.insert_tool("s1", &tool, 1).unwrap();

        let (_messages, tools, timeline) = store.load_session("s1").unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].diff_paths, vec![path.clone()]);
        assert_eq!(tools[0].diff_previews.len(), 1);
        assert_eq!(tools[0].diff_previews[0].path, path);
        assert_eq!(
            tools[0].diff_previews[0].hunks[0].lines[0].content,
            "new line"
        );
        assert!(matches!(timeline[0], TimelineItem::Tool(id) if id == tool_id));
    }

    #[test]
    fn test_delete_session_cascades() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path(), dir.path()).unwrap();

        store.create_session("s1", "gpt-4").unwrap();
        store
            .insert_message("s1", "m1", "User", "hello", 1)
            .unwrap();
        store.delete_session("s1").unwrap();

        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 0);

        let (messages, _tools, _timeline) = store.load_session("s1").unwrap();
        assert_eq!(messages.len(), 0);
    }

    #[test]
    fn test_message_count_in_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path(), dir.path()).unwrap();

        store.create_session("s1", "gpt-4").unwrap();
        store.insert_message("s1", "m1", "User", "a", 1).unwrap();
        store
            .insert_message("s1", "m2", "Assistant", "b", 2)
            .unwrap();
        store.insert_message("s1", "m3", "User", "c", 3).unwrap();

        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions[0].message_count, 3);
    }

    #[test]
    fn test_open_uses_home_sessions_dir_and_leaves_workspace_clean() {
        let dir = tempfile::tempdir().unwrap();
        let app_data = dir.path().join("home").join(".kodex");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let store = SessionStore::open(&app_data, &workspace).unwrap();
        store.create_session("s1", "gpt-4").unwrap();

        assert!(SessionStore::db_path(&app_data).is_file());
        assert!(!workspace.join(".kodex").exists());
    }

    #[test]
    fn test_list_sessions_filters_by_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let app_data = dir.path().join("home").join(".kodex");
        let workspace_a = dir.path().join("a");
        let workspace_b = dir.path().join("b");
        std::fs::create_dir_all(&workspace_a).unwrap();
        std::fs::create_dir_all(&workspace_b).unwrap();

        let store_a = SessionStore::open(&app_data, &workspace_a).unwrap();
        store_a.create_session("session-a", "gpt-4").unwrap();
        let store_b = SessionStore::open(&app_data, &workspace_b).unwrap();
        store_b.create_session("session-b", "gpt-4").unwrap();

        let sessions_a = store_a.list_sessions().unwrap();
        let sessions_b = store_b.list_sessions().unwrap();
        assert_eq!(sessions_a.len(), 1);
        assert_eq!(sessions_a[0].id, "session-a");
        assert_eq!(sessions_b.len(), 1);
        assert_eq!(sessions_b[0].id, "session-b");
    }

    #[test]
    fn test_import_legacy_workspace_db_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let app_data = dir.path().join("home").join(".kodex");
        let workspace = dir.path().join("workspace");
        let legacy_dir = workspace.join(".kodex");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_db = legacy_dir.join("sessions.db");

        let legacy = Connection::open(&legacy_db).unwrap();
        legacy
            .execute_batch(
                "
                CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL DEFAULT 'New Session',
                    model TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'Idle',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE messages (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    role TEXT NOT NULL,
                    body TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    created_at TEXT NOT NULL
                );
                INSERT INTO sessions (id, title, model, status, created_at, updated_at)
                VALUES ('legacy-session', 'Legacy', 'gpt-4', 'Idle', '1', '2');
                INSERT INTO messages (id, session_id, role, body, seq, created_at)
                VALUES ('legacy-message', 'legacy-session', 'User', 'hello', 1, '2');
                ",
            )
            .unwrap();
        drop(legacy);

        let store = SessionStore::open(&app_data, &workspace).unwrap();
        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "legacy-session");
        assert_eq!(sessions[0].message_count, 1);
        assert!(legacy_db.is_file());

        let reopened = SessionStore::open(&app_data, &workspace).unwrap();
        assert_eq!(reopened.list_sessions().unwrap().len(), 1);
    }

    #[test]
    fn test_upsert_and_load_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path(), dir.path()).unwrap();
        store.create_session("s1", "gpt-4").unwrap();

        // Insert a file change with base_text
        store
            .upsert_file_change(
                "s1",
                "/src/main.rs",
                "Modified",
                Some("old content"),
                "new content",
                5,
                2,
            )
            .unwrap();

        let changes = store.load_file_changes("s1").unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "/src/main.rs");
        assert_eq!(changes[0].old_text.as_deref(), Some("old content"));
        assert_eq!(changes[0].new_text, "new content");
        assert_eq!(changes[0].added_lines, 5);
        assert_eq!(changes[0].removed_lines, 2);
    }

    #[test]
    fn test_upsert_preserves_base_text() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path(), dir.path()).unwrap();
        store.create_session("s1", "gpt-4").unwrap();

        // First insert with base_text
        store
            .upsert_file_change(
                "s1",
                "/src/main.rs",
                "Modified",
                Some("original"),
                "v1",
                1,
                0,
            )
            .unwrap();

        // Second upsert with None base_text — should NOT overwrite existing
        store
            .upsert_file_change("s1", "/src/main.rs", "Modified", None, "v2", 3, 1)
            .unwrap();

        let changes = store.load_file_changes("s1").unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].old_text.as_deref(), Some("original")); // preserved!
        assert_eq!(changes[0].new_text, "v2"); // updated
        assert_eq!(changes[0].added_lines, 3);
    }

    #[test]
    fn test_file_changes_normalize_windows_verbatim_paths() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path(), dir.path()).unwrap();
        store.create_session("s1", "gpt-4").unwrap();

        store
            .upsert_file_change(
                "s1",
                "d:/work/kodex/AGENTS.md",
                "Modified",
                Some("old"),
                "new",
                1,
                1,
            )
            .unwrap();
        store
            .upsert_file_change(
                "s1",
                "\\\\?\\D:\\work\\kodex\\AGENTS.md",
                "Modified",
                Some("new"),
                "newer",
                2,
                1,
            )
            .unwrap();

        let changes = store.load_file_changes("s1").unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "d:/work/kodex/AGENTS.md");
        assert_eq!(changes[0].old_text.as_deref(), Some("old"));
        assert_eq!(changes[0].new_text, "newer");
    }

    #[test]
    fn test_file_changes_cascade_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path(), dir.path()).unwrap();
        store.create_session("s1", "gpt-4").unwrap();

        store
            .upsert_file_change("s1", "/a.rs", "Created", None, "content", 10, 0)
            .unwrap();
        store
            .upsert_file_change("s1", "/b.rs", "Modified", Some("old"), "new", 2, 1)
            .unwrap();

        // Delete session — file changes should cascade
        store.delete_session("s1").unwrap();

        let changes = store.load_file_changes("s1").unwrap();
        assert_eq!(changes.len(), 0);
    }
}
