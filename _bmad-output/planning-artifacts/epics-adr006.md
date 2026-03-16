---
stepsCompleted: [1, 2, 3]
inputDocuments:
  - docs/adr/ADR-006-rust-migration-gap-audit.md
  - docs/adr/ADR-003-dual-hook-handlers.md
  - docs/adr/ADR-004-tmux-only-injection.md
  - docs/adr/ADR-005-sqlite-session-storage.md
---

# claude-telegram-mirror - Epic Breakdown (ADR-006: Rust Gap Audit)

## Overview

This document provides the complete epic and story breakdown for resolving all gaps identified in ADR-006 (Rust Migration Gap Audit). Each tier maps to one epic. Each epic must be completed before the next begins. The Rust binary must NOT be promoted to default until Epics 1-3 are complete.

## Requirements Inventory

### Functional Requirements

FR1: Broadcast `approval_response` over socket after callback resolution in daemon.rs (C1)
FR2: Allow `.` in session ID validation regex — change to `[a-zA-Z0-9._-]` (C4)
FR3: Implement global token scrubbing in tracing subscriber layer using regex, not literal replacement (C6)
FR4: Emit `session_start` message from hook handler with project dir, hostname, tmux info (C2)
FR5: Emit `session_end` (not `turn_complete`) on Stop events (C3)
FR6: Change launchd label to `com.claude.claude-telegram-mirror` to match TypeScript or add migration logic (C5)
FR7: Add `WorkingDirectory` to both systemd unit and launchd plist templates (M10)
FR8: Add service-layer check to stop/restart/status CLI commands — detect systemd/launchd before PID file (H6+H7)
FR9: Port `/abort` command with inline confirmation keyboard (H1)
FR10: Port `/attach <id>` command with BotSession state (H1)
FR11: Port `/detach` command (H1)
FR12: Port `/mute` and `/unmute` commands with session muted state (H1)
FR13: Forward `tool_error` in PostToolUse hook messages (H2)
FR14: Include `tool_input` in PostToolUse metadata (H3)
FR15: Check `transcript_summary` field before falling back to JSONL file I/O on Stop (H4)
FR16: Port `formatToolDescription()` for rich approval prompts (H5)
FR17: Fix `tool_input` metadata read — `.as_str()` on JSON object always returns None, use proper serialization (H10)
FR18: Implement rename deduplication — skip if title unchanged (H8)
FR19: Call `detect_tmux_session()` at daemon startup (H9)
FR20: Edit Telegram message on answer callback to show "Selected" (M1)
FR21: Re-render keyboard with checkmarks on toggle callback (M2)
FR22: Edit Telegram message on submit callback to show "Submitted" (M3)
FR23: Send tool details as reply-to-original message (M4)
FR24: Fix /ping — send message first, then edit to measure actual round-trip latency (M5)
FR25: Show session age ("Started: Xm ago") in /sessions output (M6)
FR26: Two-per-row inline keyboard button layout to match TypeScript (M7)
FR27: Set `timeout: 310` on PreToolUse hook entry in installer (M8)
FR28: Fix CTM hook command matching to avoid false positives on substrings like "scrutm" (M9)
FR29: Check all 6 hook types in doctor, not just 3 (M12)
FR30: Log warning on JSON config parse failure instead of silent fallback (M13)
FR31: Fix output significance threshold: `>= 10` not `> 10`, add whitespace check (M14)
FR32: Add `NODE_ENV=production` to launchd plist env vars (M11)
FR33: Add `--foreground` flag to `ctm start` for script compatibility (L5)
FR34: Add `configPath` field to Rust Config struct (L7)
FR35: Improve first-run error messages with @BotFather URL and getUpdates hint (L8)
FR36: Fix `set_current_dir` in setup — pass path as parameter instead of modifying global state (L9)
FR37: Distinguish connection-refused vs timeout in hook error handling (L20)

### NonFunctional Requirements

NFR1: Each fix must include a test case validating parity with TypeScript behavior
NFR2: Rust binary must NOT be promoted to default until Tiers 1-3 (Epics 1-3) are resolved
NFR3: TypeScript implementation remains the reference for correct behavior

### Additional Requirements

