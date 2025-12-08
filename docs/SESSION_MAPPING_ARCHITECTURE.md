# Session Mapping Architecture - claude-telegram-mirror

## Overview

The `claude-telegram-mirror` system creates a bidirectional bridge between Claude Code CLI sessions and Telegram topics using a sophisticated three-layer mapping architecture with SQLite persistence.

## Critical Mappings

### 1. Claude Session ID → Telegram Topic (thread_id)

**Primary Key**: Claude's native `session_id` (from hook events)

#### How thread_id is Created and Stored

**Creation Flow** (`daemon.ts:handleSessionStart`, lines 323-387):
```typescript
// Step 1: Extract Claude's native session_id from hook event
const sessionId = msg.sessionId; // e.g., "session-abc123xyz"

// Step 2: Check if session already has a thread (daemon restart resilience)
let threadId = this.sessions.getSessionThread(sessionId);

if (threadId) {
  // Reuse existing thread - don't create duplicate topics
  this.sessionThreads.set(sessionId, threadId);
  logger.info('Reusing existing session thread');
} else if (this.config.useThreads) {
  // Step 3: Create new forum topic (only if none exists)
  const topicName = this.formatTopicName(sessionId, hostname, projectDir);
  threadId = await this.bot.createForumTopic(topicName, 0); // Blue color

  // Step 4: Persist to database AND memory cache
  this.sessions.setSessionThread(sessionId, threadId);
  this.sessionThreads.set(sessionId, threadId);
}
```

**Storage Layers**:
- **SQLite** (`sessions.db`): Persistent storage across daemon restarts
  ```sql
  CREATE TABLE sessions (
    id TEXT PRIMARY KEY,           -- Claude's session_id
    thread_id INTEGER,             -- Telegram topic ID
    ...
  );
  ```

- **Memory Cache** (`daemon.ts`): Fast lookups during daemon runtime
  ```typescript
  private sessionThreads: Map<string, number> = new Map();
  // Example: "session-abc123" → 42
  ```

#### How Messages are Routed to Correct Topics

**Outbound (CLI → Telegram)** - All handlers use `getSessionThreadId()`:
```typescript
// daemon.ts lines 459-472
private getSessionThreadId(sessionId: string): number | undefined {
  // Step 1: Check memory cache first (fast path)
  let threadId = this.sessionThreads.get(sessionId);
  if (threadId) return threadId;

  // Step 2: Fallback to database (daemon restart recovery)
  const dbThreadId = this.sessions.getSessionThread(sessionId);
  if (dbThreadId) {
    // Repopulate cache for future requests
    this.sessionThreads.set(sessionId, dbThreadId);
    return dbThreadId;
  }

  return undefined; // No thread (message goes to General topic)
}
```

**Inbound (Telegram → CLI)** - Multi-bot isolation (`daemon.ts:setupBotHandlers`, lines 238-319):
```typescript
this.bot.onMessage(async (text, chatId, threadId) => {
  if (threadId) {
    // CRITICAL: Only process topics we own (multi-bot architecture)
    session = this.sessions.getSessionByThreadId(threadId);
    if (!session) {
      // This topic belongs to another daemon - IGNORE
      logger.debug('Ignoring message for unknown topic (multi-bot)');
      return;
    }
  } else {
    // Message in General topic - use chatId fallback
    session = this.sessions.getSessionByChatId(chatId);
  }
  // ... inject input to correct tmux session
});
```

**Database Query** (`session.ts:getSessionByThreadId`, lines 232-240):
```typescript
getSessionByThreadId(threadId: number): Session | null {
  const row = this.db.prepare(`
    SELECT * FROM sessions
    WHERE thread_id = ? AND status = 'active'
    LIMIT 1
  `).get(threadId);
  return row ? this.rowToSession(row) : null;
}
```

---

### 2. Claude Session ID → tmux Target + Socket

**Purpose**: Enable Telegram → CLI input injection (reverse communication)

#### What is Stored

**Two Critical Fields**:
- `tmux_target`: Session:Window.Pane identifier (e.g., `"1:0.0"`)
  - Session `1`
  - Window `0`
  - Pane `0`
