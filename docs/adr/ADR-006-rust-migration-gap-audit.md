# ADR-006: Rust Migration Gap Audit — Unported Functionality

## Status

**Revision 3 — Resolved** (2026-03-16)

> All gaps across all three revisions are now closed. Zero deferred.
> Rev 1: 37 gaps → all resolved. Rev 2: 33 gaps (5 false positives) → 28 resolved.
> Rev 3: 15 new + 2 re-evaluated → all resolved. Total: 82 gaps identified, 72 genuine, all fixed.

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
>
> **Revision 3** (2026-03-16): Five-agent CFA swarm audit with per-domain
> researchers (CLI+Bot, Bridge, Hooks+Service, Utils+Config, Types+Entry).
> Corrected 5 false positives from Rev 2, discovered 15 net-new gaps, and
> re-evaluated 2 Rev 1 "false positives" as genuine gaps. Also incorporates
> user decision: `/abort` must be immediate (no confirmation dialog).

## Context

ADR-002 declared the phased Rust migration complete at Phase 4. Revision 1
found 37 gaps (all resolved). Revision 2 found 33 more. This Revision 3 audit
used a five-agent parallel research swarm comparing every function, type,
constant, and behavioral path across the full TS and Rust codebases. It
corrects several Rev 2 false positives and surfaces additional gaps in hook
lifecycle messaging, env var compatibility, and type-level completeness.

## Audit Methodology

### Revision 2 (original)

Three independent auditors compared file-by-file:

| Auditor | TypeScript Scope | Rust Scope |
|---------|-----------------|------------|
| Bot+Formatting+Types | `src/bot/` (4 files) | `bot.rs`, `formatting.rs`, `daemon.rs`, `types.rs` |
| Bridge+Service+Daemon | `src/bridge/` (6 files), `src/service/` (3 files) | `daemon.rs`, `injector.rs`, `session.rs`, `socket.rs`, `doctor.rs`, `service.rs`, `setup.rs` |
| CLI+Hooks+Utils+Entry | `src/hooks/` (4 files), `src/utils/` (4 files), `src/cli.ts`, `src/index.ts`, `postinstall.cjs` | `main.rs`, `hook.rs`, `installer.rs`, `config.rs`, `summarize.rs`, `formatting.rs` |

### Revision 3 (CFA swarm)

Five parallel researcher agents, each exhaustively comparing every exported
function, type, constant, and behavioral path in their domain:

| Agent | TypeScript Scope | Rust Scope |
|-------|-----------------|------------|
| CLI & Bot | `src/cli.ts`, `src/bot/*` (4 files) | `main.rs`, `bot.rs`, `formatting.rs` |
| Bridge | `src/bridge/*` (6 files) | `daemon.rs`, `session.rs`, `socket.rs`, `injector.rs` |
| Hooks & Service | `src/hooks/*` (4 files), `src/service/*` (3 files) | `hook.rs`, `installer.rs`, `service.rs`, `doctor.rs`, `setup.rs` |
| Utils & Config | `src/utils/*` (4 files) | `config.rs`, `summarize.rs`, `formatting.rs`, `Cargo.toml` |
| Types & Entry | `src/index.ts`, all `types.ts` files, `postinstall.cjs`, `resolve-binary.js` | `types.rs`, `error.rs`, `Cargo.toml`, `package.json` |

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

---

## Revision 3: Corrections to Rev 2 (False Positives)

The following Rev 2 gaps were determined to be **false positives** — the
functionality IS ported in Rust:

### H2.6 — CORRECTED: `validateConfig()` IS ported

`validate_config(config: &Config) -> (Vec<String>, Vec<String>)` exists at
`config.rs:329`. Returns `(errors, warnings)` including chunk-size range
warning. Return type differs from TS `{valid, errors, warnings}` (no boolean
shorthand — caller checks `errors.is_empty()`), but the function exists and
is called from `cmd_start`. **Reclassified: Not a gap.** The missing `valid`
boolean is a trivial API surface difference.

### H2.7 — CORRECTED: `forumEnabled` IS present

