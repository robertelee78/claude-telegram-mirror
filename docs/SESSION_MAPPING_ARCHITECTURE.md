# Session Mapping Architecture — Rust Implementation

## Overview

The `claude-telegram-mirror` daemon creates a bidirectional bridge between Claude Code CLI sessions and Telegram forum topics. The Rust implementation (`rust-crates/ctm`) uses SQLite for persistence, an in-memory cache layer for hot-path lookups, and a Unix domain socket for hook-to-daemon communication.

All session state lives in `~/.config/claude-telegram-mirror/sessions.db`. The database survives daemon restarts; in-memory caches are rebuilt lazily on the next access.

---

## 1. Session Data Model

### SQLite Schema

Initialised in `session.rs:SessionManager::init_schema`:

```sql
CREATE TABLE IF NOT EXISTS sessions (
    id            TEXT PRIMARY KEY,   -- Claude's native session_id
    chat_id       INTEGER NOT NULL,   -- Telegram group chat ID
    thread_id     INTEGER,            -- Telegram forum topic ID (nullable)
    hostname      TEXT,               -- Machine hostname
    tmux_target   TEXT,               -- Session:window.pane (e.g. "1:0.0")
    tmux_socket   TEXT,               -- Socket path (e.g. "/tmp/tmux-1000/default")
    started_at    TEXT NOT NULL,      -- ISO 8601 UTC with milliseconds
    last_activity TEXT NOT NULL,      -- ISO 8601 UTC with milliseconds
    status        TEXT DEFAULT 'active',  -- active | ended | aborted
    project_dir   TEXT,               -- cwd reported by hook
    metadata      TEXT                -- reserved JSON blob
);

CREATE TABLE IF NOT EXISTS pending_approvals (
    id          TEXT PRIMARY KEY,     -- "approval-<base36ts>-<rand8hex>"
    session_id  TEXT NOT NULL,
    prompt      TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    status      TEXT DEFAULT 'pending',  -- pending | approved | rejected | expired
    message_id  INTEGER,              -- Telegram message ID for the approval prompt
    FOREIGN KEY (session_id) REFERENCES sessions(id)
);

CREATE INDEX IF NOT EXISTS idx_sessions_chat     ON sessions(chat_id);
CREATE INDEX IF NOT EXISTS idx_sessions_status   ON sessions(status);
CREATE INDEX IF NOT EXISTS idx_approvals_session ON pending_approvals(session_id);
CREATE INDEX IF NOT EXISTS idx_approvals_status  ON pending_approvals(status);
```

**Date field design (L5.3 INTENTIONAL):** `started_at` and `last_activity` are stored as ISO 8601 `TEXT` (`to_rfc3339_opts` with millisecond precision) rather than epoch integers or `chrono::DateTime`. This matches the TypeScript predecessor, sorts lexicographically, and is human-readable in raw SQL. Converting to typed timestamps would add serde complexity without practical benefit.

**Migration:** `migrate_add_tmux_columns` checks `PRAGMA table_info(sessions)` and issues `ALTER TABLE ... ADD COLUMN` for `tmux_target` / `tmux_socket` when upgrading from an older database that predates those columns.

**File permissions:** The database file is created at `sessions.db` inside the config directory, then `chmod 0o600` is applied on Unix.

### Rust Structs

```rust
// session.rs

pub struct Session {
    pub id: String,
    pub chat_id: i64,
    pub thread_id: Option<i64>,
    pub hostname: Option<String>,
    pub tmux_target: Option<String>,
    pub tmux_socket: Option<String>,
    pub started_at: String,       // ISO 8601 TEXT
    pub last_activity: String,    // ISO 8601 TEXT
    pub status: String,           // "active" | "ended" | "aborted"
    pub project_dir: Option<String>,
    pub metadata: Option<String>,
}

pub struct PendingApproval {
    pub id: String,
    pub session_id: String,
    pub prompt: String,
    pub created_at: String,
    pub expires_at: String,
    pub status: String,           // "pending" | "approved" | "rejected" | "expired"
    pub message_id: Option<i64>,  // Telegram message ID
}
```

`SessionManager` wraps a `rusqlite::Connection`. There is no explicit `close()` — RAII drops the connection automatically when `SessionManager` is dropped (L6.9 INTENTIONAL).