- `tmux_socket`: Socket path for explicit targeting (e.g., `"/tmp/tmux-1000/default"`)
  - Extracted from `$TMUX` environment variable
  - Required to target the correct tmux server when multiple instances exist

#### How it's Captured from $TMUX

**Hook Script** (`telegram-hook.sh:get_tmux_info`, lines 112-137):
```bash
get_tmux_info() {
  if [[ -z "$TMUX" ]]; then
    echo "{}"
    return
  fi

  # $TMUX format: /path/to/socket,pid,index
  # Example: "/tmp/tmux-1000/default,12345,0"
  local socket_path="${TMUX%%,*}"  # Extract everything before first comma

  local session=$(tmux display-message -p "#S" 2>/dev/null)  # e.g., "1"
  local pane=$(tmux display-message -p "#P" 2>/dev/null)     # e.g., "0"
  local window=$(tmux display-message -p "#I" 2>/dev/null)   # e.g., "0"

  if [[ -n "$session" && -n "$window" && -n "$pane" ]]; then
    local target="${session}:${window}.${pane}"  # e.g., "1:0.0"
    jq -cn \
      --arg target "$target" \
      --arg socket "$socket_path" \
      '{tmuxTarget: $target, tmuxSocket: $socket}'
  fi
}
```

**Sent in session_start Hook** (`telegram-hook.sh`, lines 387-407):
```bash
if is_first_event; then
  TMUX_INFO=$(get_tmux_info)
  SESSION_START=$(jq -cn \
    --arg sessionId "$SESSION_ID" \
    --argjson tmux "$TMUX_INFO" \
    '{sessionId: $sessionId, metadata: ({...} + $tmux)}')
  send_to_bridge "$SESSION_START"
fi
```

#### How it Survives Daemon Restarts

**Persistence Strategy** - Three-layer architecture:

**Layer 1: Database** (`session.ts:createSession`, lines 117-149):
```typescript
createSession(
  chatId: number,
  projectDir?: string,
  threadId?: number,
  hostname?: string,
  sessionId?: string,
  tmuxTarget?: string,  // Persist to disk
  tmuxSocket?: string   // Persist to disk
): string {
  const id = sessionId || generateId('session');

  this.db.prepare(`
    INSERT INTO sessions (id, tmux_target, tmux_socket, ...)
    VALUES (?, ?, ?, ...)
  `).run(id, ..., tmuxTarget || null, tmuxSocket || null, ...);

  return id;
}
```

**Layer 2: Memory Cache** (`daemon.ts:handleSessionStart`, lines 342-345):
```typescript
// Cache in memory for fast lookups
if (tmuxTarget) {
  this.sessionTmuxTargets.set(sessionId, tmuxTarget);
  logger.info('Session tmux info stored', { sessionId, tmuxTarget, tmuxSocket });
}
```

**Layer 3: Lazy Restoration** (`daemon.ts:setupBotHandlers`, lines 264-282):
```typescript
// When message arrives, check cache first
let tmuxTarget = this.sessionTmuxTargets.get(session.id);
let tmuxSocket: string | undefined;

if (!tmuxTarget) {
  // Cache miss - restore from database (daemon restart scenario)
  const tmuxInfo = this.sessions.getTmuxInfo(session.id);
  tmuxTarget = tmuxInfo.target || undefined;
  tmuxSocket = tmuxInfo.socket || undefined;

  if (tmuxTarget) {
    // Repopulate cache for future requests
    this.sessionTmuxTargets.set(session.id, tmuxTarget);
    logger.info('Restored tmux info from database', { sessionId, tmuxTarget });
  }
}
```

**Input Injection** (`daemon.ts`, lines 284-309):
```typescript
if (tmuxTarget) {
  this.injector.setTmuxSession(tmuxTarget, tmuxSocket);
}

// Inject input into Claude Code via tmux send-keys
const injected = await this.injector.inject(text);

if (injected) {
  logger.info('Injected input to CLI', { method: this.injector.getMethod() });
} else {
  await this.bot.sendMessage(
    `⚠️ Could not send input to CLI. No tmux session found.`,
    { parseMode: 'Markdown' },
    threadId
  );
}
```