- Engineering mantra governs all work
- Read TypeScript source before fixing each gap — verify the expected behavior
- cargo clippy clean, cargo fmt clean, zero warnings after each epic
- All existing 184+ Rust tests must continue to pass

### FR Coverage Map

| FR | Epic | Source |
|----|------|--------|
| FR1 | Epic 1 | C1 — approval broadcast |
| FR2 | Epic 1 | C4 — session ID dots |
| FR3 | Epic 2 | C6 — token scrubbing |
| FR4 | Epic 3 | C2 — session_start |
| FR5 | Epic 3 | C3 — session_end |
| FR6 | Epic 4 | C5 — launchd label |
| FR7 | Epic 4 | M10 — WorkingDirectory |
| FR8 | Epic 4 | H6+H7 — service check |
| FR9 | Epic 5 | H1 — /abort |
| FR10 | Epic 5 | H1 — /attach |
| FR11 | Epic 5 | H1 — /detach |
| FR12 | Epic 5 | H1 — /mute+/unmute |
| FR13 | Epic 6 | H2 — tool_error |
| FR14 | Epic 6 | H3 — tool_input metadata |
| FR15 | Epic 6 | H4 — transcript_summary |
| FR16 | Epic 6 | H5 — rich approval prompts |
| FR17 | Epic 6 | H10 — .as_str() fix |
| FR18 | Epic 7 | H8 — rename dedup |
| FR19 | Epic 7 | H9 — injector startup |
| FR20 | Epic 7 | M1 — answer "Selected" |
| FR21 | Epic 7 | M2 — toggle checkmarks |
| FR22 | Epic 7 | M3 — submit "Submitted" |
| FR23 | Epic 7 | M4 — reply-to tool details |
| FR24 | Epic 7 | M5 — ping latency |
| FR25 | Epic 7 | M6 — session age |
| FR26 | Epic 7 | M7 — button layout |
| FR27 | Epic 7 | M8 — hook timeout |
| FR28 | Epic 8 | M9 — CTM matching |
| FR29 | Epic 8 | M12 — doctor 6 hooks |
| FR30 | Epic 8 | M13 — config parse log |
| FR31 | Epic 8 | M14 — output threshold |
| FR32 | Epic 8 | M11 — NODE_ENV plist |
| FR33 | Epic 8 | L5 — --foreground flag |
| FR34 | Epic 8 | L7 — configPath field |
| FR35 | Epic 8 | L8 — error messages |
| FR36 | Epic 8 | L9 — set_current_dir |
| FR37 | Epic 8 | L20 — error distinction |

## Epic List

### Epic 1: Fix Blocking Issues (Tier 1)
The approval workflow is non-functional and session IDs with dots are silently dropped. These two issues make the Rust binary unusable.
**FRs covered:** FR1, FR2
**BLOCKER:** Must be fixed before any production use.

### Epic 2: Fix Security Issues (Tier 2)
Bot tokens leak in Rust logs because scrubbing uses literal replacement instead of regex.
**FRs covered:** FR3
**BLOCKER:** Must be fixed before any deployment.

### Epic 3: Fix Session Lifecycle (Tier 3)
Sessions never start or end properly — hook doesn't emit the right message types.
**FRs covered:** FR4, FR5
**BLOCKER:** Must be fixed before promotion to default.

### Epic 4: Fix Service Compatibility (Tier 4)
Service management is broken across TS/Rust boundary and missing critical config.
**FRs covered:** FR6, FR7, FR8, FR32

### Epic 5: Port Missing Bot Commands (Tier 5)
Five bot commands not ported: /abort, /attach, /detach, /mute, /unmute. Requires BotSession state.
**FRs covered:** FR9, FR10, FR11, FR12

### Epic 6: Fix Data Completeness (Tier 6)
Tool errors silently dropped, tool input missing from metadata, transcript summary ignored, approval prompts bare.
**FRs covered:** FR13, FR14, FR15, FR16, FR17

### Epic 7: UX Polish (Tier 7)
Rename dedup, injector startup, callback visual feedback, ping latency, button layout, hook timeout.
**FRs covered:** FR18, FR19, FR20, FR21, FR22, FR23, FR24, FR25, FR26, FR27