---

## 2. Session Lifecycle

### State Diagram

```
                   hook fires (any event)
                         │
                         ▼
              ┌─────────────────────┐
              │  ensure_session_    │  BUG-006/BUG-010:
              │  exists() called    │  on-the-fly creation
              └────────┬────────────┘
                       │
           ┌───────────▼───────────┐
           │  session in DB?       │
           └───────────┬───────────┘
          No           │ Yes
           │           │
           │   ┌───────▼──────────┐
           │   │  status=active?  │
           │   └───────┬──────────┘
           │          No│Yes
           │           │  │
           │    BUG-009 │  └──────────────────────────┐
           │   reactivate│                             │
           │   _session()│                             │
           ▼           ▼                             ▼
     ┌──────────┐  ┌──────────┐             ┌──────────────┐
     │ Created  ├─►│  Active  │             │   Active     │
     │ (INSERT) │  │          │             │   (existing) │
     └──────────┘  └────┬─────┘             └──────────────┘
                        │
               hook Stop event
               end_session()
                        │
               ┌────────▼────────┐
               │     Ended       │──── cleanup timer ──►  topic deleted
               └─────────────────┘                        thread_id cleared

               abort_session() sets status = "aborted"
```

### On-the-Fly Creation (BUG-006/BUG-010)

In the TypeScript implementation, `session_start` had to arrive before any other message type. The Rust daemon uses `ensure_session_exists` instead — every socket handler that needs a session calls it, and the session is created transparently if it does not yet exist:

```rust
// daemon/mod.rs

async fn ensure_session_exists(ctx: &HandlerContext, msg: &BridgeMessage) {
    let existing = ctx.db_op(|sess| sess.get_session(&sid).ok().flatten()).await;

    if let Some(session) = existing {
        // BUG-009: Reactivate ended session if hooks are still firing
        if session.status != "active" {
            ctx.db_op(|sess| sess.reactivate_session(&sid)).await;
            cleanup::cancel_pending_topic_deletion(ctx, &msg.session_id).await;
        }
        return;
    }

    // BUG-002/BUG-010 + ADR-009: Atomically check-and-insert the topic creation
    // lock under a single write guard. This prevents two concurrent callers from
    // both seeing "no lock" and racing to create duplicate forum topics.
    {
        let mut locks = ctx.topic_locks.write().await;
        if let Some(state) = locks.get(&msg.session_id) {
            let notify = Arc::clone(&state.notify);
            drop(locks);
            let _ = tokio::time::timeout(Duration::from_secs(5), notify.notified()).await;
            return;
        }
        if ctx.config.use_threads {
            locks.insert(msg.session_id.clone(), Arc::new(TopicCreationState { ... }));
        }
    }

    // Session missing — create it now via handle_session_start
    socket_handlers::handle_session_start(ctx, msg).await;
}
```

The hook process sends `session_start` on **every** invocation (ADR-006 M4.4). `create_session` is idempotent: if the ID already exists it updates `last_activity` and returns without inserting a duplicate row.

### Reactivation (BUG-009)

When hook events arrive for a session that was previously `ended` or `aborted` (e.g. the user restarted Claude Code without starting a new tmux pane), `reactivate_session` flips the status back to `active` and refreshes `last_activity`:

```rust
pub fn reactivate_session(&self, session_id: &str) -> Result<()> {
    self.conn.execute(
        "UPDATE sessions SET status = 'active', last_activity = ?1 WHERE id = ?2",
        params![now_iso(), session_id],
    )?;
    Ok(())
}
```

Any pending topic-deletion timer is cancelled at the same time (see Section 8).

---

## 3. Thread Mapping

### How Sessions Map to Telegram Forum Topics

Each active session maps to one Telegram forum topic (thread). The mapping is stored in the `thread_id` column. The daemon also keeps an in-memory cache (`session_threads: Arc<RwLock<HashMap<String, i64>>>`) for sub-millisecond lookups.

**Outbound routing (socket message → correct topic):**

