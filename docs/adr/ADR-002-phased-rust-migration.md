# ADR-002: Phased Rust Migration with Zero Feature Loss

> **DO NOT BE LAZY. We have plenty of time to do it right.**
>
> No shortcuts. Never make assumptions. Always dive deep and ensure you know the problem you're solving. Make use of search as needed. Measure 3x, cut once. No fallback. No stub (todo later) code. Just pure excellence, done the right way the entire time. Chesterton's fence: always understand the current implementation fully before changing it.

## Status

**Accepted (Complete)** (2026-03-16)

### Phase 1 Progress

| Story | Status | Commit |
|-------|--------|--------|
| 1.1 Rust scaffolding + CI | DONE | `d9ba27d` |
| 1.2 Config loader | DONE | `d9ba27d` |
| 1.3 Input injector | DONE | `d9ba27d` |
| 1.4 Hook event processing | DONE | `d9ba27d` |
| 1.5 PreToolUse approval | DONE | `d9ba27d` |
| 1.6 Stop transcript extraction | DONE | `d9ba27d` |
| 1.7 Binary distribution | DONE | `e1bc607` |

**Phase 1: COMPLETE.** Binary: 2.5MB release (stripped, LTO). 13 Rust tests. Clippy clean, fmt clean. 3 platform packages (linux-x64, darwin-arm64, darwin-x64).

### Phase 2 Progress

| Story | Status | Commit |
|-------|--------|--------|
| 2.1 SQLite session manager | DONE | `9715f92` |
| 2.2+2.3 Socket server+client (flock) | DONE | `9715f92` |
| 2.4+2.5 Formatting + chunker | DONE | `9715f92` |
| 2.6 Tool summarizer | DONE | `9715f92` |

**Phase 2: COMPLETE.** 145 Rust tests (124 new). Session manager with 24 methods. Socket server with flock(2) + umask. Real escapeMarkdownV2 (not no-op). UTF-8 safe chunking. 91 summarizer tests ported.

### Phase 3 Progress

| Story | Status | Commit |
|-------|--------|--------|
| 3.1 Bridge daemon | DONE | `8ccfc1d` |
| 3.2 Telegram bot | DONE | `8ccfc1d` |
| 3.3 Bot commands | DONE | `8ccfc1d` |
| 3.4 Full CLI | DONE | `8ccfc1d` |
| 3.5 Integration testing | DONE | `8ccfc1d` |

**Phase 3: COMPLETE.** 158 Rust tests. 8.9MB release binary. Daemon (2100 lines) with all 12+ message handlers, BUG-001 through BUG-012 preserved. Bot with governor rate limiting + retry/backoff. Full CLI with all commands.

### Phase 4 Progress

| Story | Status | Commit |
|-------|--------|--------|
| 4.1 Service manager (systemd + launchd) | DONE | Phase 4 commit |
| 4.2 Interactive setup wizard | DONE | Phase 4 commit |
| 4.3 Doctor with --fix | DONE | Phase 4 commit |
| 4.4 Hook installer | DONE | Phase 4 commit |

**Phase 4: COMPLETE.** 184 Rust tests. 9.3MB release binary. Service manager (870 lines), setup wizard (903 lines), doctor (875 lines), installer (564 lines). All TypeScript delegations replaced. Single binary, zero Node.js dependency.

---

## MIGRATION COMPLETE

All 4 phases shipped. The `ctm` binary is a fully self-contained Rust implementation with zero feature loss from the TypeScript codebase. 184 Rust tests + 195 TypeScript tests = 379 total tests across both codebases.

## Date

2026-03-16

## Context

On 2026-03-15, we analyzed the DreamLab-AI Rust fork of claude-telegram-mirror. Their rewrite demonstrated genuine security and performance benefits of Rust, but lost approximately 60% of features in the process: no approval workflow, no forum topic management, no session reactivation, no stale session cleanup, no echo prevention, `escapeMarkdownV2` was a no-op, and every bug fix from BUG-001 through BUG-012 was absent. Their code is a cautionary tale: Rust is the right language, but a careless rewrite destroys years of battle-tested behavior.

This ADR charts a path to Rust that preserves everything.

The DreamLab-AI fork code is **ignored as a codebase**. We write from scratch, informed by their ideas and our deep understanding of our own TypeScript. We steal the best ideas, not their code.

ADR-001 (TypeScript security and UX fixes) ships first and independently. This ADR executes only after ADR-001 is complete. If this migration fails at any phase, we fall back to the improved TypeScript from ADR-001 -- nothing is lost.

### Current Codebase Inventory

The TypeScript codebase consists of 8,190 lines across 16 source files:

| Module | File | Lines | Phase |
|--------|------|------:|-------|
| Hook handler | `src/hooks/handler.ts` | 530 | 1 |
| Hook types | `src/hooks/types.ts` | 118 | 1 |
| Input injector | `src/bridge/injector.ts` | 412 | 1 |
| Config loader | `src/utils/config.ts` | 278 | 1 |
| Bridge types | `src/bridge/types.ts` | 55 | 1 |
| Hook shell script | `scripts/telegram-hook.sh` | 423 | 1 |
| Session manager | `src/bridge/session.ts` | 552 | 2 |
| Socket server/client | `src/bridge/socket.ts` | 480 | 2 |
| Message formatting | `src/bot/formatting.ts` | 348 | 2 |
| Message chunker | `src/utils/chunker.ts` | 171 | 2 |
| Logger | `src/utils/logger.ts` | 41 | 2 |
| Bridge daemon | `src/bridge/daemon.ts` | 1,315 | 3 |
| Telegram bot | `src/bot/telegram.ts` | 525 | 3 |
| Bot commands | `src/bot/commands.ts` | 352 | 3 |
| CLI entry point | `src/cli.ts` | 626 | 3 |
| Service manager | `src/service/manager.ts` | 624 | 4 |
| Setup wizard | `src/service/setup.ts` | 716 | 4 |
| Doctor | `src/service/doctor.ts` | 490 | 4 |
| Hook installer | `src/hooks/installer.ts` | 573 | 4 |

