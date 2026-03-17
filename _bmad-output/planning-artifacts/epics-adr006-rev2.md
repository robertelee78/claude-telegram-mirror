---
stepsCompleted: [1, 2, 3]
inputDocuments:
  - docs/adr/ADR-006-rust-migration-gap-audit.md (Revision 2)
---

# claude-telegram-mirror - Epic Breakdown (ADR-006 Revision 2)

## Overview

33 new gaps discovered in post-fix re-audit. Original 37 gaps confirmed resolved. These are mostly UX regressions, correctness issues, and API surface gaps — no hard blockers but must be fixed before promoting Rust binary as default.

## Requirements Inventory

### Functional Requirements

FR1: Edit approval messages after decision — append "Decision: Approved/Rejected/Aborted", remove keyboard buttons (C2.2)
FR2: Add optional parse_mode parameter to bot.rs edit_message — currently hardcodes Markdown (C2.3)
FR3: Restore per-user /status showing attached session + mute state from BotSession (C2.1)
FR4: Handle text messages without message_thread_id — route to session handling, don't silently drop (C2.6)
FR5: Read config.rate_limit for governor rate limiter instead of hardcoded 25/sec (C2.4)
FR6: Use hook-provided tool_use_id when present instead of generating a new one (C2.5)
FR7: Fix detect_language bash pattern — anchor to ^#.*bash not contains("bash") (M2.3)
FR8: Log warning on JSON config parse failure instead of silent unwrap_or_default (M2.13)
FR9: Handle "docker compose" two-word syntax in summarizer (M2.10)
FR10: Add instructional text to /attach reply ("You will now receive updates...") (M2.1)
FR11: Add follow-up text to /detach reply ("You will no longer receive updates.") (M2.2)
FR12: Pretty-print tool input JSON with indentation (M2.4)
FR13: Add emoji and parse_mode to setup test message (M2.8)
FR14: Doctor: distinguish legacy 3-hook installs from genuinely incomplete (M2.6)
FR15: Add claude-telegram-mirror binary alias/symlink (H2.3)
FR16: Add lib.rs with public API or document as intentional API break (H2.4)
FR17: Add post-install onboarding message to npm package (H2.5)
FR18: Port validateConfig() with warnings (chunk size range, etc.) (H2.6)
FR19: Add ChunkOptions struct (preserveCodeBlocks, addPartHeaders) (M2.5)
FR20: Use appropriate WorkingDirectory in systemd unit (M2.7)
FR21: Add reply_to_message_id to SendOptions struct (M2.9)
FR22: Enforce socket dir permissions in listen() (M2.11)
FR23: Make createSession atomic — single INSERT with all fields (M2.12)
FR24: Evaluate graceful abort via bridge vs raw Ctrl-C (H2.2)
FR25: Add forum_enabled field to Config struct (H2.7)
FR26: Send session_end on Stop for wire protocol consumers (H2.1) — or document as intentional change

### NonFunctional Requirements

NFR1: Each fix must include a test case validating parity with TypeScript
NFR2: Tier 1-2 fixes required before promoting Rust as recommended default
NFR3: Tier 3-4 fixes required before deprecating TypeScript version

### FR Coverage Map

| FR | Epic | Source |
|----|------|--------|
| FR1 | Epic 1 | C2.2 — approval message edit |
| FR2 | Epic 1 | C2.3 — edit_message parse_mode |
| FR3 | Epic 1 | C2.1 — per-user /status |
| FR4 | Epic 1 | C2.6 — General topic text |
| FR5 | Epic 2 | C2.4 — rate limit config |
| FR6 | Epic 2 | C2.5 — tool_use_id |
| FR7 | Epic 2 | M2.3 — bash detect |
| FR8 | Epic 2 | M2.13 — config parse log |
| FR9 | Epic 2 | M2.10 — docker compose |
| FR10 | Epic 3 | M2.1 — /attach text |
| FR11 | Epic 3 | M2.2 — /detach text |
| FR12 | Epic 3 | M2.4 — pretty JSON |
| FR13 | Epic 3 | M2.8 — setup emoji |
| FR14 | Epic 3 | M2.6 — doctor hooks |
| FR15 | Epic 4 | H2.3 — binary alias |
| FR16 | Epic 4 | H2.4 — lib.rs API |
| FR17 | Epic 4 | H2.5 — postinstall |
| FR18 | Epic 4 | H2.6 — validateConfig |
| FR19 | Epic 5 | M2.5 — ChunkOptions |
| FR20 | Epic 5 | M2.7 — WorkingDirectory |
| FR21 | Epic 5 | M2.9 — SendOptions reply |
| FR22 | Epic 5 | M2.11 — socket dir perms |
| FR23 | Epic 5 | M2.12 — atomic createSession |
| FR24 | Epic 5 | H2.2 — graceful abort |
| FR25 | Epic 5 | H2.7 — forum_enabled |
| FR26 | Epic 5 | H2.1 — session_end wire |

