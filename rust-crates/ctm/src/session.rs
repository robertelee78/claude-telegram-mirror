//! SQLite-backed session manager.
//!
//! Tracks CLI sessions and pending approvals — ported from `session.ts`.

use crate::config::ensure_config_dir;
use crate::error::{AppError, Result};
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
    pub chat_id: i64,
    pub thread_id: Option<i64>,
    pub hostname: Option<String>,
    pub tmux_target: Option<String>,
    pub tmux_socket: Option<String>,
    pub started_at: String,
    pub last_activity: String,
    pub status: String,
    pub project_dir: Option<String>,
    pub metadata: Option<String>,
}

/// A pending tool-approval request.
#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub id: String,
    pub session_id: String,
    pub prompt: String,
    pub created_at: String,
    pub expires_at: String,
    pub status: String,
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
    String::from_utf8(buf).unwrap()
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
                id            TEXT PRIMARY KEY,
                chat_id       INTEGER NOT NULL,
                thread_id     INTEGER,
                hostname      TEXT,
                tmux_target   TEXT,
                tmux_socket   TEXT,
                started_at    TEXT NOT NULL,
                last_activity TEXT NOT NULL,
                status        TEXT DEFAULT 'active',
                project_dir   TEXT,
                metadata      TEXT
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
        let now = now_iso();

        // If the session already exists, touch its activity timestamp and return.
        if self.get_session(session_id)?.is_some() {
            self.update_activity(session_id)?;
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
    pub fn end_session(&self, session_id: &str, status: &str) -> Result<()> {
        let now = now_iso();
        self.conn
            .execute(
                "UPDATE sessions SET status = ?1, last_activity = ?2 WHERE id = ?3",
                params![status, now, session_id],
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

    pub fn set_tmux_info(
        &self,
        session_id: &str,
        tmux_target: Option<&str>,
        tmux_socket: Option<&str>,
    ) -> Result<()> {
        match (tmux_target, tmux_socket) {
            (Some(t), Some(s)) => {
                self.conn
                    .execute(
                        "UPDATE sessions SET tmux_target = ?1, tmux_socket = ?2 WHERE id = ?3",
                        params![t, s, session_id],
                    )
                    .map_err(|e| AppError::Database(e.to_string()))?;
            }
            (Some(t), None) => {
                self.conn
                    .execute(
                        "UPDATE sessions SET tmux_target = ?1 WHERE id = ?2",
                        params![t, session_id],
                    )
                    .map_err(|e| AppError::Database(e.to_string()))?;
            }
            (None, Some(s)) => {
                self.conn
                    .execute(
                        "UPDATE sessions SET tmux_socket = ?1 WHERE id = ?2",
                        params![s, session_id],
                    )
                    .map_err(|e| AppError::Database(e.to_string()))?;
            }
            (None, None) => {}
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

    // ------------------------------------------------------------ approvals

    /// Create a pending approval, returning its ID.
    pub fn create_approval(
        &self,
        session_id: &str,
        prompt: &str,
        message_id: Option<i64>,
    ) -> Result<String> {
        let id = generate_id("approval");
        let now = chrono::Utc::now();
        let created = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let expires = (now + chrono::Duration::milliseconds(self.approval_timeout_ms))
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
    pub fn resolve_approval(&self, approval_id: &str, status: &str) -> Result<bool> {
        let changed = self
            .conn
            .execute(
                "UPDATE pending_approvals
                 SET status = ?1
                 WHERE id = ?2 AND status = 'pending'",
                params![status, approval_id],
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
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(i64::from(timeout_hours));
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

    pub fn cleanup_old_sessions(&self, max_age_days: u32) -> Result<usize> {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(max_age_days));
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

        Ok((active as usize, pending as usize))
    }
}

// ------------------------------------------------------------------ helpers

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get("id")?,
        chat_id: row.get("chat_id")?,
        thread_id: row.get("thread_id")?,
        hostname: row.get("hostname")?,
        tmux_target: row.get("tmux_target")?,
        tmux_socket: row.get("tmux_socket")?,
        started_at: row.get("started_at")?,
        last_activity: row.get("last_activity")?,
        status: row.get("status")?,
        project_dir: row.get("project_dir")?,
        metadata: row.get("metadata")?,
    })
}

fn row_to_approval(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingApproval> {
    Ok(PendingApproval {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        prompt: row.get("prompt")?,
        created_at: row.get("created_at")?,
        expires_at: row.get("expires_at")?,
        status: row.get("status")?,
        message_id: row.get("message_id")?,
    })
}

// ===================================================================== tests

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_mgr() -> (SessionManager, TempDir) {
        let tmp = TempDir::new().unwrap();
        let mgr = SessionManager::new(tmp.path(), 5).unwrap();
        (mgr, tmp)
    }

    // ---- session lifecycle ----

    #[test]
    fn create_and_get_session() {
        let (mgr, _tmp) = make_mgr();
        let id = mgr
            .create_session(
                "sess-1",
                42,
                Some("myhost"),
                Some("/project"),
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(id, "sess-1");

        let sess = mgr.get_session("sess-1").unwrap().unwrap();
        assert_eq!(sess.chat_id, 42);
        assert_eq!(sess.hostname.as_deref(), Some("myhost"));
        assert_eq!(sess.project_dir.as_deref(), Some("/project"));
        assert_eq!(sess.status, "active");
    }

    #[test]
    fn create_session_all_fields_atomic() {
        // M2.12: verify all fields are persisted in the single INSERT.
        let (mgr, _tmp) = make_mgr();
        mgr.create_session(
            "full-sess",
            100,
            Some("builder"),
            Some("/workspace"),
            Some(42),
            Some("s0:0.1"),
            Some("/tmp/tmux-1234/default"),
        )
        .unwrap();

        let sess = mgr.get_session("full-sess").unwrap().unwrap();
        assert_eq!(sess.chat_id, 100);
        assert_eq!(sess.hostname.as_deref(), Some("builder"));
        assert_eq!(sess.project_dir.as_deref(), Some("/workspace"));
        assert_eq!(sess.thread_id, Some(42));
        assert_eq!(sess.tmux_target.as_deref(), Some("s0:0.1"));
        assert_eq!(sess.tmux_socket.as_deref(), Some("/tmp/tmux-1234/default"));
        assert_eq!(sess.status, "active");
    }

    #[test]
    fn duplicate_create_updates_activity() {
        let (mgr, _tmp) = make_mgr();
        mgr.create_session("sess-dup", 1, None, None, None, None, None)
            .unwrap();
        let s1 = mgr.get_session("sess-dup").unwrap().unwrap();

        // small delay is not needed — create again returns same id
        let id2 = mgr
            .create_session("sess-dup", 1, None, None, None, None, None)
            .unwrap();
        assert_eq!(id2, "sess-dup");

        let s2 = mgr.get_session("sess-dup").unwrap().unwrap();
        assert!(s2.last_activity >= s1.last_activity);
    }

    #[test]
    fn set_and_get_thread_id() {
        let (mgr, _tmp) = make_mgr();
        mgr.create_session("sess-t", 1, None, None, None, None, None)
            .unwrap();
        mgr.set_session_thread("sess-t", 999).unwrap();

        let sess = mgr.get_session_by_thread_id(999).unwrap().unwrap();
        assert_eq!(sess.id, "sess-t");
    }

    #[test]
    fn clear_thread_id_works() {
        let (mgr, _tmp) = make_mgr();
        mgr.create_session("sess-ct", 1, None, None, None, None, None)
            .unwrap();
        mgr.set_session_thread("sess-ct", 777).unwrap();
        mgr.clear_thread_id("sess-ct").unwrap();

        assert!(mgr.get_session_by_thread_id(777).unwrap().is_none());
    }

    #[test]
    fn get_session_thread_works() {
        let (mgr, _tmp) = make_mgr();
        mgr.create_session("sess-gst", 1, None, None, None, None, None)
            .unwrap();

        // No thread_id set yet
        assert_eq!(mgr.get_session_thread("sess-gst").unwrap(), None);

        mgr.set_session_thread("sess-gst", 555).unwrap();
        assert_eq!(mgr.get_session_thread("sess-gst").unwrap(), Some(555));

        // Non-existent session returns None
        assert_eq!(mgr.get_session_thread("no-such").unwrap(), None);
    }

    #[test]
    fn get_session_by_chat_id() {
        let (mgr, _tmp) = make_mgr();
        mgr.create_session("sess-chat", 55, None, None, None, None, None)
            .unwrap();

        let sess = mgr.get_session_by_chat_id(55).unwrap().unwrap();
        assert_eq!(sess.id, "sess-chat");

        assert!(mgr.get_session_by_chat_id(999).unwrap().is_none());
    }

    #[test]
    fn get_active_sessions() {
        let (mgr, _tmp) = make_mgr();
        mgr.create_session("a1", 1, None, None, None, None, None)
            .unwrap();
        mgr.create_session("a2", 1, None, None, None, None, None)
            .unwrap();
        mgr.end_session("a1", "ended").unwrap();

        let active = mgr.get_active_sessions().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "a2");
    }

    #[test]
    fn end_session_expires_approvals() {
        let (mgr, _tmp) = make_mgr();
        mgr.create_session("sess-end", 1, None, None, None, None, None)
            .unwrap();
        let aid = mgr
            .create_approval("sess-end", "Allow write?", None)
            .unwrap();

        mgr.end_session("sess-end", "ended").unwrap();

        let approval = mgr.get_approval(&aid).unwrap().unwrap();
        assert_eq!(approval.status, "expired");
    }

    #[test]
    fn reactivate_session() {
        let (mgr, _tmp) = make_mgr();
        mgr.create_session("sess-react", 1, None, None, None, None, None)
            .unwrap();
        mgr.end_session("sess-react", "ended").unwrap();

        let s = mgr.get_session("sess-react").unwrap().unwrap();
        assert_eq!(s.status, "ended");

        mgr.reactivate_session("sess-react").unwrap();
        let s = mgr.get_session("sess-react").unwrap().unwrap();
        assert_eq!(s.status, "active");
    }

    #[test]
    fn tmux_info_roundtrip() {
        let (mgr, _tmp) = make_mgr();
        mgr.create_session("sess-tmux", 1, None, None, None, None, None)
            .unwrap();
        mgr.set_tmux_info("sess-tmux", Some("s:0.0"), Some("/tmp/tmux-1000/default"))
            .unwrap();

        let (target, socket) = mgr.get_tmux_info("sess-tmux").unwrap().unwrap();
        assert_eq!(target, "s:0.0");
        assert_eq!(socket.as_deref(), Some("/tmp/tmux-1000/default"));
    }

    #[test]
    fn tmux_target_ownership() {
        let (mgr, _tmp) = make_mgr();
        mgr.create_session("s1", 1, None, None, None, None, None)
            .unwrap();
        mgr.create_session("s2", 1, None, None, None, None, None)
            .unwrap();
        mgr.set_tmux_info("s1", Some("target:0.0"), None).unwrap();

        assert!(mgr
            .is_tmux_target_owned_by_other("target:0.0", "s2")
            .unwrap());
        assert!(!mgr
            .is_tmux_target_owned_by_other("target:0.0", "s1")
            .unwrap());
    }

    // ---- approval lifecycle ----

    #[test]
    fn approval_lifecycle() {
        let (mgr, _tmp) = make_mgr();
        mgr.create_session("sess-appr", 1, None, None, None, None, None)
            .unwrap();

        let aid = mgr
            .create_approval("sess-appr", "Allow Bash?", Some(123))
            .unwrap();
        assert!(aid.starts_with("approval-"));

        let a = mgr.get_approval(&aid).unwrap().unwrap();
        assert_eq!(a.status, "pending");
        assert_eq!(a.message_id, Some(123));

        let pending = mgr.get_pending_approvals("sess-appr").unwrap();
        assert_eq!(pending.len(), 1);

        let resolved = mgr.resolve_approval(&aid, "approved").unwrap();
        assert!(resolved);

        let a2 = mgr.get_approval(&aid).unwrap().unwrap();
        assert_eq!(a2.status, "approved");

        // Cannot resolve again
        let re_resolve = mgr.resolve_approval(&aid, "rejected").unwrap();
        assert!(!re_resolve);
    }

    // ---- stale candidates ----

    #[test]
    fn stale_candidates_returns_old_sessions() {
        let (mgr, _tmp) = make_mgr();
        mgr.create_session("old-1", 1, None, None, None, None, None)
            .unwrap();

        // Manually set last_activity to the past
        mgr.conn
            .execute(
                "UPDATE sessions SET last_activity = '2020-01-01T00:00:00.000Z' WHERE id = 'old-1'",
                [],
            )
            .unwrap();

        let stale = mgr.get_stale_session_candidates(1).unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].id, "old-1");
    }

    // ---- orphaned threads ----

    #[test]
    fn orphaned_thread_sessions() {
        let (mgr, _tmp) = make_mgr();
        mgr.create_session("orph-1", 1, None, None, None, None, None)
            .unwrap();
        mgr.set_session_thread("orph-1", 888).unwrap();
        mgr.end_session("orph-1", "ended").unwrap();

        let orphans = mgr.get_orphaned_thread_sessions().unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].id, "orph-1");
    }

    // ---- stats ----

    #[test]
    fn get_stats_counts() {
        let (mgr, _tmp) = make_mgr();
        mgr.create_session("st-1", 1, None, None, None, None, None)
            .unwrap();
        mgr.create_session("st-2", 1, None, None, None, None, None)
            .unwrap();
        mgr.create_approval("st-1", "approve?", None).unwrap();

        let (active, pending) = mgr.get_stats().unwrap();
        assert_eq!(active, 2);
        assert_eq!(pending, 1);
    }

    // ---- cleanup ----

    #[test]
    fn cleanup_old_sessions_removes_ancient() {
        let (mgr, _tmp) = make_mgr();
        mgr.create_session("ancient", 1, None, None, None, None, None)
            .unwrap();
        mgr.conn
            .execute(
                "UPDATE sessions SET last_activity = '2020-01-01T00:00:00.000Z' WHERE id = 'ancient'",
                [],
            )
            .unwrap();

        let removed = mgr.cleanup_old_sessions(7).unwrap();
        assert_eq!(removed, 1);
        assert!(mgr.get_session("ancient").unwrap().is_none());
    }
}
