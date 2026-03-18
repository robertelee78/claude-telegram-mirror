# Claude Telegram Mirror -- Architecture

A bidirectional bridge between Claude Code CLI sessions and Telegram, written in Rust. The system mirrors Claude Code activity to Telegram forum topics and routes Telegram replies back into the CLI via tmux.

## 1. System Overview

```mermaid
flowchart LR
    subgraph CLI["Claude Code (tmux)"]
        CC[Claude Code CLI]
        Hooks[Hook Events<br/>PreToolUse, PostToolUse,<br/>Stop, SessionEnd, etc.]
    end

    subgraph Hook["ctm hook binary"]
        HookProc["ctm hook<br/>(short-lived process)"]
    end

    subgraph Daemon["Bridge Daemon"]
        Socket["Unix Socket Server<br/>bridge.sock"]
        EventLoop["Async Event Loop<br/>tokio::select!"]
        Sessions["SessionManager<br/>SQLite"]
        Injector["InputInjector<br/>tmux send-keys"]
    end

    subgraph TG["Telegram"]
        BotAPI["Telegram Bot API"]
        Forum["Supergroup<br/>Forum Topics"]
    end

    CC -->|fires hook| Hooks
    Hooks -->|stdin JSON| HookProc
    HookProc -->|NDJSON via Unix socket| Socket
    Socket --> EventLoop
    EventLoop -->|sendMessage| BotAPI
    BotAPI --> Forum

    Forum -->|long polling getUpdates| EventLoop
    EventLoop -->|lookup session + tmux target| Sessions
    EventLoop --> Injector
    Injector -->|"tmux send-keys -t target"| CC

    HookProc -.->|"PreToolUse: send_and_wait<br/>blocks up to 5 min"| Socket
    Socket -.->|"approval_response"| HookProc
```

**Outbound (CLI to Telegram):** Claude Code fires hook events on tool use, completions, notifications, and prompts. Each event invokes the `ctm hook` binary, which reads the JSON event from stdin, converts it to one or more `BridgeMessage` structs, and writes them as NDJSON lines over a Unix domain socket. The bridge daemon receives these messages via a `tokio::sync::broadcast` channel, routes them to the correct Telegram forum topic, and sends them through the Bot API.

**Inbound (Telegram to CLI):** The daemon long-polls Telegram for updates. When a user replies in a forum topic, the daemon looks up the associated session in SQLite, resolves the tmux target via a three-tier lookup (in-memory cache, DB, live detection), and injects the text into the correct tmux pane using `tmux send-keys`.

**Approval workflow (PreToolUse):** For tools that require permission (Write, Edit, Bash with non-safe commands), the hook binary sends an `approval_request` message and blocks on the socket waiting for a correlated `approval_response`. The daemon presents inline keyboard buttons in Telegram. When the user taps Approve, Reject, or Abort, the daemon writes the response back to the hook's socket connection. The hook returns a `hookSpecificOutput` JSON to Claude Code with the permission decision. Timeout is 5 minutes; connection-refused means the daemon is not running, so the hook returns `None` and Claude continues normally.

## 2. Module Structure

The crate lives at `rust-crates/ctm/` and compiles to both a binary (`main.rs`) and a library (`lib.rs`). There are 30 source files across 16 top-level modules and three sub-module groups (`bot/`, `daemon/`, `service/`).

### Top-level modules

**main.rs** -- Binary entry point. Defines the `clap` CLI with subcommands (`hook`, `start`, `stop`, `restart`, `status`, `config`, `install-hooks`, `uninstall-hooks`, `hooks`, `setup`, `doctor`, `service`, `toggle`). Initializes the `tracing` subscriber with a custom `ScrubWriter` that strips Telegram bot tokens from all log output before it reaches stderr. Handles SIGINT and SIGTERM via `tokio::signal` for graceful async shutdown.

