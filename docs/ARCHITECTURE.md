# Claude Telegram Mirror - Architecture

A bidirectional bridge that mirrors Claude Code CLI sessions to Telegram, enabling remote monitoring and interaction.

## Overview

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              System Architecture                            │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌──────────────┐     Unix Socket      ┌──────────────┐     Telegram API    │
│  │  Claude Code │ ──────────────────▶  │    Bridge    │ ─────────────────▶  │
│  │     CLI      │                      │    Daemon    │                     │
│  │   (tmux)     │ ◀──────────────────  │              │ ◀─────────────────  │
│  └──────────────┘    tmux send-keys    └──────────────┘                     │
│        │                                     │                              │
│        │ hooks                               │ SQLite                       │
│        ▼                                     ▼                              │
│  ┌──────────────┐                     ┌──────────────┐                      │
│  │ PreToolUse:  │◀──────────────────▶ │ sessions.db  │                      │
│  │ handler.ts   │  bidirectional      │              │                      │
│  │ (approval)   │  request/response   └──────────────┘                      │
│  ├──────────────┤                                                           │
│  │ Other hooks: │                                                           │
│  │ telegram-    │──────────────────▶  fire & forget                         │
│  │ hook.sh      │                                                           │
│  └──────────────┘                                                           │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Components

| Component | File | Purpose |
|-----------|------|---------|
| **CLI** | `src/cli.ts` | Entry point, commands, daemon lifecycle |
| **BridgeDaemon** | `src/bridge/daemon.ts` | Central orchestrator |
| **SocketServer** | `src/bridge/socket.ts` | Unix socket IPC |
| **SessionManager** | `src/bridge/session.ts` | SQLite persistence |
| **InputInjector** | `src/bridge/injector.ts` | tmux input injection |
| **TelegramBot** | `src/bot/telegram.ts` | Telegram API wrapper |
| **PreToolUse Handler** | `src/hooks/handler.ts` | Approval buttons, async bidirectional |
| **Hook Script (Bash)** | `scripts/telegram-hook.sh` | Other hooks, fast fire-and-forget |

---

## Message Flows

### Flow 1: CLI → Telegram

```
┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│ Claude Code │    │    Hook     │    │   Bridge    │    │  Telegram   │
│    fires    │───▶│   Script    │───▶│   Daemon    │───▶│    API      │
│    hook     │    │             │    │             │    │             │
└─────────────┘    └─────────────┘    └─────────────┘    └─────────────┘
                         │                  │
                         │ NDJSON           │ Routes to
                         │ via socket       │ forum topic
                         ▼                  ▼
                   ┌─────────────┐    ┌─────────────┐
                   │ bridge.sock │    │ Topic #123  │
                   └─────────────┘    └─────────────┘
```

**Hook Events Captured:**
- `PreToolUse` → `tool_start` (+ approval workflow)
- `PostToolUse` → `tool_result`
- `Stop` → `agent_response` + `turn_complete`
- `UserPromptSubmit` → `user_input`
- `PreCompact` → `pre_compact` (context compaction warning)

**Note:** Hooks are stateless (v0.1.15+). They do NOT track session state or emit `session_start`. The daemon creates sessions on-the-fly when the first event arrives for a new session.

**Key Functions:**
1. `telegram-hook.sh::format_message()` - Constructs typed messages
2. `telegram-hook.sh::send_to_bridge()` - Sends via Unix socket
3. `daemon.ts::setupSocketHandlers()` - Routes by message type
4. `daemon.ts::ensureSessionExists()` - Creates session + topic on first event
5. `daemon.ts::handleSessionStart()` - Creates forum topic (called by ensureSessionExists)
6. `bot.sendMessage()` - Delivers to Telegram

### Flow 2: Telegram → CLI

```
┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│  Telegram   │    │   Bridge    │    │   Input     │    │ Claude Code │
│    User     │───▶│   Daemon    │───▶│  Injector   │───▶│    CLI      │
│   replies   │    │             │    │             │    │  (tmux)     │
└─────────────┘    └─────────────┘    └─────────────┘    └─────────────┘
                         │                  │
                         │ Lookup           │ tmux -S socket
                         │ session          │ send-keys -t target
                         ▼                  ▼
                   ┌─────────────┐    ┌─────────────┐
                   │ sessions.db │    │ tmux 1:0.0  │
                   └─────────────┘    └─────────────┘
```

