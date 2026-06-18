# Changelog

All notable changes to this project will be documented in this file.

## [0.2.25] - 2026-06-18

### Fixed (macOS `ctm start` reported a false failure when launchd auto-recovered)
- **`ctm start` no longer cries "Service was started but is not running — it exited immediately" when the daemon actually comes up a few seconds later.** The 0.2.24 liveness check (added so a launch that gets SIGKILLed isn't falsely reported as "started") only waited ~2.6s. But when macOS kills a non-notarized binary at first launch, the service's `KeepAlive { Crashed: true }` policy relaunches it only after the 10s throttle interval — so the check saw no process in its short window, reported a code-signing failure, and *then* launchd quietly brought the daemon up (a subsequent `ctm status` showed it Running). The error was accurate for that instant but misleading. The check now polls for a **stable PID** for up to 14s (longer than the throttle window): a healthy start still returns in ~1s, a first-launch kill is ridden out across launchd's relaunch and reported as success once the daemon is actually up, and only a service that never stabilises is reported as a failure (with a note that launchd keeps retrying in the background). A one-line "waiting…" message is shown so a multi-second wait isn't silent.

## [0.2.24] - 2026-06-18

### Fixed (macOS service: false "running" + silent code-signing kills)
- **`ctm status` / `ctm doctor` / `ctm service status` no longer report a dead daemon as "Running", and `ctm start` can recover it.** `get_launchd_status()` decided "running" by checking whether the service label merely *appeared* in `launchctl list` — but a loaded-but-dead launchd job still appears there with `-` in the PID column. So a daemon that had cleanly stopped (a clean exit is not auto-restarted by the plist's `KeepAlive`) or been killed was reported as running, and `ctm start` short-circuited to **"Daemon is already running"** — a no-op that made recovery impossible. The status now parses the PID column (via a pure, unit-tested `parse_launchd_pid()`) and is "running" only when a real positive PID is present. (systemd's `is-active` check was already correct — this was launchd-only.)
- **A service that is SIGKILLed at launch is now reported as a failure instead of a false "Service started."** On macOS the kernel can kill a non-notarized binary milliseconds after `exec` for a code-signing / launch-constraint violation (`EXC_CRASH` / "Code Signature Invalid"). `launchctl start` only confirms launchd *accepted* the request, so the old code reported success for an already-dead daemon. `start_launchd_service()` now verifies the process actually came up and stayed up (~2s), and on failure prints actionable guidance (crash report, `codesign -dvvv`, quarantine removal, `ctm doctor`).

### Changed (macOS binary robustness — defense-in-depth for code signing)
- **postinstall** now hardens the native binary on macOS: strips any `com.apple.quarantine` flag, re-applies an ad-hoc signature if the existing one is invalid, then verifies and smoke-tests `ctm --version` — warning loudly (without failing the install) if the OS would refuse to run it.
- **release CI** now Developer-ID-signs and notarizes the darwin binaries (both arm64 and x64) with a `codesign --verify --strict` gate. Gated on `APPLE_*` secrets; until those are configured, builds fall back to ad-hoc signing with a warning. (Note: shipped binaries were previously only ad-hoc/linker-signed; npm provenance is a supply-chain attestation, not Apple notarization.)

## [0.2.23] - 2026-06-17

### Fixed (ROUTING-001 — cross-session misrouting of Telegram→CLI input)
- **A reply typed in one session's topic could be injected into another session's tmux pane** (e.g. text meant for session-2 landed in pane 0 / session-1). Two independent defects, both confirmed by manual real-tmux reproduction spikes and an adversarial Codex review:
  - **TOCTOU race on a shared, stateful injector.** The single process-wide `InputInjector` held a mutable `tmux_target`. The Telegram text handler set the target under one lock, dropped it, `await`ed, then re-acquired a fresh lock to inject — reading whatever target was current. Because every Telegram update is processed on its own task, a concurrent handler for another session could overwrite the shared target in that window.
  - **`get_tmux_target` fallback poisoning.** When a session's pane was missing from cache and DB, it guessed the *first* tmux pane running `claude` as `"{session}:0.0"` (pane 0) and persisted that guess — permanently binding the session to the wrong pane.
- **Fix:** `InputInjector` is now **stateless** — every action (`inject`, `inject_literal`, `send_key`, `send_slash_command`, `capture_pane`, `validate_target`) takes the `(target, socket)` pair explicitly, so the shared injector's `Mutex` serializes tmux execution only and can never bleed one session's pane onto another's. The live-detection fallback was removed: a missing per-session mapping now **fails closed** ("tmux not detected") instead of misrouting. The startup default target (which biased every session toward pane 0) was removed. Hardening: the topic→session lookup is now deterministic (`ORDER BY last_activity DESC`) when a parent and sub-agent share a topic, and a changed tmux socket is reconciled even when the pane string is unchanged. Covered by `tests/routing_spike.rs` (real-tmux: misroute reproduced pre-fix, zero crosstalk post-fix under concurrent stress).

### Fixed (STALE-TOPICS — liveness-driven topic reconciliation)
- **Dead sessions' Telegram topics no longer pile up.** Topic teardown previously relied on the `SessionEnd` hook (which does not fire on a terminal close, `kill -9`, `tmux kill-session`, or reboot) plus a 24h inactivity timer, so orphaned topics accumulated and a daemon restart never reconciled the backlog. A new reconciliation sweep (`daemon::reconcile`) makes pane/Claude liveness the prompt, authoritative signal, keyed on the *specific Claude session* (pane gone, pane reassigned to a newer session, or pane fell back to a shell prompt → topic pruned). The liveness policy (`liveness.rs`) is a pure, unit-tested rule shared by the daemon sweep and `ctm doctor --fix`, and a persistent topic ledger lets `doctor` reconcile topics created before it started. Also fixes a teardown leak.

### Added
- **`ctm prune-topics`** — clear an accumulated backlog of stale Telegram forum topics. `--ledger` deletes ledger-recorded topics whose Claude session is no longer alive; `--from`/`--to` (with `--ids` support) sweeps a numeric topic-id range to reach legacy orphans that predate the ledger (the Bot API cannot enumerate forum topics). Both modes refuse to touch the General topic and any currently-active session's topic, so a live conversation can never be pruned.

## [0.2.22] - 2026-06-01

### Fixed (ADR-015 — multi-question AskUserQuestion injection, N-generic)
- **Answering a multi-question AskUserQuestion from Telegram now works for any number of questions (1, 2, 3, … N).** The 0.2.20/0.2.21 path treated a multi-question widget as N separately-submitted single-question widgets, so a 2-question widget left Q2+ unanswered and leaked a stray keystroke into the prompt. Empirical capture of Claude Code 2.1.159 (binary string-mining + live `tmux capture-pane` at N=1/2/3) established the real model — **one tabbed widget** with a per-question advance row labelled `Next` (non-final) / `Submit` (final) and a single end-of-widget `Ready to submit your answers?` confirm screen — and `inject_answers` was rewritten to drive it: place the cursor on each row (verified by re-reading the pane), let single-select selections / free-text commits auto-advance, navigate multi-select to its `Next`/`Submit` row, then confirm once at the end. Works for single-select, multi-select, free-text (`Type something`), and any mix across N questions.
- **No more stray keystrokes / blind Enters.** Cursor placement is verified against the live pane and **fails closed** if it can't confirm the target (the old "best-effort Enter" that leaked keys is gone). Every screen transition is awaited (next question active / confirm screen) rather than slept through.
- **Free-text answers inject cleanly.** Free-text is typed literally into the `Type something` row via a new no-trailing-Enter injection path (the previous path appended an Enter that could submit prematurely) and is sanitized (control characters stripped, length-capped).
- **Safe partial-failure handling.** If injection fails after any keystroke has landed, ctm no longer blind-retries from question 1 (which would corrupt a half-advanced widget) — it marks the answers terminal and asks you to finish at the terminal, where the widget is still on screen. If nothing was delivered, it safely restores the Telegram buttons for a retry.
- **Submit-All validation & stale-button safety.** Answers are validated per-question before submitting: an unanswered question, an out-of-range option, or an empty multi-select is rejected with a clear prompt (previously a stale button — e.g. tapping an old question's message after a new question replaced it — could finalize a partial/empty submit as success). Tapping such a stale option button now shows "no longer available" instead of crashing the daemon.

## [0.2.21] - 2026-05-31

### Fixed (ADR-015 — multi-select AskUserQuestion injection)
- **Answering an AskUserQuestion from Telegram now submits cleanly.** Live validation on Claude Code 2.1.159 revealed the multi-select widget has **no separate "review your answers" screen** (submit is an inline "Submit" button) and has rows *below* Submit ("Type something", "Chat about this"). The 0.2.20 path used a fixed Down-count plus a second blind auto-submit Enter, which could overshoot or fire a stray post-submit keystroke (surfacing as a spurious "clarify"). Replaced with **capture-pane-driven navigation**: toggle the chosen options by digit key, then press Down until the cursor sits on "Submit", then a **single** Enter — bounded by the total option count so it never under- or over-shoots, and the redundant second Enter is gone.
- **Single-select answers from Telegram** now select by the option's digit key instead of injecting the label as literal text (which risked being treated as free-text by the widget).

## [0.2.20] - 2026-05-31

### Fixed (ADR-015 — restore AskUserQuestion to both surfaces)
- **AskUserQuestion renders in BOTH the CLI and Telegram again, answerable from either.** ADR-014 PR-E (0.2.18) delivered option answers via a blocking `PreToolUse` hook returning `updatedInput`, which suppressed Claude's native terminal widget — so questions appeared *only* in Telegram. This reverts that interception: Claude renders the question in the CLI as before, ctm mirrors it to Telegram from the fire-and-forget `tool_start`, answers inject back via tmux, and the Telegram buttons stale ("✅ Answered at terminal") when the question is answered at the keyboard (detected via the `PostToolUse` result). Restores ctm's bidirectional-mirror model.
- **Multi-select submit is race-free and faster.** The fixed ~3,500 ms of blind sleeps (1500 ms render wait + 2000 ms auto-submit) that could fire the confirming Enter *before* Claude's review screen rendered is replaced with adaptive `tmux capture-pane` readiness polling (150 ms steps, 3 s cap) — typically ~150–450 ms and never premature.
- **Concurrency-hardened question handlers.** A `QuestionLifecycle` state machine is the single arbiter across all answer paths; no daemon lock is held across Telegram I/O; `pending_q` removals are `Arc::ptr_eq` identity-checked; every keyboard-arming edit re-stales if the question resolved mid-edit. Closes the lock-order deadlock, render-window, lock-across-I/O, and orphaned-button classes surfaced by multi-pass adversarial (Codex) review.

### Changed
- The approval hook timeout is now its own constant (`DEFAULT_APPROVAL_WAIT_SECS`, 300s + 10s buffer); the `question_wait_secs` / `TELEGRAM_QUESTION_WAIT_SECS` knob is removed (the question hook no longer blocks).

## [0.2.19] - 2026-05-31

### Fixed (ADR-014 post-release field defects)
- **AskUserQuestion now reaches Telegram under `--dangerously-skip-permissions`** (D1, root cause) — the `PreToolUse` hook short-circuited on `bypassPermissions` *before* the AskUserQuestion branch, so in the operator's normal mode the question was never sent and Claude fell back to its terminal TUI. AskUserQuestion is now routed ahead of the bypass check (it's input collection, not a permission gate).
- **Question widgets render as plain text** (D2) — header/question/option/answer text is arbitrary model content; sending it under Telegram Markdown v1 (with backtick-only escaping) caused HTTP 400 "can't parse entities" that silently dropped the widget (including the "Submit All" message). All question-lifecycle messages now render as plain text via a shared renderer.
- **No more silent ~5-minute hangs** (D3/D4) — every render drop/error path (no topic, missing input, empty questions, render failure, supersede) now releases the blocked hook to the terminal instead of leaving it to time out; the misleading "retrying via queue" log (which never retried) is corrected.
- **Mirror-storm resilience** (D6) — `ToolStart` previews moved to a new Low queue priority so a tool-spam storm can no longer starve or evict substantive `ToolResult`s or approvals/questions; the per-drop log spam is replaced by a throttled 5-second aggregate.
- **`ctm doctor` warns about competing PreToolUse hooks** (D7) — Claude Code ignores hook `updatedInput` when multiple `PreToolUse` hooks are registered (anthropics/claude-code#15897), which would silently break structured AskUserQuestion answers.
- **Configurable AskUserQuestion wait** (D8) — `question_wait_secs` (env `TELEGRAM_QUESTION_WAIT_SECS` / config `questionWaitSecs`, default 300) replaces hardcoded timeouts; a pre-emptive "answer at the terminal" notice posts near expiry.
- **Resilient rate-limit handling** (D5) — Telegram 429s are now retried up to 3× (honoring `retry_after`) instead of once, so a rate-limit burst can't strand the direct question/summary sends.
- **Collision-safe answer routing** — an ambiguous 20-char session-id prefix in callback data is now refused (logged) rather than risking misdelivery to the wrong session.

## [0.2.18] - 2026-05-28

### Added (ADR-014)
- **Structured AskUserQuestion answers** — option/multi-select answers are now delivered to Claude Code via a blocking `PreToolUse` hook returning `updatedInput` (the same correlation the approval flow uses), replacing fragile 300ms-per-key tmux keystroke injection. ~6 orders of magnitude lower answer-delivery latency and no TUI readiness race. Free-text retains an isolated keystroke fallback.
- **Event-driven session teardown** — the `SessionEnd` hook is now registered (`session_exit_reason` parsed, `resume` special-cased so a suspending session is not torn down). True termination deletes the forum topic immediately and clears `thread_id` synchronously; custom titles persist across daemon restarts.
- **Approval reliability** — approval requests sent at Critical priority; double-taps are idempotent (no duplicate hook responses); the message is edited to a decision+time audit line with the keyboard removed; the approval→client map no longer leaks.
- **Setup trust acknowledgment** — the wizard now requires an explicit, recorded `y/N` acknowledgment of the chat-level trust model before writing config (new setups only); mirrored in the README.

### Fixed
- Removed fabricated `adaptive_retry` dead code (a Bot API field that does not exist); rate-limit backoff honors `retry_after` only.
- Multi-select answer labels are joined with a bare comma (Claude Code format), verified against the reference implementation.

See `docs/adr/ADR-014-lifecycle-hooks-approval-ux-and-input-reliability.md` for the full design, benchmark, and review log.

## [0.2.1] - 2026-03-17

### Changed
- Version bump from 0.2.0 due to partial npm publish (linux-x64 0.2.0 already on registry)

## [0.2.0] - 2026-03-17 (updated 2026-03-17)

### Release Readiness (post-initial)

**Security:**
- **Bounded NDJSON line reading** -- replaced `AsyncBufReadExt::read_line` (which accumulates without limit before the newline is found) with a new `read_bounded_line` helper that stops accumulating once `MAX_LINE_BYTES` are consumed, then drains to the next newline to keep the stream frame-aligned. Prevents a newline-free payload from exhausting memory before the size check fires.

**Type safety:**
- **`SessionStatus` and `ApprovalStatus` enums propagated to all call sites** -- `end_session` and `resolve_approval` now accept typed enum values instead of raw `&str`, eliminating the runtime string-validation step. `row_to_session` and `row_to_approval` parse DB strings into enums at deserialization time with a safe fallback for unknown values.

**Test coverage expanded to 512 tests:**
- **`bot_tests.rs`** -- new integration test file covering Telegram bot client: message sending, forum topic management, rate limiting, and callback query handling
- **`daemon_handlers.rs`** -- new integration test file covering socket and Telegram handler logic: session routing, approval flow, echo prevention, and cleanup sequences
- `concurrency.rs`, `config_validation.rs`, and `session_lifecycle.rs` expanded with additional cases

### Polish (ADR-009)

- **Eliminated process-global umask race** — socket permissions now set via `chmod` instead of `umask`, fixing thread-safety issue that caused intermittent test failures and could affect multi-threaded deployments
- **Socket path validation tightened to AF_UNIX limit** — 256 → 104 bytes to match actual kernel limit
- **Topic creation race condition fixed** — atomic check-and-insert prevents duplicate forum topics under concurrent session starts
- **Message queue bounded** — capped at 500 messages with oldest-eviction to prevent OOM under sustained send failures
- **Rate limiter clamped** — `[1, 30]` msgs/sec to stay within Telegram's API limits
- **Config parse logging** — invalid env var values now warn instead of silently falling back to defaults
- **Mirror status write errors logged** — previously silently discarded
- **Consistent char-count measurement** — `estimate_chunks`, `needs_chunking`, and `truncate` all use character count (not byte length)
- **Transcript state file cleanup** — `.last_line_*` files cleaned up on session end instead of accumulating indefinitely
- **Removed duplicate code** — consolidated `truncate_path` → `short_path`, removed duplicate test coverage
- **Retry backoff overflow-safe** — `saturating_mul` prevents integer overflow at high retry counts
- **Echo prevention key uses null separator** — `\0` instead of `:` eliminates the theoretical collision class between session IDs and text
- **Renamed `escape_markdown` to `escape_markdown_v1`** — clarifies this is Telegram Markdown v1 escaping (backticks only)

### Deep Audit Fixes (ADR-010)

**Security (Round 1 -- all resolved):**
- **S-1: Path traversal on `transcript_path` fixed** -- hook-supplied paths are now validated (absolute, canonicalized, safe-prefix check, no null bytes) before `fs::File::open()`
- **S-2: Approval response routing fixed** -- responses are routed to the specific socket client that submitted the request, not broadcast to all connected clients
- **S-3: `db_op` panic replaced with `Err`** -- `spawn_blocking` task cancellation during shutdown now returns an error instead of crashing the daemon
- **S-4: `Config` Debug redaction** -- custom `Debug` impl redacts `bot_token` to `"[REDACTED]"`, preventing token leakage through `{:?}` formatting

**Correctness (Round 1 -- all resolved):**
- **C-1: Echo key separator mismatch fixed** -- `add_echo_key` and `handle_user_input` now use the same `\0` separator (were using `\0` vs `:`)
- **C-2: RAII processing guard** -- `ProcessingGuard` drop guard prevents permanent queue stalls if an async task panics or is cancelled
- **C-3: TOPIC_CLOSED error return** -- failed reopen now returns `Err` immediately instead of falling through to unrelated retry logic
- **C-4: Atomic `end_session`** -- session status update and approval expiry wrapped in a single SQLite transaction
- **C-5: Session ID validation at persistence boundary** -- `is_valid_session_id()` called before all database writes
- **C-6: Status enum validation** -- `SessionStatus` and `ApprovalStatus` enums replace raw strings, preventing typo-induced data corruption

**Unicode / Formatting (Round 1 -- all resolved):**
- **U-1: Char-boundary-safe message chunking** -- all length checks use `.chars().count()`, split points use `char_indices()`, header size reserved before splitting
- **U-2: Char-safe truncation** -- topic name and filename truncation use `.chars().take(N)` instead of byte slicing

**Packaging (Round 1 -- all resolved):**
- **P-2: `prepublishOnly` guard** -- platform packages fail to publish if `bin/ctm` binary is missing
- **P-3: `setup-node` for npm provenance** -- `actions/setup-node@v4` added to release workflow for OIDC token injection

**Round 2 blockers identified (7 items):**
- **R2-B3: Rate limit default changed from 1 to 20** -- previous default caused extreme message delays under normal load
- **R2-B6: `flock()` advisory lock on PID file** -- prevents double-start race where two concurrent `ctm start` commands both create daemons
- **R2-B7: CI failure exit when platform packages unavailable** -- registry propagation loop now exits non-zero instead of silently publishing a broken main package

### Breaking Changes

- **TypeScript source removed** — the package now ships a pre-compiled native binary; there are no `.js` or `.ts` files to import
- **Node.js no longer required at runtime** — Node.js is used only during `npm install` to download the binary for the target platform; the daemon and hook binary run standalone
- **`telegram-hook` bin entry removed** — replaced by the unified `ctm hook` subcommand
- **Public library API removed** — `import { ... } from 'claude-telegram-mirror'` is no longer supported; the package is now a CLI/binary distribution only

### Added

- **Complete Rust rewrite** — 30 source files (14 top-level modules + 3 sub-module groups), 512 tests (unit + 10 integration test files), ~12,000 lines of Rust replacing the TypeScript implementation
- **Single static binary** — ~9 MB self-contained binary with sub-millisecond hook latency (<1 ms)
- **Tool summarizer** — 30+ regex patterns condense verbose tool output into compact Telegram messages
- **AskUserQuestion rendering** — inline keyboard buttons displayed in Telegram for interactive Claude prompts
- **Photo and document download** — files sent to a Telegram topic are downloaded and injected into the Claude session
- **Session rename via `/rename`** — renames both the Telegram forum topic and the active tmux window to keep labels in sync with Claude Code
- **`doctor --fix` auto-remediation** — detects and automatically corrects common configuration problems
- **Governor token-bucket rate limiter** — per-chat rate limiting with configurable burst and refill, including retry/backoff for Telegram API calls
- **`flock(2)` atomic PID locking** — eliminates the TOCTOU race present in the previous read-then-write PID-file scheme
- **Global regex-based token scrubbing** — bot tokens and other secrets are redacted from all log output before writing
- **SIGTERM signal handler** — daemon performs a clean shutdown (flushes queues, closes sockets) when it receives SIGTERM
- **`linux-arm64` platform support** — pre-built binary available for ARM64 Linux (e.g., Raspberry Pi, AWS Graviton)
- **Interactive setup wizard** — `ctm setup` uses `dialoguer` to guide first-time configuration without manual config editing
- **TypeScript detection in code blocks** — code blocks in Claude output are annotated with the detected language for syntax-highlighted display
- **Integration test suite** (ADR-008) — 10 test files covering CLI smoke tests, concurrency, config validation, formatting, hook pipeline, session lifecycle, socket roundtrip, summarizer, bot client, and daemon handlers
- **Binary integrity verification** (ADR-008) — `checksums.json` in the release workflow for verifiable artifact hashes
- **Structural decomposition** (ADR-008) — bot/, daemon/, and service/ modules split into focused sub-modules (e.g., `bot/client.rs`, `bot/queue.rs`, `daemon/event_loop.rs`, `daemon/cleanup.rs`, `service/systemd.rs`, `service/launchd.rs`, `service/env.rs`)

### Security

- **Shell injection eliminated** — all subprocess calls use `Command::arg` (no shell interpolation); `execSync` with user-controlled strings is gone
- **Bot token scrubbing** — token is redacted from logs and error messages at the point of emission
- **Session ID validation** — session identifiers are validated against `[a-zA-Z0-9._-]`, maximum 128 characters, before use in any file path or socket name
- **Socket path traversal prevention** — computed socket paths are checked to confirm they remain within the expected runtime directory
- **Config directory permissions enforced** — config directory is created with mode `0o700`; existing directories with wrong permissions are rejected
- **File permissions enforced** — config and PID files are created with mode `0o600`
- **NDJSON line size limits** — incoming NDJSON lines are capped at 1 MB to prevent memory exhaustion
- **Connection concurrency limits** — the Unix socket listener rejects connections beyond a limit of 64 concurrent clients
- **IDOR check on approval callbacks** — callback query payloads are validated to ensure the requesting Telegram user matches the session owner before approving a tool call
- **`chmod(0o600)` after socket bind** — socket file permissions set via post-bind `chmod` (ADR-009: replaced process-global `umask` which caused race conditions in multi-threaded contexts)

### Fixed

- **BUG-001: tmux target auto-refresh** — stale tmux pane targets are detected and refreshed automatically
- **BUG-002: Topic creation race prevention** — concurrent session-start events cannot create duplicate forum topics
- **BUG-003: Stale session cleanup with differentiated timeouts** — sessions without tmux info use a shorter inactivity timeout than sessions with a known-dead pane
- **BUG-004: Escape vs Ctrl-C distinction** — `/stop` sends Escape (pause Claude); `/kill` sends Ctrl-C (exit Claude)
- **BUG-005: Ignore General topic** — messages posted to the forum's General topic are silently dropped
- **BUG-006: Stateless hooks** — hooks contain no local state; the daemon is the single source of truth
- **BUG-009: Session reactivation** — sessions previously marked `ended` are reactivated when a new hook event arrives
- **BUG-010: On-the-fly session creation** — the daemon creates a forum topic on the first hook event for an unknown session without requiring a prior `session_start` signal
- **BUG-011: Echo prevention** — text injected from Telegram into tmux is not echoed back as a new Telegram message
- **BUG-012: Topic deletion cancellation** — deleting a forum topic from Telegram does not terminate the underlying Claude session

### Internal

- **10 Architecture Decision Records (ADRs)** documenting key design choices (binary distribution, rate limiting, PID locking, socket security, token scrubbing, session validation, migration gap audit, release readiness audit, broken windows elimination, deep release readiness evaluation)
- **SECURITY.md** with a full threat model covering all attack surfaces
- **CI pipeline updated to Rust-only** — `cargo check`, `clippy`, `fmt`, and `cargo test` replace the TypeScript build/lint/test steps
- **Release workflow** — GitHub Actions builds binaries for 4 platforms (`linux-x64`, `linux-arm64`, `darwin-x64`, `darwin-arm64`) and publishes scoped npm packages alongside the root package
- **Binary distribution via scoped npm packages** — platform-specific packages (e.g., `@claude-telegram-mirror/linux-x64`) are installed as optional dependencies; the root package selects the correct one at install time

## [0.1.20] - 2025-12-09

### Fixed
- **BUG-012: Project hook installs missing PreToolUse** - `ctm install-hooks -p` now installs PreToolUse and PostToolUse hooks
  - Root cause: Installer intentionally skipped these for project installs, assuming global hooks would handle them
  - Problem: Claude Code's project hooks override global hooks (they don't merge)
  - If a project has its own PreToolUse hooks (e.g., claude-flow), the global telegram hooks never run
  - Fix: Project installs now include all hook types, same as global installs
  - After upgrading, run `ctm install-hooks -p` in affected projects to add the missing hooks

## [0.1.19] - 2025-12-09

### Fixed
- **BUG-011: Missing hostname in topic names** - Forum topics now include hostname for all sessions
  - Root cause: Bash hook script (`telegram-hook.sh`) didn't include hostname in metadata
  - Node handler (`handler.ts`) included hostname but bash hook handled most events
  - Fix: `get_tmux_info()` in bash hook now includes hostname in returned JSON
  - New sessions will have hostname in topic name (e.g., "agidreams | project-name")
  - Existing sessions need to be closed and recreated to get hostname in topic name

## [0.1.18] - 2025-12-09

### Fixed
- **BUG-010: Topic creation on clean install** - Forum topics are now created correctly on fresh installations
  - Root cause: BUG-006 removed `session_start` emission from hooks, but daemon still waited for it to create topics
  - Fix: `ensureSessionExists()` now calls `handleSessionStart()` directly instead of waiting
  - Topics are created immediately when the first hook event arrives for a new session
  - Race condition safety preserved: Promise-based locking prevents duplicate topics when concurrent events arrive
  - Verified no regressions to BUG-001 through BUG-009 fixes

## [0.1.17] - 2025-12-09

### Fixed
- **BUG-009: Reactivate ended sessions on new hook events** - Sessions marked as 'ended' are now automatically reactivated when new hook events arrive
  - Fixes issue where Telegram → CLI input silently failed after session was incorrectly marked ended
  - Added `reactivateSession()` method to SessionManager
  - `ensureSessionExists()` now checks session status and reactivates if needed

## [0.1.16] - 2025-12-09

### Added
- **FEAT-001: CLI lifecycle commands** - New `ctm stop` and `ctm restart` commands
  - `ctm stop` - Gracefully stop the running daemon (sends SIGTERM, waits up to 5s)
  - `ctm stop --force` - Force kill if graceful shutdown fails
  - `ctm restart` - Stop and restart the daemon in one command
  - Commands auto-detect if running as OS service and delegate appropriately
  - Cleans up stale PID and socket files automatically

- **Enhanced `ctm status` command** - Now shows daemon running state
  - Shows PID when daemon is running directly
  - Shows "(via system service)" when running under systemd/launchd
  - Shows socket file status
  - Detects stale PID files

### Changed
- `isServiceInstalled()` function exported from service manager for CLI use
- README.md updated with complete CLI command documentation

## [0.1.15] - 2025-12-09

### Fixed
- **BUG-005: Ignore General topic messages** - Messages in the forum's General topic are now ignored
  - Only messages in specific forum topics (with threadId) are routed to Claude sessions
  - Daemon can still write to General topic (startup/shutdown notifications)
  - Prevents confusion when user accidentally posts in General instead of session topic

- **BUG-006: Remove file-based session tracking** - Daemon SQLite is now single source of truth
  - Removed `.session_active_*` file tracking from both bash hook and Node handler
  - Hooks are now stateless - they just forward events to daemon
  - Eliminates inconsistency between bash (kept tracking on Stop) and Node (cleared on Stop)
  - Daemon's `ensureSessionExists()` handles all session creation via SQLite

## [0.1.14] - 2025-12-09

### Fixed
- **BUG-003: Stale session cleanup** - Sessions with dead tmux panes are now automatically cleaned up
  - New `staleSessionTimeoutHours` config (default 72 hours, configurable via env or config file)
  - Cleanup only triggers when: `lastActivity > 72h` AND (pane gone OR pane reassigned to another session)
  - Sends "Session ended (terminal closed)" message before closing forum topic
  - Prevents stale "active" sessions from accumulating indefinitely

- **BUG-004: Stop command sends wrong key** - Fixed interrupt behavior for Claude Code
  - `sendKey` method now includes `-S socket` flag for correct tmux server targeting
  - **Interrupt commands** (`stop`, `cancel`, `abort`, `esc`, `escape`) now send **Escape** to pause Claude
  - **Kill commands** (`kill`, `exit`, `quit`, `ctrl+c`, `ctrl-c`, `^c`) send **Ctrl-C** to exit Claude entirely
  - All commands work with or without leading `/` (e.g., `stop` or `/stop`)

### Added
- `TELEGRAM_STALE_SESSION_TIMEOUT_HOURS` environment variable for configuring stale session cleanup
- New kill command category for exiting Claude entirely (vs just interrupting)

## [0.1.13] - 2025-12-08

### Fixed
- **BUG-002: Race condition in topic creation** - Messages no longer leak to General topic when events arrive out-of-order
  - Added promise-based topic lock with 5-second timeout
  - All handlers now await topic creation before sending messages
  - Explicit failure (error log + drop message) on timeout instead of silent misdirection

- **Closed topic auto-reopen** - Bot automatically reopens topics closed by user in Telegram
  - Detects `TOPIC_CLOSED` error and calls `reopenForumTopic()`
  - Sends "Topic reopened" notification after recovery
  - Retries original message after successful reopen

- **PreToolUse regression: Missing tool details** - Restored detailed tool call information in Telegram
  - PreToolUse now runs BOTH bash script (tool details) AND Node.js handler (approvals) in parallel
  - Safe tools (ls, cat, pwd, etc.) now appear in Telegram - they were silently skipped before
  - Rich expandable context restored for all tool invocations

### Changed
- **Smart hook installer** - Auto-fixes configuration without `--force` flag
  - Compares existing CTM hooks with expected configuration
  - Only updates hooks that need changes, preserves user's other hooks
  - Reports what changed: `added`, `updated`, or `unchanged`
  - Removed `--force` option (no longer needed)

## [0.1.11] - 2025-12-08

### Fixed
- **Respect bypass permissions mode** - Skip Telegram approval prompts when Claude Code is in `bypassPermissions` mode
- Deployed with bypass fix included (0.1.10 was missing the fix)

## [0.1.9] - 2025-12-08

### Fixed
- **Critical: Telegram approval buttons now work correctly**
  - Fixed hook event type mismatch: Claude Code sends `hook_event_name` but handler was checking `type`
  - PreToolUse hooks now properly send `approval_request` messages to daemon
  - Approval buttons (Approve/Reject/Abort) now appear in Telegram for dangerous operations

- **Fixed message update after approval**
  - Changed to plain text mode to avoid Markdown parsing conflicts
  - Message now correctly updates to show decision after clicking approval button

### Changed
- Updated `types.ts` to use `hook_event_name` instead of `type` to match Claude Code's actual JSON format
- Added fallback timestamps for hook events where timestamp is optional
- Added additional Claude Code fields to hook types: `transcript_path`, `cwd`, `permission_mode`

## [0.1.8] - 2025-12-07

### Added
- Initial release with Telegram approval buttons feature
- Bidirectional Claude Code ↔ Telegram integration
- Session mirroring with forum topics
- Tool execution notifications
- Input injection from Telegram to CLI