`forum_enabled: bool` exists at `config.rs:75`, defaulting to `false` at
`config.rs:321`. **Reclassified: Not a gap.**

### L2.1 — CORRECTED: `estimateChunks()` and `needsChunking()` ARE ported

Both exist in `formatting.rs:374-385` with identical logic and have tests
at `formatting.rs:751-773`. **Reclassified: Not a gap.**

### M2.5 — CORRECTED: `ChunkOptions` IS ported

`ChunkOptions` struct exists at `formatting.rs:402-410` with all three fields
(`max_length`, `preserve_code_blocks`, `add_part_headers`).
`chunk_message_with_options(text, opts)` at `formatting.rs:427` accepts it.
Tests at `formatting.rs:657-677` exercise both flags. **Reclassified: Not a
gap.**

### Agent #4 Safe Command Set — FALSE ALARM

`SAFE_COMMANDS` in `types.rs:146-148` contains the exact same 9 commands as TS:
`ls`, `pwd`, `cat`, `head`, `tail`, `echo`, `grep`, `find`, `which`.
**No divergence.**

---

## Revision 3: Re-Evaluated Rev 1 "False Positives"

### C2 RE-EVAL: Hook `session_start` message — GENUINE GAP

Rev 1 classified C2 as a false positive, stating "TS hooks also don't send
`session_start`." The Rev 3 audit found this is **incorrect**: TS
`handler.ts` has an explicit `handleSessionStart()` method that sends a
`session_start` bridge message with `hostname`, tmux info, and `projectDir`.
The Rust hook (`hook.rs`) never emits a `session_start` message type.

The Rev 1 statement was likely conflating hook behavior with daemon behavior
(the daemon creates sessions on-the-fly regardless). However, the hook's
`session_start` message carried **metadata** (hostname, projectDir) that the
daemon used for topic naming and context. The `projectDir` metadata fix in
Rev 1 was necessary but insufficient — the message type itself is still absent.

**Re-classified as C3.1 below.**

### C3 RE-EVAL: Hook `session_end` message — CONFIRMED genuine gap

Rev 1 classified C3 as a false positive because "the daemon handles both
`session_end` and `turn_complete`." This is true for the daemon's internal
routing. However, **external socket consumers** listening for `session_end`
will never see it from the Rust hook. This was already captured as H2.1 in
Rev 2 but is confirmed again here. The wire protocol changed.

---

## Revision 3: NEW Critical Gaps

### C3.1: Hook never sends `session_start` message

**TS** (`handler.ts`): `handleSessionStart()` sends an explicit `session_start`
bridge message on the first hook event for a new session. Includes `hostname`,
tmux window/pane info, and `projectDir` in metadata.

**Rust** (`hook.rs`): No `session_start` message is ever emitted. The daemon
must infer session starts from other event types (e.g., first `agent_response`
or `tool_start` for an unknown session ID).

**Impact**: If the daemon or any external consumer relies on `session_start` to
initialize session state (create a topic thread, set up routing, record
hostname/projectDir), this data arrives late or not at all.

### C3.2: `/attach` doesn't reset `muted` to false

**TS** (`commands.ts:142`): `ctx.session.muted = false` — explicitly resets
mute state when attaching to a new session.

**Rust** (`daemon.rs`): `/attach` handler sets `attached_session_id` but does
NOT write `state.muted = false`. A previously muted session remains muted
after re-attach.

**Impact**: Users who mute one session and attach to another continue to see
no output. They must manually `/unmute` after every `/attach`.

### C3.3: `start` command doesn't exit on config validation errors

**TS** (`cli.ts:53-59`): Calls `validateConfig()`, checks `validation.valid`,
and calls `process.exit(1)` if false — preventing daemon launch with bad config.

**Rust** (`main.rs`): `cmd_start` calls `load_config(true)` which only checks
for missing token/chatId. Even though `validate_config()` exists (see H2.6
correction above), it is **not called from `cmd_start`**. The daemon will
launch with out-of-range `chunk_size`, zero `rate_limit`, or other invalid
settings.

**Impact**: Invalid configuration causes runtime failures instead of
clear startup errors.

### C3.4: `/abort` must be immediate (decision: revert to TS behavior)

