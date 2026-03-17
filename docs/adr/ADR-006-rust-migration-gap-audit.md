# ADR-006: Rust Migration Gap Audit — Unported Functionality

## Status

**Revision 2 — In Progress** (2026-03-16)

> **Revision 1** (2026-03-16): Original 37 gaps identified and resolved across 8 epics.
> C2 and C3 verified as false positives (TS hooks also don't send
> session_start/session_end — daemon creates on-the-fly). C2's real gap
> (missing projectDir in metadata) was fixed.
>
> **Revision 2** (2026-03-16): Post-fix re-audit by three independent agents
> comparing all 24 TypeScript source files against all 16 Rust source files.
> Found 33 net-new gaps not covered in the original audit. Many original gaps
> were confirmed fixed; new gaps fall into categories: UX regressions, missing
> library API surface, behavioral differences, and edge-case divergences.

## Context

ADR-002 declared the phased Rust migration complete at Phase 4. The original
audit (Revision 1) found 37 gaps which were resolved. This second comprehensive
line-by-line audit was conducted to verify completeness and discovered additional
gaps that were either introduced during fix work, masked by the original audit's
scope, or in areas not previously examined.

## Audit Methodology

Three independent auditors compared file-by-file:

| Auditor | TypeScript Scope | Rust Scope |
|---------|-----------------|------------|
| Bot+Formatting+Types | `src/bot/` (4 files) | `bot.rs`, `formatting.rs`, `daemon.rs`, `types.rs` |
| Bridge+Service+Daemon | `src/bridge/` (6 files), `src/service/` (3 files) | `daemon.rs`, `injector.rs`, `session.rs`, `socket.rs`, `doctor.rs`, `service.rs`, `setup.rs` |
| CLI+Hooks+Utils+Entry | `src/hooks/` (4 files), `src/utils/` (4 files), `src/cli.ts`, `src/index.ts`, `postinstall.cjs` | `main.rs`, `hook.rs`, `installer.rs`, `config.rs`, `summarize.rs`, `formatting.rs` |

---

## Original Gaps (Revision 1) — Resolution Status

All 37 original gaps (C1–C6, H1–H10, M1–M14, L1–L20) have been resolved.
Key resolutions:

- **C1** (approval broadcast): Fixed — socket broadcast added
- **C2** (session_start): Verified false positive — TS hooks don't send this either; projectDir metadata gap fixed
- **C3** (session_end vs turn_complete): Verified false positive — daemon handles both message types
- **C4** (session ID dots): Fixed — dots now allowed in `is_valid_session_id`
- **C5** (launchd label): Fixed — labels aligned
- **C6** (token scrubbing): Fixed — global regex scrubber via `ScrubWriter` in tracing layer
- **H1** (5 missing commands): Fixed — `/abort`, `/attach`, `/detach`, `/mute`, `/unmute` ported
- **H2–H5, H8–H10**: All fixed
- **M1–M14**: All fixed
- **L1–L20**: All fixed

---

## NEW: Critical Gaps (Revision 2)

### C2.1: `/status` command shows aggregate counts instead of per-user state

**TS** (`commands.ts:58-66`): `/status` reads `session.attachedSessionId` and
`session.muted` from grammY session — shows which session the _current user_
is attached to and their mute state.

**Rust** (`daemon.rs:1937-1950`): `/status` shows aggregate counts (`Active
sessions: N`, `Pending approvals: N`). No output about which session the caller
is attached to or whether they are muted.

**Impact**: Users lose their primary "what am I looking at?" command. This is a
functional regression from the original TS behavior.

### C2.2: Approval messages never edited after decision

**TS** (`commands.ts:322-344`): After resolving an approval callback, edits the
original approval message to append `"\n\nDecision: ✅ Approved"` (or
Rejected/Aborted) and removes inline keyboard buttons.

**Rust** (`daemon.rs:2401-2458`): After broadcasting socket response and
resolving DB record, does NOT edit the Telegram message. The `_cb` parameter is
named with leading underscore indicating intentionally unused. Buttons remain
active on the original message.

**Impact**: Users don't know if their button press registered. Stale buttons
invite double-clicks and duplicate approval/rejection attempts.

### C2.3: `edit_message` always forces Markdown parse mode