**Key Functions:**
1. `bot.onMessage()` - Receives Telegram message
2. `sessions.getSessionByThreadId()` - Finds session by topic
3. `sessions.getTmuxInfo()` - Gets tmux target + socket
4. `injector.setTmuxSession()` - Configures target
5. `injector.injectViaTmux()` - Executes `tmux send-keys`

---

## Session Mapping

The system maintains a **three-way mapping** for each Claude session:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           Session Mapping Chain                             │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   Claude Session ID ◀───────────────────────────────────▶ Telegram Topic    │
│   "a1b2c3d4-..."                                          thread_id: 123    │
│         │                                                                   │
│         │                                                                   │
│         ▼                                                                   │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │                        SQLite: sessions                             │   │
│   ├─────────────────────────────────────────────────────────────────────┤   │
│   │  id              │ thread_id │ tmux_target │ tmux_socket            │   │
│   │  "a1b2c3d4-..."  │ 123       │ "1:0.0"     │ "/tmp/tmux-1000/default│   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│         │                                                                   │
│         │                                                                   │
│         ▼                                                                   │
│   tmux Session ◀────────────────────────────────────────▶ CLI Pane          │
│   socket: /tmp/tmux-1000/default                          session 1:0.0     │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Mapping Details

| Mapping | Source | Storage | Purpose |
|---------|--------|---------|---------|
| Session → Topic | `ensureSessionExists()` → `handleSessionStart()` | `thread_id` column | Route messages to correct topic |
| Session → tmux | `$TMUX` env var in hook | `tmux_target`, `tmux_socket` | Inject input to correct pane |

**tmux Auto-Refresh (v0.1.12+):** Every hook event includes current tmux info. If the user moves Claude to a different pane, the daemon auto-updates the mapping when the next event arrives. This self-heals stale targets without user intervention.

### tmux Target Format

The hook extracts tmux info from the `$TMUX` environment variable:

```bash
# $TMUX format: /path/to/socket,pid,window_index
TMUX="/tmp/tmux-1000/default,12345,1"

# Extracted:
tmux_socket="/tmp/tmux-1000/default"
tmux_target="session_name:window.pane"  # e.g., "1:0.0"
```

### Persistence & Recovery

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                          Daemon Restart Recovery                            │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  [Before Restart]           [After Restart]                                 │
│                                                                             │
│  Memory Cache:              Memory Cache:                                   │
│  ┌─────────────────┐        ┌─────────────────┐                             │
│  │ sessionThreads  │        │     (empty)     │                             │
│  │ sessionTmux     │        │                 │                             │
│  └─────────────────┘        └────────┬────────┘                             │
│                                      │                                      │
│                                      │ Cache miss                           │
│                                      ▼                                      │
│  SQLite Database:           SQLite Database:                                │
│  ┌─────────────────┐        ┌─────────────────┐                             │
│  │ Persisted data  │ ────▶  │ Restore from DB │                             │
│  │ survives        │        │ on first access │                             │
│  └─────────────────┘        └─────────────────┘                             │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Multi-System Architecture

Multiple hosts can share a single Telegram supergroup:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Multi-System Deployment                             │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐                          │
│  │   Host A    │  │   Host B    │  │   Host C    │                          │
│  │  (Linux)    │  │  (macOS)    │  │  (Linux)    │                          │
│  ├─────────────┤  ├─────────────┤  ├─────────────┤                          │
│  │ Daemon A    │  │ Daemon B    │  │ Daemon C    │                          │
│  │ sessions.db │  │ sessions.db │  │ sessions.db │                          │
│  │ Bot Token A │  │ Bot Token B │  │ Bot Token C │                          │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘                          │
│         │                │                │                                 │
│         └────────────────┼────────────────┘                                 │
│                          │                                                  │
│                          ▼                                                  │
│               ┌─────────────────────┐                                       │
│               │  Telegram Supergroup│                                       │
│               │  (shared chat_id)   │                                       │
│               ├─────────────────────┤                                       │
│               │ Topic #1 (Host A)   │ ◀── Only Daemon A responds            │
│               │ Topic #2 (Host B)   │ ◀── Only Daemon B responds            │
│               │ Topic #3 (Host A)   │ ◀── Only Daemon A responds            │
│               │ Topic #4 (Host C)   │ ◀── Only Daemon C responds            │
│               └─────────────────────┘                                       │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Topic Ownership