```rust
// daemon/mod.rs HandlerContext::get_thread_id

async fn get_thread_id(&self, session_id: &str) -> Option<i64> {
    // 1. Memory cache (fast path)
    if let Some(tid) = self.session_threads.read().await.get(session_id) {
        return Some(*tid);
    }
    // 2. Database fallback (daemon restart recovery)
    let session = self.db_op(|sess| sess.get_session(&sid).ok().flatten()).await;
    if let Some(s) = session {
        if let Some(tid) = s.thread_id {
            self.session_threads.write().await.insert(session_id.to_string(), tid);
            return Some(tid);
        }
    }
    None
}
```

**Inbound routing (Telegram message → correct session):**

```rust
// daemon/telegram_handlers.rs

let session = ctx
    .db_op(move |sess| sess.get_session_by_thread_id(thread_id).ok().flatten())
    .await;

if session.is_none() {
    return; // Thread belongs to another daemon instance — silent drop
}
```

The database query is: `SELECT * FROM sessions WHERE thread_id = ?1 AND status = 'active' LIMIT 1`. If the thread_id is not in this daemon's database, the message is silently dropped, enabling multi-daemon isolation.

### Topic Naming

Topic names are formatted by `HandlerContext::format_topic_name`:

```
{hostname} • {project_basename} • {session_id_prefix8}
```

For example: `builder • my-project • abc12345`

If hostname or project are absent, only the available parts are joined. The short ID is the first 8 characters of the session ID after stripping the `session-` prefix.

### Custom Title via /rename

When a `/rename` slash command is issued inside Claude Code's chat, a `custom-title` record is written to the JSONL transcript. The hook reads the last 8 KB of the transcript, finds the most recent `custom-title` entry, and sends a `session_rename` message to the daemon. The daemon calls `bot.edit_forum_topic` to update the Telegram topic name.

---

## 4. Stale Session Cleanup

### Cleanup Loop

The cleanup task runs every 5 minutes (`CLEANUP_INTERVAL_SECS = 300`) inside the event loop. It performs four operations in sequence:

1. **Expire old approvals** — marks `pending_approvals` rows past `expires_at` as `expired`.
2. **Clean stale sessions** — differentiated timeouts (see below).
3. **Clean orphaned threads** — removes `thread_id` from `ended` sessions that still hold one.
4. **Clean old downloads** — deletes files in `~/.config/claude-telegram-mirror/downloads/` older than 24 hours.

### Differentiated Timeouts

Defined in `daemon/mod.rs`:

```rust
const TMUX_SESSION_TIMEOUT_HOURS: u32 = 24;
const NO_TMUX_SESSION_TIMEOUT_HOURS: u32 = 1;
```

`get_stale_session_candidates(NO_TMUX_SESSION_TIMEOUT_HOURS)` fetches all active sessions with `last_activity` older than 1 hour. For each candidate:

| Session has `tmux_target`? | `last_activity` age | Pane still alive? | Action |
|---|---|---|---|
| No | > 1 hour | N/A | End session immediately |
| Yes | < 24 hours | N/A | Skip (not yet stale) |
| Yes | > 24 hours | Yes, same owner | Skip (still active) |
| Yes | > 24 hours | No (dead pane) | End session |
| Yes | > 24 hours | Pane reassigned | End session |

Pane liveness is checked via `InputInjector::is_pane_alive` (calls `tmux list-panes -t <target>`). Reassignment is detected via `SessionManager::is_tmux_target_owned_by_other`.

### Orphaned Thread Cleanup

Sessions in `ended` status that still have a `thread_id` set are cleaned up by `cleanup_orphaned_threads`. For each, the daemon attempts `bot.delete_forum_topic(tid)`, then clears `thread_id` in the database via `clear_thread_id`. Rate-limiting: 200 ms sleep between deletions, max 50 per cleanup cycle.

### Stale Session Teardown Sequence

When a stale session is torn down by `handle_stale_session_cleanup`:

1. Send "Session ended (terminal closed)" message to the session's topic.
2. If `auto_delete_topics` is enabled: delete the forum topic; otherwise close it.
3. Remove the session from in-memory caches (`session_threads`, `session_tmux`, `custom_titles`).
4. Call `end_session(sid, "ended")`, which also expires all pending approvals for that session.

---

## 5. Approval Flow

### Request Lifecycle