## Epic List

### Epic 1: UX Regressions (Tier 1)
Users notice these immediately — approval buttons stay active, /status is wrong, messages drop silently.
**FRs:** FR1, FR2, FR3, FR4

### Epic 2: Correctness (Tier 2)
Rate limits wrong, IDs don't correlate, language detection false positives, config parse silent.
**FRs:** FR5, FR6, FR7, FR8, FR9

### Epic 3: Information Completeness (Tier 3)
Command replies missing text, JSON unreadable, setup message plain, doctor over-warns.
**FRs:** FR10, FR11, FR12, FR13, FR14

### Epic 4: API Surface and Distribution (Tier 4)
Binary name missing, no library API, no post-install, no validateConfig.
**FRs:** FR15, FR16, FR17, FR18

### Epic 5: Edge Cases and Polish (Tier 5)
Chunk options, service config, socket permissions, atomic sessions, abort behavior.
**FRs:** FR19, FR20, FR21, FR22, FR23, FR24, FR25, FR26

### Epic 6: Low Priority (Tier 6)
L2.1-L2.20 items. Address opportunistically. Not blocking.

---

## Epic 1: UX Regressions

### Story 1.1: Edit approval messages after decision + fix edit_message parse_mode

As a user who taps Approve/Reject on a tool,
I want visual confirmation that my decision was received,
So that I don't double-tap stale buttons.

**Acceptance Criteria:**

**Given** a user taps "Approve" on an approval keyboard
**When** the daemon processes the callback
**Then** the original message is edited to append "\n\nDecision: Approved"
**And** the inline keyboard is removed
**And** the edit uses no parse_mode (or optional parse_mode parameter) to avoid Markdown conflicts with tool names containing underscores

**Given** bot.rs `edit_message`
**When** called
**Then** it accepts an optional `parse_mode` parameter instead of hardcoding "Markdown"
**And** callers can pass `None` to avoid parse errors on text with special chars

**Files:** `rust-crates/ctm/src/daemon.rs`, `rust-crates/ctm/src/bot.rs`

### Story 1.2: Per-user /status and General topic text handling

As a user checking my current state,
I want /status to show my attached session and mute state,
So that I know what I'm looking at.

**Acceptance Criteria:**

**Given** `/status` sent in a forum topic
**When** the user has an attached session (via /attach) and is muted
**Then** the response includes "Attached to: {session_id}" and "Muted: yes"
**And** still shows aggregate counts

**Given** text sent in the General topic (no thread_id)
**When** the daemon receives it
**Then** it is handled appropriately (route to most recent session or show a helpful message) instead of being silently dropped

**Files:** `rust-crates/ctm/src/daemon.rs`

---

## Epic 2: Correctness

### Story 2.1: Rate limit, tool_use_id, language detection, config parse, docker compose

As a user relying on correct behavior,
I want rate limits respected, IDs correlated, and no false positives,
So that everything works as documented.

**Acceptance Criteria:**

**Given** `config.rate_limit` is set to 1
**When** the governor rate limiter is created
**Then** it uses `config.rate_limit` (not hardcoded 25)
**And** defaults to a sane value (e.g., 20/sec) when config says 1 msg/sec (since governor operates differently from delay-based)

**Given** a hook provides `tool_use_id` in metadata
**When** the daemon processes tool_start
**Then** it uses the hook-provided ID, not a generated one

**Given** code containing the word "bash" in a sentence (not a shebang or comment)
**When** `detect_language` runs
**Then** it does NOT detect as bash (fix pattern to `^#.*bash` or `^#!/`)

**Given** a malformed config.json
**When** parsed
**Then** a warning is logged with the file path and error

**Given** a Bash command `docker compose up -d`
**When** summarized
**Then** it returns "Starting containers" (not generic "Running docker compose")

**Files:** `rust-crates/ctm/src/bot.rs`, `rust-crates/ctm/src/daemon.rs`, `rust-crates/ctm/src/formatting.rs`, `rust-crates/ctm/src/config.rs`, `rust-crates/ctm/src/summarize.rs`

---

## Epic 3: Information Completeness

### Story 3.1: Command reply text, pretty JSON, setup emoji, doctor hooks

As a user interacting with the bot,
I want complete, informative responses,
So that I understand what happened.

**Acceptance Criteria:**

**Given** `/attach` succeeds
**When** the reply is sent
**Then** it includes "You will now receive updates from this session.\nReply with text to send input."

**Given** `/detach` succeeds
**When** the reply is sent
**Then** it includes "You will no longer receive updates."

**Given** tool input displayed in Telegram
**When** formatted
**Then** JSON is pretty-printed with 2-space indentation (not compact single-line)

**Given** setup test message
**When** sent
**Then** it includes the robot emoji and uses Markdown parse_mode

**Given** doctor checks hooks on a legacy 3-hook install
**When** reporting
**Then** it distinguishes "3/6 hooks (legacy install — run ctm install-hooks to update)" from "3/6 hooks (incomplete)"