**TS** (`commands.ts:209-220`): `/abort` immediately calls
`bridge.abortSession(sessionId)` for graceful shutdown.

**Rust** (`daemon.rs:2326-2339`): Shows a confirmation dialog with inline
buttons before acting, then sends `Ctrl-C` via tmux.

**Decision**: Per user directive, `/abort` should be immediate like TS — no
confirmation dialog. Additionally, the graceful bridge abort should be
preferred over raw `Ctrl-C` which may interrupt Claude mid-tool-use without
cleanup.

---

## Revision 3: NEW High-Severity Gaps

### H3.1: `LOG_LEVEL` env var renamed to `RUST_LOG` without backward compat

**TS** (`logger.ts:36`): Reads `LOG_LEVEL` env var to set log verbosity.

**Rust** (`main.rs`): Uses `tracing_subscriber` which reads `RUST_LOG` env var.

**Impact**: Any deployment scripts, systemd unit files, CI configs, or
documentation that set `LOG_LEVEL=debug` will silently have no effect on the
Rust binary. Operators must know to use `RUST_LOG=debug`.

**Fix**: Read `LOG_LEVEL` as a fallback if `RUST_LOG` is not set, or document
the change prominently.

### H3.2: `session_start` not broadcast back to socket clients after creation

**TS** (`daemon.ts:1041-1047`): After creating a session and topic,
broadcasts `{ type: 'session_start', sessionId, metadata: { threadId } }` back
to all socket clients.

**Rust** (`daemon.rs`): `handle_session_start()` creates the session and topic
but does NOT broadcast back to socket clients.

**Impact**: Hook clients never learn the `threadId` assigned to their session.
Any hook that needs to reference the thread (e.g., for deep-linking or
targeted messages) has no mechanism to discover it.

---

## Revision 3: NEW Medium-Severity Gaps

### M3.1: `lastActivity` not updated for photo/document messages

**TS** (`telegram.ts`): grammY middleware updates `lastActivity` on **every**
incoming update, including photos, documents, and non-command text.

**Rust** (`daemon.rs`): `last_activity` is only updated inside command handlers
and `handle_telegram_text`. Photo and document handlers
(`handle_telegram_photo`, `handle_telegram_document`) do not call
`update_activity()`.

**Impact**: Sessions where users send only media (photos/documents) will appear
stale and may be prematurely cleaned up by the stale-session reaper.

### M3.2: `HookEventBase` missing `hook_id` and `timestamp` fields

**TS** (`hooks/types.ts`): Base event includes `hook_id?: string` and
`timestamp?: string`.

**Rust** (`types.rs`): `HookEventBase` struct omits both fields.

**Impact**: Any downstream consumer that logs or correlates events by `hook_id`
or timestamps will receive `null`/missing values from Rust hooks.

### M3.3: `linux-arm64` not in binary platform map

**TS** (`scripts/resolve-binary.js`): Platform map includes `linux-x64`,
`darwin-arm64`, `darwin-x64`.

**Missing**: `linux-arm64`. AWS Graviton, Raspberry Pi, and Apple M-chip Linux
VMs running arm64 fall through to the TS fallback silently.

**Fix**: Add `linux-arm64` to the platform map and publish
`@agidreams/ctm-linux-arm64`.

### M3.4: Setup wizard `useThreads` defaults to `true` regardless of existing config

**TS** (`setup.ts`): `useThreads` prompt defaults to the value from the
existing config file (respects prior choices).

**Rust** (`setup.rs`): Always defaults to `true` for the `useThreads` prompt,
ignoring any existing configuration.

### M3.5: Approval `formatAndChunk` convenience function not exported

**TS** (`formatting.ts`): Exports `formatAndChunk(content, maxLength?)` — a
single-call function that strips ANSI then chunks.

**Rust**: No equivalent. Callers must compose `strip_ansi()` +
`chunk_message()` manually. Not user-visible but reduces API ergonomics.

---

## Revision 3: NEW Low-Severity Gaps

### L3.1: Named types missing as Rust structs

The following TS types exist as named interfaces/types but have no named Rust
struct equivalent. Their functionality is typically inlined at call sites:

- `SendOptions` (`bot/types.ts`) — Rust `SendOptions` exists but is missing
  `replyToMessageId`, `disableNotification`, `threadId` fields (only has
  `parse_mode`)
- `InlineButton` (`bot/types.ts`) — inline keyboard buttons built ad-hoc
- `BotSession` (`bot/types.ts`) — `BotSessionState` in daemon covers
  `attached_session_id` and `muted` but not `lastActivity`
- `MessageQueueItem` (`bot/types.ts`) — `QueuedMessage` in bot.rs exists but
  missing `createdAt` (see L2.9)
- `HookHandlerConfig` (`hooks/types.ts`) — config passed via `Config` struct
- `HookResult` (`hooks/types.ts`) — decision/reason/modified_input not as
  named struct; `block` decision value not recognized (only `approve`,
  `reject`, `abort`, unknown→`ask`)

### L3.2: Daemon `isRunning()` and `getStatus()` public methods missing

**TS** (`daemon.ts:2098-2118`): Exposes programmatic health query returning
`{ running, clients, sessions }`.

**Rust**: No equivalent API on `Daemon`. The `/status` Telegram command
provides similar info, but no programmatic interface for the CLI.

### L3.3: `sendToSession(sessionId, text)` socket API missing

**TS** (`commands.ts:145-151`): Broadcasts `user_input` to a specific session
via socket. Used by system components for programmatic input.

**Rust**: No equivalent. Input goes through Telegram `/attach` + text, or
tmux injection.

### L3.4: Injector accessor methods missing

`getMethod()`, `getTmuxSession()`, `getTmuxSocket()` — TS public getters used
for startup logging. Rust `InputInjector` has private fields with no public
accessors.

### L3.5: `createInjector()` factory function missing

**TS** (`injector.ts:404-408`): Convenience factory that creates and
initializes an injector in one call. Rust callers use `InputInjector::new()`
directly.

### L3.6: `escapeTmuxText()` (deprecated) not ported

TS exports this as a public (but deprecated) method. Not used internally.
Low-priority public API gap.

### L3.7: Setup completion project-hooks reminder box omitted

**TS**: Shows a final `printBox()` reminder about project hooks at setup
completion.

**Rust**: Omits this reminder entirely.

### L3.8: No binary integrity verification in `resolve-binary.js`

The resolved native binary is executed without any checksum or signature
verification. An attacker who replaces the binary in node_modules gets
arbitrary code execution.

### L3.9: Colorized log output absent

**TS** (`logger.ts`): Configures Winston `colorize()` for console transport.

**Rust**: `tracing_subscriber` `.compact()` format without color. Cosmetic.

### L3.10: `SubagentStopEvent.subagent_id` relaxed from required to optional

**TS**: `subagent_id: string` (required).

**Rust**: `subagent_id: Option<String>` (optional).

Type loosening — could mask bugs where `subagent_id` is unexpectedly absent.

---

## Intentional Differences (not gaps)

These behavioral changes are documented, tested, and represent improvements:

