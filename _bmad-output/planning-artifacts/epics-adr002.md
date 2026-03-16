---
stepsCompleted: [1, 2, 3]
inputDocuments:
  - docs/adr/ADR-002-phased-rust-migration.md
---

# claude-telegram-mirror - Epic Breakdown (ADR-002: Rust Migration)

## Overview

This document provides the complete epic and story breakdown for the phased Rust migration of claude-telegram-mirror, decomposing the requirements from ADR-002 into implementable stories. Each phase is an epic. Each epic must achieve 100% feature parity before proceeding.

## Requirements Inventory

### Functional Requirements

FR1: Rust binary `ctm hook` handles all 6 hook event types (PreToolUse, PostToolUse, Notification, Stop, UserPromptSubmit, PreCompact)
FR2: PreToolUse approval blocking with bypassPermissions check, safe command whitelist, 5-min timeout, hookSpecificOutput JSON
FR3: Stop event transcript extraction with `.last_line_${SESSION_ID}` state tracking
FR4: Rust input injector: injectText, sendKey, sendSlashCommand, detectTmuxSession, findClaudeCodeSession, target validation (BUG-001, BUG-004)
FR5: Rust config loader: 13 env vars, config file, defaults, priority order, validation
FR6: Binary distribution: `@agidreams/ctm-linux-x64`, `@agidreams/ctm-darwin-arm64`, resolve-binary.js, CI builds
FR7: Graceful behavior: silent exit when daemon absent, 1MB stdin limit, session ID validation
FR8: Rust SQLite session manager: all 24 methods, identical schema, migrations, message_id on approvals
FR9: Rust socket server: NDJSON, flock(2) PID locking, umask on bind, connection/line limits
FR10: Rust socket client: sendAndWait with correlation, reconnection, clean disconnect
FR11: Rust message formatting: escapeMarkdownV2 (NOT no-op), detectLanguage, wrapInCodeBlock, formatToolDetails for all tools
FR12: Rust message chunker: code-block-aware, natural break points, part headers, UTF-8-safe
FR13: Rust logger: stderr-only, structured fields, log levels
FR14: Rust tool summarizer: port from ADR-001 Item 9
FR15: Rust bridge daemon: all 12 message types, all BUG fixes (001-012), echo prevention, topic lifecycle, stale cleanup with differentiated timeouts
FR16: Rust Telegram bot: MessageQueue with retry/backoff + governor token-bucket, TOPIC_CLOSED recovery, entity fallback, forum topic CRUD, security middleware, file download
FR17: Rust bot commands: all 10 commands, approval keyboard, tool details callback, abort confirmation, AskUserQuestion rendering
FR18: Rust CLI: start, stop, restart, status, config, install-hooks, uninstall-hooks, hooks, setup, doctor, service (6 sub-commands)
FR19: Rust service manager: systemd unit generation + launchd plist generation, install/uninstall/start/stop/restart/status, env file parsing
FR20: Rust interactive setup wizard: 8-step flow with live API validation, auto-detect groups, config save, hook/service install
FR21: Rust doctor: 9 diagnostic checks with --fix, auto-remediation for safe fixes, suggestions for unsafe
FR22: Rust hook installer: programmatic settings.json modification, global + project install, idempotent, preserve non-CTM hooks, all 6 hook types
FR23: Wire compatibility: Rust and TS components must communicate via identical NDJSON protocol during transition
FR24: TypeScript fallback: if Rust binary unavailable, TS handlers must still work
FR25: CI: cargo build, clippy, fmt, test for Rust; existing npm CI for TS
FR26: Each phase exit criteria met before proceeding (100% parity, clippy clean, zero warnings)
FR27: Chesterton's fence: full feature inventory verified against actual code before each phase
FR28: No unimplemented!() macros in shipped code

### NonFunctional Requirements

NFR1: Hook latency <5ms for fire-and-forget events
NFR2: Idle RSS <10MB
NFR3: Binary size <20MB static release
NFR4: cargo clippy clean, cargo fmt clean, zero warnings per phase
NFR5: Wire-compatible NDJSON protocol between Rust and TS during transition
NFR6: Each phase independently valuable — product works at every checkpoint