### Epic 8: Correctness and Minor Fixes (Tier 8 + Low)
Doctor checks incomplete, config parse silent, output threshold off, CLI compatibility, error messages.
**FRs covered:** FR28, FR29, FR30, FR31, FR33, FR34, FR35, FR36, FR37

---

## Epic 1: Fix Blocking Issues

The Rust binary is non-functional for approval workflows and may silently drop all hook events.

### Story 1.1: Broadcast approval_response over socket

As a user approving tools from Telegram,
I want my approval decision to reach the hook process,
So that Claude actually receives my approve/reject and continues working.

**Acceptance Criteria:**

**Given** a user taps "Approve" on an approval keyboard in Telegram
**When** the daemon resolves the approval in the database
**Then** an `approval_response` message is broadcast over the socket with `{ type: "approval_response", sessionId, content: "approve", metadata: { approvalId } }`
**And** the hook process blocked in `send_and_wait()` receives it and returns the decision to Claude

**Given** a user taps "Reject"
**When** the daemon processes it
**Then** an `approval_response` with `content: "reject"` is broadcast

**Given** a user taps "Abort"
**When** the daemon processes it
**Then** an `approval_response` with `content: "abort"` is broadcast
**And** the session is ended

**Files:** `rust-crates/ctm/src/daemon.rs`

### Story 1.2: Allow dots in session ID validation

As a Claude Code user,
I want all valid session IDs accepted by the hook,
So that hook events are never silently dropped.

**Acceptance Criteria:**

**Given** a session ID containing `.` characters (e.g., `abc.def-123`)
**When** the hook validates it
**Then** it passes validation and is processed normally

**Given** the regex in `types.rs` `is_valid_session_id()`
**When** checked
**Then** the allowed character set is `[a-zA-Z0-9._-]` (dot added)

**And** existing tests updated to cover dot-containing IDs

**Files:** `rust-crates/ctm/src/types.rs`, `rust-crates/ctm/src/hook.rs` (if validation duplicated)

---

## Epic 2: Fix Security Issues

### Story 2.1: Global regex-based token scrubbing in tracing

As a user who might share logs,
I want bot tokens scrubbed from ALL log output automatically,
So that credentials never leak regardless of which code path logs the URL.

**Acceptance Criteria:**

**Given** any `tracing::warn!` or `tracing::error!` that interpolates a Telegram API URL containing the bot token
**When** the log line is emitted to stderr
**Then** the token is replaced with `[REDACTED]` using regex `bot\d+:[A-Za-z0-9_-]+/`

**Given** the current `scrub_bot_token(text, token)` in bot.rs
**When** reviewed
**Then** it is replaced with a regex-based approach that works globally (not just on the configured token)

**And** the scrubbing is applied at the tracing subscriber layer (format function) so ALL log output is covered without requiring explicit calls

**Files:** `rust-crates/ctm/src/bot.rs`, `rust-crates/ctm/src/main.rs` (tracing subscriber setup)

---

## Epic 3: Fix Session Lifecycle

### Story 3.1: Emit session_start from hook

As a bridge daemon,
I want to receive session_start messages when new sessions begin,
So that I can create the session in SQLite with the correct metadata.

**Acceptance Criteria:**

**Given** any hook event for a session that hasn't been seen before
**When** the hook processes it
**Then** a `session_start` message is emitted BEFORE other messages, containing: project_dir (from cwd), hostname, tmux_target, tmux_socket

**And** the message type is `"session_start"` not `"turn_complete"`

**Files:** `rust-crates/ctm/src/hook.rs`

### Story 3.2: Emit session_end on Stop events

As a bridge daemon,
I want Stop events to emit session_end,
So that sessions are properly closed and cleanup triggers.

**Acceptance Criteria:**

**Given** a Stop hook event
**When** the hook processes it
**Then** after sending agent_response and turn_complete, it also sends a `session_end` message with tmux metadata

**And** the daemon's session_end handler triggers topic deletion scheduling, approval expiry, etc.

**Files:** `rust-crates/ctm/src/hook.rs`

---

## Epic 4: Fix Service Compatibility

### Story 4.1: Fix launchd label and add WorkingDirectory

