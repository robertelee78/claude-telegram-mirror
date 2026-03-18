//! SQLite-backed session manager.
//!
//! Tracks CLI sessions and pending approvals — ported from `session.ts`.

use crate::config::ensure_config_dir;
use crate::error::{AppError, Result};
use crate::types::{ApprovalStatus, SessionStatus};
use rusqlite::{params, Connection};
use std::path::Path;

/// An active or ended Claude Code session.
///
/// L5.3 (INTENTIONAL): Date fields (`started_at`, `last_activity`) are `String`
/// rather than `chrono::DateTime` or epoch integers. This is a deliberate design
/// choice: SQLite stores them as ISO 8601 TEXT (`to_rfc3339_opts`), which is
/// human-readable in raw SQL queries, sorts lexicographically, and avoids
/// timezone-conversion bugs. The TypeScript implementation used the same TEXT
/// representation. Converting to typed timestamps would add serde complexity
/// with no practical benefit.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    #[allow(dead_code)] // Deserialized from DB; Library API
    pub chat_id: i64,
    pub thread_id: Option<i64>,
    pub hostname: Option<String>,
    pub tmux_target: Option<String>,
    pub tmux_socket: Option<String>,
    pub started_at: String,
    pub last_activity: String,
    pub status: SessionStatus,
    pub project_dir: Option<String>,
    #[allow(dead_code)] // Deserialized from DB; Library API
    pub metadata: Option<String>,
    /// ADR-013: Parent session ID for sub-agent sessions (extracted from transcript_path).
    #[allow(dead_code)] // Deserialized from DB; Library API — used by daemon routing (ADR-013)
    pub parent_session_id: Option<String>,
    /// ADR-013: Agent ID for sub-agent sessions (e.g. "agent-abc123").
    #[allow(dead_code)] // Deserialized from DB; Library API — used by daemon routing (ADR-013)
    pub agent_id: Option<String>,
    /// ADR-013: Agent type for sub-agent sessions (e.g. "Explore", "researcher").
    #[allow(dead_code)]
    pub agent_type: Option<String>,
}

/// A pending tool-approval request.
#[derive(Debug, Clone)]
pub struct PendingApproval {
    #[allow(dead_code)] // Deserialized from DB; Library API
    pub id: String,
    pub session_id: String,
    #[allow(dead_code)] // Deserialized from DB; Library API
    pub prompt: String,
    #[allow(dead_code)] // Deserialized from DB; Library API
    pub created_at: String,
    #[allow(dead_code)] // Deserialized from DB; Library API
    pub expires_at: String,
    #[allow(dead_code)] // Deserialized from DB; Library API
    pub status: ApprovalStatus,
    #[allow(dead_code)] // Deserialized from DB; Library API
    pub message_id: Option<i64>,
}

/// Generate a unique ID with an optional prefix.
fn generate_id(prefix: &str) -> String {
    let ts = chrono::Utc::now().timestamp_millis();
    // base-36 timestamp
    let ts36 = radix36(ts as u64);
    let random = uuid::Uuid::new_v4().simple().to_string();
    let rand_hex = &random[..8];
    if prefix.is_empty() {
        format!("{ts36}-{rand_hex}")
    } else {
        format!("{prefix}-{ts36}-{rand_hex}")
    }
}