### Additional Requirements

- Engineering mantra governs all work: no shortcuts, no stubs, Chesterton's fence
- DreamLab-AI fork code is IGNORED — write from scratch
- Use "whitelist" terminology, never "allowlist"
- Binary distribution follows swictation pattern (scoped optional npm packages)
- TypeScript remains fallback at every phase
- BUG-001 through BUG-012 must all be preserved

### UX Design Requirements

N/A — no UX design document. UX requirements are embedded in functional requirements (bot commands, setup wizard, doctor output).

### FR Coverage Map

| FR | Epic | Description |
|----|------|------------|
| FR1 | Epic 1 | Hook event handling (6 types) |
| FR2 | Epic 1 | PreToolUse approval logic |
| FR3 | Epic 1 | Stop transcript extraction |
| FR4 | Epic 1 | Input injector |
| FR5 | Epic 1 | Config loader |
| FR6 | Epic 1 | Binary distribution |
| FR7 | Epic 1 | Graceful behavior + validation |
| FR8 | Epic 2 | SQLite session manager |
| FR9 | Epic 2 | Socket server with flock |
| FR10 | Epic 2 | Socket client with sendAndWait |
| FR11 | Epic 2 | Message formatting |
| FR12 | Epic 2 | Message chunker |
| FR13 | Epic 2 | Logger |
| FR14 | Epic 2 | Tool summarizer |
| FR15 | Epic 3 | Bridge daemon |
| FR16 | Epic 3 | Telegram bot |
| FR17 | Epic 3 | Bot commands |
| FR18 | Epic 3 | CLI commands |
| FR19 | Epic 4 | Service manager |
| FR20 | Epic 4 | Setup wizard |
| FR21 | Epic 4 | Doctor |
| FR22 | Epic 4 | Hook installer |
| FR23 | Epic 1-3 | Wire compatibility (cross-cutting) |
| FR24 | Epic 1 | TypeScript fallback |
| FR25 | Epic 1 | Rust CI |
| FR26 | Epic 1-4 | Exit criteria (cross-cutting) |
| FR27 | Epic 1-4 | Chesterton's fence (cross-cutting) |
| FR28 | Epic 1-4 | No unimplemented!() (cross-cutting) |

## Epic List

### Epic 1: Hook Binary + Injector + Config (Phase 1)
Replace the hottest, most security-critical code path with a Rust binary. Users get faster hooks (<5ms vs 100-200ms), eliminated shell injection class, and single-binary hook handling. TypeScript daemon continues unchanged.
**FRs covered:** FR1, FR2, FR3, FR4, FR5, FR6, FR7, FR23, FR24, FR25
**NFRs covered:** NFR1, NFR4, NFR5, NFR6

### Epic 2: Session + Socket + Formatting (Phase 2)
Replace the data and communication layers with Rust. Users get atomic PID locking (flock), proper socket permissions (umask), UTF-8-safe chunking, and wire-compatible session management.
**FRs covered:** FR8, FR9, FR10, FR11, FR12, FR13, FR14, FR23
**NFRs covered:** NFR4, NFR5

### Epic 3: Daemon + Bot + Full CLI (Phase 3)
Replace the core orchestration, Telegram integration, and CLI with Rust. Users get the full `ctm` binary with all commands, governor rate limiting, and every BUG fix preserved.
**FRs covered:** FR15, FR16, FR17, FR18, FR23
**NFRs covered:** NFR2, NFR3, NFR4

### Epic 4: Services + Setup + Doctor + Installer (Phase 4)
Replace operational tooling with Rust. Users get a complete single-binary experience: interactive setup, systemd/launchd service management, diagnostic doctor, and hook installation — all without Node.js.
**FRs covered:** FR19, FR20, FR21, FR22
**NFRs covered:** NFR4

---

## Epic 1: Hook Binary + Injector + Config

Replace the hottest, most security-critical code path with a Rust binary.

### Story 1.1: Rust Project Scaffolding and CI