## Decision

We will migrate claude-telegram-mirror from TypeScript to Rust in four sequential phases, each independently valuable, each requiring 100% feature parity before proceeding to the next. The DreamLab-AI fork code is not used. We write every line from scratch.

### Why Rust?

These are specific, measurable benefits -- not marketing:

1. **Shell injection eliminated by language design.** Rust's `std::process::Command::arg()` is the only way to invoke subprocesses. There is no `execSync` equivalent, no template string interpolation into shell commands. The entire class of shell injection vulnerabilities becomes impossible to write, not merely unlikely.

2. **`flock(2)` native via `nix` crate.** Proper PID file locking without TOCTOU races (BUG-008). The current TypeScript implementation checks PID file existence, reads it, checks `/proc`, then writes -- four separate operations with race windows between each. `flock(2)` is atomic.

3. **Single static binary.** Users run `ctm` without Node.js, npm, or `node_modules`. No supply chain risk from npm dependencies. No version compatibility issues between Node.js releases. No `postinstall.cjs` scripts. The binary is the product.

4. **~10MB RSS vs ~50MB+ for Node.js daemon.** V8's heap overhead is unavoidable. A Rust binary with SQLite linked statically uses an order of magnitude less memory.

5. **Hook binary cold start <1ms vs ~100-200ms for Node.js.** Every hook invocation currently pays V8 initialization cost. Hooks fire on every tool use, every prompt submission, every notification. Over a coding session with hundreds of tool calls, this adds up to minutes of cumulative latency. A native binary is warm before Node.js finishes parsing `package.json`.