**TS** (`telegram.ts:573-585`): `editMessage` uses
`options?.parseMode || 'Markdown'` — callers can pass `parse_mode: undefined`
to avoid Markdown conflicts (e.g., `commands.ts:338` for approval text edits).

**Rust** (`bot.rs:549-561`): `edit_message` hard-codes
`"parse_mode": "Markdown"` with no override parameter.

**Impact**: Editing messages containing Markdown special characters will cause
Telegram API parse errors. This also blocks fixing C2.2 — approval text often
contains tool names with underscores that break Markdown.

### C2.4: Rate limit 25x higher than TS default, ignores config

**TS** (`telegram.ts:31-33`): Rate limit configurable via `config.rateLimit`
(messages per second), defaulting to 1 msg/sec. Delay is `1000 / rateLimit` ms.

**Rust** (`bot.rs:187-188`): Hard-coded `Quota::per_second(NonZeroU32::new(25))`,
ignoring `config.rate_limit` entirely.

**Impact**: At 25 msg/sec, a burst of tool results could hit Telegram's global
rate limit (30 msg/sec per bot), resulting in 429 errors and potential temporary
ban.

### C2.5: `tool_use_id` generated instead of using hook-provided ID

**TS** (`daemon.ts`): Assigns `tool_use_id` from `msg.metadata.toolUseId`
(passed by the hook) for end-to-end correlation between hook event and approval
response.

**Rust** (`daemon.rs:851-855`): Generates its own `tool_use_id` via
`timestamp + UUID`, regardless of whether the hook provided one.

**Impact**: The hook's `tool_use_id` and the daemon's generated ID never match.
Any external tool expecting hook-provided `tool_use_id` in the Details callback
data will fail to correlate.

### C2.6: Text messages without `message_thread_id` silently dropped

**TS** (`telegram.ts:445-455`): `onMessage` handler processes all text messages
that don't start with `/`, regardless of thread ID. Session routing is upstream.

**Rust** (`daemon.rs:1624-1630`): `handle_telegram_text` immediately returns if
`message_thread_id` is `None`. Text messages in the General topic (no thread)
are silently dropped.

**Impact**: Users sending text in the General topic of a forum-enabled group get
no response and no error. TS would route these to session handling.

---

## NEW: High-Severity Gaps (Revision 2)

### H2.1: Stop hook sends `turn_complete` instead of `session_end`

**TS** (`handler.ts:177`): `handleStop` sends message type `session_end`.

**Rust** (`hook.rs:319`): Sends `turn_complete` instead.

**Note**: Original C3 was marked as false positive because the daemon handles
both types. However, any **external** consumer listening on the socket for
`session_end` messages will never see them from the Rust hook. The daemon's
internal handling is correct, but the wire protocol changed.

### H2.2: `/abort` sends raw Ctrl-C instead of graceful bridge abort

**TS** (`commands.ts:209-220`): On confirm, calls `bridge.abortSession(sessionId)`
which handles graceful shutdown. Also has a "bridge not connected" fallback path
(`commands.ts:225-231`) showing `_(Bridge not connected - session may still be running)_`.

**Rust** (`daemon.rs:2326-2339`): Sends `Ctrl-C` directly into the tmux pane via
injector. No graceful bridge abort; no "bridge disconnected" user message.

**Impact**: More forceful abort behavior. May interrupt Claude mid-tool-use
without proper cleanup.

### H2.3: Binary name `claude-telegram-mirror` missing

**TS**: `package.json` registers both `claude-telegram-mirror` and `ctm` as bin
entries pointing to the same entry point.

**Rust** (`Cargo.toml`): Only defines `[[bin]]` named `ctm`.

**Impact**: Users with scripts, aliases, or muscle memory using
`claude-telegram-mirror <subcommand>` get "command not found" after migration.

### H2.4: No public library API — `src/index.ts` exports broken

**TS** (`src/index.ts`): Exports ~20 symbols: `TelegramBot`,
`registerCommands`, `registerApprovalHandlers`, `formatAgentResponse`,
`formatToolExecution`, `chunkMessage`, `needsChunking`, `estimateChunks`,
`BridgeDaemon`, `SessionManager`, `SocketServer`, `SocketClient`,
`DEFAULT_SOCKET_PATH`, `InputInjector`, `createInjector`, `HookHandler`,
`installHooks`, `uninstallHooks`, `checkHookStatus`, `loadConfig`,
`validateConfig`, and all type exports.

