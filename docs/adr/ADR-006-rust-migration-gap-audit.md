# ADR-006: Rust Migration Gap Audit — Unported Functionality

## Status

**Accepted (Resolved)** (2026-03-16)

> All 37 gaps resolved across 8 epics. C2 and C3 verified as false positives
> (TS hooks also don't send session_start/session_end — daemon creates on-the-fly).
> C2's real gap (missing projectDir in metadata) was fixed.

## Context

ADR-002 declared the phased Rust migration complete at Phase 4. A comprehensive line-by-line audit of all 24 TypeScript source files against all 16 Rust source files revealed functional gaps, behavioral regressions, and missing features that were not ported. This ADR documents every finding for tracking and resolution.

## Audit Methodology

Three independent auditors compared file-by-file:

| Auditor | TypeScript Scope | Rust Scope |
|---------|-----------------|------------|
| Bot+Bridge | `src/bot/` (4 files), `src/bridge/` (6 files) | `bot.rs`, `formatting.rs`, `daemon.rs`, `injector.rs`, `session.rs`, `socket.rs`, `types.rs` |
| Hooks+Service | `src/hooks/` (4 files), `src/service/` (3 files) | `hook.rs`, `installer.rs`, `doctor.rs`, `service.rs`, `setup.rs` |
| Utils+CLI+Entry | `src/utils/` (4 files), `src/cli.ts`, `src/index.ts` | `config.rs`, `summarize.rs`, `main.rs`, `error.rs` |

---

## Critical Gaps (broken functionality)

### C1: Approval response never broadcast over socket

**TS** (`daemon.ts:185-210`): After resolving an approval callback, broadcasts `{ type: 'approval_response', sessionId, content: action, metadata: { approvalId } }` over the socket. This is how the hook client (blocked in `sendAndWait`) learns the approve/reject/abort decision.

**Rust** (`daemon.rs:2011-2057`): After updating the database, logs a debug trace and returns. No socket broadcast is sent. The hook client waiting for `approval_response` will always time out.

**Impact**: The entire approval flow is non-functional. Claude never receives the user's decision.

### C2: `session_start` message never sent from hook

**TS** (`HookHandler.handleSessionStart()`): Sends a `session_start` message to the bridge with project directory, hostname, tmux session, tmux pane, tmux target, and tmux socket path.

**Rust** (`hook.rs`): No `session_start` message type is ever emitted. The hook only emits `tool_start`, `tool_result`, `agent_response`, `session_rename`, `turn_complete`, `pre_compact`, `user_input`, and `error`.

**Impact**: Bridge never receives session start notification. Session metadata (project dir, tmux info) not communicated.

### C3: `session_end` replaced by `turn_complete` on Stop

**TS** (`handleStop()`): Sends a `session_end` message (with tmux metadata) at the end of every Stop event.

**Rust**: Sends `turn_complete` instead. These are different message types.

**Impact**: Any daemon/bot code keying on `session_end` to close sessions or clean up state will never trigger. Sessions never marked closed; potential resource leaks and stale session state.

### C4: Session ID validation rejects dot-containing IDs

**TS**: No session ID validation. All session IDs forwarded as-is.

**Rust** (`hook.rs`): `is_valid_session_id()` allows only `[a-zA-Z0-9_-]` (max 128 chars). Claude Code session IDs may contain `.` characters.

**Impact**: All hook events silently dropped for sessions with dots in their ID. Could break all hook processing.

### C5: launchd label mismatch — `com.claude.*` vs `com.agidreams.*`

**TS** (`manager.ts`): Uses `com.claude.claude-telegram-mirror` as the launchd bundle identifier.

**Rust** (`service.rs`): Uses `com.agidreams.claude-telegram-mirror`.

**Impact**: Different plist file paths, different service labels. `get_launchd_status()`, `start_service()`, `stop_service()` all use the `com.agidreams` label. Any TS-installed service registered as `com.claude.*` is invisible to Rust. Cross-version service management broken.

### C6: Token scrubbing regression — security issue

**TS** (`logger.ts`): Uses regex `bot\d+:[A-Za-z0-9_-]+\/` applied globally to every log message via winston format pipeline. Scrubs any matching token in any URL, regardless of configured value.

**Rust** (`bot.rs`): `scrub_bot_token(text, token)` does literal `text.replace(token, "[REDACTED]")` using the runtime-loaded token. Only called when code explicitly invokes it.

**Impact**: Any `tracing::warn!` or `tracing::error!` that interpolates a URL containing the bot token (e.g., from a failed reqwest call) leaks the raw token to stderr. Bot token exposed in logs.

---

## High-Severity Gaps (missing features, broken behavior)

### H1: Five bot commands not ported

**TS** (`commands.ts`): Registers `/abort`, `/attach`, `/detach`, `/mute`, `/unmute`.

- `/abort` — inline confirmation keyboard (`confirm_abort:<id>` / `cancel_abort`), then `bridge.abortSession()`
- `/attach <id>` — sets `session.attachedSessionId` and `session.muted = false` in grammy session
- `/detach` — clears `session.attachedSessionId`
- `/mute` / `/unmute` — toggles `session.muted`

**Rust** (`daemon.rs`): Only `/start`, `/help`, `/status`, `/sessions`, `/ping`, `/rename` implemented. The `BotSession` type (with `attachedSessionId`, `muted`, `lastActivity`) does not exist in Rust.

### H2: `tool_error` field silently dropped on PostToolUse

**TS** (`hook.ts`): Includes `event.tool_error` in metadata under `error` key; falls back to `event.tool_error || 'No output'` when `tool_output` is absent.

**Rust** (`hook.rs`): Only reads `e.tool_output` (defaulting to `""`). Never reads `tool_error`. Tool failure information invisible to Telegram.

### H3: `tool_input` missing from PostToolUse metadata

**TS**: Puts `input: event.tool_input` in metadata of every `tool_result` message.

**Rust**: Only puts `tool` (the name). Downstream consumers get incomplete tool result context.

### H4: `transcript_summary` field ignored on Stop

**TS** (`handleStop()`): Checks `event.transcript_summary` and sends it directly as `agent_response` before `session_end`.

**Rust**: Completely ignores `transcript_summary`. Falls back to expensive transcript JSONL file I/O that may yield nothing.

### H5: PreToolUse approval content is bare tool name

**TS**: Calls `formatToolDescription()` to produce rich Markdown with tool-specific sections (file path for Write/Edit, command block for Bash, JSON dump for others) truncated at 500/200 chars.

**Rust**: Sends `"Allow {tool_name} tool?"` as content. Much less informative approval prompts.

### H6: `cmd_stop` and `cmd_restart` skip service-layer check

**TS** (`cli.ts`): Checks `isServiceInstalled()` first and delegates to `stopService()` / `restartService()`.

**Rust** (`main.rs`): Goes straight to PID file. On systemd/launchd machines, kills the process but the service unit restarts it. User sees "Stopped" but daemon comes right back.

### H7: `cmd_status` missing service check and hook status

**TS**: Checks OS service manager first (`isServiceInstalled()` / `getServiceStatus()`), shows "Running (via system service)". Also calls `printHookStatus()`.

**Rust**: Checks only PID file. Never checks service layer. Never prints hook status. Reports "Not running" even when systemd/launchd is managing the daemon.

### H8: `check_for_session_rename` skips deduplication

**TS** (`daemon.ts:1249-1286`): Checks `sessionCustomTitles.get(sessionId)` to avoid renaming if title unchanged. Returns `null` for same title.

**Rust** (`daemon.rs:1194-1231`): Contains comment `// Note: We can't easily do async check here, so we skip dedup` and `let _ = custom_titles;`. Every message triggers redundant `editForumTopic` API calls and repeated "Topic renamed" messages.

### H9: Injector auto-detect not called at daemon startup

**TS** (`injector.ts:65-92`): `injector.init()` on daemon startup checks if tmux is available, runs `detectTmuxSession()` (checks `$TMUX`, falls back to `findClaudeCodeSession()`), sets method to `'tmux'` or `'none'`.

**Rust** (`daemon.rs:116`): Injector created with `tmux_target: None`. `detect_tmux_session()` and `find_claude_code_session()` exist but are never called at startup. Injection silently disabled after daemon restart until first hook message with metadata arrives.

### H10: `tool_input` metadata read via `.as_str()` on JSON object — always `None`

**Rust** (`daemon.rs:920-921`): `meta.and_then(|m| m.get("input")).and_then(|v| v.as_str())` — the `input` field is a JSON object, not a string. `.as_str()` always returns `None`. Tool input display permanently suppressed.

---

## Medium-Severity Gaps (UX regressions, missing feedback)

### M1: Answer callback — message never edited to show "Selected"

**TS** (`commands.ts:421-443`): After single-select answer, edits original message to append `"\n\n✅ Selected"`.

**Rust**: Answer registered and injected, but original Telegram message never edited. No visual confirmation.

### M2: Toggle callback — keyboard never re-rendered with checkmarks

**TS** (`commands.ts:464-481`): After multi-select toggle, `ctx.editMessageReplyMarkup` re-renders keyboard with updated checkmark labels.

**Rust**: Toggle state updated, but keyboard never re-rendered. User cannot see which options are selected.

### M3: Submit callback — message never edited to show "Submitted"

**TS** (`commands.ts:499-516`): After submit, edits message to append `"\n\n✅ Submitted"`.

**Rust**: Submission processed but message not edited.

### M4: Tool details sent as plain message, not reply-to-original

**TS** (`commands.ts:373-390`): Tool details sent as reply to original message using `reply_parameters: { message_id }`.

**Rust** (`daemon.rs:2060-2091`): Sent as plain `send_message` with no `reply_to`. Context link lost on mobile.

### M5: `/ping` measures local time instead of Telegram round-trip

**TS**: Sends message, records start time, edits message, measures round-trip latency.

**Rust**: Records start time before `send_message`, formats latency immediately. Always shows ~0ms.

### M6: `/sessions` doesn't show session age

**TS**: Each session shows `"Started: Xm ago"`.

**Rust**: Only shows session ID and project dir. `started_at` exists in struct but is not displayed.

### M7: Inline keyboard layout — one button per row vs two-per-row

**TS** (`telegram.ts:172-177`): Buttons laid out two per row.

**Rust** (`bot.rs:669-681`): Each button gets its own row. Approval prompts look different.

### M8: PreToolUse dual-handler not ported

**TS** (`installer.ts`): Installs TWO hooks for PreToolUse: (1) bash script (fire-and-forget tool info capture) and (2) Node.js handler with 310-second timeout for approval workflow.

**Rust**: Installs single `ctm hook` command for all hook types. No `timeout: 310` on hook entry. Claude Code's default timeout may interrupt approval flow.

### M9: `extractCtmHookCommands()` false-positive risk

**TS**: Matches `telegram-hook` OR `hooks/handler` as CTM command identifiers.

**Rust**: Also matches any command containing `"ctm"` substring. Commands like `scrutm-linter` or paths containing `ctm` in directory names would be incorrectly identified and replaced.

### M10: `WorkingDirectory` missing from service templates

**TS**: Both `generateLaunchdPlist()` and `generateSystemdService()` include `WorkingDirectory`.

**Rust**: Neither launchd plist nor systemd unit includes `WorkingDirectory`. Process cwd undefined at service startup.

### M11: `NODE_ENV=production` missing from launchd plist

**TS** (`generateLaunchdPlist()`): Always emits `<key>NODE_ENV</key><string>production</string>`.

**Rust**: Only emits `HOME` and `PATH` plus user env-file vars. `NODE_ENV` absent.

### M12: Doctor checks only 3 of 6 hook types

**TS/Rust**: Both check `PreToolUse`, `PostToolUse`, `Notification`. But 6 types are installed: also `Stop`, `UserPromptSubmit`, `PreCompact`. Doctor reports "All hooks installed" when 3 hooks are missing.

### M13: JSON config parse failure silently ignored

**TS** (`config.ts`): `logger.warn('Failed to parse config file: ${CONFIG_FILE}', { error })`.

**Rust**: `serde_json::from_str` failure uses `unwrap_or_default()` with no log message. Malformed config silently treated as empty.

### M14: `isSignificantOutput` threshold and whitespace handling

**TS**: `output.length >= 10` AND not all whitespace.

**Rust**: `output.len() > 10` (strictly greater than, not >=) with no whitespace check. Output of exactly 10 chars sent by TS but not Rust. 100-byte whitespace string forwarded by Rust but suppressed by TS.

---

## Low-Severity Gaps (minor, library API, cosmetic)

### L1: `estimateChunks()` and `needsChunking()` not ported

Exported from `chunker.ts` via `index.ts`. Simple utilities (`Math.ceil(text.length / maxLength)` and `text.length > maxLength`). No Rust equivalent.

### L2: `isMirrorEnabled()` fast-path env check not ported

**TS** (`config.ts`): Lightweight check of `TELEGRAM_MIRROR=true && TELEGRAM_BOT_TOKEN set && TELEGRAM_CHAT_ID set` from env vars only (no file I/O).

**Rust**: Only `load_config()` which always reads config file.

### L3: `validateConfig()` with chunk-size warning not ported

**TS**: Returns `{valid, errors, warnings}` with warning if `chunkSize < 1000 || chunkSize > 4096`.

**Rust**: `cmd_start` calls `load_config(true)` which only errors on missing token/chatId. No chunk-size warning.

### L4: `ChunkOptions` interface gone

`preserveCodeBlocks` and `addPartHeaders` hardcoded to `true` in Rust. No way to disable.

### L5: `start --foreground` CLI flag absent in Rust

**TS**: Accepts `--foreground` (no behavioral effect, but documented). Rust has no such flag. Scripts using `ctm start --foreground` will error.

### L6: `HookHandlerConfig` and `HookResult` types not represented

`HookResult.modified_input` (ability to modify tool input before execution) has no representation in Rust.

### L7: `configPath` field missing from Rust `Config` struct

**TS**: `TelegramMirrorConfig.configPath` points to resolved JSON file path.

**Rust**: Has `config_dir: PathBuf` but not the full path to `config.json`.

### L8: First-run error messages less helpful

**TS**: Includes `@BotFather` URL and `https://api.telegram.org/bot{token}/getUpdates` in config error messages.

**Rust**: Only `"TELEGRAM_BOT_TOKEN is required"` and `"Supergroup IDs start with -100."`.

### L9: `set_current_dir` used for project hook install

**Rust** (`setup.rs`): Calls `std::env::set_current_dir(&full_path)` to change process working directory before `install_hooks(true)`. Modifies global process state; not thread-safe.

**TS**: Passes `projectPath` as parameter without modifying process state.

### L10: Doctor doesn't suggest `ctm install-hooks -f`

**TS**: Suggests `ctm install-hooks -f` for partial installs.

**Rust**: Suggests `ctm install-hooks` only (no `-f` variant mentioned).

### L11: `handleAgentResponse()` standalone method missing

**TS** (`HookHandler.handleAgentResponse(text)`): Can be called with arbitrary agent text to forward as `agent_response`.

**Rust**: Agent responses only extracted from transcript JSONL. No public method to send arbitrary agent text.

### L12: `handleSessionStart()` and `handleSessionEnd()` standalone methods missing

**TS**: `handleSessionStart()` and `handleSessionEnd()` can be called independently of the Stop handler.

**Rust**: No equivalent methods.

### L13: Hook installer `checkHookStatus()` path awareness missing

**TS**: Returns structured result with `installed: boolean`, active hook types list, and `scriptPath` discovery. Accepts arbitrary project paths.

**Rust**: `print_hook_status()` only checks global settings path. No path discovery or arbitrary project-path support.

### L14: `compareHookConfig()` detail strings absent

**TS**: Produces `"added bash+node"`, `"cleaned up old config"`, `"2→1 handlers"`.

**Rust**: Returns only `Added/Updated/Unchanged`. No detail string.

### L15: `MessageQueueItem.createdAt` not tracked

**TS**: `createdAt: Date` as first-class field. Allows age-based queue management.

**Rust**: `QueuedMessage` has `retries: u32` but no `created_at`.

### L16: `forumEnabled` field missing from Rust `Config` struct

**TS**: `TelegramMirrorConfig.forumEnabled: boolean`. Detected at runtime, stored as typed field.

**Rust**: Forum detection lives implicitly in `daemon.rs`. Not a named field on config.

### L17: Setup test message missing `parse_mode: 'Markdown'`

**TS** (`setup.ts`): Sends setup test message with `parse_mode: 'Markdown'`.

**Rust** (`setup.rs`): Sends without `parse_mode`. Also no emoji prefix.

### L18: Setup completion project-hooks reminder box missing

**TS**: Renders final cyan-bordered box with "REMEMBER: Project-specific hooks" reminder.

**Rust**: Omits this final reminder block.

### L19: Socket send partial failure not distinguished

**TS**: Per-message `client.send()` returns boolean success.

**Rust**: First write error aborts entire batch. No partial failure reporting.

### L20: Connection-refused vs timeout not distinguished in hook

**TS**: Distinguishes "bridge not available" (graceful exit) from "approval timeout" (fallback to `ask`).

**Rust**: Lumps both under `Err(_)` and always returns "timed out" fallback.

---

## Recommended Fix Priority

### Tier 1 — Blocking (approval flow broken)
1. **C1** — Broadcast `approval_response` over socket after callback resolution
2. **C4** — Allow `.` in session ID validation (or remove validation entirely)

### Tier 2 — Security
3. **C6** — Implement global token scrubbing in tracing subscriber layer

### Tier 3 — Session lifecycle
4. **C2** — Emit `session_start` message from hook handler
5. **C3** — Emit `session_end` (not `turn_complete`) on Stop events

### Tier 4 — Service compatibility
6. **C5** — Change launchd label to `com.claude.*` or add migration logic
7. **M10** — Add `WorkingDirectory` to both service templates
8. **H6+H7** — Add service-layer check to stop/restart/status commands

### Tier 5 — Missing commands
9. **H1** — Port `/abort`, `/attach`, `/detach`, `/mute`, `/unmute` with `BotSession` state

### Tier 6 — Data completeness
10. **H2** — Forward `tool_error` in PostToolUse
11. **H3** — Include `tool_input` in PostToolUse metadata
12. **H4** — Check `transcript_summary` before falling back to JSONL
13. **H5** — Port `formatToolDescription()` for rich approval prompts
14. **H10** — Fix `.as_str()` on JSON object (use `.to_string()` or serialize)

### Tier 7 — UX polish
15. **H8** — Implement rename deduplication
16. **H9** — Call `detect_tmux_session()` at daemon startup
17. **M1-M3** — Edit messages on answer/toggle/submit callbacks
18. **M4** — Send tool details as reply-to-original
19. **M5** — Fix ping latency measurement (send, then edit)
20. **M6** — Show session age in `/sessions`
21. **M7** — Two-per-row button layout
22. **M8** — Set `timeout: 310` on PreToolUse hook entry

### Tier 8 — Correctness
23. **M12** — Check all 6 hook types in doctor
24. **M13** — Log warning on JSON config parse failure
25. **M14** — Fix `>= 10` threshold and add whitespace check

## Decision

Accept this ADR as the definitive gap list for tracking Rust migration parity. Each item should be resolved before the Rust binary is promoted as the default (replacing the TypeScript implementation). Items in Tiers 1-3 are blockers for any production use.

## Consequences

- The Rust binary should NOT be used in production until at least C1 (approval broadcast) and C4 (session ID validation) are fixed
- C6 (token scrubbing) is a security issue that should be addressed before any deployment
- The TypeScript implementation remains the reference for correct behavior
- Each fix should include a test case that validates parity with the TS behavior