As a macOS user upgrading from TypeScript to Rust,
I want service management to work across the transition,
So that my existing service isn't orphaned.

**Acceptance Criteria:**

**Given** `ctm service install` on macOS
**When** the plist is generated
**Then** the label is `com.claude.claude-telegram-mirror` (matching TypeScript)
**And** `WorkingDirectory` is set to the user's home directory

**Given** `ctm service install` on Linux
**When** the unit file is generated
**Then** `WorkingDirectory` is set

**Given** `NODE_ENV=production` in the launchd environment
**When** the plist is generated
**Then** `NODE_ENV` is included in the `EnvironmentVariables` dict

**Files:** `rust-crates/ctm/src/service.rs`

### Story 4.2: Service-aware stop/restart/status

As a user running ctm as a system service,
I want stop/restart/status to detect and use the service manager,
So that stopping the daemon doesn't cause an immediate restart by systemd/launchd.

**Acceptance Criteria:**

**Given** the daemon is managed by systemd
**When** `ctm stop` is called
**Then** it calls `systemctl --user stop claude-telegram-mirror` instead of sending SIGTERM to the PID

**Given** the daemon is managed by launchd
**When** `ctm stop` is called
**Then** it calls `launchctl unload` on the plist

**Given** `ctm status` is called with a service installed
**When** the service is running
**Then** it shows "Running (via system service)" not just "Running"
**And** hook installation status is displayed

**Files:** `rust-crates/ctm/src/main.rs`, `rust-crates/ctm/src/service.rs`

---

## Epic 5: Port Missing Bot Commands

### Story 5.1: Port /abort, /attach, /detach, /mute, /unmute

As a Telegram user controlling Claude sessions,
I want all bot commands available,
So that I have full control matching the TypeScript version.

**Acceptance Criteria:**

**Given** `/abort` sent in a topic
**When** processed
**Then** a confirmation keyboard (Confirm/Cancel) appears
**And** on Confirm, the session is aborted via the bridge

**Given** `/attach <session-id>` sent in a topic
**When** processed
**Then** the current thread is attached to the specified session
**And** messages in this thread route to that session

**Given** `/detach` sent in a topic
**When** processed
**Then** the thread is detached from its session

**Given** `/mute` sent in a topic
**When** processed
**Then** agent responses for this session are suppressed in Telegram

**Given** `/unmute` sent in a topic
**When** processed
**Then** agent responses resume

**And** a `BotSession` state struct tracks `attached_session_id`, `muted`, `last_activity` per thread

**Files:** `rust-crates/ctm/src/daemon.rs`

---

## Epic 6: Fix Data Completeness

### Story 6.1: Fix tool error, tool input, transcript summary, approval prompts, and input metadata

As a user monitoring Claude from Telegram,
I want complete and accurate information in all notifications,
So that I can understand what Claude is doing without switching to the terminal.

**Acceptance Criteria:**

**Given** a PostToolUse event with `tool_error` set and `tool_output` empty
**When** the hook processes it
**Then** `tool_error` is included in the metadata under `error` key
**And** the content falls back to `tool_error` when `tool_output` is absent

**Given** a PostToolUse event
**When** the hook sends the `tool_result` message
**Then** `tool_input` is included in metadata (as JSON, not stringified)

**Given** a Stop event with `transcript_summary` populated
**When** the hook processes it
**Then** `transcript_summary` is sent as `agent_response` BEFORE falling back to JSONL file I/O

**Given** a PreToolUse approval prompt
**When** sent to Telegram
**Then** the content includes rich formatting: file path for Write/Edit, command block for Bash, JSON for others (matching `formatToolDescription()` from TypeScript)

**Given** `tool_input` in daemon.rs metadata reading
**When** the `"input"` field is accessed
**Then** it uses `serde_json::to_string()` or handles the JSON Value properly (not `.as_str()` which returns None for objects)

**Files:** `rust-crates/ctm/src/hook.rs`, `rust-crates/ctm/src/daemon.rs`

---

## Epic 7: UX Polish

### Story 7.1: Rename dedup, injector startup, callback feedback

As a user interacting with Claude from Telegram,
I want polished visual feedback and correct behavior,
So that the experience matches the TypeScript version.