**Rust**: Binary-only crate, no `lib.rs`.

**Impact**: Any consumer doing `import { ... } from 'claude-telegram-mirror'`
breaks entirely. This is an API contract break for downstream library consumers.

### H2.5: `postinstall.cjs` has no Rust equivalent

**TS**: `package.json` `postinstall` hook runs `node postinstall.cjs` which
prints a formatted onboarding banner (Quick Setup, Commands list, Documentation
link, existing-config detection).

**Rust**: No post-install guidance for any install path (`cargo install`, npm
optional packages, or direct binary download).

**Impact**: New users installing the Rust binary cold get no onboarding text.

### H2.6: `validateConfig()` not ported

**TS** (`config.ts:278-308`): Exports `validateConfig(config)` returning
`{ valid: boolean, errors: string[], warnings: string[] }` including chunk size
range warning (`1000-4096`). Called by `cmd_start` before daemon launch.

**Rust**: No `validate_config` function. `cmd_start` calls `load_config(true)`
which only errors on missing token/chat_id. No chunk-size warnings, no
`"⚠️ Warnings:"` block.

### H2.7: `forumEnabled` config field missing

**TS** (`config.ts:11-38`): `TelegramMirrorConfig.forumEnabled: boolean` set to
`false` at load time, detected at runtime by the daemon.

**Rust** (`config.rs:8-25`): No `forum_enabled` field on `Config` struct. Forum
detection lives implicitly in daemon startup. Any code that needs to check forum
status outside the daemon has no typed field to read.

---

## NEW: Medium-Severity Gaps (Revision 2)

### M2.1: `/attach` confirmation missing instructional text

**TS** (`commands.ts:121-124`): Reply is:
```
✅ Attached to session `{sessionId}`
You will now receive updates from this session.
Reply with text to send input.
```

**Rust** (`daemon.rs:2127-2133`): Reply is only:
```
✅ Attached to session `{matched_id}`
```

### M2.2: `/detach` reply missing follow-up text

**TS** (`commands.ts:141-144`): Includes `"You will no longer receive updates."`.

**Rust** (`daemon.rs:2148-2155`): Only the detach emoji line, no follow-up.

### M2.3: `detect_language` bash pattern too permissive

**TS** (`formatting.ts:199`): Bash pattern `^\$ |^#.*bash|^#!/` — anchored to
`#` comment lines containing "bash" or shebang.

**Rust** (`formatting.rs:332-333`):
`t.starts_with("$ ") || t.starts_with("#!") || t.contains("bash")` — the
`contains("bash")` matches "bash" anywhere in any line, causing false positives
(e.g., a line containing the word "bash" in a sentence gets detected as shell).

### M2.4: Tool input displayed as compact JSON (not pretty-printed)

**TS** (`formatting.ts:50-85`): `JSON.stringify(input, null, 2)` produces
indented, readable JSON for tool execution input display.

**Rust** (`daemon.rs:987-992`): Uses `.to_string()` on the JSON value producing
compact single-line JSON. Tool inputs with nested objects become unreadable.

### M2.5: `ChunkOptions` not ported — headers and code-block detection always on

**TS** (`chunker.ts:8-12`): `chunkMessage(text, { maxLength?, preserveCodeBlocks?, addPartHeaders? })`.
Setting `preserveCodeBlocks: false` disables code-block awareness;
`addPartHeaders: false` suppresses "Part N/M" prefix.

**Rust** (`formatting.rs`): `chunk_message(text, max_length)` — no options
struct. Code-block awareness and part headers always enabled.

**Impact**: Callers that passed `{ addPartHeaders: false }` (e.g., for raw API
payloads) now always get unwanted headers.

### M2.6: Doctor hooks check: 6 types vs 3 — false warnings on old installs

**TS**: Doctor checks 3 hook types: `PreToolUse`, `PostToolUse`, `Notification`.

**Rust** (`doctor.rs:296-303`): Checks 6 hook types: adds `Stop`,
`UserPromptSubmit`, `PreCompact`.

