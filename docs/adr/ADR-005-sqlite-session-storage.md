# ADR-005: SQLite Session Storage

**Status:** Accepted
**Date:** 2026-03-16

## Context

The bridge daemon needs to track session state (active sessions, pending
approvals, thread mappings) across multiple processes:

- The **daemon process** creates sessions, routes messages to Telegram
  threads, and resolves approvals.
- The **Node.js hook handler** connects to the daemon to request approval
  decisions and needs sessions to exist for routing.
- The **bash hook script** sends fire-and-forget messages that reference
  session IDs.

Earlier versions used file-based tracking (JSONL files and state files in the
config directory). This caused several problems:

- **Race conditions:** Multiple hook invocations could read and write the
  same state file concurrently, causing lost updates.
- **No structured queries:** Finding a session by thread ID or chat ID
  required reading and parsing all session files.
- **No atomic transactions:** Resolving an approval and updating a session
  status needed to happen atomically to avoid orphaned approvals.
- **Stale state:** File-based tracking had no reliable way to detect and
  clean up sessions whose Claude Code process had exited.

## Decision

Session and approval state is stored in a SQLite database
(`~/.config/claude-telegram-mirror/sessions.db`) using the `better-sqlite3`
library.

### Schema

**File:** `src/bridge/session.ts`

```sql
CREATE TABLE sessions (
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

CREATE TABLE pending_approvals (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  prompt TEXT NOT NULL,
  created_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  status TEXT DEFAULT 'pending',
  message_id INTEGER,
  FOREIGN KEY (session_id) REFERENCES sessions(id)
);
```

Indexes on `chat_id`, `status`, `session_id`, and approval `status` support
the common query patterns.

### Key properties

- **Concurrent access:** SQLite handles concurrent reads and serialized
  writes via its file-level locking. The daemon holds the database open for
  its lifetime, while hook handlers connect to the daemon via socket rather
  than opening the database directly. This avoids SQLite busy errors.

- **Synchronous API:** `better-sqlite3` provides a synchronous API that
  matches Node.js patterns in the daemon (which processes socket messages
  synchronously in event handlers). No async/await overhead for database
  operations.

- **Schema migration:** The `migrateAddTmuxTarget()` method uses
  `PRAGMA table_info` to check for missing columns and `ALTER TABLE` to add
  them. This allows the schema to evolve without requiring users to delete
  their database.

- **Atomic approval resolution:** `resolveApproval()` updates the approval
  status in a single `UPDATE ... WHERE status = 'pending'` statement,
  ensuring no double-resolution. Session end atomically expires all pending
  approvals for that session.

- **Structured queries:** Sessions can be looked up by `id`, `chat_id`,
  `thread_id`, or `status`. The `getStaleSessionCandidates()` method queries
  for active sessions whose `last_activity` exceeds a configurable timeout.

- **Single-file storage:** The entire state is one file in the config
  directory. Backup is a file copy. Permissions are enforced at 0o600.

- **No external dependency:** SQLite is embedded via `better-sqlite3` (a
  native Node.js addon). No separate database server is needed.

### Stateless hooks

With SQLite as the single source of truth, hook scripts are stateless. The
bash hook sends events to the daemon socket. The daemon calls
`ensureSessionExists()` to upsert the session in SQLite on the first
message. This eliminated the file-based session tracking that caused
BUG-006 (stale session files, race conditions between hooks and daemon).

## Consequences

### Positive

- Race conditions between hook invocations are eliminated. The daemon is the
  sole writer to the database.
- Session lookup by thread ID is a single indexed query, replacing O(n) file
  scans.
- Approval resolution is atomic and idempotent (the `WHERE status = 'pending'`
  guard prevents double-processing).
- Schema evolution is handled via `ALTER TABLE` migrations without data loss.
- Stale session cleanup is a single SQL query against `last_activity`.

### Negative

- `better-sqlite3` is a native addon that requires a C++ compilation step
  during `npm install`. This can fail on systems without a C++ toolchain,
  though prebuilt binaries are available for common platforms.
- SQLite's file-level locking means only one process should open the
  database for writes. The current architecture routes all writes through the
  daemon, but future multi-process architectures would need to account for
  this.

### Neutral

- The database file is small (typically under 100 KB) and grows slowly.
  The `cleanupOldSessions()` method deletes sessions older than 7 days.
- WAL mode is not explicitly enabled; the default rollback journal is
  sufficient for the single-writer pattern.