```
Claude Code (PreToolUse hook)
    │
    │ approval_request message over Unix socket
    ▼
Daemon: handle_approval_request()
    │
    │ create_approval() → INSERT pending_approvals row
    │ message_id = Telegram message ID of the prompt
    │
    │ Send Telegram message with [Approve] [Reject] inline buttons
    ▼
User clicks button in Telegram
    │
    │ callback_query arrives: "approve:<approval_id>" or "reject:<approval_id>"
    ▼
Daemon: handle_callback_query() → resolve_approval(id, "approved"|"rejected")
    │
    │ UPDATE pending_approvals SET status = ?1 WHERE id = ?2 AND status = 'pending'
    │ returns true if row was actually changed (prevents double-resolution)
    │
    │ Send approval_response message back over socket to blocked hook process
    ▼
Hook process: receives response, writes hookSpecificOutput to stdout
    │
    │ {"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}
    ▼
Claude Code proceeds (or is denied)
```

### Approval ID Generation

```rust
// session.rs generate_id("approval")
// Format: "approval-<base36(timestamp_millis)>-<8hex>"
// Example: "approval-lhv7f2k4-a3b4c5d6"
```

### IDOR Defence

`resolve_approval` only updates rows where `status = 'pending'`. A callback with a valid `approval_id` but already-resolved status returns `changed == 0` (false), and the daemon discards the duplicate without side effects. The `session_id` foreign key constraint and the `message_id` field together allow the daemon to verify that the callback originates from the correct session's message.

### Expiry

`create_approval` sets `expires_at = now + approval_timeout_ms`. The cleanup loop calls `expire_old_approvals()` every 5 minutes. `end_session` also expires all pending approvals for the session atomically:

```sql
UPDATE pending_approvals
SET status = 'expired'
WHERE session_id = ?1 AND status = 'pending'
```

---

## 6. tmux Target Management

### Capture Path

The hook process (`hook.rs`) calls `InputInjector::detect_tmux_session()` on every invocation:

```rust
// injector.rs
pub fn detect_tmux_session() -> Option<TmuxInfo> {
    let tmux_env = std::env::var("TMUX").ok()?;
    // $TMUX format: "/path/to/socket,pid,index"
    let socket_path = tmux_env.split(',').next().map(|s| s.to_string());

    // tmux display-message -p "#S" / "#I" / "#P"
    let target = format!("{}:{}.{}", session, window, pane);
    Some(TmuxInfo { session, pane, target, socket: socket_path })
}
```

This information is included in every bridge message's `metadata` as `tmuxTarget` and `tmuxSocket`.

### Auto-Refresh (BUG-001)

On every incoming socket message, `check_and_update_tmux_target` inspects the message metadata. If `tmuxTarget` differs from the cached value, the cache and the database are updated immediately:

```rust
// daemon/mod.rs

async fn check_and_update_tmux_target(ctx: &HandlerContext, msg: &BridgeMessage) {
    let new_target = meta.get("tmuxTarget").and_then(|v| v.as_str())?;
    let current = ctx.session_tmux.read().await.get(&msg.session_id).cloned();
    if current.as_deref() == Some(new_target) { return; }

    // Update memory cache
    ctx.session_tmux.write().await.insert(session_id, new_target.to_string());
    // Persist to DB
    ctx.db_op(|sess| sess.set_tmux_info(&sid, Some(target), socket)).await;
}
```

This ensures the daemon always routes to the correct pane even if Claude Code migrated to a different window or pane.

### Socket Path Validation

When `InputInjector::set_target` is called, the socket path is validated:

- Must be an absolute path (starts with `/`).
- Must not contain `..` (directory traversal prevention).
- Must be at most 256 characters (tmux socket paths).

The bridge socket path itself is validated against the AF_UNIX `sun_path` limit of 104 bytes (ADR-009: tightened from 256 to match the actual kernel limit on Linux -- 108 minus overhead).

Invalid paths are rejected and logged; `tmux_socket` is set to `None`.

### Target Validation Before Injection

Before every `inject()` call, `validate_target()` runs `tmux list-panes -t <target>`. If the pane no longer exists, `inject` returns `Ok(false)` and the daemon sends the user an actionable error:

> "Pane not found. Claude may have moved to a different pane. Send any command in Claude to refresh the connection."

All tmux commands use `Command::arg()` — no shell interpolation is possible regardless of user-supplied session names or socket paths.

---

## 7. Echo Prevention

### Problem