**Impact**: Users who installed hooks with an older version see `3/6 hooks
installed` as a warning, even though their installation was "complete" under
the old criteria. Should either auto-fix or suppress warning for legacy installs.

### M2.7: Systemd `WorkingDirectory` changed from package dir to `%h`

**TS** (`service.ts:123`): `WorkingDirectory=${packageDir}` (npm package
install directory).

**Rust** (`service.rs:216`): `WorkingDirectory=%h` (systemd specifier = home
directory).

**Impact**: If any relative path logic in the daemon assumes cwd = package root,
the Rust service will behave differently.

### M2.8: Setup test message missing emoji and parse_mode

**TS** (`setup.ts:133`): Sends
`"🤖 Claude Telegram Mirror - Setup test successful!"` with
`parse_mode: 'Markdown'`.

**Rust** (`setup.rs:162`): Sends
`"Claude Telegram Mirror - Setup test successful!"` without parse_mode and
without the robot emoji.

### M2.9: `SendOptions.replyToMessageId` field absent from struct

**TS** (`types.ts:8`): `replyToMessageId?: number` on `SendOptions`.

**Rust** (`bot.rs:38`): `SendOptions` has only `parse_mode` and
`disable_notification`. Reply-to exists as separate `send_message_reply_to`
method, but any caller setting `replyToMessageId` in options silently drops it.

### M2.10: `docker compose` (two-word modern syntax) not handled in summarizer

**TS** (`summarize.ts:232`): Handles both `"docker-compose"` (hyphenated) and
`"docker compose"` (two words, modern Docker CLI).

**Rust** (`summarize.rs:291`): Only handles `"docker-compose"`. Modern
`"docker compose up"` falls through to generic `Running \`docker compose\``
instead of `"Starting containers"`.

### M2.11: Socket directory creation/chmod not enforced in `listen()`

**TS** (`socket.ts:131-143`): `listen()` explicitly creates socket dir with
`mkdirSync(..., { mode: 0o700 })` and `chmodSync(socketDir, 0o700)`.

**Rust** (`socket.rs`): Relies on `config::ensure_config_dir` having been called
earlier. If it wasn't, the socket binds without verifying `0o700` on parent dir.

### M2.12: `createSession` atomicity lost — thread_id and tmux set separately

**TS** (`session.ts:117-149`): `createSession` accepts 7 parameters including
`threadId`, `tmuxTarget`, `tmuxSocket` in a single atomic INSERT.

**Rust** (`session.rs:177`): `create_session` takes 4 parameters. `thread_id`
and `tmux_target`/`tmux_socket` must be set by separate calls. The daemon does
call both, but atomicity of the single-INSERT is lost — a crash between calls
leaves partial state.

### M2.13: JSON config parse failure silently ignored (re-confirmed)

**TS** (`config.ts`): Logs `logger.warn('Failed to parse config file')`.

**Rust**: `serde_json::from_str` failure uses `unwrap_or_default()` with no log.
Malformed config silently treated as empty.

**Note**: This was L-tier in Revision 1 but is now M-tier because users have
reported confusion when config changes seem ignored.

---

## NEW: Low-Severity Gaps (Revision 2)

### L2.1: `estimateChunks()` and `needsChunking()` utilities missing

Exported from `chunker.ts` via `index.ts`. No Rust equivalent. Simple utilities
but part of the public API contract.

### L2.2: `isMirrorEnabled()` fast-path check not ported

**TS** (`config.ts:241-244`): Lightweight env-var-only check. Rust only has
`load_config()` which reads config file.

### L2.3: `DEFAULT_MAX_LENGTH` constant not exported

**TS** (`chunker.ts:6`): `DEFAULT_MAX_LENGTH = 4000`. Rust uses inline parameter.

### L2.4: `TelegramBot.isRunning()` and `getSession()` methods missing

**TS** (`telegram.ts:553-564`): Public `isRunning(): boolean` and
`getSession(chatId): SessionData | null`.

**Rust**: No equivalents on `TelegramBot` struct.

### L2.5: `createApprovalKeyboard` not extracted as reusable function

**TS** (`commands.ts:291-297`): Exported standalone function.

**Rust**: Approval buttons built inline in `handle_approval_request`. Button
text also differs: `"🛑 Abort"` vs TS's `"🛑 Abort Session"`.