Each daemon only processes topics it created:

```typescript
// In setupBotHandlers():
session = this.sessions.getSessionByThreadId(threadId);
if (!session) {
  // Topic not in our database = belongs to another daemon
  // Silently ignore
  return;
}
```

---

## Database Schema

```sql
-- sessions table
CREATE TABLE sessions (
  id TEXT PRIMARY KEY,           -- Claude's session_id
  chat_id INTEGER NOT NULL,      -- Telegram chat
  thread_id INTEGER,             -- Telegram topic
  hostname TEXT,                 -- Machine name
  tmux_target TEXT,              -- "session:window.pane"
  tmux_socket TEXT,              -- "/path/to/socket"
  started_at TEXT NOT NULL,
  last_activity TEXT NOT NULL,
  status TEXT DEFAULT 'active',  -- active|ended|aborted
  project_dir TEXT,
  metadata TEXT                  -- JSON blob
);

-- pending_approvals table
CREATE TABLE pending_approvals (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  prompt TEXT NOT NULL,
  created_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  status TEXT DEFAULT 'pending', -- pending|approved|rejected|expired
  message_id INTEGER,
  FOREIGN KEY (session_id) REFERENCES sessions(id)
);
```

---

## Configuration

### Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `TELEGRAM_BOT_TOKEN` | Yes | - | Bot token from @BotFather |
| `TELEGRAM_CHAT_ID` | Yes | - | Supergroup ID (starts with `-100`) |
| `TELEGRAM_MIRROR` | No | `false` | Enable/disable mirroring |
| `TELEGRAM_MIRROR_VERBOSE` | No | `false` | Verbose logging |
| `TELEGRAM_USE_THREADS` | No | `true` | Use forum topics |
| `TELEGRAM_BRIDGE_SOCKET` | No | `~/.config/.../bridge.sock` | Socket path |
| `TELEGRAM_STALE_SESSION_TIMEOUT_HOURS` | No | `72` | Auto-cleanup threshold for dead sessions |

### File Locations

| File | Purpose |
|------|---------|
| `~/.telegram-env` | Environment variables |
| `~/.config/claude-telegram-mirror/bridge.sock` | Unix socket |
| `~/.config/claude-telegram-mirror/sessions.db` | SQLite database |
| `~/.config/claude-telegram-mirror/bridge.pid` | PID lock file |

---

## Service Management

### systemd (Linux)

```bash
# Service file: ~/.config/systemd/user/claude-telegram-mirror.service
systemctl --user start claude-telegram-mirror
systemctl --user status claude-telegram-mirror
journalctl --user -u claude-telegram-mirror -f
```

**Important:** `PrivateTmp=false` is required so the daemon can access tmux sockets in `/tmp/tmux-$UID/`.

### launchd (macOS)

```bash
# Plist: ~/Library/LaunchAgents/com.claude.claude-telegram-mirror.plist
launchctl load ~/Library/LaunchAgents/com.claude.claude-telegram-mirror.plist
launchctl start com.claude.claude-telegram-mirror
launchctl list | grep claude  # Check status
tail -f ~/.config/claude-telegram-mirror/daemon.log  # View logs
```

**Key launchd configuration:**
- `HOME` and `PATH` environment variables are explicitly set (launchd has minimal env)
- `KeepAlive.Crashed=true` restarts on crashes; `SuccessfulExit=false` doesn't restart clean exits
- `ThrottleInterval=10` prevents rapid restart loops
- Logs to `~/.config/claude-telegram-mirror/daemon.log`

---

## Security Considerations

1. **Socket Security**: Unix socket in user config dir with `0600` permissions
2. **PID Locking**: Prevents multiple daemon instances
3. **Chat Whitelist**: Only responds to configured `TELEGRAM_CHAT_ID`
4. **Topic Ownership**: Each daemon only processes its own topics
5. **No Secrets in Logs**: Tokens and sensitive data not logged

---

## Message Types

| Type | Direction | Description |
|------|-----------|-------------|
| `session_start` | Internal | Session created (topic creation, not sent by hooks) |
| `session_end` | CLI → TG | Session ended |
| `agent_response` | CLI → TG | Claude's text response |
| `tool_start` | CLI → TG | Tool execution started |
| `tool_result` | CLI → TG | Tool execution completed |
| `user_input` | Both | User prompt/message |
| `approval_request` | CLI → TG | Permission request |
| `approval_response` | TG → CLI | User approval/rejection |
| `turn_complete` | CLI → TG | Claude turn finished (not session end) |
| `pre_compact` | CLI → TG | Context compaction starting |