**Files:** `rust-crates/ctm/src/daemon.rs`, `rust-crates/ctm/src/setup.rs`, `rust-crates/ctm/src/doctor.rs`

---

## Epic 4: API Surface and Distribution

### Story 4.1: Binary alias, postinstall, validateConfig

As a user upgrading from TypeScript to Rust,
I want backward compatibility and helpful onboarding,
So that nothing breaks and I know what to do.

**Acceptance Criteria:**

**Given** a user runs `claude-telegram-mirror start`
**When** the binary is installed
**Then** it works (symlink or alias from `claude-telegram-mirror` to `ctm`)

**Given** npm install completes
**When** postinstall runs
**Then** a formatted onboarding banner is displayed (Quick Setup, Commands, Documentation link)

**Given** `ctm start` is called
**When** config is loaded
**Then** `validate_config()` runs and prints warnings (e.g., chunk_size outside 1000-4096 range)
**And** follows the TS pattern of {errors, warnings} output

**Files:** `rust-crates/ctm/Cargo.toml` (binary alias), `scripts/postinstall-rust.js` (NEW), `rust-crates/ctm/src/config.rs`, `rust-crates/ctm/src/main.rs`

### Story 4.2: Public library API decision

As a downstream consumer of claude-telegram-mirror,
I want to know if the library API is maintained,
So that I can plan my migration.

**Acceptance Criteria:**

**Given** the decision on H2.4
**When** documented
**Then** EITHER a `lib.rs` is created with public exports matching `src/index.ts`
**OR** a migration guide documents the intentional API break with alternatives

**Note:** This may be a documentation-only story if the decision is to break the API.

**Files:** `rust-crates/ctm/src/lib.rs` (NEW, if creating) OR `docs/MIGRATION.md` (NEW)

---

## Epic 5: Edge Cases and Polish

### Story 5.1: ChunkOptions, service config, socket permissions, atomic session

As a developer relying on correct edge-case behavior,
I want all boundary conditions handled properly,
So that nothing fails silently.

**Acceptance Criteria:**

**Given** `chunk_message` called with code-block-heavy content
**When** caller wants no part headers
**Then** a `ChunkOptions { add_part_headers: bool, preserve_code_blocks: bool }` parameter is available

**Given** systemd service installed
**When** WorkingDirectory is set
**Then** it points to the appropriate directory (package root or home)

**Given** `send_message_reply_to` is called
**When** the caller uses SendOptions
**Then** `reply_to_message_id` is available as a field on SendOptions (not only as a separate method)

**Given** socket server starts
**When** `listen()` is called
**Then** socket parent directory permissions are verified/enforced as 0o700

**Given** a new session is created
**When** `create_session` is called with thread_id and tmux info
**Then** all fields are set in a single atomic INSERT

**Given** `/abort` with confirmation
**When** user confirms
**Then** behavior is evaluated: graceful bridge abort (if possible) or document that Ctrl-C is the intentional Rust approach

**Given** forum detection needed outside daemon
**When** config is loaded
**Then** `forum_enabled: bool` field exists on Config struct

**Files:** `rust-crates/ctm/src/formatting.rs`, `rust-crates/ctm/src/service.rs`, `rust-crates/ctm/src/bot.rs`, `rust-crates/ctm/src/socket.rs`, `rust-crates/ctm/src/session.rs`, `rust-crates/ctm/src/daemon.rs`, `rust-crates/ctm/src/config.rs`

---

## Epic 6: Low Priority

L2.1-L2.20 items. These are minor and can be addressed opportunistically:
- estimateChunks/needsChunking utilities
- isMirrorEnabled fast-path
- DEFAULT_MAX_LENGTH constant export
- isRunning/getSession on TelegramBot
- createApprovalKeyboard extraction
- SocketClientInfo type
- DEFAULT_SOCKET_PATH/SOCKET_DIR exports
- checkSocketStatus utility
- MessageQueueItem.createdAt
- short_path empty component filtering
- Installer details strings
- --project custom path support
- handleAgentResponse standalone method
- handleSessionStart/End standalone methods
- checkHookStatus programmatic API
- Hook install emoji output + restart reminder
- First-run error message URLs (partially done)
- Status CLI emoji icons
- config --show flag handling

No dedicated stories — address during related work or as a cleanup sprint.

---

## Summary

| Epic | Tier | Stories | FRs |
|------|------|---------|-----|
| 1 | UX Regressions | 2 | FR1-FR4 |
| 2 | Correctness | 1 | FR5-FR9 |
| 3 | Info Completeness | 1 | FR10-FR14 |
| 4 | API + Distribution | 2 | FR15-FR18 |
| 5 | Edge Cases + Polish | 1 | FR19-FR26 |
| 6 | Low Priority | (opportunistic) | L2.x |
| **Total** | | **7 stories** | **26 FRs** |