### L2.6: `SocketClientInfo` type not represented

**TS** (`types.ts:51-55`): `{ id, connectedAt, sessionId? }`. Not referenced
in daemon logic — types-only gap.

### L2.7: `DEFAULT_SOCKET_PATH`, `SOCKET_DIR` constants not exported

**TS** (`socket.ts:500`): Explicitly exported. Rust computes from
`config.get_config_dir()` at runtime.

### L2.8: `checkSocketStatus()` standalone utility missing

**TS** (`socket.ts:23-63`): Async function with connectivity test and 1-second
timeout. Rust uses flock-based locking (better mechanism, but no equivalent
exported utility).

### L2.9: `MessageQueueItem.createdAt` not tracked

**TS** (`types.ts:31`): `createdAt: Date`. Allows age-based queue management.

**Rust**: `QueuedMessage` has `retries` but no `created_at`.

### L2.10: `scrub_bot_token` regex-based vs token-literal

**TS**: Scrubs the literal runtime bot token by string replacement.

**Rust** (`bot.rs:810-814`): Uses regex `bot\d+:[A-Za-z0-9_-]+/` that scrubs
any Telegram-looking token. Documented as intentional. Practically equivalent
for URL leaks but a bare token string (not in URL form) would be scrubbed by
TS but not Rust.

### L2.11: `short_path` filters empty path components

**TS** (`formatting.ts:232-236`): No filtering of empty strings from split.

**Rust** (`formatting.rs:369-374`): Filters empty strings
(`filter(|s| !s.is_empty())`). Minor edge case on paths like `//foo/bar`.

### L2.12: Installer `details` string missing from `HookChangeReport`

**TS**: Produces `"added bash+node"`, `"cleaned up old config"`, `"2→3 handlers"`.

**Rust**: `HookChangeReport` has no `details` field.

### L2.13: Installer `--project` flag can't accept custom path

**TS** (`installer.ts:286`): `installHooks({ projectPath?: string })` accepts
any directory. Rust `install_hooks(project: bool)` only uses `current_dir()`.
`install_hooks_for_project(path)` exists but only called from setup, not CLI.

### L2.14: `handleAgentResponse()` standalone method missing

**TS** (`HookHandler.handleAgentResponse(text)`): Public method for sending
arbitrary agent text. Rust only extracts from transcript JSONL.

### L2.15: `handleSessionStart()` / `handleSessionEnd()` standalone methods missing

**TS**: Independent of Stop handler. Rust has no equivalents.

### L2.16: `checkHookStatus()` programmatic API missing

**TS**: Returns `{ installed, hooks, scriptPath, error }`. Rust's
`print_hook_status()` combines check+print with no return value.

### L2.17: Hook install output: flat list vs grouped with emoji

**TS**: Three labeled groups (`✅ Added`, `🔄 Updated`, `✓ Already correct`)
plus `"💡 Restart Claude Code to activate changes."`.

**Rust**: Flat list `[+] Added` / `[~] Updated` / `[ ] Unchanged`. No restart
reminder.

### L2.18: First-run error messages less helpful

**TS**: Includes `@BotFather` URL and API debug URL in error messages.

**Rust**: Terse messages only.

### L2.19: Status CLI output has no emoji icons

**TS**: `✅ Set` / `❌ Not set` for config fields. Rust: plain text.

### L2.20: `config --show` flag accepted but ignored

Rust destructures the flag with `show: _` and discards it. The default behavior
(print config) makes it a no-op in both versions, but Rust explicitly ignores a
documented flag.

---

## Intentional Differences (not gaps)

These behavioral changes are documented, tested, and represent improvements:

| Item | TS Behavior | Rust Behavior | Rationale |
|------|-------------|---------------|-----------|
| Session ID allows dots | Rejected dots | Allows `.` | Supports Claude's native session IDs |
| PID locking | Read-compare-write | `flock(LOCK_EX\|LOCK_NB)` | No TOCTOU race |
| Single hook handler | Dual bash+node for PreToolUse | Single `ctm hook` | Architectural simplification |
| `PreCompact` hook type | Not present | Added | New capability |
| Doctor Node.js check | Validates Node >= 18 | Validates binary version | Correct for compiled binary |
| Key map | 5 keys | 7 keys (adds `Ctrl-D`, `Ctrl-L`) | Superset |
| `/attach` partial match | Exact match only | Partial session ID matching | UX improvement |
| Token scrubbing scope | Per-field in Winston formatter | Entire log line via `ScrubWriter` | Strictly stronger |
| Doctor check numbering | No numbering | `[1/10]`, `[2/10]`, etc. | Better UX |
| `/sessions` age display | Minutes only | Hours for sessions >= 60 min | UX improvement |