**Acceptance Criteria:**

**Given** a session is renamed to the same title it already has
**When** the daemon processes the rename
**Then** no `editForumTopic` API call is made and no "Topic renamed" message is sent

**Given** the daemon starts
**When** initialization runs
**Then** `detect_tmux_session()` is called and the injector target is set if tmux is available

**Given** a user taps an answer button on AskUserQuestion
**When** the callback is processed
**Then** the original message is edited to append "Selected: {option}"
**And** the keyboard is removed

**Given** a user toggles multi-select options
**When** the toggle callback fires
**Then** the keyboard is re-rendered with checkmarks on selected options

**Given** a user taps Submit on multi-select
**When** the submit callback fires
**Then** the message is edited to show "Submitted: {options}"

**Files:** `rust-crates/ctm/src/daemon.rs`, `rust-crates/ctm/src/bot.rs`

### Story 7.2: Ping, sessions, tool details, button layout, hook timeout

As a user,
I want accurate latency, session info, proper threading, and correct layout,
So that every interaction feels polished.

**Acceptance Criteria:**

**Given** `/ping` is sent
**When** processed
**Then** the bot sends a message, records the time, then EDITS the message to show "Pong! {ms}ms" with actual round-trip latency

**Given** `/sessions` is sent
**When** processed
**Then** each session shows "Started: Xm ago" with calculated age

**Given** a tool details callback
**When** the details are sent
**Then** they are sent as a REPLY to the original message (using `reply_to_message_id`)

**Given** inline keyboard buttons
**When** rendered
**Then** layout is two buttons per row (matching TypeScript)

**Given** the hook installer writes PreToolUse entry
**When** the entry is created
**Then** `timeout: 310` is set on the hook configuration

**Files:** `rust-crates/ctm/src/daemon.rs`, `rust-crates/ctm/src/bot.rs`, `rust-crates/ctm/src/installer.rs`

---

## Epic 8: Correctness and Minor Fixes

### Story 8.1: Doctor, config, output threshold fixes

As a user running diagnostics or with edge-case configurations,
I want correct behavior in all cases,
So that nothing silently fails.

**Acceptance Criteria:**

**Given** `ctm doctor` checks hooks
**When** it verifies installation
**Then** all 6 hook types are checked (PreToolUse, PostToolUse, Notification, Stop, UserPromptSubmit, PreCompact), not just 3

**Given** a malformed `config.json`
**When** config loading fails to parse
**Then** a warning is logged (not silent fallback)

**Given** tool output of exactly 10 characters
**When** significance is checked
**Then** it passes (>= 10, matching TypeScript)

**Given** tool output that is all whitespace
**When** significance is checked
**Then** it is suppressed (matching TypeScript)

**Given** CTM hook command matching
**When** checking for existing hooks
**Then** the match uses exact path matching or `ctm hook` as literal, not substring `"ctm"` which could match unrelated commands

**Files:** `rust-crates/ctm/src/doctor.rs`, `rust-crates/ctm/src/config.rs`, `rust-crates/ctm/src/hook.rs`, `rust-crates/ctm/src/installer.rs`

### Story 8.2: CLI compatibility and minor improvements

As a user with existing scripts or first-time setup,
I want CLI flags to work and error messages to be helpful,
So that nothing breaks on upgrade and setup is smooth.

**Acceptance Criteria:**

**Given** `ctm start --foreground`
**When** called
**Then** the flag is accepted (even if behavior is same as default)

**Given** a first-time user with no TELEGRAM_BOT_TOKEN
**When** config validation fails
**Then** the error message includes: "Create a bot via @BotFather" and the getUpdates URL hint

**Given** setup wizard project hook installation
**When** a project path is provided
**Then** the path is passed as parameter, not via `set_current_dir` (thread-safe)

**Given** a hook connection is refused (daemon not running)
**When** the error is returned
**Then** it is distinguished from a timeout (approval expired) with different messages

**Files:** `rust-crates/ctm/src/main.rs`, `rust-crates/ctm/src/config.rs`, `rust-crates/ctm/src/setup.rs`, `rust-crates/ctm/src/hook.rs`