---

## Dual Handler Architecture (v0.1.8+)

The system uses two different handlers for different hook types:

### PreToolUse: Node.js Handler (`handler.ts`)

Used for approval workflows where we need to wait for user response:

```
┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│ Claude Code │───▶│  handler.ts │───▶│   Bridge    │───▶│  Telegram   │
│ PreToolUse  │    │   (Node)    │    │   Daemon    │    │  (buttons)  │
└─────────────┘    └──────┬──────┘    └──────┬──────┘    └──────┬──────┘
                         │                   │                  │
                         │◀── approval_response ◀───────────────┘
                         │     (allow/deny/abort)    User clicks
                         ▼
                   Returns to Claude:
                   hookSpecificOutput: {
                     permissionDecision: 'allow'|'deny'
                   }
```

**Why Node.js?**
- Requires async `sendAndWait()` for bidirectional response
- 5-minute timeout for user to respond
- Returns structured output to Claude's permission system

### Other Hooks: Bash Script (`telegram-hook.sh`)

Used for fire-and-forget notifications (Stop, PostToolUse, Notification, etc.):

```
┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│ Claude Code │───▶│ telegram-   │───▶│   Bridge    │───▶ Telegram
│  Stop/etc   │    │ hook.sh     │    │   Daemon    │
└─────────────┘    └─────────────┘    └─────────────┘
                   (exits immediately)
```

**Why Bash?**
- Faster startup (~5ms vs ~50ms for Node)
- No async needed - just send and exit
- Lower overhead for high-frequency events

---

## Session ID Stability (v0.1.8 Fix)

### The Problem (pre-v0.1.8)

The Node handler generated its own session IDs:

```typescript
// OLD (broken): Generated random IDs
this.sessionId = this.config.sessionId || this.generateSessionId();
// Result: "hook-m1w2x3-abc123" - different each invocation!
```

This caused multiple Telegram topics per Claude session.

### The Fix (v0.1.8)

Now uses Claude's native `session_id` from hook events:

```typescript
// NEW (fixed): Uses Claude's session_id
const event = JSON.parse(input) as AnyHookEvent;
const handler = new HookHandler({
  sessionId: event.session_id  // Claude's stable ID
});
```

```bash
# Bash script also uses Claude's session_id
CLAUDE_SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty')
SESSION_ID="${CLAUDE_SESSION_ID:-$(date +%s)-$$}"
```

**Result:** All events from the same Claude session route to the same Telegram topic.

---

## CLI Lifecycle Commands (v0.1.16+)

The CLI provides commands to manage the daemon lifecycle:

```bash
ctm start              # Start daemon (foreground mode)
ctm stop               # Graceful shutdown (SIGTERM, 5s timeout)
ctm stop --force       # Force kill if graceful fails
ctm restart            # Stop + start in one command
ctm status             # Show running state, PID, socket status
```

**Auto-detection:** Commands detect whether daemon runs directly or via OS service (systemd/launchd) and delegate appropriately.

**Cleanup:** Stale PID and socket files are automatically removed on stop/restart.

---

## Stale Session Cleanup (v0.1.14+)

Sessions with dead tmux panes are automatically cleaned up:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        Stale Session Detection                              │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Cleanup triggers when ALL conditions met:                                  │
│                                                                             │
│  1. last_activity > TELEGRAM_STALE_SESSION_TIMEOUT_HOURS (default: 72h)     │
│  2. AND one of:                                                             │
│     - tmux pane no longer exists                                            │
│     - tmux pane reassigned to different Claude session                      │
│                                                                             │
│  Actions on cleanup:                                                        │
│  - Send "Session ended (terminal closed)" to Telegram topic                 │
│  - Close the forum topic                                                    │
│  - Mark session as 'ended' in database                                      │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

Cleanup runs every 5 minutes while daemon is running.

---

## Session Reactivation (v0.1.17+)

