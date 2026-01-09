/**
 * Session Management with SQLite
 * Tracks CLI sessions and pending approvals
 */

import Database from 'better-sqlite3';
import { join } from 'path';
import { mkdirSync, existsSync } from 'fs';
import { homedir } from 'os';
import { randomBytes } from 'crypto';
import logger from '../utils/logger.js';
import type { Session, PendingApproval } from './types.js';

const CONFIG_DIR = join(homedir(), '.config', 'claude-telegram-mirror');
const DB_PATH = join(CONFIG_DIR, 'sessions.db');

/**
 * Generate a unique ID
 */
function generateId(prefix: string = ''): string {
  const timestamp = Date.now().toString(36);
  const random = randomBytes(4).toString('hex');
  return prefix ? `${prefix}-${timestamp}-${random}` : `${timestamp}-${random}`;
}

/**
 * Session Manager
 * Handles session and approval lifecycle
 */
export class SessionManager {
  private db: Database.Database;
  private approvalTimeout: number;

  constructor(dbPath: string = DB_PATH, approvalTimeoutMinutes: number = 5) {
    // Ensure config directory exists
    if (!existsSync(CONFIG_DIR)) {
      mkdirSync(CONFIG_DIR, { recursive: true });
    }

    this.db = new Database(dbPath);
    this.approvalTimeout = approvalTimeoutMinutes * 60 * 1000;
    this.initSchema();

    logger.info('Session manager initialized', { dbPath });
  }