As a developer starting the Rust migration,
I want a properly configured Cargo workspace with CI,
So that all subsequent Rust code has a home with automated quality checks.

**Acceptance Criteria:**

**Given** the project root
**When** the Rust scaffolding is created
**Then** `rust-crates/Cargo.toml` defines a workspace with a `ctm` binary crate
**And** `rust-crates/ctm/Cargo.toml` includes dependencies: tokio, serde, serde_json, clap (derive), nix, tracing, tracing-subscriber, anyhow, thiserror
**And** `rust-crates/ctm/src/main.rs` has a clap CLI with `hook` subcommand that compiles
**And** `cargo build --release` succeeds with zero warnings
**And** `cargo clippy` is clean
**And** `cargo fmt --check` passes

**Given** a push to master
**When** GitHub Actions CI runs
**Then** a `rust` job runs: cargo check, cargo clippy, cargo fmt --check, cargo test
**And** the existing TypeScript CI jobs continue to pass

### Story 1.2: Config Loader in Rust

As a hook binary,
I want to load configuration from env vars, config file, and defaults,
So that I know where the socket is and what options are set.

**Acceptance Criteria:**

**Given** `TELEGRAM_BOT_TOKEN` and `TELEGRAM_CHAT_ID` are set as env vars
**When** `load_config(require_auth: true)` is called
**Then** the config struct contains the token and chat ID from env vars

**Given** a config file at `~/.config/claude-telegram-mirror/config.json` with `botToken` and `chatId`
**When** env vars are NOT set and `load_config(false)` is called
**Then** values are loaded from the config file

**Given** neither env vars nor config file
**When** `load_config(false)` is called
**Then** all fields use defaults (verbose=true, approvals=true, useThreads=true, chunkSize=4000, rateLimit=1, sessionTimeout=30, staleSessionTimeoutHours=72, autoDeleteTopics=true, topicDeleteDelayMinutes=1440)

**Given** all 13 environment variables are set
**When** config is loaded
**Then** every field matches the env var value with correct type parsing

**Given** a socket path containing `..` in the env var
**When** config is loaded
**Then** the path is rejected and falls back to the default socket path

**And** unit tests cover all 13 env vars, config file parsing, priority order, validation, and edge cases

### Story 1.3: Input Injector in Rust

As a hook binary or daemon,
I want to inject text into tmux sessions safely,
So that Telegram messages reach Claude Code without shell injection risk.

**Acceptance Criteria:**

**Given** a valid tmux target `session:window.pane`
**When** `inject(text)` is called
**Then** `Command::new("tmux").arg("send-keys").arg("-t").arg(target).arg("-l").arg(text)` executes followed by Enter

**Given** a slash command `/clear`
**When** `send_slash_command("/clear")` is called
**Then** the command is validated against whitelist `[a-zA-Z0-9_\- /]` and sent with `-l` flag

**Given** a slash command with unsafe characters
**When** `send_slash_command` is called
**Then** the command is rejected and a warning is logged

**Given** a special key `Ctrl-C`
**When** `send_key("Ctrl-C")` is called
**Then** `C-c` is sent to tmux via `Command::arg()`

**Given** the `$TMUX` env var is set
**When** `detect_tmux_session()` is called
**Then** it parses the socket path and queries tmux to build the target

**Given** `$TMUX` is NOT set
**When** `find_claude_code_session()` is called
**Then** it searches all tmux sessions for processes matching `claude` or `node`

**Given** a tmux target that doesn't exist
**When** `validate_target()` is called
**Then** it returns an error with an actionable message (BUG-001)

**And** unit tests cover whitelist validation, key mapping, and socket path validation

### Story 1.4: Hook Event Processing in Rust

As the `ctm hook` binary,
I want to read hook events from stdin and forward them to the daemon via socket,
So that Claude Code events reach Telegram.

**Acceptance Criteria:**

**Given** a `PostToolUse` JSON event on stdin
**When** `ctm hook` processes it
**Then** a `tool_result` NDJSON message is sent to the Unix socket and the process exits with code 0