| Item | TS Behavior | Rust Behavior | Rationale |
|------|-------------|---------------|-----------|
| Session ID allows dots | Rejected dots | Allows `.` | Supports Claude's native session IDs |
| PID locking | Read-compare-write (`TOCTOU` race) | `flock(LOCK_EX\|LOCK_NB)` | Kernel-held, race-free |
| Single hook handler | Dual bash+node for PreToolUse | Single `ctm hook` | Architectural simplification |
| `PreCompact` hook type | Not present | Added | New capability |
| Doctor Node.js check | Validates Node >= 18 | Validates binary version | Correct for compiled binary |
| Key map | 5 keys | 7 keys (adds `Ctrl-D`, `Ctrl-L`) | Superset |
| `/attach` partial match | Exact match only | Partial session ID matching | UX improvement |
| Token scrubbing scope | Per-field in Winston formatter | Entire log line via `ScrubWriter` | Strictly stronger |
| Doctor check numbering | No numbering | `[1/10]`, `[2/10]`, etc. | Better UX |
| `/sessions` age display | Minutes only | Hours for sessions >= 60 min | UX improvement |
| `resetConfig()` singleton | Proxy singleton + `resetConfig()` for tests | Direct `load_config()` calls | Correct for compiled binary — no shared process state |
| Config `valid` boolean | Returns `{valid, errors, warnings}` | Returns `(Vec<String>, Vec<String>)` | Caller checks `errors.is_empty()` — idiomatic Rust |
| Approval message post-edit | No edit after decision | Edits message to show decision, removes buttons | UX improvement |
| General topic response | Silently drops messages | Sends help message guiding user to session topic | UX improvement |
| `truncate` char safety | Byte-based `slice()` | Char-count safe (handles multi-byte/emoji) | Correctness improvement |
| `short_path` empty components | No filtering | Filters empty strings from `//foo/bar` | Edge case fix |
| `reply_to_message_id` | Applied to all chunks | Applied only to first chunk | Avoids noisy reply threading |
| Key whitelist enforcement | Unknown keys fail silently | Explicit rejection with warning log | Security improvement |
| Bot commands | 3 commands via TS | 10+ commands (adds `/start`, `/help`, `/status`, `/ping`, `/rename`, etc.) | Feature expansion |
| Setup group selection | Numbered text list | `dialoguer::Select` (arrow keys) | Modern TUI UX |

---

## Recommended Fix Priority (Updated Rev 3)

### Tier 1 — Critical UX Regressions + Correctness (users will notice immediately)

| # | Gap | Effort | Description |
|---|-----|--------|-------------|
| 1 | C3.4 | Medium | **`/abort` must be immediate** — remove confirmation dialog, use graceful bridge abort like TS, no raw Ctrl-C |
| 2 | C3.2 | Small | **`/attach` must reset muted=false** — add `state.muted = false` in attach handler |
| 3 | C3.3 | Small | **`cmd_start` must call `validate_config()` and exit on errors** |
| 4 | C2.2 | Medium | Edit approval messages after decision, remove buttons |
| 5 | C2.3 | Small | Add optional `parse_mode` parameter to `edit_message` |
| 6 | C2.1 | Medium | Restore per-user `/status` showing attached session + mute state |
| 7 | C2.4 | Small | Read `config.rate_limit`, default to sane value (not 25/sec) |
| 8 | C2.6 | Small | Handle text messages without `message_thread_id` |

### Tier 2 — Hook Lifecycle + Correctness

| # | Gap | Effort | Description |
|---|-----|--------|-------------|
| 9 | C3.1 | Medium | **Hook must send `session_start` message** with hostname, projectDir, tmux info |
| 10 | H3.1 | Small | **Read `LOG_LEVEL` as fallback** if `RUST_LOG` not set |
| 11 | H3.2 | Small | **Broadcast `session_start` back to socket clients** after session creation |
| 12 | C2.5 | Small | Use hook-provided `tool_use_id` when present |
| 13 | M3.1 | Small | **Update `lastActivity` for photo/document messages** |
| 14 | M3.2 | Small | Add `hook_id` and `timestamp` fields to `HookEventBase` |
| 15 | M2.3 | Small | Fix `detect_language` bash pattern to `^#.*bash` |
| 16 | M2.13 | Small | Log warning on JSON config parse failure |

### Tier 3 — Information Completeness + UX Polish

| # | Gap | Effort | Description |
|---|-----|--------|-------------|
| 17 | M2.1 | Tiny | Add instructional text to `/attach` reply |
| 18 | M2.2 | Tiny | Add follow-up text to `/detach` reply |
| 19 | M2.4 | Small | Pretty-print tool input JSON |
| 20 | M2.8 | Tiny | Add emoji and parse_mode to setup test message |
| 21 | M2.6 | Small | Doctor: distinguish legacy 3-hook installs from incomplete |
| 22 | M2.10 | Small | Handle `"docker compose"` two-word syntax |
| 23 | M3.4 | Small | Setup: default `useThreads` to existing config value, not always `true` |

### Tier 4 — API Surface & Distribution