  /**
   * Initialize database schema
   */
  private initSchema(): void {
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS sessions (
        id TEXT PRIMARY KEY,
        chat_id INTEGER NOT NULL,
        thread_id INTEGER,
        hostname TEXT,
        tmux_target TEXT,
        tmux_socket TEXT,
        started_at TEXT NOT NULL,
        last_activity TEXT NOT NULL,
        status TEXT DEFAULT 'active',
        project_dir TEXT,
        metadata TEXT
      );

      -- Migration: Add tmux columns if they don't exist (for existing DBs)
      -- SQLite doesn't support IF NOT EXISTS for ALTER TABLE, so we use a pragma check

      CREATE TABLE IF NOT EXISTS pending_approvals (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        prompt TEXT NOT NULL,
        created_at TEXT NOT NULL,
        expires_at TEXT NOT NULL,
        status TEXT DEFAULT 'pending',
        message_id INTEGER,
        FOREIGN KEY (session_id) REFERENCES sessions(id)
      );

      CREATE INDEX IF NOT EXISTS idx_sessions_chat ON sessions(chat_id);
      CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);
      CREATE INDEX IF NOT EXISTS idx_approvals_session ON pending_approvals(session_id);
      CREATE INDEX IF NOT EXISTS idx_approvals_status ON pending_approvals(status);
    `);

    // Migration: Add tmux_target column to existing databases
    this.migrateAddTmuxTarget();
  }

  /**
   * Migration: Add tmux columns to existing databases
   */
  private migrateAddTmuxTarget(): void {
    try {
      const tableInfo = this.db.prepare(`PRAGMA table_info(sessions)`).all() as Array<{ name: string }>;
      const columns = new Set(tableInfo.map(col => col.name));

      if (!columns.has('tmux_target')) {
        this.db.exec(`ALTER TABLE sessions ADD COLUMN tmux_target TEXT`);
        logger.info('Migration: Added tmux_target column to sessions table');
      }

      if (!columns.has('tmux_socket')) {
        this.db.exec(`ALTER TABLE sessions ADD COLUMN tmux_socket TEXT`);
        logger.info('Migration: Added tmux_socket column to sessions table');
      }
    } catch (error) {
      logger.debug('Migration check for tmux columns', { error });
    }
  }

  // ============ Session Methods ============

  /**
   * Create a new session with optional specific ID (for using Claude's native session_id)
   */
  createSession(
    chatId: number,
    projectDir?: string,
    threadId?: number,
    hostname?: string,
    sessionId?: string,
    tmuxTarget?: string,
    tmuxSocket?: string
  ): string {
    // Use provided sessionId (from Claude) or generate one
    const id = sessionId || generateId('session');
    const now = new Date().toISOString();

    // Check if session already exists
    const existing = this.getSession(id);
    if (existing) {
      logger.info('Session already exists, updating activity', { sessionId: id });
      this.updateActivity(id);
      // Update tmux info if provided and session exists
      if (tmuxTarget || tmuxSocket) {
        this.setTmuxInfo(id, tmuxTarget, tmuxSocket);
      }
      return id;
    }

    this.db.prepare(`
      INSERT INTO sessions (id, chat_id, thread_id, hostname, tmux_target, tmux_socket, started_at, last_activity, status, project_dir)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'active', ?)
    `).run(id, chatId, threadId || null, hostname || null, tmuxTarget || null, tmuxSocket || null, now, now, projectDir || null);

    logger.info('Session created', { sessionId: id, chatId, threadId, hostname, tmuxTarget, tmuxSocket });
    return id;
  }

  /**
   * Update session thread ID
   */
  setSessionThread(sessionId: string, threadId: number): void {
    this.db.prepare(`
      UPDATE sessions SET thread_id = ? WHERE id = ?
    `).run(threadId, sessionId);
    logger.info('Session thread set', { sessionId, threadId });
  }

  /**
   * Get session thread ID
   */
  getSessionThread(sessionId: string): number | null {
    const row = this.db.prepare(`
      SELECT thread_id FROM sessions WHERE id = ?
    `).get(sessionId) as { thread_id: number | null } | undefined;
    return row?.thread_id || null;
  }

  /**
   * Set session tmux info (target and socket for input injection)
   */
  setTmuxInfo(sessionId: string, tmuxTarget?: string, tmuxSocket?: string): void {
    if (tmuxTarget && tmuxSocket) {
      this.db.prepare(`
        UPDATE sessions SET tmux_target = ?, tmux_socket = ? WHERE id = ?
      `).run(tmuxTarget, tmuxSocket, sessionId);
    } else if (tmuxTarget) {
      this.db.prepare(`
        UPDATE sessions SET tmux_target = ? WHERE id = ?
      `).run(tmuxTarget, sessionId);
    } else if (tmuxSocket) {
      this.db.prepare(`
        UPDATE sessions SET tmux_socket = ? WHERE id = ?
      `).run(tmuxSocket, sessionId);
    }
    logger.info('Session tmux info set', { sessionId, tmuxTarget, tmuxSocket });
  }

  /**
   * Get session tmux info (target and socket)
   */
  getTmuxInfo(sessionId: string): { target: string | null; socket: string | null } {
    const row = this.db.prepare(`
      SELECT tmux_target, tmux_socket FROM sessions WHERE id = ?
    `).get(sessionId) as { tmux_target: string | null; tmux_socket: string | null } | undefined;
    return {
      target: row?.tmux_target || null,
      socket: row?.tmux_socket || null
    };
  }

  /**
   * Clear thread_id for a session (called when topic is auto-deleted)
   * This allows the session to be resumed with a new topic
   */
  clearThreadId(sessionId: string): void {
    this.db.prepare(`
      UPDATE sessions SET thread_id = NULL WHERE id = ?
    `).run(sessionId);
    logger.info('Cleared thread_id for session', { sessionId });
  }

  /**
   * Get session by ID
   */
  getSession(sessionId: string): Session | null {
    const row = this.db.prepare(`
      SELECT * FROM sessions WHERE id = ?
    `).get(sessionId) as SessionRow | undefined;

    return row ? this.rowToSession(row) : null;
  }

  /**
   * Get session by chat ID (most recent active)
   */
  getSessionByChatId(chatId: number): Session | null {
    const row = this.db.prepare(`
      SELECT * FROM sessions
      WHERE chat_id = ? AND status = 'active'
      ORDER BY last_activity DESC
      LIMIT 1
    `).get(chatId) as SessionRow | undefined;

    return row ? this.rowToSession(row) : null;
  }

  /**
   * Get session by thread ID (for routing Telegram replies)
   */
  getSessionByThreadId(threadId: number): Session | null {
    const row = this.db.prepare(`
      SELECT * FROM sessions
      WHERE thread_id = ? AND status = 'active'
      LIMIT 1
    `).get(threadId) as SessionRow | undefined;

    return row ? this.rowToSession(row) : null;
  }

  /**
   * Get all active sessions
   */
  getActiveSessions(): Session[] {
    const rows = this.db.prepare(`
      SELECT * FROM sessions WHERE status = 'active'
      ORDER BY last_activity DESC
    `).all() as SessionRow[];

    return rows.map(row => this.rowToSession(row));
  }

  /**
   * Update session activity
   */
  updateActivity(sessionId: string): void {
    this.db.prepare(`
      UPDATE sessions SET last_activity = ? WHERE id = ?
    `).run(new Date().toISOString(), sessionId);
  }

  /**
   * End a session
   */
  endSession(sessionId: string, status: 'ended' | 'aborted' = 'ended'): void {
    this.db.prepare(`
      UPDATE sessions SET status = ?, last_activity = ? WHERE id = ?
    `).run(status, new Date().toISOString(), sessionId);

    // Expire any pending approvals
    this.db.prepare(`
      UPDATE pending_approvals
      SET status = 'expired'
      WHERE session_id = ? AND status = 'pending'
    `).run(sessionId);

    logger.info('Session ended', { sessionId, status });
  }

  /**
   * Reactivate an ended/aborted session back to active status.
   * BUG-009 fix: When hook events arrive for an ended session,
   * it means Claude is still running - reactivate the session.
   */
  reactivateSession(sessionId: string): void {
    this.db.prepare(`
      UPDATE sessions SET status = 'active', last_activity = ? WHERE id = ?
    `).run(new Date().toISOString(), sessionId);

    logger.info('Session reactivated', { sessionId });
  }

  // ============ Approval Methods ============

  /**
   * Create a pending approval
   */
  createApproval(sessionId: string, prompt: string, messageId?: number): string {
    const id = generateId('approval');
    const now = new Date();
    const expiresAt = new Date(now.getTime() + this.approvalTimeout);

    this.db.prepare(`
      INSERT INTO pending_approvals
      (id, session_id, prompt, created_at, expires_at, status, message_id)
      VALUES (?, ?, ?, ?, ?, 'pending', ?)
    `).run(
      id,
      sessionId,
      prompt,
      now.toISOString(),
      expiresAt.toISOString(),
      messageId || null
    );

    logger.info('Approval created', { approvalId: id, sessionId });
    return id;
  }

  /**
   * Get approval by ID
   */
  getApproval(approvalId: string): PendingApproval | null {
    const row = this.db.prepare(`
      SELECT * FROM pending_approvals WHERE id = ?
    `).get(approvalId) as ApprovalRow | undefined;

    return row ? this.rowToApproval(row) : null;
  }

  /**
   * Get pending approvals for a session
   */
  getPendingApprovals(sessionId: string): PendingApproval[] {
    const rows = this.db.prepare(`
      SELECT * FROM pending_approvals
      WHERE session_id = ? AND status = 'pending'
      ORDER BY created_at DESC
    `).all(sessionId) as ApprovalRow[];

    return rows.map(row => this.rowToApproval(row));
  }

  /**
   * Resolve an approval
   */
  resolveApproval(
    approvalId: string,
    status: 'approved' | 'rejected'
  ): boolean {
    const result = this.db.prepare(`
      UPDATE pending_approvals
      SET status = ?
      WHERE id = ? AND status = 'pending'
    `).run(status, approvalId);

    if (result.changes > 0) {
      logger.info('Approval resolved', { approvalId, status });
      return true;
    }

    logger.warn('Approval not found or already resolved', { approvalId });
    return false;
  }

  /**
   * Expire old approvals
   */
  expireOldApprovals(): number {
    const now = new Date().toISOString();
    const result = this.db.prepare(`
      UPDATE pending_approvals
      SET status = 'expired'
      WHERE status = 'pending' AND expires_at < ?
    `).run(now);

    if (result.changes > 0) {
      logger.info('Expired old approvals', { count: result.changes });
    }

    return result.changes;
  }

  // ============ Cleanup Methods ============

  /**
   * Get sessions that are candidates for stale cleanup (BUG-003)
   * Returns sessions where lastActivity > timeoutHours ago AND status is 'active'
   */
  getStaleSessionCandidates(timeoutHours: number): Session[] {
    const cutoff = new Date();
    cutoff.setHours(cutoff.getHours() - timeoutHours);

    const rows = this.db.prepare(`
      SELECT * FROM sessions
      WHERE status = 'active' AND last_activity < ?
      ORDER BY last_activity ASC
    `).all(cutoff.toISOString()) as SessionRow[];

    return rows.map(row => this.rowToSession(row));
  }

  /**
   * Check if a tmux target belongs to a different active session (BUG-003)
   * Used to detect if a pane was recycled for a new Claude session
   */
  isTmuxTargetOwnedByOtherSession(tmuxTarget: string, excludeSessionId: string): boolean {
    const row = this.db.prepare(`
      SELECT id FROM sessions
      WHERE tmux_target = ? AND status = 'active' AND id != ?
      LIMIT 1
    `).get(tmuxTarget, excludeSessionId) as { id: string } | undefined;

    return !!row;
  }

  /**
   * Clean up old sessions
   */
  cleanupOldSessions(maxAgeDays: number = 7): number {
    const cutoff = new Date();
    cutoff.setDate(cutoff.getDate() - maxAgeDays);

    // Delete old approvals first (foreign key)
    this.db.prepare(`
      DELETE FROM pending_approvals
      WHERE session_id IN (
        SELECT id FROM sessions WHERE last_activity < ?
      )
    `).run(cutoff.toISOString());

    // Delete old sessions
    const result = this.db.prepare(`
      DELETE FROM sessions WHERE last_activity < ?
    `).run(cutoff.toISOString());

    if (result.changes > 0) {
      logger.info('Cleaned up old sessions', { count: result.changes });
    }

    return result.changes;
  }

  /**
   * Get database stats
   */
  getStats(): { activeSessions: number; pendingApprovals: number } {
    const sessions = this.db.prepare(`
      SELECT COUNT(*) as count FROM sessions WHERE status = 'active'
    `).get() as { count: number };

    const approvals = this.db.prepare(`
      SELECT COUNT(*) as count FROM pending_approvals WHERE status = 'pending'
    `).get() as { count: number };

    return {
      activeSessions: sessions.count,
      pendingApprovals: approvals.count
    };
  }

  /**
   * Close database connection
   */
  close(): void {
    this.db.close();
    logger.info('Session manager closed');
  }

  // ============ Private Helpers ============

  private rowToSession(row: SessionRow): Session {
    return {
      id: row.id,
      chatId: row.chat_id,
      threadId: row.thread_id || undefined,
      hostname: row.hostname || undefined,
      projectDir: row.project_dir || undefined,
      tmuxTarget: row.tmux_target || undefined,
      tmuxSocket: row.tmux_socket || undefined,
      startedAt: new Date(row.started_at),
      lastActivity: new Date(row.last_activity),
      status: row.status as Session['status'],
      metadata: row.metadata ? JSON.parse(row.metadata) : undefined
    };
  }

  private rowToApproval(row: ApprovalRow): PendingApproval {
    return {
      id: row.id,
      sessionId: row.session_id,
      prompt: row.prompt,
      createdAt: new Date(row.created_at),
      expiresAt: new Date(row.expires_at),
      status: row.status as PendingApproval['status']
    };
  }
}

// Type for database rows
interface SessionRow {
  id: string;
  chat_id: number;
  thread_id: number | null;
  hostname: string | null;
  tmux_target: string | null;
  tmux_socket: string | null;
  started_at: string;
  last_activity: string;
  status: string;
  project_dir: string | null;
  metadata: string | null;
}

interface ApprovalRow {
  id: string;
  session_id: string;
  prompt: string;
  created_at: string;
  expires_at: string;
  status: string;
  message_id: number | null;
}

export default SessionManager;