When the user types in Telegram and the daemon injects text into the tmux pane, Claude Code's `UserPromptSubmit` hook fires and sends the same text back through the bridge as a `user_input` message. Without prevention, this creates a visible echo in Telegram.

### Implementation (BUG-011)

`recent_telegram_inputs` is an `Arc<RwLock<HashSet<String>>>` keyed by `"<session_id>\0<text>"` (null separator -- ADR-009 item 11 changed from `:` to `\0`, which cannot appear in session IDs or UTF-8 text, eliminating the theoretical key collision class). The TTL is 10 seconds (`ECHO_TTL_SECS = 10`).

**When Telegram input is injected:**

```rust
// telegram_handlers.rs
add_echo_key(ctx, &session.id, text.trim()).await;
```

`add_echo_key` inserts the key and spawns a task that removes it after 10 seconds:

```rust
async fn add_echo_key(ctx: &HandlerContext, session_id: &str, text: &str) {
    // Use \0 as separator — cannot appear in session IDs (alphanumeric + . _ -)
    // or in UTF-8 text, preventing key collisions.
    let key = format!("{session_id}\0{text}");
    ctx.recent_inputs.write().await.insert(key.clone());
    let inputs = Arc::clone(&ctx.recent_inputs);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(ECHO_TTL_SECS)).await;
        inputs.write().await.remove(&key);
    });
}
```

**When a `user_input` socket message arrives:**

The `handle_user_input` handler checks whether the text is present in `recent_telegram_inputs` before forwarding to Telegram. If found, the message is silently dropped.

---

## 8. Topic Auto-Deletion

### Configurable Delay (default 24 hours)

When a session ends normally (Stop hook event), the daemon calls `schedule_topic_deletion` with a delay configured via `auto_delete_delay_ms` (default: 86 400 000 ms = 24 hours):

```rust
// daemon/cleanup.rs

pub(super) async fn schedule_topic_deletion(
    ctx: &HandlerContext,
    session_id: &str,
    thread_id: i64,
    delay_ms: u64,
) {
    let handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        if bot.delete_forum_topic(thread_id).await.unwrap_or(false) {
            session_threads.write().await.remove(&sid);
            sess.blocking_lock().clear_thread_id(&sid);
        } else {
            bot.close_forum_topic(thread_id).await;
        }
    });
    ctx.pending_del.write().await.insert(session_id.to_string(), handle);
}
```

The `JoinHandle` is stored in `pending_del: Arc<RwLock<HashMap<String, JoinHandle<()>>>>`.

### Cancellation on Session Resume (BUG-012)

If new hook events arrive for a session that has a pending deletion scheduled, `cancel_pending_topic_deletion` aborts the timer before it fires:

```rust
pub(super) async fn cancel_pending_topic_deletion(ctx: &HandlerContext, session_id: &str) {
    if let Some(handle) = ctx.pending_del.write().await.remove(session_id) {
        handle.abort();
        tracing::info!(session_id, "Cancelled pending topic deletion (session resumed)");
    }
}
```

This is called from both `handle_session_start` (BUG-012) and `ensure_session_exists` (BUG-009), covering all session-resume paths.

If the delay expires before the session resumes, the topic is deleted and `thread_id` is cleared in the database. The next session-start or `ensure_session_exists` call detects `thread_id IS NULL` and creates a new forum topic, posting a "Session resumed — new topic created" message.

---

## 9. Complete Message Flow Reference

### Hook Event → Telegram

```
Claude Code fires hook (any event type)
  └── hook.rs: process_hook()
        ├── InputInjector::detect_tmux_session()  -- reads $TMUX env var
        ├── build_messages()                       -- always prepends session_start
        └── send_messages() → Unix socket (NDJSON)

Daemon receives message
  └── event_loop.rs: handle_socket_message()
        ├── check_and_update_tmux_target()         -- BUG-001 auto-refresh
        ├── ensure_session_exists()                -- BUG-006/BUG-010/BUG-009
        └── dispatch to socket handler
              └── bot.send_message(content, thread_id)
                    thread_id from: session_threads cache → DB fallback
```

### Telegram Message → Claude Code