Sessions may be incorrectly marked as 'ended' while Claude is still running. The daemon auto-reactivates them:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Session Reactivation Flow                           │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  1. Session exists with status='ended'                                      │
│  2. New hook event arrives for that session                                 │
│  3. ensureSessionExists() detects status != 'active'                        │
│  4. reactivateSession() sets status='active', updates last_activity         │
│  5. Messages now route correctly to the existing topic                      │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Why needed:** A session may be marked 'ended' when a Stop hook fires, but Claude can continue if the user sends more input. Reactivation ensures Telegram→CLI input works even after premature session end.

---

## General Topic Filtering (v0.1.15+)

Messages in the forum's General topic (no `threadId`) are ignored:

```typescript
// In setupBotHandlers()
if (!threadId) {
  // Message in General topic - ignore
  // Bot can still WRITE to General (startup/shutdown notifications)
  return;
}
```

**Why:** Prevents confusion when users accidentally post in General instead of a session topic. Only messages in specific forum topics are routed to Claude sessions.

---

## Topic Creation Race Condition Handling (v0.1.13+, updated v0.1.18)

When events arrive for a new session, the daemon creates the topic on-the-fly:

```typescript
// ensureSessionExists() handles all topic creation
private async ensureSessionExists(msg: BridgeMessage): Promise<void> {
  // Check if session exists and is active
  const existing = this.sessions.getSession(msg.sessionId);
  if (existing) {
    if (existing.status !== 'active') {
      this.sessions.reactivateSession(msg.sessionId);  // BUG-009 fix
    }
    return;
  }

  // Check if another call is already creating this topic
  if (this.topicCreationPromises.has(sessionId)) {
    await this.topicCreationPromises.get(sessionId);  // Wait for it
    return;
  }

  // Create Promise BEFORE async work (synchronous check-and-set)
  const promise = new Promise(...);
  this.topicCreationPromises.set(sessionId, promise);

  // Create session + topic immediately
  await this.handleSessionStart(msg);
}
```

**Behavior:**
- First event for a new session triggers topic creation immediately
- Concurrent events wait for the Promise (prevents duplicate topics)
- 5-second timeout prevents indefinite blocking
- On timeout: error logged, message dropped (prevents misdirection to General topic)

**Race condition safety:** JavaScript's single-threaded event loop guarantees no interleaving between the synchronous check (`has()`) and set (`set()`) operations.

---

## Telegram Input Commands

Commands sent from Telegram to control Claude:

### Claude Code Commands (cc prefix)

Use `cc <command>` to forward commands to Claude Code:

| Command | Description |
|---------|-------------|
| `cc clear` | Clear conversation history |
| `cc compact` | Compress context |
| `cc cost` | Show token usage |
| `cc review` | Code review mode |
| `cc model` | Switch model |
| `cc <skill>` | Run any custom skill |

**Why `cc` instead of `/`?** Telegram's slash commands cannot be passed correctly inside topics/threads. The `cc` prefix works reliably as plain text.

### Control Commands (sends special keys)

| Category | Commands | Key Sent |
|----------|----------|----------|
| **Interrupt** | `stop`, `cancel`, `abort`, `esc`, `escape` | Escape |
| **Kill** | `kill`, `exit`, `quit`, `ctrl+c`, `ctrl-c`, `^c` | Ctrl-C |

Control commands work with or without prefix: `stop`, `/stop`, or `cc stop` all work.

**Interrupt vs Kill:**
- **Escape** pauses Claude mid-generation (can resume)
- **Ctrl-C** exits Claude entirely (session ends)

---

## Troubleshooting

| Issue | Cause | Solution |
|-------|-------|----------|
| "No tmux session found" | Daemon can't access tmux socket | Set `PrivateTmp=false` in systemd |
| Messages not appearing | Hook not installed | Run `ctm install-hooks` |
| Wrong topic | Session mapping lost | Check `sessions.db` for correct `thread_id` |
| Duplicate topics | Daemon restarted mid-session | Topics are reused if `thread_id` exists in DB |
| Stale sessions | Old sessions not cleaned | Check `TELEGRAM_STALE_SESSION_TIMEOUT_HOURS` |
| Topics not created (clean install) | Outdated daemon version | Upgrade to v0.1.18+ (fixed topic creation on first event) |
| Input from Telegram silently fails | Session was marked 'ended' | Session auto-reactivates on next hook event (v0.1.17+) |
| macOS: "node not found" | launchd minimal PATH | Reinstall service to regenerate plist with full PATH |
| macOS: Daemon crashes on start | Missing HOME env | Check `daemon.err.log` for details |