---

### 3. Multi-System Architecture

**Design Philosophy**: Multiple independent daemons can coexist on different machines, each managing their own subset of Telegram topics.

#### How Multiple Daemons Coexist

**Key Insight**: Each daemon has its own SQLite database (`~/.config/claude-telegram-mirror/sessions.db`)

**Database Isolation**:
```
Machine A: ~/.config/claude-telegram-mirror/sessions.db
  ├─ session-abc123 → thread_id: 42
  ├─ session-def456 → thread_id: 43

Machine B: ~/.config/claude-telegram-mirror/sessions.db
  ├─ session-xyz789 → thread_id: 44
  ├─ session-uvw012 → thread_id: 45
```

**No Central Coordination Required**:
- Each daemon creates and tracks its own topics
- Telegram group supports unlimited topics (forum mode)
- No collision risk because thread_ids are globally unique (assigned by Telegram)

#### How Each Daemon Knows Which Topics Belong to It

**Topic Ownership Check** (`daemon.ts:setupBotHandlers`, lines 240-252):
```typescript
this.bot.onMessage(async (text, chatId, threadId) => {
  if (threadId) {
    // Message is in a specific topic - check ownership
    session = this.sessions.getSessionByThreadId(threadId);

    if (!session) {
      // This topic is NOT in our database - belongs to another daemon
      // Silently ignore - another daemon will handle it
      logger.debug('Ignoring message for unknown topic (multi-bot)', { threadId });
      return;  // CRITICAL: Early exit prevents processing
    }
  }
  // ... only processes messages for topics we created
});
```

**Database-First Approach**:
1. Telegram message arrives with `threadId: 42`
2. Daemon A queries: `SELECT * FROM sessions WHERE thread_id = 42`
   - **Found** → Daemon A handles it
3. Daemon B queries: `SELECT * FROM sessions WHERE thread_id = 42`
   - **Not found** → Daemon B ignores it
4. Daemon C queries: `SELECT * FROM sessions WHERE thread_id = 42`
   - **Not found** → Daemon C ignores it

**Result**: Each daemon only responds to topics it created, enabling true multi-tenant operation.

---

## Complete Mapping Chain

### Session Start (Claude Code → Telegram)

```
1. User starts Claude Code in tmux
   └─ $TMUX = "/tmp/tmux-1000/default,12345,0"

2. Hook fires: UserPromptSubmit
   └─ telegram-hook.sh extracts:
      ├─ session_id: "session-abc123xyz" (from Claude)
      ├─ tmuxTarget: "1:0.0" (from tmux display-message)
      └─ tmuxSocket: "/tmp/tmux-1000/default" (from $TMUX)

3. daemon.ts:handleSessionStart receives:
   └─ Creates/updates session in SQLite:
      ├─ id: "session-abc123xyz" (PRIMARY KEY)
      ├─ tmux_target: "1:0.0"
      ├─ tmux_socket: "/tmp/tmux-1000/default"
      └─ thread_id: NULL (not yet created)

4. daemon.ts creates Telegram topic:
   └─ bot.createForumTopic("hostname • project • abc123")
      ├─ Returns: threadId = 42
      └─ Updates SQLite: thread_id = 42

5. Final SQLite state:
   ┌─────────────────────┬───────────┬────────────┬──────────────────────────────┐
   │ id                  │ thread_id │ tmux_target│ tmux_socket                  │
   ├─────────────────────┼───────────┼────────────┼──────────────────────────────┤
   │ session-abc123xyz   │ 42        │ 1:0.0      │ /tmp/tmux-1000/default       │
   └─────────────────────┴───────────┴────────────┴──────────────────────────────┘

6. Memory caches populated:
   ├─ sessionThreads: {"session-abc123xyz" → 42}
   └─ sessionTmuxTargets: {"session-abc123xyz" → "1:0.0"}
```