| # | Gap | Effort | Description |
|---|-----|--------|-------------|
| 24 | H2.3 | Small | Add `claude-telegram-mirror` symlink or alias |
| 25 | H2.4 | Large | Add `lib.rs` with public API (or document as intentional break) |
| 26 | H2.5 | Small | Add post-install message to npm package scripts |
| 27 | M3.3 | Medium | Add `linux-arm64` to platform map + publish binary package |
| 28 | L3.8 | Medium | Binary integrity verification (checksum/signature) in resolve-binary.js |

### Tier 5 — Edge Cases & Polish

| # | Gap | Effort | Description |
|---|-----|--------|-------------|
| 29 | M2.7 | Tiny | Use appropriate `WorkingDirectory` in systemd unit |
| 30 | M2.9 | Small | Add `reply_to_message_id` to `SendOptions` struct |
| 31 | M2.11 | Small | Enforce socket dir permissions in `listen()` |
| 32 | M2.12 | Medium | Make `createSession` atomic (single INSERT with all fields) |
| 33 | M3.5 | Tiny | Export `formatAndChunk` convenience function |
| 34 | L3.7 | Tiny | Add setup completion project-hooks reminder |

### Tier 6 — Low Priority

All L2.x and L3.x items not listed above. Address opportunistically:
- L3.1: Named types (SendOptions fields, InlineButton, BotSession, HookResult)
- L3.2: Daemon `isRunning()`/`getStatus()` programmatic API
- L3.3: `sendToSession` socket API
- L3.4: Injector accessor methods
- L3.5: `createInjector()` factory
- L3.6: `escapeTmuxText()` (deprecated)
- L3.9: Colorized log output
- L3.10: `SubagentStopEvent.subagent_id` type loosening

### Items Resolved (Rev 2 False Positives — No Fix Needed)

| Gap | Reason |
|-----|--------|
| ~~H2.6~~ | `validate_config()` exists at `config.rs:329` |
| ~~H2.7~~ | `forum_enabled` exists at `config.rs:75` |
| ~~L2.1~~ | `estimate_chunks`/`needs_chunking` exist at `formatting.rs:374-385` |
| ~~M2.5~~ | `ChunkOptions` struct + `chunk_message_with_options` exist at `formatting.rs:402-427` |

---

## Decision

Accept Revision 3 as the canonical gap list. Summary of gap evolution:

| Revision | Gaps Found | Resolved | False Positives | Net Open |
|----------|-----------|----------|-----------------|----------|
| Rev 1 | 37 | 37 | 2 (C2, C3 — later re-evaluated) | 0 |
| Rev 2 | 33 | 0 | 4 (H2.6, H2.7, L2.1, M2.5) | 29 |
| Rev 3 | 15 new + 2 re-evaluated | 0 | 0 | 46 total open |

**Key findings in Revision 3**:

1. **5 Rev 2 items were false positives** — the functionality IS ported. The
   original auditors missed existing Rust implementations.

2. **Rev 1's C2 "false positive" was incorrect** — TS hooks DO send
   `session_start`. The hook-level lifecycle messaging is genuinely absent.

3. **User decision**: `/abort` must be immediate like TS. No confirmation
   dialog. Graceful bridge abort, not raw Ctrl-C.

4. **The Rust rewrite is ~95% functionally complete.** The remaining gaps are
   concentrated in three areas: hook lifecycle messaging (C3.1, H3.1, H3.2),
   UX fidelity (C3.2, C3.4, C2.1-C2.6), and env var compatibility (H3.1).

## Consequences

- **Tier 1 (8 items)** must be fixed before promoting the Rust binary as the
  recommended default — these are user-facing regressions
- **Tier 2 (8 items)** must be fixed before deprecating the TS version — these
  affect correctness and hook lifecycle
- **Tier 3-4 (12 items)** should be addressed before removing TS code entirely
- The public library API question (H2.4) needs a decision: port to `lib.rs` or
  document as an intentional API break with a migration guide
- Each fix should include a regression test validating parity with TS behavior
- `linux-arm64` support (M3.3) blocks deployment on Graviton/Pi targets
- Binary integrity verification (L3.8) is a security prerequisite for the npm
  distribution channel