fn radix36(mut n: u64) -> String {
    if n == 0 {
        return "0".to_string();
    }
    const CHARS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut buf = Vec::new();
    while n > 0 {
        buf.push(CHARS[(n % 36) as usize]);
        n /= 36;
    }
    buf.reverse();
    String::from_utf8(buf).expect("radix36 CHARS table is entirely ASCII")
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Manages session and approval lifecycle in SQLite.
///
/// L6.9 (INTENTIONAL): There is no explicit `close()` method.  Rust uses RAII:
/// when a `SessionManager` is dropped, the inner `rusqlite::Connection` is
/// automatically closed via its `Drop` implementation, which flushes any
/// pending WAL frames and releases file locks.  An explicit `close()` would be
/// redundant and would require consuming `self`, complicating ownership.
pub struct SessionManager {
    conn: Connection,
    approval_timeout_ms: i64,
}

impl SessionManager {
    /// Open (or create) the session database.
    ///
    /// * `config_dir` — parent directory for `sessions.db`
    /// * `approval_timeout_minutes` — how long an approval stays valid
    pub fn new(config_dir: &Path, approval_timeout_minutes: u32) -> Result<Self> {
        ensure_config_dir(config_dir)?;

        let db_path = config_dir.join("sessions.db");
        let conn = Connection::open(&db_path).map_err(|e| AppError::Database(e.to_string()))?;

        // Secure file permissions: 0o600
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if db_path.exists() {
                let _ = std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600));
            }
        }

        let mgr = Self {
            conn,
            approval_timeout_ms: i64::from(approval_timeout_minutes) * 60 * 1000,
        };
        mgr.init_schema()?;
        Ok(mgr)
    }

    // ------------------------------------------------------------------ schema

    fn init_schema(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS sessions (
                id                TEXT PRIMARY KEY,
                chat_id           INTEGER NOT NULL,
                thread_id         INTEGER,
                hostname          TEXT,
                tmux_target       TEXT,
                tmux_socket       TEXT,
                started_at        TEXT NOT NULL,
                last_activity     TEXT NOT NULL,
                status            TEXT DEFAULT 'active',
                project_dir       TEXT,
                metadata          TEXT,
                parent_session_id TEXT,
                agent_id          TEXT,
                agent_type        TEXT
            );

            CREATE TABLE IF NOT EXISTS pending_approvals (
                id          TEXT PRIMARY KEY,
                session_id  TEXT NOT NULL,
                prompt      TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                expires_at  TEXT NOT NULL,
                status      TEXT DEFAULT 'pending',
                message_id  INTEGER,
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );

            CREATE INDEX IF NOT EXISTS idx_sessions_chat     ON sessions(chat_id);
            CREATE INDEX IF NOT EXISTS idx_sessions_status   ON sessions(status);
            CREATE INDEX IF NOT EXISTS idx_approvals_session ON pending_approvals(session_id);
            CREATE INDEX IF NOT EXISTS idx_approvals_status  ON pending_approvals(status);
            ",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        self.migrate_add_tmux_columns()?;
        self.migrate_add_parent_columns()?;
        Ok(())
    }

    /// Migration: add tmux_target / tmux_socket if upgrading from an older DB.
    fn migrate_add_tmux_columns(&self) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare("PRAGMA table_info(sessions)")
            .map_err(|e| AppError::Database(e.to_string()))?;

        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| AppError::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        if !columns.iter().any(|c| c == "tmux_target") {
            self.conn
                .execute_batch("ALTER TABLE sessions ADD COLUMN tmux_target TEXT")
                .map_err(|e| AppError::Database(e.to_string()))?;
        }
        if !columns.iter().any(|c| c == "tmux_socket") {
            self.conn
                .execute_batch("ALTER TABLE sessions ADD COLUMN tmux_socket TEXT")
                .map_err(|e| AppError::Database(e.to_string()))?;
        }
        Ok(())
    }

    /// ADR-013 Migration: add parent_session_id / agent_id if upgrading from an older DB.
    ///
    /// For new databases these columns are included in the CREATE TABLE statement,
    /// so this migration is a no-op. For databases created before ADR-013, the ALTER
    /// TABLE adds the missing columns. The "duplicate column name" error is caught
    /// to handle concurrent migrations racing on the same DB file.
    fn migrate_add_parent_columns(&self) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare("PRAGMA table_info(sessions)")
            .map_err(|e| AppError::Database(e.to_string()))?;

        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| AppError::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        if !columns.iter().any(|c| c == "parent_session_id") {
            match self
                .conn
                .execute_batch("ALTER TABLE sessions ADD COLUMN parent_session_id TEXT")
            {
                Ok(()) => {}
                Err(e) if e.to_string().contains("duplicate column name") => {
                    // Another connection already added this column concurrently — safe to ignore.
                }
                Err(e) => return Err(AppError::Database(e.to_string())),
            }
        }
        if !columns.iter().any(|c| c == "agent_id") {
            match self
                .conn
                .execute_batch("ALTER TABLE sessions ADD COLUMN agent_id TEXT")
            {
                Ok(()) => {}
                Err(e) if e.to_string().contains("duplicate column name") => {
                    // Another connection already added this column concurrently — safe to ignore.
                }
                Err(e) => return Err(AppError::Database(e.to_string())),
            }
        }
        if !columns.iter().any(|c| c == "agent_type") {
            match self
                .conn
                .execute_batch("ALTER TABLE sessions ADD COLUMN agent_type TEXT")
            {
                Ok(()) => {}
                Err(e) if e.to_string().contains("duplicate column name") => {}
                Err(e) => return Err(AppError::Database(e.to_string())),
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------- sessions

    /// Create (or reactivate) a session with all fields in a single atomic INSERT.
    ///
    /// Accepts the full set of session fields so callers can set everything at
    /// creation time without requiring subsequent `set_session_thread` /
    /// `set_tmux_info` calls.  If the session already exists, its `last_activity`
    /// is updated and the existing ID is returned unchanged.
    ///
    /// Returns the session ID actually used.
    #[allow(clippy::too_many_arguments)]
    pub fn create_session(
        &self,
        session_id: &str,
        chat_id: i64,
        hostname: Option<&str>,
        project_dir: Option<&str>,
        thread_id: Option<i64>,
        tmux_target: Option<&str>,
        tmux_socket: Option<&str>,
    ) -> Result<String> {
        if !crate::types::is_valid_session_id(session_id) {
            return Err(AppError::Config(format!(
                "invalid session_id: {}",
                session_id
            )));
        }

        let now = now_iso();

        // If the session already exists, touch its activity timestamp and
        // auto-heal any tmux/hostname/project_dir metadata that was provided
        // (ADR-013 F3: idempotency guard no longer discards metadata).
        if self.get_session(session_id)?.is_some() {
            self.update_activity(session_id)?;

            // Auto-heal: update tmux info if provided
            if tmux_target.is_some() || tmux_socket.is_some() {
                self.set_tmux_info(session_id, tmux_target, tmux_socket)?;
            }

            // Auto-heal: update hostname/project_dir if provided
            if hostname.is_some() || project_dir.is_some() {
                let mut updates = Vec::new();
                let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

                if let Some(h) = hostname {
                    updates.push("hostname = ?");
                    param_values.push(Box::new(h.to_string()));
                }
                if let Some(p) = project_dir {
                    updates.push("project_dir = ?");
                    param_values.push(Box::new(p.to_string()));
                }

                if !updates.is_empty() {
                    let sql = format!(
                        "UPDATE sessions SET {} WHERE id = ?",
                        updates.join(", ")
                    );
                    param_values.push(Box::new(session_id.to_string()));
                    let params: Vec<&dyn rusqlite::types::ToSql> =
                        param_values.iter().map(|p| p.as_ref()).collect();
                    self.conn
                        .execute(&sql, params.as_slice())
                        .map_err(|e| AppError::Database(e.to_string()))?;
                }
            }

            return Ok(session_id.to_string());
        }

        self.conn
            .execute(
                "INSERT INTO sessions
                 (id, chat_id, thread_id, hostname, tmux_target, tmux_socket,
                  started_at, last_activity, status, project_dir)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 'active', ?8)",
                params![
                    session_id,
                    chat_id,
                    thread_id,
                    hostname,
                    tmux_target,
                    tmux_socket,
                    now,
                    project_dir
                ],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(session_id.to_string())
    }

    pub fn set_session_thread(&self, session_id: &str, thread_id: i64) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sessions SET thread_id = ?1 WHERE id = ?2",
                params![thread_id, session_id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    /// L6.11: Retrieve the thread_id for a session directly, without loading
    /// the full `Session` struct.  Returns `None` if the session does not exist
    /// or has no thread_id set.
    #[allow(dead_code)] // Library API
    pub fn get_session_thread(&self, session_id: &str) -> Result<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT thread_id FROM sessions WHERE id = ?1")
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![session_id], |row| row.get::<_, Option<i64>>(0))
            .map_err(|e| AppError::Database(e.to_string()))?;

        match rows.next() {
            Some(Ok(tid)) => Ok(tid),
            Some(Err(e)) => Err(AppError::Database(e.to_string())),
            None => Ok(None),
        }
    }

    /// L6.10: Clear the thread_id for a session.
    pub fn clear_thread_id(&self, session_id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sessions SET thread_id = NULL WHERE id = ?1",
                params![session_id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn get_session(&self, session_id: &str) -> Result<Option<Session>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM sessions WHERE id = ?1")
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![session_id], row_to_session)
            .map_err(|e| AppError::Database(e.to_string()))?;

        match rows.next() {
            Some(Ok(s)) => Ok(Some(s)),
            Some(Err(e)) => Err(AppError::Database(e.to_string())),
            None => Ok(None),
        }
    }

    pub fn get_session_by_thread_id(&self, thread_id: i64) -> Result<Option<Session>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT * FROM sessions
                 WHERE thread_id = ?1 AND status = 'active'
                 LIMIT 1",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![thread_id], row_to_session)
            .map_err(|e| AppError::Database(e.to_string()))?;

        match rows.next() {
            Some(Ok(s)) => Ok(Some(s)),
            Some(Err(e)) => Err(AppError::Database(e.to_string())),
            None => Ok(None),
        }
    }

    /// Look up a session by thread_id regardless of status.
    ///
    /// Used by Telegram message handlers to recover ended sessions: if the user
    /// sends a message to a topic whose session was cleaned up, we can find the
    /// ended session and reactivate it instead of silently dropping the message.
    pub fn get_session_by_thread_id_any_status(&self, thread_id: i64) -> Result<Option<Session>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT * FROM sessions
                 WHERE thread_id = ?1
                 ORDER BY last_activity DESC
                 LIMIT 1",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![thread_id], row_to_session)
            .map_err(|e| AppError::Database(e.to_string()))?;

        match rows.next() {
            Some(Ok(s)) => Ok(Some(s)),
            Some(Err(e)) => Err(AppError::Database(e.to_string())),
            None => Ok(None),
        }
    }

    #[allow(dead_code)] // Library API
    pub fn get_session_by_chat_id(&self, chat_id: i64) -> Result<Option<Session>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT * FROM sessions
                 WHERE chat_id = ?1 AND status = 'active'
                 ORDER BY last_activity DESC
                 LIMIT 1",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![chat_id], row_to_session)
            .map_err(|e| AppError::Database(e.to_string()))?;

        match rows.next() {
            Some(Ok(s)) => Ok(Some(s)),
            Some(Err(e)) => Err(AppError::Database(e.to_string())),
            None => Ok(None),
        }
    }

    pub fn get_active_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT * FROM sessions WHERE status = 'active'
                 ORDER BY last_activity DESC",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let rows = stmt
            .query_map([], row_to_session)
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| AppError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn update_activity(&self, session_id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sessions SET last_activity = ?1 WHERE id = ?2",
                params![now_iso(), session_id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    /// End a session and expire its pending approvals.
    ///
    /// Both SQL statements execute inside a single transaction so that a crash
    /// between them cannot leave the session marked ended while its approvals
    /// remain pending (C-4 atomicity fix).
    pub fn end_session(&self, session_id: &str, status: SessionStatus) -> Result<()> {
        let now = now_iso();
        self.conn
            .execute_batch("BEGIN")
            .map_err(|e| AppError::Database(e.to_string()))?;

        let result = (|| -> Result<()> {
            self.conn
                .execute(
                    "UPDATE sessions SET status = ?1, last_activity = ?2 WHERE id = ?3",
                    params![status.as_str(), now, session_id],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;

            self.conn
                .execute(
                    "UPDATE pending_approvals
                     SET status = 'expired'
                     WHERE session_id = ?1 AND status = 'pending'",
                    params![session_id],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;

            Ok(())
        })();

        match result {
            Ok(()) => {
                self.conn
                    .execute_batch("COMMIT")
                    .map_err(|e| AppError::Database(e.to_string()))?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// BUG-009: Reactivate an ended/aborted session.
    pub fn reactivate_session(&self, session_id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sessions SET status = 'active', last_activity = ?1 WHERE id = ?2",
                params![now_iso(), session_id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    /// ADR-013 F2: Update tmux info for a session.  Checks that the UPDATE
    /// affected at least 1 row; logs a warning if the session row does not
    /// exist (the row should exist by now if `create_session` ran first).
    pub fn set_tmux_info(
        &self,
        session_id: &str,
        tmux_target: Option<&str>,
        tmux_socket: Option<&str>,
    ) -> Result<()> {
        let rows_changed = match (tmux_target, tmux_socket) {
            (Some(t), Some(s)) => {
                self.conn
                    .execute(
                        "UPDATE sessions SET tmux_target = ?1, tmux_socket = ?2 WHERE id = ?3",
                        params![t, s, session_id],
                    )
                    .map_err(|e| AppError::Database(e.to_string()))?
            }
            (Some(t), None) => {
                self.conn
                    .execute(
                        "UPDATE sessions SET tmux_target = ?1 WHERE id = ?2",
                        params![t, session_id],
                    )
                    .map_err(|e| AppError::Database(e.to_string()))?
            }
            (None, Some(s)) => {
                self.conn
                    .execute(
                        "UPDATE sessions SET tmux_socket = ?1 WHERE id = ?2",
                        params![s, session_id],
                    )
                    .map_err(|e| AppError::Database(e.to_string()))?
            }
            (None, None) => return Ok(()),
        };

        if rows_changed == 0 {
            tracing::warn!(
                session_id = %session_id,
                "set_tmux_info: UPDATE affected 0 rows — session row does not exist yet"
            );
        }
        Ok(())
    }

    pub fn get_tmux_info(&self, session_id: &str) -> Result<Option<(String, Option<String>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT tmux_target, tmux_socket FROM sessions WHERE id = ?1")
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![session_id], |row| {
                let target: Option<String> = row.get(0)?;
                let socket: Option<String> = row.get(1)?;
                Ok((target, socket))
            })
            .map_err(|e| AppError::Database(e.to_string()))?;

        match rows.next() {
            Some(Ok((Some(target), socket))) => Ok(Some((target, socket))),
            Some(Ok((None, _))) => Ok(None),
            Some(Err(e)) => Err(AppError::Database(e.to_string())),
            None => Ok(None),
        }
    }

    /// ADR-013: Store parent_session_id, agent_id, and agent_type for a child (sub-agent) session.
    #[allow(dead_code)] // Library API — used by daemon routing (ADR-013)
    pub fn set_parent_info(
        &self,
        session_id: &str,
        parent_session_id: &str,
        agent_id: Option<&str>,
        agent_type: Option<&str>,
    ) -> Result<()> {
        let rows_changed = self
            .conn
            .execute(
                "UPDATE sessions SET parent_session_id = ?1, agent_id = ?2, agent_type = ?3 WHERE id = ?4",
                params![parent_session_id, agent_id, agent_type, session_id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        if rows_changed == 0 {
            tracing::warn!(
                session_id = %session_id,
                parent_session_id = %parent_session_id,
                "set_parent_info: UPDATE affected 0 rows — session row does not exist"
            );
        }
        Ok(())
    }

    /// ADR-013 GAP-5: Get all active child sessions for a parent session.
    /// Used by handle_session_end to cascade session end to children.
    #[allow(dead_code)]
    pub fn get_child_sessions(&self, parent_session_id: &str) -> Result<Vec<Session>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT * FROM sessions WHERE parent_session_id = ?1 AND status = 'active'",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let rows = stmt
            .query_map(params![parent_session_id], row_to_session)
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| AppError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    // ------------------------------------------------------------ approvals

    /// Create a pending approval, returning its ID.
    pub fn create_approval(
        &self,
        session_id: &str,
        prompt: &str,
        message_id: Option<i64>,
    ) -> Result<String> {
        if !crate::types::is_valid_session_id(session_id) {
            return Err(AppError::Config(format!(
                "invalid session_id: {}",
                session_id
            )));
        }

        let id = generate_id("approval");
        let now = chrono::Utc::now();
        let created = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let expires = (now
            + chrono::TimeDelta::try_milliseconds(self.approval_timeout_ms)
                .unwrap_or(chrono::TimeDelta::zero()))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        self.conn
            .execute(
                "INSERT INTO pending_approvals
                 (id, session_id, prompt, created_at, expires_at, status, message_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6)",
                params![id, session_id, prompt, created, expires, message_id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(id)
    }

    pub fn get_approval(&self, approval_id: &str) -> Result<Option<PendingApproval>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM pending_approvals WHERE id = ?1")
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![approval_id], row_to_approval)
            .map_err(|e| AppError::Database(e.to_string()))?;

        match rows.next() {
            Some(Ok(a)) => Ok(Some(a)),
            Some(Err(e)) => Err(AppError::Database(e.to_string())),
            None => Ok(None),
        }
    }

    #[allow(dead_code)] // Library API
    pub fn get_pending_approvals(&self, session_id: &str) -> Result<Vec<PendingApproval>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT * FROM pending_approvals
                 WHERE session_id = ?1 AND status = 'pending'
                 ORDER BY created_at DESC",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let rows = stmt
            .query_map(params![session_id], row_to_approval)
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| AppError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    /// Resolve an approval; returns true if a row was actually updated.
    pub fn resolve_approval(&self, approval_id: &str, status: ApprovalStatus) -> Result<bool> {
        let changed = self
            .conn
            .execute(
                "UPDATE pending_approvals
                 SET status = ?1
                 WHERE id = ?2 AND status = 'pending'",
                params![status.as_str(), approval_id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(changed > 0)
    }

    pub fn expire_old_approvals(&self) -> Result<usize> {
        let now = now_iso();
        let changed = self
            .conn
            .execute(
                "UPDATE pending_approvals
                 SET status = 'expired'
                 WHERE status = 'pending' AND expires_at < ?1",
                params![now],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(changed)
    }

    // -------------------------------------------------------------- cleanup

    pub fn get_stale_session_candidates(&self, timeout_hours: u32) -> Result<Vec<Session>> {
        let cutoff = chrono::Utc::now()
            - chrono::TimeDelta::try_hours(i64::from(timeout_hours))
                .unwrap_or(chrono::TimeDelta::hours(24));
        let cutoff_iso = cutoff.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        let mut stmt = self
            .conn
            .prepare(
                "SELECT * FROM sessions
                 WHERE status = 'active' AND last_activity < ?1
                 ORDER BY last_activity ASC",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let rows = stmt
            .query_map(params![cutoff_iso], row_to_session)
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| AppError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn get_orphaned_thread_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT * FROM sessions
                 WHERE status = 'ended' AND thread_id IS NOT NULL
                 ORDER BY last_activity ASC",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let rows = stmt
            .query_map([], row_to_session)
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| AppError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn is_tmux_target_owned_by_other(
        &self,
        tmux_target: &str,
        exclude_session_id: &str,
    ) -> Result<bool> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id FROM sessions
                 WHERE tmux_target = ?1 AND status = 'active' AND id != ?2
                 LIMIT 1",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut rows = stmt
            .query_map(params![tmux_target, exclude_session_id], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(rows.next().is_some())
    }

    /// Test helper: overwrite `last_activity` for a session via raw SQL.
    ///
    /// This exists so that integration tests can simulate stale sessions
    /// without accessing the private `conn` field.
    #[doc(hidden)]
    #[allow(dead_code)] // Used by integration tests only
    pub fn test_set_last_activity(&self, session_id: &str, iso_timestamp: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE sessions SET last_activity = ?1 WHERE id = ?2",
                params![iso_timestamp, session_id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    #[allow(dead_code)] // Library API
    pub fn cleanup_old_sessions(&self, max_age_days: u32) -> Result<usize> {
        let cutoff = chrono::Utc::now()
            - chrono::TimeDelta::try_days(i64::from(max_age_days))
                .unwrap_or(chrono::TimeDelta::days(30));
        let cutoff_iso = cutoff.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        // Delete old approvals first (foreign key)
        self.conn
            .execute(
                "DELETE FROM pending_approvals
                 WHERE session_id IN (
                     SELECT id FROM sessions WHERE last_activity < ?1
                 )",
                params![cutoff_iso],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let deleted = self
            .conn
            .execute(
                "DELETE FROM sessions WHERE last_activity < ?1",
                params![cutoff_iso],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(deleted)
    }

    /// Returns `(active_count, pending_approval_count)`.
    pub fn get_stats(&self) -> Result<(usize, usize)> {
        let active: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE status = 'active'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let pending: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pending_approvals WHERE status = 'pending'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok((
            usize::try_from(active).unwrap_or(0),
            usize::try_from(pending).unwrap_or(0),
        ))
    }
}

// ------------------------------------------------------------------ helpers

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let status_str: String = row.get("status")?;
    let status = SessionStatus::try_from(status_str.as_str()).unwrap_or(SessionStatus::Active); // Fallback for unknown DB values
    Ok(Session {
        id: row.get("id")?,
        chat_id: row.get("chat_id")?,
        thread_id: row.get("thread_id")?,
        hostname: row.get("hostname")?,
        tmux_target: row.get("tmux_target")?,
        tmux_socket: row.get("tmux_socket")?,
        started_at: row.get("started_at")?,
        last_activity: row.get("last_activity")?,
        status,
        project_dir: row.get("project_dir")?,
        metadata: row.get("metadata")?,
        // ADR-013: Parent-child session fields (migrated columns, always present after init_schema)
        parent_session_id: row.get("parent_session_id")?,
        agent_id: row.get("agent_id")?,
        agent_type: row.get::<_, Option<String>>("agent_type").unwrap_or(None),
    })
}

fn row_to_approval(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingApproval> {
    let status_str: String = row.get("status")?;
    let status = ApprovalStatus::try_from(status_str.as_str()).unwrap_or(ApprovalStatus::Pending); // Fallback for unknown DB values
    Ok(PendingApproval {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        prompt: row.get("prompt")?,
        created_at: row.get("created_at")?,
        expires_at: row.get("expires_at")?,
        status,
        message_id: row.get("message_id")?,
    })
}

// ------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Create a `SessionManager` backed by a temporary on-disk SQLite database.
    ///
    /// The returned `TempDir` must be kept alive for the duration of the test;
    /// dropping it deletes the directory and its database file.
    fn make_mgr() -> (SessionManager, tempfile::TempDir) {
        let tmp = tempdir().expect("tempdir");
        let mgr = SessionManager::new(tmp.path(), 5).expect("SessionManager::new");
        (mgr, tmp)
    }

}