### Message Flow (CLI → Telegram)

```
1. Claude Code outputs text
   └─ Hook fires: Stop

2. telegram-hook.sh sends:
   └─ {type: "agent_response", sessionId: "session-abc123xyz", content: "..."}

3. daemon.ts:handleAgentResponse:
   ├─ Calls getSessionThreadId("session-abc123xyz")
   │  ├─ Checks sessionThreads cache → found: 42
   │  └─ Returns 42
   └─ bot.sendMessage(content, threadId: 42)

4. Message appears in Telegram topic #42 ✓
```

### Message Flow (Telegram → CLI)

```
1. User types in Telegram topic #42

2. daemon.ts:setupBotHandlers receives:
   └─ onMessage(text, chatId, threadId: 42)

3. Topic ownership check:
   └─ getSessionByThreadId(42)
      ├─ Query: SELECT * FROM sessions WHERE thread_id = 42
      └─ Returns: {id: "session-abc123xyz", tmux_target: "1:0.0", ...}

4. Get tmux info (with restart resilience):
   ├─ Check cache: sessionTmuxTargets.get("session-abc123xyz")
   │  └─ Cache miss (daemon restarted)
   ├─ Query database: getTmuxInfo("session-abc123xyz")
   │  └─ Returns: {target: "1:0.0", socket: "/tmp/tmux-1000/default"}
   └─ Repopulate cache for future requests

5. Input injection:
   └─ injector.setTmuxSession("1:0.0", "/tmp/tmux-1000/default")
      └─ tmux -S /tmp/tmux-1000/default send-keys -t 1:0.0 "user text" Enter

6. Text appears in Claude Code CLI ✓
```

### Daemon Restart Recovery

```
1. Daemon stops (kill process)
   └─ Memory caches cleared
      ├─ sessionThreads: {}
      └─ sessionTmuxTargets: {}

2. Daemon starts again
   └─ Database intact: sessions.db still has all mappings

3. Telegram message arrives (threadId: 42)
   └─ getSessionThreadId(42):
      ├─ Cache miss (empty)
      ├─ Query database → {id: "session-abc123xyz", thread_id: 42, tmux_target: "1:0.0"}
      └─ Repopulate cache

4. Full functionality restored ✓
   └─ All mappings recovered from persistent SQLite storage
```

---

## Database Schema

```sql
CREATE TABLE sessions (
  id TEXT PRIMARY KEY,              -- Claude's native session_id
  chat_id INTEGER NOT NULL,         -- Telegram chat (group) ID
  thread_id INTEGER,                -- Telegram topic ID (forum thread)
  hostname TEXT,                    -- Machine hostname (for multi-system)
  tmux_target TEXT,                 -- tmux session:window.pane (e.g., "1:0.0")
  tmux_socket TEXT,                 -- tmux socket path (e.g., "/tmp/tmux-1000/default")
  started_at TEXT NOT NULL,         -- Session start timestamp
  last_activity TEXT NOT NULL,      -- Last message timestamp
  status TEXT DEFAULT 'active',     -- active | ended | aborted
  project_dir TEXT,                 -- Working directory
  metadata TEXT                     -- JSON blob for extensions
);

CREATE INDEX idx_sessions_chat ON sessions(chat_id);
CREATE INDEX idx_sessions_status ON sessions(status);
```

**Key Queries**:
- `getSession(sessionId)`: `SELECT * FROM sessions WHERE id = ?`
- `getSessionByThreadId(threadId)`: `SELECT * FROM sessions WHERE thread_id = ? AND status = 'active'`
- `getSessionByChatId(chatId)`: `SELECT * FROM sessions WHERE chat_id = ? AND status = 'active' ORDER BY last_activity DESC LIMIT 1`
- `getTmuxInfo(sessionId)`: `SELECT tmux_target, tmux_socket FROM sessions WHERE id = ?`

---

## Persistence Strategy Summary