**Given** a `Notification` JSON event with `notification_type !== "idle_prompt"`
**When** processed
**Then** an `agent_response` message is sent to the socket

**Given** a `UserPromptSubmit` event
**When** processed
**Then** a `user_input` message is sent with the prompt text

**Given** a `PreCompact` event
**When** processed
**Then** a `pre_compact` message is sent

**Given** stdin exceeds 1MB
**When** the hook reads input
**Then** it logs a warning and exits gracefully (code 0)

**Given** the bridge socket does not exist
**When** any hook event is processed
**Then** the hook exits silently with code 0

**And** all messages include tmux metadata (target, socket, hostname)

### Story 1.5: PreToolUse Approval Workflow in Rust

As a user who controls Claude from Telegram,
I want the hook binary to block on dangerous tool calls until I approve them,
So that I can prevent unwanted file writes or command execution.

**Acceptance Criteria:**

**Given** a `PreToolUse` event for tool `Write`
**When** the hook processes it
**Then** an `approval_request` message is sent via `send_and_wait()` with 300-second timeout

**Given** the daemon responds with `approve`
**When** the hook receives the response
**Then** it writes the allow decision JSON to stdout

**Given** the timeout expires (300 seconds)
**When** no response is received
**Then** it writes the ask/fallback decision JSON to stdout

**Given** `permission_mode === "bypassPermissions"` in the event
**When** the hook processes PreToolUse
**Then** it returns null (auto-approve)

**Given** a `Bash` tool with command starting with `ls`
**When** the safe command whitelist is checked
**Then** the tool is auto-approved without sending to Telegram

**And** the safe command whitelist matches TypeScript exactly: ls, pwd, cat, head, tail, echo, grep, find, which

### Story 1.6: Stop Event Transcript Extraction in Rust

As a user monitoring Claude from Telegram,
I want Stop events to extract Claude's latest response from the transcript,
So that I see what Claude said in Telegram.

**Acceptance Criteria:**

**Given** a `Stop` event with `transcript_path` pointing to a valid JSONL file
**When** the hook processes it
**Then** it reads from the last-known line position, extracts assistant text, sends agent_response + turn_complete, updates state file

**Given** the state file does not exist
**When** processing
**Then** it reads from the beginning of the transcript

**Given** no new lines since the last Stop
**When** processing
**Then** only `turn_complete` is sent

**Given** the transcript contains a `custom-title` record
**When** detected
**Then** a `session_rename` message is sent

### Story 1.7: Binary Distribution and Integration

As a user installing claude-telegram-mirror,
I want the Rust binary to be automatically available after npm install,
So that hooks use the fast native binary without manual setup.

**Acceptance Criteria:**

**Given** the npm package is installed on Linux x86_64
**When** `resolve-binary.js` is called
**Then** it finds `@agidreams/ctm-linux-x64/bin/ctm` in node_modules

**Given** an unsupported platform
**When** `resolve-binary.js` is called
**Then** it returns null and the TypeScript fallback is used

**Given** a `v*` tag is pushed
**When** GitHub Actions release workflow runs
**Then** binaries are built natively on each platform, stripped, and published to npm

**And** platform packages have correct `"os"`, `"cpu"`, `"files"` fields
**And** main package declares them as `optionalDependencies`

---

## Epic 2: Session + Socket + Formatting

Replace the data and communication layers with Rust.

### Story 2.1: SQLite Session Manager in Rust

As a bridge daemon,
I want a Rust session manager with identical schema and methods,
So that session state is managed atomically with proper file permissions.

**Acceptance Criteria:**

**Given** the config directory exists
**When** `SessionManager::new(config_dir)` is called
**Then** `sessions.db` is created with `0o600` permissions, identical schema, indexes, and migrations

**Given** all 24 methods
**When** called with identical inputs to TypeScript
**Then** they produce identical state and return values

**Given** `reactivate_session` on an ended session
**When** called
**Then** status = 'active', last_activity updated (BUG-009)

**Given** `end_session`
**When** called
**Then** pending approvals expired atomically

**And** unit tests cover full lifecycle, approvals, stale candidates, orphaned threads, reactivation, stats