```
User types in Telegram topic
  └── telegram_handlers.rs: handle_telegram_text()
        ├── session = get_session_by_thread_id(thread_id)   -- ownership check
        │     None → silent drop (belongs to another daemon)
        ├── get_tmux_target() → cache → DB fallback
        ├── injector.set_target(target, socket)
        ├── add_echo_key()                                   -- BUG-011
        └── injector.inject(text)                           -- tmux send-keys -t -l
              validate_target() before injection             -- BUG-001
```

### Daemon Restart Recovery

```
Daemon stops → in-memory state lost:
  session_threads: {}
  session_tmux: {}
  recent_telegram_inputs: {}

Daemon restarts → database intact

Next socket message for session X:
  get_thread_id("X") → cache miss → DB query → repopulate cache
  get_tmux_target("X") → cache miss → get_tmux_info() → repopulate cache

Full functionality restored from SQLite.
```

---

## 10. Multi-Daemon Isolation

Each daemon instance has its own `sessions.db`. Thread IDs are assigned by Telegram and are globally unique within the group. Ownership is determined purely by a database lookup: if `thread_id` is not in this daemon's `sessions` table, the message is silently ignored.

```
Telegram group (forum mode)
  ├── Topic 42   ← Machine A session
  ├── Topic 43   ← Machine A session
  ├── Topic 44   ← Machine B session
  └── Topic 45   ← Machine C session

Message to Topic 44:
  Daemon A: get_session_by_thread_id(44) → None → drop
  Daemon B: get_session_by_thread_id(44) → Session → handle
  Daemon C: get_session_by_thread_id(44) → None → drop
```

---

## 11. Key Files

| File | Purpose |
|------|---------|
| `rust-crates/ctm/src/session.rs` | `SessionManager`, SQLite schema, all CRUD operations |
| `rust-crates/ctm/src/daemon/mod.rs` | `Daemon`, `HandlerContext`, `ensure_session_exists`, BUG-001/009/010 fixes |
| `rust-crates/ctm/src/daemon/cleanup.rs` | Stale cleanup, orphaned threads, `schedule_topic_deletion`, BUG-012 |
| `rust-crates/ctm/src/daemon/socket_handlers.rs` | `handle_session_start`, approval request, agent response, tool events |
| `rust-crates/ctm/src/daemon/telegram_handlers.rs` | Telegram → CLI routing, echo prevention, BUG-004/011 |
| `rust-crates/ctm/src/daemon/callback_handlers.rs` | Approval resolution, inline button callbacks |
| `rust-crates/ctm/src/daemon/event_loop.rs` | Main event loop, cleanup timer spawn |
| `rust-crates/ctm/src/injector.rs` | `InputInjector`, tmux detection, socket path validation |
| `rust-crates/ctm/src/hook.rs` | Hook entry point, `session_start` prepend, approval send-and-wait |
| `rust-crates/ctm/src/types.rs` | `BridgeMessage`, `MessageType`, `HookEvent`, `is_valid_session_id` |

---

## 12. Bug Fix Index

| Bug | Root cause | Rust fix |
|-----|------------|----------|
| BUG-001 | Stale tmux target after pane change | `check_and_update_tmux_target` on every socket message; `validate_target()` before inject |
| BUG-002 | Duplicate topic creation under concurrency | `topic_creation_locks: RwLock<HashMap<String, Arc<TopicCreationState>>>` with `Notify`; ADR-009 changed to atomic write lock for check-and-insert (single `write()` guard prevents two callers from both seeing "no lock") |
| BUG-003 | Sessions never cleaned up | Periodic cleanup in event loop: 5-minute interval, differentiated timeouts |
| BUG-004 | Special keys (Escape, Ctrl-C) not sent correctly | Whitelist in `ALLOWED_TMUX_KEYS`; `send_key` uses `-S socket` flag |
| BUG-006 | Messages dropped if `session_start` not first | `ensure_session_exists` called by every handler |
| BUG-009 | Ended session ignores resumed hook events | `reactivate_session()` called by `ensure_session_exists` |
| BUG-010 | Race: on-the-fly creation creates duplicate topics | `topic_creation_locks` guard; concurrent callers wait on `Notify` |
| BUG-011 | Telegram input echoed back as `user_input` | `recent_telegram_inputs` HashSet with 10-second TTL |
| BUG-012 | Topic deleted while session was mid-restart | `cancel_pending_topic_deletion` called on `session_start` and `ensure_session_exists` |