**lib.rs** -- Library re-exports. Makes all modules public so downstream Rust consumers can depend on the crate (e.g., `use ctm::session::SessionManager`).

**config.rs** -- Configuration loading with three-tier priority: environment variables, then `~/.config/claude-telegram-mirror/config.json` (supporting both camelCase and snake_case keys via serde aliases), then compiled defaults. Key fields: `bot_token`, `chat_id`, `enabled`, `verbose`, `approvals`, `use_threads`, `chunk_size`, `rate_limit` (default 20), `session_timeout`, `stale_session_timeout_hours`, `auto_delete_topics`, `topic_delete_delay_minutes` (default 15), `inactivity_delete_threshold_minutes` (default 720), `socket_path`, `config_dir`, `config_path`, and `forum_enabled`. Validates socket paths against traversal attacks. Manages the runtime `status.json` file for the mirroring toggle. Ensures the config directory exists with `0o700` permissions. Custom `Debug` impl redacts `bot_token` to `"[REDACTED]"` (ADR-010 S-4).

**error.rs** -- Centralized error types using `thiserror`. Defines `AppError` with variants for Config, Io, Json, Socket, Injection, Hook, Database, Lock, Telegram, Reqwest, and RateLimited errors. All public functions return `Result<T, AppError>`.

**types.rs** -- Shared data types. Defines `HookEvent` (a tagged enum deserialized from Claude Code's JSON, with 8 variants: `Stop`, `SubagentStop`, `PreToolUse`, `PostToolUse`, `Notification`, `UserPromptSubmit`, `PreCompact`, and `SessionEnd`). `Stop` fires after every assistant turn; `SessionEnd` fires exactly once when the session actually terminates (process exit, `/clear`, logout). The distinction is critical: sending `session_end` on `Stop` would kill the session after every turn. Also defines `BridgeMessage` (the NDJSON wire format), `MessageType` (an enum with a forward-compatible `Unknown` catch-all via `#[serde(other)]`), `HookResult`, `SessionStatus` and `ApprovalStatus` typed enums, validation functions (`is_valid_session_id`, `is_valid_agent_id`, `is_valid_slash_command`), security constants (`SAFE_COMMANDS`, `ALLOWED_TMUX_KEYS`, `MAX_SESSION_ID_LEN`, `MAX_LINE_BYTES`), and ADR-013 helpers (`extract_parent_session_id`, `extract_agent_id`).

**hook.rs** -- Hook event processing, the entry point for `ctm hook`. Reads stdin with a 1 MiB size limit, parses the JSON into a `HookEvent`, validates the session ID, detects the current tmux session from `$TMUX`, and builds one or more `BridgeMessage` values. For `PreToolUse` events that require approval, it calls `send_and_wait` to block on the socket for up to 5 minutes. For `Stop` events, it extracts the transcript summary or falls back to reading the JSONL transcript file, with `transcript_path` validated against path traversal before opening (ADR-010 S-1). For `SessionEnd` events, it sends a `session_end` message and cleans up the transcript state file. Always sends a `session_start` message first (the daemon deduplicates).

**injector.rs** -- Input injection via tmux. All tmux commands use `Command::arg()` with no shell interpolation, preventing command injection. Validates tmux socket paths (must be absolute, no `..`, max 256 chars). Provides `inject()` (text with `-l` literal flag + Enter), `send_key()` (from a whitelist of safe keys), and `send_slash_command()` (character-validated). Includes `detect_tmux_session()` which reads `$TMUX` and queries tmux for session/window/pane, `find_claude_code_session()` as a fallback that searches running pane commands, and `is_pane_alive()` for stale session detection.

**session.rs** -- SQLite-backed session and approval manager. Uses `rusqlite` with file permissions set to `0o600`. Schema has two tables: `sessions` (columns: `id`, `chat_id`, `thread_id`, `hostname`, `tmux_target`, `tmux_socket`, `started_at`, `last_activity`, `status`, `project_dir`, `metadata`, `parent_session_id`, `agent_id`, `agent_type`) and `pending_approvals` (with expiry tracking). The `Session` struct has 14 fields matching these columns. Supports session creation (with auto-heal of tmux/hostname metadata on re-insert), reactivation, thread ID management, tmux info storage, parent-child tracking (`set_parent_info`, `get_child_sessions`), stale candidate queries, orphaned thread detection, and approval lifecycle (create, resolve, expire). Uses ISO 8601 TEXT timestamps. Includes automatic schema migrations for tmux columns and ADR-013 parent columns (`parent_session_id`, `agent_id`, `agent_type`). Session IDs validated via `is_valid_session_id()` at the persistence boundary (ADR-010 C-5). `end_session` wraps its two SQL statements in a single transaction for atomicity (ADR-010 C-4).

**socket.rs** -- Unix domain socket server and client with NDJSON framing. The server uses `flock(2)` on the PID file for atomic single-instance enforcement. After `bind()`, applies `chmod(0o600)` to the socket file (ADR-009). Supports up to 64 concurrent connections. Each client is handled in a spawned tokio task that reads NDJSON lines and broadcasts them via `tokio::sync::broadcast`. The client side provides `connect`, `send`, `send_and_wait` (with timeout and session ID correlation), and `disconnect`.

**bot/ (module)** -- Telegram Bot API client. Split into three sub-modules: `client.rs` (the `TelegramBot` struct with dual HTTP clients -- a 15s-timeout client for API calls and a 45s-timeout `poll_client` for `getUpdates` long polling, kept in separate connection pools so long-poll latency never blocks API calls; AIMD adaptive rate control via `AimdState` with additive increase of 0.5 msg/sec on success and multiplicative decrease by 0.5x on 429, debounced to at most once per second; `governor` rate limiter clamped to `[1, 30]` msgs/sec (ADR-009)), `queue.rs` (three-tier priority message queue `PriorityMessageQueue` with per-tier caps: `MAX_CRITICAL = 50`, `MAX_NORMAL = 300`, `MAX_LOW = 150` (500 total), with oldest-eviction per tier, RAII `ProcessingGuard` to prevent permanent queue stalls on task cancellation (ADR-010 C-2), overflow-safe exponential backoff using `saturating_mul`, jitter on 429 retry via `simple_jitter_fraction()`, TOPIC_CLOSED recovery with immediate error return on failed reopen (ADR-010 C-3), and entity parse error fallback to plain text), and `types.rs` (Telegram API response types including `Update`, `TgMessage`, `CallbackQuery`, `InlineButton`, `ResponseParameters` with `adaptive_retry` field for Bot API 8.0+, etc.). Bot token scrubbing uses a compiled regex `bot\d+:[A-Za-z0-9_-]+/` applied globally.

**daemon/ (module)** -- The bridge daemon, the central orchestrator. Split into seven sub-modules: `mod.rs` (the `Daemon` struct with `start()`/`stop()`, `DaemonState` with all shared state, the `HandlerContext` passed to all handlers with 16 fields, documented lock ordering, `ensure_session_exists` with atomic topic creation locks, `get_tmux_target` with three-tier detection (cache, DB, live fallback via `find_claude_code_session`), startup cache warming from DB, echo prevention via null-separated keys, and `db_op` returning `R::default()` instead of panicking on task cancellation (ADR-010 S-3)), `event_loop.rs` (the main `tokio::select!` loop multiplexing socket messages, Telegram long-polling, and a 5-minute cleanup timer, with bounded concurrency via `Semaphore(50)` and poll backoff with jitter), `socket_handlers.rs` (handlers for each `MessageType` including parent-child routing via `extract_parent_session_id`, child message prefix via `get_child_prefix`, orphan cascade on parent `session_end`, spawn notifications with agent_type, and SubagentStop completion with Details button), `telegram_handlers.rs` (handlers for Telegram messages and commands, ADR-012 tentative free-text answers via `handle_free_text_answer`, and ADR-013 injection failure warnings on every attempt), `callback_handlers.rs` (handlers for inline keyboard callbacks: approval responses, tool detail expansion, ADR-012 tentative selection with `TentativeAnswer` enum, multi-select toggle, review screen with "Submit All"/"Change QN" buttons, per-key `Mutex<PendingQuestion>` to prevent races between free-text and button taps, and ADR-013 sub-agent Details button with `.md` file attachment), `cleanup.rs` (stale session detection with differentiated timeouts, two-stage topic lifecycle (close after `topic_delete_delay_minutes` then delete after `inactivity_delete_threshold_minutes`), cache size limits (`MAX_SESSION_CACHE=200`, `MAX_TOOL_CACHE=500`), download directory cleanup, `.last_line_*` state file cleanup (ADR-009), and `/tmp/ctm-subagent-*.md` temp file cleanup), and `files.rs` (file download handling for photos and documents, `sanitize_filename`, `validate_send_image_path`, `is_image_extension`).

**formatting.rs** -- Message formatting and chunking for Telegram display. ANSI escape stripping, MarkdownV2 escaping, message chunking that respects code block boundaries, tool-specific detail formatting, heuristic language detection, and path truncation. All chunking uses `char_indices()` for char-boundary safety with multibyte UTF-8 (ADR-010 U-1, U-2).

**summarize.rs** -- Human-readable one-liner summaries for tool actions. Handles 30+ Bash command patterns, strips wrapper commands, and skips trivial prefixes.

**installer.rs** -- Hook installer that modifies Claude Code's `settings.json`. Installs hooks for 6 event types. Idempotent with Added/Updated/Unchanged reporting.

**service/ (module)** -- OS service management for systemd (Linux) and launchd (macOS). Four sub-modules: `mod.rs`, `systemd.rs`, `launchd.rs`, and `env.rs`.

**setup.rs** -- Interactive setup wizard using `dialoguer`. **doctor.rs** -- Diagnostic checker with `--fix` auto-remediation. **colors.rs** -- ANSI color helpers for terminal output.

## 3. Binary Distribution

The Rust binary is distributed through npm using a platform-specific optional dependency pattern.

```
claude-telegram-mirror (main package)
  |
  +-- scripts/ctm-wrapper.cjs     # Entry point (npm "bin" field)
  |     Tries native binary first, falls back to local dev build.
  |
  +-- scripts/resolve-binary.cjs  # Binary resolution logic
  |     Maps os.platform()+os.arch() to package name.
  |
  +-- optionalDependencies:
        @agidreams/ctm-linux-x64    # x86_64 Linux (glibc)
        @agidreams/ctm-linux-arm64  # aarch64 Linux (glibc)
        @agidreams/ctm-darwin-arm64 # Apple Silicon macOS
        @agidreams/ctm-darwin-x64   # Intel macOS
```

Each platform package contains a single pre-compiled `ctm` binary. The wrapper resolves the correct binary at runtime with `verifyBinaryIntegrity()` (ADR-006 L3.8). Falls back to `rust-crates/target/release/ctm` on unsupported platforms.

## 4. Communication Flow

### CLI to Telegram path

1. Claude Code fires a hook event and invokes `ctm hook`.
2. `ctm hook` reads stdin (1 MiB limit), parses it as a `HookEvent`, validates `session_id`.
3. The hook detects tmux from `$TMUX` and resolves hostname.
4. `build_messages()` constructs `BridgeMessage` values. A `session_start` is always first (daemon deduplicates). The specific types depend on the event: `tool_start` for PreToolUse, `tool_result` for PostToolUse, `agent_response`/`turn_complete` for Stop, `session_end` for SessionEnd, `user_input` for UserPromptSubmit, etc.
5. Messages are serialized as NDJSON and written to the Unix socket.
6. The daemon broadcasts them via `tokio::sync::broadcast`.
7. The event loop spawns a handler task (bounded by `Semaphore(50)`).
8. `handle_socket_message()` validates the session ID, updates activity timestamps, auto-refreshes the tmux target, checks the mirroring toggle, and dispatches.
9. `ensure_session_exists()` creates session and topic on-the-fly with `TopicCreationState`/`Notify` to prevent duplicate topics. Sub-agent hooks carry `agent_id` in the base event and share the parent's `session_id` — no new session or topic is created. Headless daemon tasks (detected via `CLAUDE_CODE_HEADLESS` env var) are suppressed entirely.
10. The handler formats and sends the message to the correct forum topic. Child session messages are prefixed with an agent label via `get_child_prefix`.

### Telegram to CLI path

1. The daemon calls `bot.get_updates()` with long polling (30s timeout) via the dedicated `poll_client`.
2. `handle_telegram_message()` checks the thread, looks up the session, and resolves the tmux target via `get_tmux_target` (three-tier: cache, DB, live detection).
3. Special commands (/status, /help, /mute, /attach, /abort, /toggle, /ping, /sessions, /compact, /clear, /rename) are handled directly.
4. Interrupt commands send Escape; kill commands send C-c.
5. Regular text is injected via `InputInjector::inject()` with `-l` literal flag + Enter.
6. An echo key is added to `recent_telegram_inputs` with a 10-second TTL.
7. If tmux is unavailable or injection fails, the user sees a warning on every attempt (ADR-013 D1/D2).

### Approval workflow

1. A PreToolUse hook event arrives for a tool requiring approval.
2. The hook sends `approval_request` and calls `send_and_wait()` (5-minute timeout).
3. The daemon creates a `PendingApproval` in SQLite, sends it to Telegram with inline keyboard.
4. `handle_callback_query()` resolves the approval, routes the response to the specific socket client (ADR-010 S-2), edits the Telegram message, and answers the callback.
5. The hook returns `permissionDecision: "allow"`, `"deny"`, or `"ask"` (fallback to CLI on timeout).

### AskUserQuestion workflow (ADR-012)

1. An `AskUserQuestion` tool use arrives with questions and options.
2. The daemon renders each question as a Telegram message with inline buttons and stores a `PendingQuestion` keyed by session ID.
3. Button taps set tentative answers via `TentativeAnswer::Option` or `TentativeAnswer::MultiOption` (toggle). Free-text replies set `TentativeAnswer::FreeText`. All mutations use a per-key `Mutex<PendingQuestion>`.
4. When all questions have tentative answers, a summary review screen is sent with "Submit All" and "Change QN" buttons.
5. "Submit All" injects answers into tmux (single-select as text+Enter, multi-select as Space/Down key sequences). "Change QN" returns to the question.

## 5. Session Lifecycle

### Creation

Sessions are created lazily via `ensure_session_exists()`. When the first event arrives for a new session ID:

1. Check SQLite. If found and active, return immediately.
2. If found but ended/aborted, reactivate (status set to `active`, cancel pending topic deletion).
3. If not found, acquire a topic creation lock (`TopicCreationState` with `Notify`) and create both the SQLite row and Telegram forum topic.
4. Sub-agent detection (ADR-013 Phase 9): Claude Code's hook events carry `agent_id` in the base fields for sub-agents, and sub-agents share the parent's `session_id`. When `agent_id` is present, the daemon knows this is a sub-agent — no new session or topic is created. The path heuristic (`/subagents/` in `transcript_path`) is retained as a secondary signal. Headless sessions without `agent_id` (claude-flow daemon tasks) are suppressed — no topic created.

### Active

Every hook event updates `last_activity`. The tmux target is auto-refreshed on every message if changed. Child session messages are prefixed with the agent label.

### Stale cleanup

The daemon runs cleanup every 5 minutes:

- **Sessions with tmux info:** Candidates older than 24 hours. Daemon checks `is_pane_alive()`. Dead or reassigned panes trigger cleanup.
- **Sessions without tmux info:** Candidates older than 1 hour (shorter timeout to avoid orphan accumulation).
- **Inactivity sweep (ADR-013 E2):** Active sessions idle longer than `inactivity_delete_threshold_minutes` (default 720 = 12 hours) have their topics deleted and sessions ended.

### End

When a `session_end` message arrives (from a `SessionEnd` hook event -- not `Stop`):

1. Mark the session as `ended` in SQLite (atomic transaction with approval expiry).
2. Send a session-ended notification to the forum topic.
3. Cascade to child sessions: end all active children by `parent_session_id` (ADR-013 GAP-5).
4. Schedule two-stage topic lifecycle (ADR-013 E5):
   - **Stage 1:** Close the topic after `topic_delete_delay_minutes` (default 15). Closes the topic (hides from list, preserves history).
   - **Stage 2:** Delete the topic after `inactivity_delete_threshold_minutes` (default 720). Full cleanup.
5. If a new event arrives before deletion, the pending deletion is cancelled and the session is reactivated.

## 6. Security Model

For full details, see `docs/SECURITY.md`. Key properties:

**No shell interpolation.** All `Command::arg()`, never shell strings.

**flock(2) for single-instance.** Atomic lock on PID file, no TOCTOU.

**Post-bind chmod for socket.** `chmod(0o600)` after `bind()`, parent dir `0o700`.

**Transcript path validation (ADR-010 S-1).** Must be absolute, canonicalized, within home dir, no null bytes.

**Approval response routing (ADR-010 S-2).** Routed to the specific socket client that submitted the request.

**Agent ID validation (ADR-013 GAP-1).** `is_valid_agent_id()` rejects `/`, `\`, `..`, and non-ASCII-alphanumeric characters. Prevents path traversal when constructing `/tmp/ctm-subagent-{agent_id}.md` file paths from user-controlled callback data.

**Bot token scrubbing.** Global `ScrubWriter` on stderr with regex. `Config` implements `Debug` with `bot_token` redacted (ADR-010 S-4).

**Session ID validation.** Non-empty, max 128 chars, ASCII alphanumerics plus `-`, `_`, `.`.

**Slash command validation.** Character whitelist rejects shell metacharacters.

**tmux socket path validation.** Absolute, no `..`, max 256 chars. Bridge socket validated against AF_UNIX `sun_path` limit of 104 bytes (ADR-009).

**Secure file permissions.** Config dir `0o700`, SQLite/config/status files `0o600`, downloads `0o600`.

**Input size limits.** Stdin and NDJSON lines limited to 1 MiB. Socket server max 64 connections. Handler concurrency bounded by `Semaphore(50)`.

## 7. Concurrency Model

The application uses the tokio async runtime with a multi-threaded executor.

### Event loop

The daemon's main event loop (`event_loop.rs`) uses `tokio::select!` to multiplex three event sources:

1. **Socket messages** via `tokio::sync::broadcast::Receiver<BridgeMessage>`. Each spawned in a tokio task bounded by `Semaphore(50)` (ADR-011 Fix #6).
2. **Telegram updates** via `bot.get_updates()` long polling. Poll failures use exponential backoff with jitter (10s/20s/40s/80s cap, ~20% jitter) (ADR-011 Fix #4).
3. **Cleanup timer** every 5 minutes via `tokio::time::interval`. Exempt from the semaphore.

### Shared state

The `HandlerContext` struct has 16 fields. Lock types use `Arc<RwLock<T>>` for read-heavy data and `Arc<Mutex<T>>` for write-heavy or externally-synchronized data:

| State | Type | Rationale |
|-------|------|-----------|
| `sessions` | `Arc<Mutex<SessionManager>>` | SQLite single-writer; `spawn_blocking` via `db_op` |
| `session_threads` | `Arc<RwLock<HashMap>>` | Read-heavy, writes on topic creation |
| `session_tmux` | `Arc<RwLock<HashMap>>` | Read-heavy, updated on tmux target change |
| `recent_inputs` | `Arc<RwLock<HashSet>>` | Echo suppression keys with TTL |
| `tool_cache` | `Arc<RwLock<HashMap>>` | Cached tool inputs for Details button |
| `compacting` | `Arc<RwLock<HashSet>>` | Sessions in compact state |
| `pending_del` | `Arc<RwLock<HashMap>>` | Pending topic deletion `JoinHandle`s |
| `custom_titles` | `Arc<RwLock<HashMap>>` | Session custom title cache |
| `pending_q` | `Arc<RwLock<HashMap<_, Arc<Mutex<PendingQuestion>>>>>` | ADR-012 per-key Mutex for tentative answers |
| `topic_locks` | `Arc<RwLock<HashMap>>` | BUG-002 topic creation dedup |
| `bot_sessions` | `Arc<RwLock<HashMap>>` | Per-thread bot session state |
| `mirroring_enabled` | `Arc<AtomicBool>` | Runtime toggle |
| `config` | `Arc<Config>` | Immutable after startup |
| `injector` | `Arc<Mutex<InputInjector>>` | tmux target state |
| `socket_clients` | `SocketClients` | Client connection map |
| `pending_approval_clients` | `Arc<RwLock<HashMap>>` | S-2: approval_id -> client_id |

**Lock ordering** is documented above `HandlerContext`: (1) sessions, (2) session_threads, (3) session_tmux, (4) all other RwLocks in field declaration order (recent_inputs, tool_cache, compacting, pending_del, custom_titles, pending_q, topic_locks, bot_sessions), (5) injector, (6) socket_clients. Most handlers acquire only one lock at a time.

### Rate limiting (ADR-011)

Two-layer rate control on the `TelegramBot`:

1. **AIMD adaptive delay** (`AimdState`): starts at `max_rate` (from config, default 20 msg/sec). Additive increase of 0.5 msg/sec per successful send, multiplicative decrease by 0.5x on Telegram 429. Debounced to at most one decrease per second. Floor of 0.5 msg/sec.
2. **Governor ceiling**: `governor::RateLimiter` clamped to `[1, 30]` msgs/sec.

Queue bound: 500 total (50 critical + 300 normal + 150 low). Retry: overflow-safe exponential backoff with `saturating_mul` (3 retries at 1s/2s/4s). Jitter on 429 via `simple_jitter_fraction()`. TOPIC_CLOSED recovery: reopen + retry. Entity parse error: strip formatting, retry as plain text.

### Cache size limits (ADR-011 Fix #7)

During cleanup, caches are evicted when they exceed thresholds: `MAX_SESSION_CACHE = 200` (for `session_threads`, `session_tmux`, `custom_titles`) and `MAX_TOOL_CACHE = 500`. Eviction prioritizes entries for inactive sessions.

### Signal handling

SIGINT and SIGTERM trigger graceful shutdown: notification to Telegram, socket cleanup, clean exit.

### Mirroring toggle

`Arc<AtomicBool>` with `status.json` persistence. Safety-critical paths (approvals, commands) always bypass the toggle.

## 8. Test Suite

The project has 550+ tests across unit and integration test suites, with 0 failures and 0 clippy warnings.

### Unit tests

Co-located in each source module (30 source files across `src/`). Cover configuration parsing, session management, formatting, summarization, socket communication, error handling, security validation, AIMD rate control, priority queue ordering, and ADR-013 parent/agent ID extraction.

### Integration tests

Ten integration test files in `rust-crates/ctm/tests/`:

| File | Coverage |
|------|----------|
| `bot_tests.rs` | Telegram bot client: message sending, forum topic management, rate limiting, callback handling |
| `cli_smoke.rs` | Binary invocation, subcommand parsing, `--help` and `--version` output |
| `concurrency.rs` | Multi-threaded socket access, concurrent session operations |
| `config_validation.rs` | Environment variable parsing, config file loading, three-tier priority |
| `daemon_handlers.rs` | Socket and Telegram handler logic: session routing, approval flow, echo prevention, cleanup |
| `formatting_tests.rs` | ANSI stripping, MarkdownV2 escaping, message chunking, path truncation |
| `hook_pipeline.rs` | Hook event parsing, message building, approval workflow plumbing |
| `session_lifecycle.rs` | Session create/end/reactivate, stale cleanup, approval expiry |
| `socket_roundtrip.rs` | Full NDJSON roundtrip through socket server, connection limits, PID locking |
| `summarize_tests.rs` | Tool summarizer coverage for all 30+ command patterns |

### Running tests

```bash
cd rust-crates
cargo test          # All 550+ tests
cargo test -- -q    # Quiet output
cargo clippy        # Lint (0 warnings required)
```

## 9. Architecture Decision Records

| ADR | Title | Key changes |
|-----|-------|-------------|
| ADR-008 | v0.2.0 Release Readiness Audit | Structural decomposition (17 -> 30 source files), integration test suite (8 files), binary integrity verification, npm distribution pipeline |
| ADR-009 | Release Polish -- Broken Windows | 19 fixes: umask race elimination, socket path limit (104 bytes), topic creation atomic write lock, rate limiter bounds `[1, 30]`, queue bound (500), overflow-safe backoff, char-count consistency, state file cleanup, duplicate code removal |
| ADR-010 | Deep Release Readiness Evaluation | 8-agent swarm audit (2 rounds). Round 1: 13 blockers fixed -- path traversal on `transcript_path` (S-1), approval response routing to specific client (S-2), `db_op` panic replaced with default (S-3), custom `Debug` impl redacting `bot_token` (S-4), echo key separator fix (C-1), RAII processing guard (C-2), TOPIC_CLOSED error return (C-3), atomic `end_session` transaction (C-4), session ID validation at persistence boundary (C-5), status enum validation (C-6), char-boundary-safe message chunking (U-1), char-safe truncation (U-2). Round 2: rate limit default 1->20, PID file `flock` locking, CI failure exit |
| ADR-011 | Resilience Architecture | AIMD adaptive rate control (additive increase 0.5/sec, multiplicative decrease 0.5x on 429), dual HTTP clients (15s API + 45s poll), cache size limits (`MAX_SESSION_CACHE=200`, `MAX_TOOL_CACHE=500`), bounded task spawning (`Semaphore(50)`), poll backoff with jitter, graceful degradation |
| ADR-012 | AskUserQuestion Tentative Selection | `TentativeAnswer` enum (Option, MultiOption, FreeText), `PendingQuestion` state machine with per-key `Mutex`, tentative selection with review screen ("Submit All"/"Change QN"), multi-select injection as Space/Down key sequences, per-entry lock preventing races between free-text input and button taps |
| ADR-013 | Session Hierarchy and tmux Reliability | Parent-child session routing via `extract_parent_session_id`, child-to-parent topic reuse, child message prefix (`get_child_prefix`), `agent_type` tracking, orphan cascade on parent end, spawn/completion notifications with Details button + `.md` file attachment, `is_valid_agent_id()` path traversal prevention, three-tier tmux detection (cache/DB/live), startup cache warming (F8), injection failure warnings (D1/D2), tmux status indicator (D3), two-stage topic lifecycle (close then delete), configurable thresholds (`topic_delete_delay_minutes=15`, `inactivity_delete_threshold_minutes=720`), inactivity cleanup sweep, temp file cleanup |

Full ADR documents are in `docs/adr/`.
