use crate::error::{AppError, Result};
use crate::types::{ApprovalStatus, PendingApproval, Session, SessionStatus};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub struct SessionManager {
    db: Connection,
    approval_timeout_ms: u64,
}

impl SessionManager {
    pub fn new(config_dir: &Path, approval_timeout_minutes: u64) -> Result<Self> {
        // Ensure config directory exists with secure permissions
        if !config_dir.exists() {
            fs::create_dir_all(config_dir)?;
        }
        fs::set_permissions(config_dir, fs::Permissions::from_mode(0o700))?;

        let db_path = config_dir.join("sessions.db");
        let db = Connection::open(&db_path)?;

        // Security fix #3: Set database file permissions to 0o600
        if db_path.exists() {
            fs::set_permissions(&db_path, fs::Permissions::from_mode(0o600))?;
        }

        let mut mgr = Self {
            db,
            approval_timeout_ms: approval_timeout_minutes * 60 * 1000,
        };
        mgr.init_schema()?;
        tracing::info!(?db_path, "Session manager initialized");
        Ok(mgr)
    }

    fn init_schema(&mut self) -> Result<()> {
        self.db.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                chat_id INTEGER NOT NULL,
                thread_id INTEGER,
                hostname TEXT,
                tmux_target TEXT,
                tmux_socket TEXT,
                started_at TEXT NOT NULL,
                last_activity TEXT NOT NULL,
                status TEXT DEFAULT 'active',
                project_dir TEXT
            );

            CREATE TABLE IF NOT EXISTS pending_approvals (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                prompt TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                status TEXT DEFAULT 'pending',
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );

            CREATE INDEX IF NOT EXISTS idx_sessions_chat ON sessions(chat_id);
            CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);
            CREATE INDEX IF NOT EXISTS idx_approvals_session ON pending_approvals(session_id);
            CREATE INDEX IF NOT EXISTS idx_approvals_status ON pending_approvals(status);",
        )?;
        self.migrate_add_tmux_columns()?;
        Ok(())
    }

    fn migrate_add_tmux_columns(&self) -> Result<()> {
        let columns: Vec<String> = self
            .db
            .prepare("PRAGMA table_info(sessions)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();

        if !columns.iter().any(|c| c == "tmux_target") {
            self.db
                .execute("ALTER TABLE sessions ADD COLUMN tmux_target TEXT", [])?;
            tracing::info!("Migration: added tmux_target column");
        }
        if !columns.iter().any(|c| c == "tmux_socket") {
            self.db
                .execute("ALTER TABLE sessions ADD COLUMN tmux_socket TEXT", [])?;
            tracing::info!("Migration: added tmux_socket column");
        }
        Ok(())
    }

    // ============ Session Methods ============

    pub fn create_session(
        &self,
        session_id: Option<&str>,
        chat_id: i64,
        project_dir: Option<&str>,
        hostname: Option<&str>,
        tmux_target: Option<&str>,
        tmux_socket: Option<&str>,
    ) -> Result<String> {
        let id = session_id
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("session-{}", Uuid::new_v4()));
        let now = Utc::now().to_rfc3339();

        // Check if session already exists
        if self.get_session(&id).is_some() {
            tracing::info!(session_id = %id, "Session already exists, updating activity");
            self.update_activity(&id);
            if tmux_target.is_some() || tmux_socket.is_some() {
                self.set_tmux_info(&id, tmux_target, tmux_socket);
            }
            return Ok(id);
        }

        self.db.execute(
            "INSERT INTO sessions (id, chat_id, hostname, tmux_target, tmux_socket, started_at, last_activity, status, project_dir)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', ?8)",
            params![id, chat_id, hostname, tmux_target, tmux_socket, now, now, project_dir],
        )?;

        tracing::info!(session_id = %id, %chat_id, "Session created");
        Ok(id)
    }

    pub fn get_session(&self, session_id: &str) -> Option<Session> {
        self.db
            .query_row(
                "SELECT * FROM sessions WHERE id = ?1",
                params![session_id],
                |row| Self::row_to_session(row),
            )
            .ok()
    }

    pub fn get_session_by_thread_id(&self, thread_id: i64) -> Option<Session> {
        self.db
            .query_row(
                "SELECT * FROM sessions WHERE thread_id = ?1 AND status = 'active' LIMIT 1",
                params![thread_id],
                |row| Self::row_to_session(row),
            )
            .ok()
    }

    pub fn get_active_sessions(&self) -> Vec<Session> {
        let mut stmt = self
            .db
            .prepare("SELECT * FROM sessions WHERE status = 'active' ORDER BY last_activity DESC")
            .unwrap();
        stmt.query_map([], |row| Self::row_to_session(row))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    pub fn set_session_thread(&self, session_id: &str, thread_id: i64) {
        let _ = self.db.execute(
            "UPDATE sessions SET thread_id = ?1 WHERE id = ?2",
            params![thread_id, session_id],
        );
    }

    pub fn get_session_thread(&self, session_id: &str) -> Option<i64> {
        self.db
            .query_row(
                "SELECT thread_id FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten()
    }

    pub fn clear_thread_id(&self, session_id: &str) {
        let _ = self.db.execute(
            "UPDATE sessions SET thread_id = NULL WHERE id = ?1",
            params![session_id],
        );
    }

    pub fn set_tmux_info(
        &self,
        session_id: &str,
        tmux_target: Option<&str>,
        tmux_socket: Option<&str>,
    ) {
        if let Some(target) = tmux_target {
            let _ = self.db.execute(
                "UPDATE sessions SET tmux_target = ?1 WHERE id = ?2",
                params![target, session_id],
            );
        }
        if let Some(socket) = tmux_socket {
            let _ = self.db.execute(
                "UPDATE sessions SET tmux_socket = ?1 WHERE id = ?2",
                params![socket, session_id],
            );
        }
    }

    pub fn get_tmux_info(&self, session_id: &str) -> (Option<String>, Option<String>) {
        self.db
            .query_row(
                "SELECT tmux_target, tmux_socket FROM sessions WHERE id = ?1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .unwrap_or((None, None))
    }

    pub fn update_activity(&self, session_id: &str) {
        let now = Utc::now().to_rfc3339();
        let _ = self.db.execute(
            "UPDATE sessions SET last_activity = ?1 WHERE id = ?2",
            params![now, session_id],
        );
    }

    pub fn end_session(&self, session_id: &str, status: &str) {
        let now = Utc::now().to_rfc3339();
        let _ = self.db.execute(
            "UPDATE sessions SET status = ?1, last_activity = ?2 WHERE id = ?3",
            params![status, now, session_id],
        );
        let _ = self.db.execute(
            "UPDATE pending_approvals SET status = 'expired' WHERE session_id = ?1 AND status = 'pending'",
            params![session_id],
        );
        tracing::info!(%session_id, %status, "Session ended");
    }

    pub fn reactivate_session(&self, session_id: &str) {
        let now = Utc::now().to_rfc3339();
        let _ = self.db.execute(
            "UPDATE sessions SET status = 'active', last_activity = ?1 WHERE id = ?2",
            params![now, session_id],
        );
        tracing::info!(%session_id, "Session reactivated");
    }

    // ============ Approval Methods ============

    pub fn create_approval(&self, session_id: &str, prompt: &str) -> Result<String> {
        let id = format!("approval-{}", Uuid::new_v4());
        let now = Utc::now();
        let expires_at = now + chrono::Duration::milliseconds(self.approval_timeout_ms as i64);

        self.db.execute(
            "INSERT INTO pending_approvals (id, session_id, prompt, created_at, expires_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
            params![
                id,
                session_id,
                prompt,
                now.to_rfc3339(),
                expires_at.to_rfc3339()
            ],
        )?;

        tracing::info!(approval_id = %id, %session_id, "Approval created");
        Ok(id)
    }

    pub fn get_approval(&self, approval_id: &str) -> Option<PendingApproval> {
        self.db
            .query_row(
                "SELECT * FROM pending_approvals WHERE id = ?1",
                params![approval_id],
                |row| {
                    Ok(PendingApproval {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        prompt: row.get(2)?,
                        created_at: parse_datetime(&row.get::<_, String>(3)?),
                        expires_at: parse_datetime(&row.get::<_, String>(4)?),
                        status: ApprovalStatus::from_str(&row.get::<_, String>(5)?),
                    })
                },
            )
            .ok()
    }

    pub fn resolve_approval(&self, approval_id: &str, status: &str) -> bool {
        let result = self.db.execute(
            "UPDATE pending_approvals SET status = ?1 WHERE id = ?2 AND status = 'pending'",
            params![status, approval_id],
        );
        match result {
            Ok(n) => {
                if n > 0 {
                    tracing::info!(%approval_id, %status, "Approval resolved");
                    true
                } else {
                    false
                }
            }
            Err(_) => false,
        }
    }

    pub fn expire_old_approvals(&self) -> usize {
        let now = Utc::now().to_rfc3339();
        self.db
            .execute(
                "UPDATE pending_approvals SET status = 'expired' WHERE status = 'pending' AND expires_at < ?1",
                params![now],
            )
            .unwrap_or(0)
    }

    // ============ Cleanup Methods ============

    pub fn get_stale_session_candidates(&self, timeout_hours: u64) -> Vec<Session> {
        let cutoff = Utc::now() - chrono::Duration::hours(timeout_hours as i64);
        let mut stmt = self
            .db
            .prepare(
                "SELECT * FROM sessions WHERE status = 'active' AND last_activity < ?1 ORDER BY last_activity ASC",
            )
            .unwrap();
        stmt.query_map(params![cutoff.to_rfc3339()], |row| {
            Self::row_to_session(row)
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn get_orphaned_thread_sessions(&self) -> Vec<Session> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT * FROM sessions WHERE status = 'ended' AND thread_id IS NOT NULL ORDER BY last_activity ASC",
            )
            .unwrap();
        stmt.query_map([], |row| Self::row_to_session(row))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    pub fn is_tmux_target_owned_by_other(&self, tmux_target: &str, exclude_session_id: &str) -> bool {
        self.db
            .query_row(
                "SELECT id FROM sessions WHERE tmux_target = ?1 AND status = 'active' AND id != ?2 LIMIT 1",
                params![tmux_target, exclude_session_id],
                |_row| Ok(()),
            )
            .is_ok()
    }

    pub fn get_stats(&self) -> (usize, usize) {
        let active: usize = self
            .db
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE status = 'active'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let pending: usize = self
            .db
            .query_row(
                "SELECT COUNT(*) FROM pending_approvals WHERE status = 'pending'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        (active, pending)
    }

    fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<Session> {
        Ok(Session {
            id: row.get("id")?,
            chat_id: row.get("chat_id")?,
            thread_id: row.get("thread_id")?,
            hostname: row.get("hostname")?,
            project_dir: row.get("project_dir")?,
            tmux_target: row.get("tmux_target")?,
            tmux_socket: row.get("tmux_socket")?,
            started_at: parse_datetime(&row.get::<_, String>("started_at")?),
            last_activity: parse_datetime(&row.get::<_, String>("last_activity")?),
            status: SessionStatus::from_str(&row.get::<_, String>("status")?),
        })
    }
}

fn parse_datetime(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_manager() -> (SessionManager, TempDir) {
        let dir = TempDir::new().unwrap();
        let mgr = SessionManager::new(dir.path(), 5).unwrap();
        (mgr, dir)
    }

    #[test]
    fn test_create_and_get_session() {
        let (mgr, _dir) = test_manager();
        let id = mgr
            .create_session(Some("test-session"), 12345, Some("/tmp"), None, None, None)
            .unwrap();
        assert_eq!(id, "test-session");

        let session = mgr.get_session("test-session").unwrap();
        assert_eq!(session.chat_id, 12345);
        assert_eq!(session.status, SessionStatus::Active);
    }

    #[test]
    fn test_session_lifecycle() {
        let (mgr, _dir) = test_manager();
        let id = mgr
            .create_session(None, 12345, None, None, None, None)
            .unwrap();

        mgr.end_session(&id, "ended");
        let session = mgr.get_session(&id).unwrap();
        assert_eq!(session.status, SessionStatus::Ended);

        mgr.reactivate_session(&id);
        let session = mgr.get_session(&id).unwrap();
        assert_eq!(session.status, SessionStatus::Active);
    }

    #[test]
    fn test_approval_lifecycle() {
        let (mgr, _dir) = test_manager();
        let session_id = mgr
            .create_session(None, 12345, None, None, None, None)
            .unwrap();

        let approval_id = mgr.create_approval(&session_id, "Allow Bash?").unwrap();
        let approval = mgr.get_approval(&approval_id).unwrap();
        assert_eq!(approval.status, ApprovalStatus::Pending);

        assert!(mgr.resolve_approval(&approval_id, "approved"));
        let approval = mgr.get_approval(&approval_id).unwrap();
        assert_eq!(approval.status, ApprovalStatus::Approved);
    }
}