### Story 2.2: Socket Server with flock(2) in Rust

As a bridge daemon,
I want a Unix domain socket server with atomic PID locking,
So that only one daemon runs and connections are properly managed.

**Acceptance Criteria:**

**Given** no daemon running
**When** `SocketServer::listen()` is called
**Then** PID lock acquired via `flock(2)`, socket created with `umask(0o177)`

**Given** another daemon holds the flock
**When** second daemon tries to start
**Then** `EWOULDBLOCK` returned, clear error message

**Given** stale socket file
**When** server starts
**Then** stale socket removed and new one created

**And** connection limit (64), line limit (1MB), NDJSON wire compatibility with TypeScript, Drop cleanup

### Story 2.3: Socket Client with sendAndWait in Rust

As a hook binary needing approval responses,
I want a socket client that can send and wait for correlated responses,
So that the PreToolUse approval workflow works.

**Acceptance Criteria:**

**Given** `send_and_wait(message, timeout)` called
**When** a matching response arrives
**Then** it is returned to the caller

**Given** timeout expires
**When** no response arrives
**Then** timeout error returned

**And** NDJSON wire compatibility with TypeScript, non-matching messages ignored

### Story 2.4: Message Formatting in Rust

As a Telegram message sender,
I want properly formatted messages with MarkdownV2 escaping,
So that messages render correctly without parse errors.

**Acceptance Criteria:**

**Given** text with MarkdownV2 special characters
**When** `escape_markdownv2` is called
**Then** all special chars outside code blocks are escaped (NOT a no-op)

**Given** each supported tool
**When** `format_tool_details` is called
**Then** output matches TypeScript formatter's structure

**And** unit tests cover all tools, nested code blocks, adversarial input

### Story 2.5: Message Chunker in Rust (UTF-8 Safe)

As a message sender,
I want code-block-aware chunking that never breaks UTF-8,
So that long messages split cleanly.

**Acceptance Criteria:**

**Given** a message with code blocks spanning chunk boundary
**When** chunked
**Then** code block stays intact in one chunk

**Given** multi-byte UTF-8 characters at split point
**When** chunked
**Then** no character split mid-byte

**And** split priority: double newline > single newline > period+space > space > char boundary
**And** part headers on multi-chunk messages

### Story 2.6: Logger and Tool Summarizer in Rust

As a Rust binary,
I want structured logging and human-readable tool summaries,
So that diagnostics work and notifications are useful.

**Acceptance Criteria:**

**Given** tracing initialized
**When** logging occurs
**Then** all output goes to stderr, level controlled by RUST_LOG

**Given** the tool summarizer
**When** called for any of the 30+ patterns
**Then** output matches TypeScript summarizer

**And** unit tests port all 91 test cases from TypeScript

---

## Epic 3: Daemon + Bot + Full CLI

Replace core orchestration, Telegram integration, and CLI with Rust.

### Story 3.1: Bridge Daemon Core in Rust

As the central orchestrator,
I want all 12 message type handlers with every BUG fix preserved,
So that events flow correctly between hooks and Telegram.

**Acceptance Criteria:**

**Given** BUG-002 (topic creation race)
**When** two messages arrive simultaneously for a new session
**Then** only ONE topic created (tokio::sync::Mutex)

**Given** BUG-003 (stale sessions)
**When** cleanup runs
**Then** 1h without tmux, 24h+ with tmux only if pane dead

**Given** BUG-009, BUG-011, BUG-012
**When** their specific scenarios occur
**Then** each behaves identically to TypeScript

**And** all 12 message types handled, echo prevention with 10s TTL, topic lifecycle, cc command transformer

### Story 3.2: Telegram Bot with Rate Limiting in Rust

As the Telegram communication layer,
I want token-bucket rate limiting AND retry/backoff,
So that messages are delivered reliably.

**Acceptance Criteria:**

**Given** burst of 30 messages
**When** sent
**Then** governor limits to 25/sec, excess queued

**Given** transient API error
**When** send fails
**Then** retries up to 3x with exponential backoff