| Component | Storage Type | Survives Restart? | Recovery Method |
|-----------|--------------|-------------------|-----------------|
| **Session ID** | SQLite (PRIMARY KEY) | ✅ Yes | N/A (always persisted) |
| **Thread ID** | SQLite + Memory cache | ✅ Yes | `getSessionThread()` → cache repopulation |
| **tmux Target** | SQLite + Memory cache | ✅ Yes | `getTmuxInfo()` → cache repopulation |
| **tmux Socket** | SQLite + Session object | ✅ Yes | `getTmuxInfo()` → cache repopulation |
| **Topic Ownership** | SQLite (presence check) | ✅ Yes | `getSessionByThreadId()` → NULL = not ours |

**Recovery Pattern**:
1. Check memory cache (fast path)
2. On miss, query SQLite (restart recovery)
3. Repopulate cache for subsequent requests
4. Log restoration for debugging

---

## Multi-Bot Isolation

**Scenario**: 3 machines running bridge daemons, all connected to same Telegram group

```
┌─────────────────────────────────────────────────────────────┐
│                    Telegram Group (Forum Mode)              │
│  ┌──────────┬──────────┬──────────┬──────────┬──────────┐  │
│  │ Topic 42 │ Topic 43 │ Topic 44 │ Topic 45 │ Topic 46 │  │
│  └──────────┴──────────┴──────────┴──────────┴──────────┘  │
└─────────────────────────────────────────────────────────────┘
       ▲            ▲            ▲            ▲
       │            │            │            │
   ┌───┴───┐    ┌───┴───┐    ┌───┴───┐    ┌───┴───┐
   │Daemon │    │Daemon │    │Daemon │    │Daemon │
   │   A   │    │   A   │    │   B   │    │   C   │
   └───────┘    └───────┘    └───────┘    └───────┘
   Machine A    Machine A    Machine B    Machine C

   sessions.db  sessions.db  sessions.db  sessions.db
   ├─ T42 ✓     ├─ T42 ✓     ├─ T44 ✓     ├─ T46 ✓
   └─ T43 ✓     └─ T43 ✓     └─ T45 ✓     └─ (none)
```

**Message to Topic 44**:
- Daemon A: `getSessionByThreadId(44)` → NULL → **IGNORE**
- Daemon B: `getSessionByThreadId(44)` → session → **HANDLE** ✓
- Daemon C: `getSessionByThreadId(44)` → NULL → **IGNORE**

**Result**: Only Daemon B processes the message. Perfect isolation.

---

## Key Files

| File | Purpose | Lines of Interest |
|------|---------|-------------------|
| `src/bridge/session.ts` | SQLite schema & persistence | 52-84 (schema), 117-149 (createSession), 174-202 (tmux methods) |
| `src/bridge/daemon.ts` | Message routing & coordination | 238-319 (bot handlers), 323-387 (handleSessionStart), 459-472 (getSessionThreadId) |
| `scripts/telegram-hook.sh` | tmux info extraction | 112-137 (get_tmux_info), 387-407 (session_start event) |
| `src/bridge/types.ts` | Type definitions | 26-38 (Session interface) |

---

## Architecture Highlights

✅ **Restart Resilient**: All critical state persisted to SQLite
✅ **Multi-Bot Safe**: Database-first ownership checks prevent conflicts
✅ **Two-Way Communication**: tmux socket enables Telegram → CLI input injection
✅ **Claude-Native Sessions**: Uses Claude's own session_id as primary key
✅ **Lazy Cache Repopulation**: Automatic recovery after daemon restart
✅ **Topic Isolation**: Each daemon only handles topics it created

---

## Troubleshooting

**Problem**: Messages go to wrong topic
→ **Check**: `sessionThreads` cache vs database `thread_id`
→ **Fix**: Restart daemon to repopulate cache from database

**Problem**: Input injection fails after restart
→ **Check**: `getTmuxInfo()` returns null or stale data
→ **Fix**: Verify tmux session still running, check socket permissions

**Problem**: Multiple daemons respond to same message
→ **Check**: `getSessionByThreadId()` ownership check
→ **Fix**: Ensure each daemon has separate SQLite database

---

**End of Analysis**