6. **Type safety deeper than TypeScript.** Lifetimes and ownership prevent use-after-free. Exhaustive `match` on enums prevents unhandled cases at compile time (not at runtime like TypeScript's `default` clause that may or may not exist). No `undefined`, no `null` surprises, no `any` escape hatch.

7. **`umask(2)` for atomic file permission setting.** Socket files can be created with correct permissions atomically, rather than the create-then-chmod pattern that has a race window.

8. **Token-bucket rate limiting via `governor` crate.** Proper token-bucket algorithm instead of hand-rolled delay loops.

### What We Are NOT Doing

- **NOT adopting the DreamLab-AI fork code.** Their codebase is a reference for ideas only. We write from scratch.
- **NOT losing any features, commands, or bug fixes.** Every feature in every module is enumerated in this document. If it exists in TypeScript, it must exist in Rust.
- **NOT shipping stub code or TODO-later placeholders.** Every function compiles, runs, and passes tests before a phase is considered complete. No `unimplemented!()` macros in shipped code.
- **NOT making assumptions about what is safe to remove.** Chesterton's fence applies to every module, every function, every comment. If you do not understand why something exists, you must find out before touching it.

### Binary Distribution Strategy

Based on research of `/opt/swictation` (Robert's prior art with the scoped optional npm package pattern):

1. **Create platform packages:**
   - `@agidreams/ctm-linux-x64` -- contains `bin/ctm` compiled for Linux x86_64
   - `@agidreams/ctm-darwin-arm64` -- contains `bin/ctm` compiled for macOS ARM64

2. **Each platform package** specifies `"os"` and `"cpu"` fields in its `package.json`. npm automatically installs only the matching platform package.

3. **Main `claude-telegram-mirror` package** declares them as `optionalDependencies`. This means installation succeeds even if the platform package is unavailable (falling back to TypeScript).

4. **`resolve-binary.js` module** discovers the binary at runtime. It walks `node_modules` via `npm root -g` and directory traversal -- the same pattern used in swictation. It returns the absolute path to the `ctm` binary or `null` if no native binary is available.

5. **CI builds natively:** Linux on `ubuntu-latest`, macOS on `macos-latest`. No cross-compilation. Cross-compilation introduces subtle ABI issues; native builds are reliable.

6. **Publish order:** Platform packages first. Wait for npm registry propagation (verify with `npm view`). Then publish the meta-package that depends on them.

7. **Platforms:** Linux x86_64 and macOS ARM64, matching the existing `"os": ["linux", "darwin"]` in our `package.json`.

---

## Phase 1: Hook Binary + Injector + Config (1-2 weeks)

### What Moves to Rust

| TypeScript Module | Lines | Rust Replacement |
|-------------------|------:|------------------|
| `scripts/telegram-hook.sh` | 423 | `ctm hook` subcommand |
| `src/hooks/handler.ts` | 530 | `ctm hook` subcommand |
| `src/bridge/injector.ts` | 412 | `ctm::injector` module |
| `src/utils/config.ts` | 278 | `ctm::config` module |
| `src/hooks/types.ts` | 118 | `ctm::hooks::types` module |
| `src/bridge/types.ts` | 55 | `ctm::bridge::types` module |

**Total:** 1,816 lines of TypeScript + shell replaced.

### Why This First

- **Hottest code path.** Hooks fire on every tool use, every prompt submission, every notification. A coding session easily triggers hundreds of hook invocations.
- **Most security-critical.** The hook handler and injector are where shell injection and socket path traversal attacks surface.
- **Stateless.** Hook invocations are fire-and-forget (except PreToolUse approval). No long-running state, no connection management. This is the simplest module to extract.
- **Fastest startup benefit.** Each hook invocation currently pays ~100-200ms of V8 initialization. Rust eliminates this entirely.
- **Isolated.** The hook binary communicates with the daemon only via Unix domain socket (NDJSON messages). It does not import the daemon, bot, or any other module. Clean interface boundary.

### Feature Parity Requirements

Every feature listed below MUST work identically to the TypeScript implementation. This is not a wish list; it is a checklist.

#### Hook Event Types (all 6)

All six hook event types registered in `~/.claude/settings.json` must be handled:

1. **PreToolUse** -- Approval blocking. Reads stdin, parses JSON, checks `permission_mode`, checks safe command whitelist, sends `approval_request` via socket, waits up to 5 minutes for response. Returns JSON `{"decision": "approve"}` or `{"decision": "reject", "reason": "..."}`.
2. **PostToolUse** -- Fire-and-forget. Sends tool result to daemon for display.
3. **Notification** -- Fire-and-forget. Sends notification to daemon.
4. **Stop** -- Fire-and-forget. Extracts text from transcript, sends `turn_complete` to daemon. Tracks last-read position via `.last_line_${SESSION_ID}` state file.
5. **UserPromptSubmit** -- Fire-and-forget. Sends user prompt to daemon.
6. **PreCompact** -- Fire-and-forget. Sends `pre_compact` event to daemon.

#### PreToolUse Approval Logic

- `permission_mode === 'bypassPermissions'` check: if set, auto-approve everything.
- Safe command whitelist for Bash tool: `ls`, `pwd`, `cat`, `head`, `tail`, `echo`, `grep`, `find`, `which`. If the Bash tool's command starts with any of these, auto-approve without bothering the user.
- For all other tools and commands: send `approval_request` to daemon, wait for response with 5-minute timeout.
- Return well-formed JSON to Claude Code's hook system.

#### Stop Event Processing

- Read transcript file from `transcript_path` in the hook event.
- Track last-read line position in `.last_line_${SESSION_ID}` state file (in config directory).
- Extract only new lines since last read.
- Parse assistant text content from JSONL transcript entries.
- Send extracted text as `turn_complete` message to daemon.

#### SubagentStop Handling

- SubagentStop events must be recognized and handled (distinct from Stop events).

#### Input Injector

- `injectText`: Send text to tmux pane using `tmux send-keys -t <target> -l` with proper text escaping. The `-l` flag prevents tmux from interpreting escape sequences.
- `sendKey`: Send special keys (Enter, Escape, Tab, Ctrl-C, Ctrl-U) to tmux pane. Each key maps to its tmux representation.
- `sendSlashCommand`: Send slash commands with character whitelist validation and `-l` flag.
- `detectTmuxSession`: Read `$TMUX` environment variable, parse socket path and session info, verify with `tmux display-message -p '#S:#I.#P'`.
- `findClaudeCodeSession`: Auto-detect Claude Code's tmux session by searching all tmux sessions for matching patterns.
- Target validation before injection (BUG-001 fix): verify tmux target exists before attempting send-keys.
- Socket flag (`-S`) for correct tmux server targeting (BUG-004 fix).
- Actionable error messages on injection failure (BUG-001 fix).

#### Config Loader

- Priority order: environment variables > config file (`~/.config/claude-telegram-mirror/config.json`) > defaults.
- All 13 environment variables with exact same names and defaults:
  - `TELEGRAM_BOT_TOKEN` (required, no default)
  - `TELEGRAM_CHAT_ID` (required, no default)
  - `TELEGRAM_MIRROR` (default: `false`)
  - `TELEGRAM_MIRROR_VERBOSE` (default: `true`)
  - `TELEGRAM_MIRROR_APPROVALS` (default: `true`)
  - `TELEGRAM_BRIDGE_SOCKET` (default: `~/.config/claude-telegram-mirror/bridge.sock`)
  - `TELEGRAM_USE_THREADS` (default: `true`)
  - `TELEGRAM_CHUNK_SIZE` (default: `4000`)
  - `TELEGRAM_RATE_LIMIT` (default: `1`)
  - `TELEGRAM_SESSION_TIMEOUT` (default: `30`)
  - `TELEGRAM_STALE_SESSION_TIMEOUT_HOURS` (default: `72`)
  - `TELEGRAM_AUTO_DELETE_TOPICS` (default: `true`)
  - `TELEGRAM_TOPIC_DELETE_DELAY_MINUTES` (default: `1440`)
- Boolean parsing: `"true"` or `"1"` means true, everything else means false.
- Number parsing: `parseInt` equivalent with fallback to default on `NaN`.
- `forumEnabled` is always `false` from config (detected at runtime by the daemon).
- Config file parse failure logs a warning and falls back to defaults (does not crash).

#### Validation

- Socket path validation: no `..` components, must be absolute path, maximum 256 characters.
- 1MB stdin limit on hook reads (prevent memory exhaustion from malformed input).
- Session ID validation: maximum 128 characters, character set `[a-zA-Z0-9_-]`.

#### Graceful Behavior

- When the bridge socket is absent (daemon not running), hooks exit silently with success. They do not crash, do not print errors to stdout (which would confuse Claude Code), and do not block.
- BUG-006: Hooks are stateless. The daemon's SQLite database is the single source of truth for session state. Hooks do not maintain their own session tracking (except the transcript line counter for Stop events).

### What Stays TypeScript in Phase 1

Everything not listed above remains TypeScript:

- Bridge daemon (`src/bridge/daemon.ts`)
- Telegram bot (`src/bot/telegram.ts`)
- Bot commands (`src/bot/commands.ts`)
- CLI entry point (`src/cli.ts`)
- Session manager (`src/bridge/session.ts`)
- Socket server/client (`src/bridge/socket.ts`)
- Message formatting (`src/bot/formatting.ts`)
- Message chunker (`src/utils/chunker.ts`)
- Logger (`src/utils/logger.ts`)
- Service manager (`src/service/manager.ts`)
- Setup wizard (`src/service/setup.ts`)
- Doctor (`src/service/doctor.ts`)
- Hook installer (`src/hooks/installer.ts`)

### Binary Integration

- The Rust binary is invoked as `ctm hook` subcommand.
- Hook configuration in `~/.claude/settings.json` points to `ctm hook` instead of `telegram-hook.sh` + `node handler.ts`.
- The binary is discovered via `resolve-binary.js` at install time.
- If the Rust binary is not available (unsupported platform, build failure), the TypeScript hook handler remains functional as fallback.

### Bug Fixes Preserved

- **BUG-001** (partial): tmux target validation and actionable error messages in the injector.
- **BUG-004** (partial): Socket flag (`-S`) for tmux server targeting in the injector.
- **BUG-006**: Hooks are stateless; daemon SQLite is single source of truth.

### Exit Criteria

1. Every hook event type fires correctly (all 6 types).
2. PreToolUse approval workflow works end-to-end: Telegram button press -> approval response -> Claude Code continues.
3. Safe command whitelist auto-approves correctly.
4. `bypassPermissions` mode auto-approves everything.
5. Stop event transcript extraction produces identical output to TypeScript.
6. tmux injection works with `-l` flag, special keys, slash commands.
7. Config loading respects priority order with all 13 environment variables.
8. Socket path validation rejects traversal attempts.
9. No regressions in existing test suite.
10. Hook latency <5ms for fire-and-forget events (measured, not assumed).
11. `cargo clippy` clean, `cargo fmt` clean, zero warnings.

---

## Phase 2: Session + Socket + Formatting (2-3 weeks)

### What Moves to Rust

| TypeScript Module | Lines | Rust Replacement |
|-------------------|------:|------------------|
| `src/bridge/session.ts` | 552 | `ctm::bridge::session` module |
| `src/bridge/socket.ts` | 480 | `ctm::bridge::socket` module |
| `src/bot/formatting.ts` | 348 | `ctm::bot::formatting` module |
| `src/utils/chunker.ts` | 171 | `ctm::utils::chunker` module |
| `src/utils/logger.ts` | 41 | `ctm::utils::logger` module |
| Tool summarizer (new from ADR-001 Item 9) | ~100 | `ctm::utils::summarize` module |

**Total:** ~1,692 lines of TypeScript replaced.

### Feature Parity Requirements

#### Session Manager (SQLite)

Identical SQLite schema with all migrations. Every method must be present:

- `createSession(sessionId, chatId, hostname?, projectDir?)` -- insert session row
- `setSessionThread(sessionId, threadId)` -- link session to forum topic
- `getSessionThread(sessionId)` -- retrieve thread ID for session
- `clearThreadId(sessionId)` -- unlink thread from session
- `setTmuxInfo(sessionId, tmuxTarget?, tmuxSocket?)` -- persist tmux connection info
- `getTmuxInfo(sessionId)` -- retrieve tmux target and socket path
- `getSession(sessionId)` -- get session by internal ID
- `getSessionByChatId(chatId)` -- get active session for a chat
- `getSessionByThreadId(threadId)` -- get session for a forum topic thread
- `getActiveSessions()` -- list all sessions with status `'active'`
- `updateActivity(sessionId)` -- touch `last_activity` timestamp
- `endSession(sessionId, status)` -- mark session as `'ended'` or `'aborted'`
- `reactivateSession(sessionId)` -- BUG-009 fix: set status back to `'active'`, update activity
- `createApproval(sessionId, prompt, messageId?)` -- create pending approval with `message_id` column
- `getApproval(approvalId)` -- retrieve single approval by ID
- `getPendingApprovals(sessionId)` -- list pending approvals for session
- `resolveApproval(approvalId, status, resolvedBy?)` -- approve or reject
- `expireOldApprovals()` -- expire approvals past their deadline
- `getStaleSessionCandidates(timeoutHours)` -- BUG-003: find stale sessions for cleanup
- `getOrphanedThreadSessions()` -- find sessions with thread IDs but ended status
- `isTmuxTargetOwnedByOtherSession(tmuxTarget, excludeSessionId)` -- BUG-003: prevent tmux target conflicts
- `cleanupOldSessions(maxAgeDays)` -- purge old sessions
- `getStats()` -- return `{activeSessions, pendingApprovals}` counts
- `close()` -- close database connection

The `pending_approvals` table MUST include the `message_id` column (INTEGER, nullable) for editing approval messages after a decision is made.

#### Socket Server

- Unix domain socket with NDJSON framing (newline-delimited JSON).
- **PID locking with `flock(2)`** -- atomic, no TOCTOU race (BUG-008 fix). The TypeScript implementation uses check-read-check-write; Rust uses `flock(2)` via the `nix` crate.
- Stale socket cleanup: if socket file exists but no process holds the lock, remove it and proceed.
- Connection limit: configurable maximum concurrent connections.
- Line size limit: reject messages exceeding maximum line length.
- `umask(2)` for socket bind (ADR-001 Item 19): create socket with correct permissions atomically.

#### Socket Client

- `sendAndWait(message, timeout)`: send NDJSON message, wait for correlated response by matching on a correlation ID. Used by hook handler for approval workflow.
- Reconnection logic: automatic reconnect on connection loss.
- Clean disconnect.

#### Message Formatting

- `escapeMarkdownV2(text)` -- properly escape all MarkdownV2 special characters: `_`, `*`, `[`, `]`, `(`, `)`, `~`, `` ` ``, `>`, `#`, `+`, `-`, `=`, `|`, `{`, `}`, `.`, `!`. This MUST NOT be a no-op (the DreamLab-AI fork made it a no-op, breaking all formatted messages).
- `detectLanguage(content)` -- heuristic language detection for code block syntax highlighting.
- `wrapInCodeBlock(content, language?)` -- wrap text in Markdown code fences.
- `formatToolDetails(toolName, toolInput)` -- human-readable formatting for ALL tools:
  - **Edit** -- show file path, old/new strings
  - **Write** -- show file path, content preview
  - **Read** -- show file path
  - **Bash** -- show command
  - **Grep** -- show pattern, path, flags
  - **Glob** -- show pattern, path
  - **Task** -- show task description
  - **WebFetch** -- show URL
  - **WebSearch** -- show query
  - **TodoWrite** -- show todos
  - **Generic fallback** -- JSON dump for unknown tools

#### Message Chunker

- Code-block-aware chunking: never split inside a code block.
- Natural break points: prefer splitting at paragraph boundaries, then sentence boundaries, then word boundaries.
- Part headers on multi-chunk messages: `[Part 1/3]`, `[Part 2/3]`, `[Part 3/3]`.
- UTF-8-safe truncation (ADR-001 Item 23): never split in the middle of a multi-byte character.

#### Logger

- stderr-only output (already the case from ADR-001).
- Structured log fields.
- Log levels: error, warn, info, debug.

#### Tool Summarizer (new from ADR-001 Item 9)

- Summarize tool inputs for display in Telegram messages.
- Configurable detail level.

### Bug Fixes Preserved

- **BUG-008**: TOCTOU PID locking -- now solved with `flock(2)`.
- **BUG-009**: Session reactivation -- `reactivateSession()` method preserved.
- **BUG-003** (partial): `getStaleSessionCandidates()` and `isTmuxTargetOwnedByOtherSession()` methods preserved.

### New Capabilities from Deferred Items

- **ADR-001 Item 18**: `flock(2)` atomic PID locking via `nix` crate.
- **ADR-001 Item 19**: `umask(2)` trick for socket bind -- atomic file permissions.
- **ADR-001 Item 23**: UTF-8-safe truncation in chunker.

### Exit Criteria

1. SQLite schema is byte-identical to TypeScript version (same table names, column names, types, constraints).
2. All session methods produce identical results for identical inputs.
3. Socket server handles concurrent connections, enforces limits, cleans up stale sockets.
4. `flock(2)` locking prevents duplicate daemon instances.
5. NDJSON framing is wire-compatible with TypeScript (existing TypeScript daemon can talk to Rust socket client and vice versa during transition).
6. `escapeMarkdownV2` correctly escapes all special characters (test with adversarial input).
7. Chunker never produces invalid Markdown (unclosed code blocks, split multi-byte characters).
8. `cargo clippy` clean, `cargo fmt` clean, zero warnings.

---

## Phase 3: Daemon + Bot + Full CLI (3-4 weeks)

### What Moves to Rust

| TypeScript Module | Lines | Rust Replacement |
|-------------------|------:|------------------|
| `src/bridge/daemon.ts` | 1,315 | `ctm::bridge::daemon` module |
| `src/bot/telegram.ts` | 525 | `ctm::bot::telegram` module |
| `src/bot/commands.ts` | 352 | `ctm::bot::commands` module |
| `src/cli.ts` | 626 | `ctm` binary CLI |

**Total:** 2,818 lines of TypeScript replaced. This is the largest and most complex phase.

### Feature Parity Requirements

#### Bridge Daemon

The daemon is the central orchestrator. It is 1,315 lines of TypeScript with intricate state management, race condition prevention, and a dozen bug fixes baked in. Every feature listed below is mandatory.

**Message type handlers (all 12):**

1. `session_start` -- Create session in SQLite, create forum topic if threads enabled, send startup notification.
2. `session_end` -- End session, schedule topic deletion (with configurable delay), send shutdown notification.
3. `agent_response` -- Format and send agent text to Telegram thread.
4. `tool_start` -- Send tool start notification with tool name.
5. `tool_result` -- Send tool result with formatting.
6. `user_input` -- Display user input in thread (with echo prevention, see below).
7. `approval_request` -- Send approval keyboard to Telegram, create pending approval in SQLite.
8. `approval_response` -- Resolve approval, edit original message, inject response into tmux.
9. `command` -- Handle CLI commands received via socket.
10. `error` -- Display error in Telegram thread.
11. `turn_complete` -- Process turn completion, extract and send agent response text.
12. `pre_compact` -- Mark session as compacting (for UI indicator).

**Echo prevention (BUG-011):**
- `recentTelegramInputs` set with 10-second TTL.
- When user sends text via Telegram and it gets injected into tmux, the hook captures it as `user_input`. The daemon must recognize this as an echo and suppress it, not display it twice.
- Key format and TTL must match TypeScript exactly.

**Topic auto-deletion with configurable delay (BUG-012):**
- When a session ends, schedule topic deletion after `topicDeleteDelayMinutes` (default: 1440 minutes / 24 hours).
- Topic deletion cancellation on session resume (BUG-012): if a new session starts in the same topic, cancel the pending deletion.

**Tool input cache for Details button:**
- Cache tool inputs keyed by `tool_use_id` with 5-minute TTL.
- "Details" inline keyboard button on tool messages.
- Callback handler retrieves cached input and replies (not edits) with formatted details.

**Compacting session tracking:**
- `pre_compact` message sets a flag on the session.
- Next `turn_complete` after compaction clears the flag.
- Used for UI indicators ("compacting...").

**Session reactivation (BUG-009):**
- When a hook event arrives for a session that was marked as `ended` or `aborted`, reactivate it.
- This handles Claude Code sessions that outlive the daemon's session tracking.

**Stale session cleanup (BUG-003):**
- 5-minute interval timer.
- **Differentiated timeouts:**
  - Sessions **without tmux info**: clean up after 1 hour of inactivity.
  - Sessions **with tmux info**: clean up after 24+ hours of inactivity, AND only if the tmux pane is no longer alive (check with `tmux has-session`).
- Clean up orphaned thread sessions (sessions with thread IDs but ended status).

**tmux target auto-refresh (BUG-001):**
- On every hook event, update the session's stored tmux target from the hook event metadata.
- If the tmux target has changed (e.g., pane was moved), the daemon picks up the new target automatically.

**`cc <command>` prefix transformer:**
- Messages from Telegram starting with `cc ` are treated as Claude Code commands.
- The `cc ` prefix is stripped and the remainder is injected into tmux.

**Interrupt vs kill distinction (BUG-004):**
- Interrupt command sends Escape (pauses Claude, allows editing).
- Kill/abort command sends Ctrl-C (terminates the current operation).
- These are distinct operations with different tmux key sequences.

**Startup and shutdown notifications:**
- Daemon sends a message to the Telegram chat when it starts and when it stops.

**Topic name augmentation (ADR-001 Item 10):**
- `/rename` events update the forum topic name.

**Photo/document download (ADR-001 Item 11):**
- When a user sends a photo or document via Telegram, download it and make it available to the Claude Code session.

**On-the-fly session creation with race prevention (BUG-010):**
- When a hook event arrives for an unknown session, create the session on the fly.
- Promise-based lock (BUG-002 pattern) prevents duplicate topic creation when multiple hook events arrive simultaneously for the same new session.

**Topic creation race prevention (BUG-002):**
- Promise-based lock in TypeScript. In Rust, use `tokio::sync::Mutex` or `tokio::sync::oneshot` channels to achieve the same serialization.
- Every message handler that needs a topic must wait for topic creation to complete if it is in progress.

**Ignore General topic (BUG-005):**
- Messages in the General topic (no `threadId`) are ignored entirely.

#### Telegram Bot

**MessageQueue with exponential backoff:**
- Maximum 3 retries per message.
- Exponential backoff between retries.
- Configurable base rate limit.
- `governor` crate token-bucket rate limiting (ADR-001 Item 20). The bot MUST have BOTH token-bucket rate limiting AND retry/backoff. These are complementary: token-bucket prevents burst overload, retry/backoff handles transient failures.

**TOPIC_CLOSED error recovery:**
- When sending a message fails because the topic is closed, automatically reopen the topic and retry the send.

**Entity parse fallback:**
- If Telegram rejects MarkdownV2 formatting, fall back to plain text and retry.

**Forum topic management:**
- Create topic (with name).
- Close topic.
- Reopen topic.
- Delete topic.
- Unpin all messages in topic (auto-pinned first message).
- Edit topic name (rename).

**Security middleware:**
- Verify `chat_id` on ALL update types (messages AND callback queries).
- Reject updates from unauthorized chats silently.

**Session middleware:**
- Attach `sessionId` to updates based on thread ID.
- Track `muted` state.
- Track `lastActivity`.

**Bot token scrubbing (ADR-001 Item 2):**
- On any error that might contain the bot token in a URL, scrub the token before logging or displaying.

**File download:**
- Download files from Telegram (photos, documents) using `getFile` API and HTTPS download.

#### Bot Commands (ALL of them)

Every command must work identically:

- `/start` -- Welcome message with usage instructions.
- `/help` -- Detailed help text.
- `/status` -- Show daemon status, active sessions, system info.
- `/sessions` -- List all active sessions with details.
- `/attach <session>` -- Attach current thread to a session.
- `/detach` -- Detach current thread from its session.
- `/mute` -- Suppress messages in current thread.
- `/unmute` -- Resume messages in current thread.
- `/abort` -- Abort the attached session (with confirmation keyboard).
- `/ping` -- Respond with latency measurement (round-trip time).

**Inline keyboards:**
- Approval keyboard: Approve, Reject, Abort buttons.
- Tool details callback: `tooldetails:<tool_use_id>` with 5-minute expiry, responds with reply (not edit).
- Abort confirmation keyboard: Confirm / Cancel buttons.

#### CLI (ALL commands)

Every CLI command must work identically:

**Daemon management:**
- `ctm start` -- Start the daemon (daemonize by default).
- `ctm stop` -- Stop the daemon gracefully. `--force` flag for SIGKILL.
- `ctm restart` -- Stop then start.
- `ctm status` -- Show daemon status (running/stopped, PID, uptime, session count).

**Configuration:**
- `ctm config --show` -- Display current configuration.
- `ctm config --test` -- Validate configuration and test Telegram connectivity.

**Hook management:**
- `ctm install-hooks` -- Install hooks globally to `~/.claude/settings.json`.
- `ctm install-hooks -p` / `--project` -- Install hooks to project-local `.claude/settings.json`.
- `ctm uninstall-hooks` -- Remove hooks from settings.
- `ctm hooks` -- Show hook installation status.

**Setup and diagnostics:**
- `ctm setup` -- Interactive setup wizard.
- `ctm doctor` -- Run diagnostic checks. `--fix` flag for auto-repair.

**Service management:**
- `ctm service install` -- Install as system service (systemd or launchd).
- `ctm service uninstall` -- Remove system service.
- `ctm service start` -- Start the system service.
- `ctm service stop` -- Stop the system service.
- `ctm service restart` -- Restart the system service.
- `ctm service status` -- Show system service status.

### Bug Fixes That MUST Be Preserved (every single one)

| Bug | Description | TypeScript Location | Rust Equivalent |
|-----|-------------|--------------------|--------------------|
| BUG-001 | tmux target auto-refresh + actionable error on injection failure | `daemon.ts:188`, `injector.ts:105,141` | Update tmux info on every hook event; validate target before send-keys |
| BUG-002 | Topic creation race prevention | `daemon.ts:43` (Promise lock) | `tokio::sync::Mutex` or oneshot channel serialization |
| BUG-003 | Stale session cleanup with differentiated timeouts | `daemon.ts:455-630` | 1h without tmux info, 24h+ with tmux info + pane-alive check |
| BUG-004 | Escape vs Ctrl-C distinction | `daemon.ts:262,362,385` | Separate key sequences for interrupt (Escape) vs kill (Ctrl-C) |
| BUG-005 | Ignore General topic | `daemon.ts:302` | Skip messages with no `threadId` |
| BUG-006 | Hooks stateless, SQLite is truth | `handler.ts:476` | No session state in hook binary |
| BUG-008 | TOCTOU PID locking | `socket.ts` (check-read-write) | `flock(2)` via `nix` crate |
| BUG-009 | Session reactivation on hook after end | `daemon.ts:816,828` | Reactivate session when hook event arrives for ended session |
| BUG-010 | On-the-fly session creation with race prevention | `daemon.ts:811,850,859` | Mutex-guarded session creation |
| BUG-011 | Echo prevention for Telegram inputs | `daemon.ts:39,353,1159` | `recentTelegramInputs` set with 10s TTL |
| BUG-012 | Topic deletion cancellation on session resume | `installer.ts:342` | Cancel pending deletion timer on new session in same topic |

### New Capabilities from Deferred Items

- **ADR-001 Item 20**: `governor` token-bucket rate limiting WITH `MessageQueue` retry/backoff.
- **ADR-001 Item 24**: Typed error enum -- Rust's `thiserror` crate with exhaustive error variants.
- **ADR-001 Item 25**: `HookEvent` typed union with exhaustive `match` -- Rust enums are perfect for this.

### Exit Criteria

1. Full end-to-end workflow: hook fires -> daemon processes -> Telegram message appears -> user responds -> injected into tmux -> Claude Code continues.
2. All 12 message types handled correctly.
3. All bot commands functional.
4. All CLI commands functional.
5. All BUG-001 through BUG-012 fixes verified with specific test cases.
6. Rate limiting works under load (burst test).
7. Echo prevention works (no duplicate messages).
8. Topic lifecycle works (create, rename, close, reopen, delete with delay and cancellation).
9. `cargo clippy` clean, `cargo fmt` clean, zero warnings.

---

## Phase 4: Service Manager + Setup Wizard + Hook Installer + Doctor (1-2 weeks)

### What Moves to Rust

| TypeScript Module | Lines | Rust Replacement |
|-------------------|------:|------------------|
| `src/service/manager.ts` | 624 | `ctm::service::manager` module |
| `src/service/setup.ts` | 716 | `ctm::service::setup` module |
| `src/service/doctor.ts` | 490 | `ctm::service::doctor` module |
| `src/hooks/installer.ts` | 573 | `ctm::hooks::installer` module |

**Total:** 2,403 lines of TypeScript replaced.

### Feature Parity Requirements

#### Service Manager

**systemd (Linux):**
- Generate user-level systemd unit file (`~/.config/systemd/user/claude-telegram-mirror.service`).
- `EnvironmentFile` directive pointing to `~/.telegram-env`.
- `Restart=on-failure` with `RestartSec`.
- Correct `ExecStart` path to `ctm` binary.
- `WorkingDirectory` set appropriately.
- `systemctl --user daemon-reload` after install.

**launchd (macOS):**
- Generate plist file (`~/Library/LaunchAgents/com.agidreams.claude-telegram-mirror.plist`).
- `KeepAlive` for auto-restart.
- `ThrottleInterval` to prevent rapid restart loops.
- `PATH` construction that includes nvm and Homebrew paths.
- `StandardOutPath` and `StandardErrorPath` for logging.

**Commands (both platforms):**
- `install` -- Write unit file/plist, reload daemon manager.
- `uninstall` -- Stop service, remove unit file/plist, reload daemon manager.
- `start` -- Start the service via systemctl/launchctl.
- `stop` -- Stop the service.
- `restart` -- Stop then start.
- `status` -- Query and display service status.

**Environment file parsing (`parseEnvFile`):**
- Handle `export VAR=value` (strip `export` prefix).
- Handle quoted values (single and double quotes).
- Handle inline comments (strip `# comment` after value).
- Skip blank lines and comment-only lines.

#### Setup Wizard

The setup wizard MUST be interactive. It uses terminal prompts, not `println!` statements. Use the `dialoguer` or `inquire` crate for interactive terminal prompts.

**Steps (in order):**

1. **Bot token collection** -- Prompt for bot token, validate immediately with live `getMe` API call. Display bot username on success. Reject and re-prompt on failure.
2. **Privacy mode reminder** -- Inform user about BotFather privacy mode settings and their implications.
3. **Supergroup + forum topics setup** -- Auto-detect chat type via `getUpdates`. Guide user to send a message to the bot, capture `chat_id` from the update. Detect if chat is a supergroup with forum topics enabled.
4. **Bot permissions verification** -- Send a test message to the chat to verify the bot has send permissions. Delete the test message after verification.
5. **Configuration options** -- Prompt for optional settings (verbose mode, approval mode, thread mode, chunk size, etc.).
6. **Config file save** -- Write configuration to both `~/.config/claude-telegram-mirror/config.json` and `~/.telegram-env` (for systemd `EnvironmentFile`).
7. **Optional hook installation** -- Ask if user wants to install hooks now. If yes, run hook installation. Provide guidance for project-level hook installation.
8. **Optional service installation** -- Ask if user wants to install as a system service. If yes, run service installation.

#### Doctor

**Diagnostic checks (all of them):**

1. **Runtime check** -- Verify `ctm` binary version (or Node.js version if in TypeScript fallback mode).
2. **Config directory permissions** -- Check that `~/.config/claude-telegram-mirror/` exists with correct permissions (700). With `--fix`: create directory and set permissions.
3. **Environment variable validation** -- Check all required environment variables are set. Report which are missing.
4. **Hook installation check** -- Read `~/.claude/settings.json`, verify CTM hooks are installed and point to the correct binary.
5. **Socket status check** -- Check if socket file exists, if daemon is running (via PID lock).
6. **tmux availability** -- Check if `tmux` is installed and accessible. Detect running tmux sessions.
7. **Service status check** -- Check systemd/launchd service status (installed, running, failed).
8. **Telegram API connectivity** -- Live `getMe` API call to verify bot token works and Telegram API is reachable.
9. **Database accessibility** -- Open SQLite database, run a simple query, report session statistics.

Each check reports: PASS, WARN, or FAIL with a human-readable explanation and (when `--fix` is provided) automatic remediation.

#### Hook Installer

**Write to `~/.claude/settings.json`:**
- Programmatic JSON modification (read, modify, write).
- MUST preserve non-CTM hooks. If the user has other hooks in their settings, they must not be removed or modified.

**Global and per-project install:**
- `ctm install-hooks` -- writes to `~/.claude/settings.json`.
- `ctm install-hooks -p` / `--project` -- writes to `.claude/settings.json` in current directory.
- BUG-012 fix: project installs need PreToolUse too, because Claude Code's hook resolution checks project settings for PreToolUse specifically.

**Idempotency:**
- `compareHookConfig`: compare existing hook configuration with desired configuration.
- Report each hook type as: added, updated, or unchanged.
- Running install twice produces no changes on the second run.

**All 6 hook types installed:**
- PreToolUse, PostToolUse, Notification, Stop, UserPromptSubmit, PreCompact.
- Each hook type points to the `ctm hook` command with appropriate arguments.

**Uninstall:**
- Remove only CTM hooks from settings.json.
- Preserve non-CTM hooks.

**Status:**
- Display installed hook types, their commands, and whether they match the expected configuration.

### Exit Criteria

1. `ctm setup` walks a new user through complete setup interactively (not just printing instructions).
2. `ctm doctor` catches and (with `--fix`) repairs all common issues.
3. `ctm doctor` with no issues reports all-PASS.
4. `ctm install-hooks` and `ctm install-hooks -p` correctly modify `settings.json` without destroying other hooks.
5. `ctm uninstall-hooks` removes only CTM hooks.
6. `ctm service install` + `ctm service start` works on Linux (systemd).
7. `ctm service install` + `ctm service start` works on macOS (launchd).
8. `cargo clippy` clean, `cargo fmt` clean, zero warnings.

---

## Phase Exit Criteria (applies to ALL phases)

Each phase MUST meet ALL of these requirements before proceeding to the next:

1. **100% feature parity** with the TypeScript modules being replaced. Not 99%. Not "most features." Every feature, every edge case, every error path.
2. **All existing tests pass**, adapted to test the Rust implementation.
3. **New tests** covering Rust-specific behavior (ownership edge cases, concurrent access, flock behavior).
4. **No regressions** in end-to-end workflow: hook fires -> Telegram message appears -> user responds -> injected into tmux -> Claude Code continues.
5. **Performance equal or better** than TypeScript. Measured, not assumed.
6. **`cargo clippy` clean**, `cargo fmt` clean, **zero warnings**. Not "warnings are acceptable." Zero.
7. **Documentation updated** for any changed behavior or new configuration.

---

## Risk Mitigation

1. **TypeScript remains the fallback at every phase.** If Phase 2 fails, Phase 1 Rust hook binary + TypeScript daemon still works. If Phase 3 fails, Phase 1+2 Rust + TypeScript bot still works. The product is never broken.

2. **Each phase is independently valuable.** We can stop at any phase and have a working, improved product:
   - After Phase 1: faster, more secure hooks.
   - After Phase 2: atomic PID locking, proper socket permissions, UTF-8-safe chunking.
   - After Phase 3: single binary for core functionality.
   - After Phase 4: complete Rust binary, no Node.js dependency.

3. **Chesterton's fence.** Before rewriting any module, the developer MUST read and understand the ENTIRE TypeScript implementation, including all bug fix comments (BUG-001 through BUG-012), all edge case handling, all error recovery paths. If you cannot explain why a line of code exists, you cannot remove it.

4. **No module is rewritten until a comprehensive feature inventory is completed.** The inventories in this ADR are the starting point, not the complete list. The developer must verify each inventory against the actual code before writing Rust.

5. **Wire compatibility during transition.** During Phases 1-3, Rust and TypeScript components communicate via the same NDJSON socket protocol. The protocol must be identical on both sides.

---

## Consequences

### If migration succeeds (all 4 phases complete):

- Single static binary distribution -- users install `ctm` and it works. No Node.js, no npm, no `node_modules`.
- ~10MB RSS memory footprint (vs ~50MB+ with Node.js).
- Shell injection impossible by language design.
- Atomic file permissions (`umask`) and PID locking (`flock`).
- <1ms hook cold start latency (vs ~100-200ms with Node.js V8 initialization).
- No npm supply chain risk for the binary itself.
- Exhaustive pattern matching on all enums prevents unhandled cases at compile time.
- `governor` token-bucket rate limiting with proper algorithm.
- Typed error hierarchy via `thiserror` crate.

### If migration stops at Phase 1:

- Faster, more secure hooks (the most frequently executed code path).
- TypeScript daemon continues to work unchanged.
- All ADR-001 improvements in place.
- Binary distribution infrastructure established for future phases.

### If migration stops at Phase 2:

- All Phase 1 benefits plus atomic PID locking, proper socket permissions, UTF-8-safe message chunking.
- TypeScript bot and daemon continue to work with Rust session/socket layer.

### If migration is abandoned entirely:

- ADR-001 TypeScript fixes stand on their own.
- Nothing is lost. The TypeScript codebase is improved regardless.
- Time spent understanding the codebase deeply (Chesterton's fence) is never wasted.

---

## Timeline

| Phase | Scope | Duration | Depends On |
|-------|-------|----------|-----------|
| ADR-001 | TypeScript security + UX fixes | 1-2 weeks | Nothing |
| Phase 1 | Hook + Injector + Config | 1-2 weeks | ADR-001 complete |
| Phase 2 | Session + Socket + Formatting | 2-3 weeks | Phase 1 complete |
| Phase 3 | Daemon + Bot + CLI | 3-4 weeks | Phase 2 complete |
| Phase 4 | Services + Setup + Doctor + Installer | 1-2 weeks | Phase 3 complete |

**Total: 8-13 weeks** for complete migration, with a working product at every checkpoint.

---

## References

- ADR-001: TypeScript Security and UX Fixes (prerequisite)
- DreamLab-AI Rust fork analysis (2026-03-15) -- ideas only, code ignored
- `/opt/swictation` -- Robert's prior art for scoped npm binary distribution
- `package.json` -- existing platform targets: `"os": ["linux", "darwin"]`
- BUG-001 through BUG-012 -- bug fix comments throughout TypeScript codebase