**Given** TOPIC_CLOSED error
**When** sending
**Then** topic reopened and message retried

**Given** entity parse error
**When** sending MarkdownV2
**Then** falls back to plain text

**And** security middleware on ALL update types, token scrubbing, file download, forum topic CRUD

### Story 3.3: Bot Commands in Rust

As a Telegram user,
I want all 10 commands and inline keyboards working,
So that I can fully control Claude from my phone.

**Acceptance Criteria:**

**Given** each of /start, /help, /status, /sessions, /attach, /detach, /mute, /unmute, /abort, /ping
**When** sent
**Then** behavior matches TypeScript

**And** approval keyboard, tool details callback (5-min), abort confirmation, AskUserQuestion rendering, /rename injection

### Story 3.4: CLI Commands in Rust

As a terminal user,
I want all CLI commands in the Rust binary,
So that `ctm` is a single command for everything.

**Acceptance Criteria:**

**Given** start, stop (--force), restart, status, config (--show, --test)
**When** executed
**Then** each works identically to TypeScript

**Given** install-hooks, uninstall-hooks, hooks, setup, doctor, service commands
**When** executed in Phase 3
**Then** they delegate to TypeScript equivalents (working delegation, not stubs) until Phase 4 replaces them

### Story 3.5: Phase 3 Integration Testing and Verification

As a developer completing Phase 3,
I want comprehensive end-to-end verification,
So that every BUG fix and feature works.

**Acceptance Criteria:**

**Given** full Rust binary
**When** end-to-end tested
**Then** hook → daemon → Telegram → user → tmux → Claude works

**Given** all BUG-001 through BUG-012
**When** tested
**Then** all pass

**And** cargo clippy clean, cargo fmt clean, RSS <10MB idle, binary <20MB

---

## Epic 4: Services + Setup + Doctor + Installer

Replace operational tooling. Complete single-binary experience.

### Story 4.1: Service Manager in Rust

As a user wanting auto-start on boot,
I want systemd and launchd service management,
So that the daemon starts automatically.

**Acceptance Criteria:**

**Given** `ctm service install` on Linux
**When** executed
**Then** systemd unit file written with Restart=on-failure, EnvironmentFile, daemon-reload

**Given** `ctm service install` on macOS
**When** executed
**Then** launchd plist written with KeepAlive, ThrottleInterval, correct PATH

**And** start, stop, restart, status, uninstall on both platforms
**And** env file parsing (export stripping, quotes, comments)

### Story 4.2: Interactive Setup Wizard in Rust

As a new user,
I want an interactive setup experience,
So that I can configure everything without reading docs.

**Acceptance Criteria:**

**Given** `ctm setup`
**When** executed
**Then** interactive wizard with 8 steps: token (live validation), privacy mode, supergroup (auto-detect), permissions (test message), config options, save (config.json + .telegram-env), hooks (optional), service (optional)

**And** uses dialoguer or inquire crate for terminal prompts

### Story 4.3: Doctor with --fix in Rust

As a user troubleshooting,
I want comprehensive diagnostics with auto-repair,
So that common problems fix themselves.

**Acceptance Criteria:**

**Given** `ctm doctor`
**When** executed
**Then** 9 checks: binary, config dir, env vars, hooks, socket, tmux, service, Telegram API, database

**Given** `ctm doctor --fix`
**When** fixable issues found
**Then** permissions corrected, stale files removed, dirs created

**And** suggestions (not auto-fix) for hooks and services

### Story 4.4: Hook Installer in Rust

As a user setting up hooks,
I want programmatic installation preserving my settings,
So that hooks are configured correctly.

**Acceptance Criteria:**

**Given** `ctm install-hooks`
**When** no existing CTM hooks
**Then** all 6 types added, non-CTM hooks preserved

**Given** run twice
**When** hooks match
**Then** reports "unchanged" (idempotent)

**Given** `ctm install-hooks -p`
**When** in project dir
**Then** writes to `.claude/settings.json`

**Given** `ctm uninstall-hooks`
**When** executed
**Then** only CTM hooks removed

**And** `ctm hooks` shows status of each hook type