---

## Recommended Fix Priority

### Tier 1 — UX Regressions (users will notice immediately)

| # | Gap | Effort | Description |
|---|-----|--------|-------------|
| 1 | C2.2 | Medium | Edit approval messages after decision, remove buttons |
| 2 | C2.3 | Small | Add optional `parse_mode` parameter to `edit_message` |
| 3 | C2.1 | Medium | Restore per-user `/status` showing attached session + mute state |
| 4 | C2.6 | Small | Handle text messages without `message_thread_id` |

### Tier 2 — Correctness

| # | Gap | Effort | Description |
|---|-----|--------|-------------|
| 5 | C2.4 | Small | Read `config.rate_limit`, default to sane value (not 25/sec) |
| 6 | C2.5 | Small | Use hook-provided `tool_use_id` when present |
| 7 | M2.3 | Small | Fix `detect_language` bash pattern to `^#.*bash` |
| 8 | M2.13 | Small | Log warning on JSON config parse failure |
| 9 | M2.10 | Small | Handle `"docker compose"` two-word syntax |

### Tier 3 — Information Completeness

| # | Gap | Effort | Description |
|---|-----|--------|-------------|
| 10 | M2.1 | Tiny | Add instructional text to `/attach` reply |
| 11 | M2.2 | Tiny | Add follow-up text to `/detach` reply |
| 12 | M2.4 | Small | Pretty-print tool input JSON |
| 13 | M2.8 | Tiny | Add emoji and parse_mode to setup test message |
| 14 | M2.6 | Small | Doctor: distinguish legacy 3-hook installs from incomplete |

### Tier 4 — API Surface & Distribution

| # | Gap | Effort | Description |
|---|-----|--------|-------------|
| 15 | H2.3 | Small | Add `claude-telegram-mirror` symlink or alias |
| 16 | H2.4 | Large | Add `lib.rs` with public API (or document as intentional break) |
| 17 | H2.5 | Small | Add post-install message to npm package scripts |
| 18 | H2.6 | Medium | Port `validateConfig()` with warnings |

### Tier 5 — Edge Cases & Polish

| # | Gap | Effort | Description |
|---|-----|--------|-------------|
| 19 | M2.5 | Small | Add `ChunkOptions` struct |
| 20 | M2.7 | Tiny | Use appropriate `WorkingDirectory` in systemd unit |
| 21 | M2.9 | Small | Add `reply_to_message_id` to `SendOptions` |
| 22 | M2.11 | Small | Enforce socket dir permissions in `listen()` |
| 23 | M2.12 | Medium | Make `createSession` atomic (single INSERT with all fields) |
| 24 | H2.2 | Medium | Evaluate graceful abort via bridge vs raw Ctrl-C |
| 25 | H2.7 | Small | Add `forum_enabled` to `Config` struct |

### Tier 6 — Low Priority

All L2.x items. Address opportunistically during related work.

---

## Decision

Accept Revision 2 as the updated gap list. The original 37 gaps are confirmed
resolved. The 33 new gaps represent a second wave of findings at a finer
granularity — mostly UX polish, edge cases, and API surface rather than
blocking functional breaks.

**Key change from Revision 1**: No gaps in Revision 2 are hard blockers for
production use (the approval flow works, sessions track correctly, hooks fire).
The critical items (C2.1–C2.6) are UX regressions that degrade the experience
but don't break core functionality.

## Consequences

- Tier 1 and Tier 2 items (9 total) should be addressed before promoting the
  Rust binary as the recommended default
- Tier 3 and Tier 4 items should be addressed before deprecating the TS version
- The public library API question (H2.4) needs a decision: port to `lib.rs` or
  document as an intentional API break with a migration guide
- Each fix should include a regression test validating parity with TS behavior
- A third audit should be conducted after Tier 1–2 fixes are applied
