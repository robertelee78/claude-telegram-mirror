# ADR-010: Deep Release Readiness Evaluation — 8-Agent Swarm Audit

> **DO NOT BE LAZY. We have plenty of time to do it right.**
> No shortcuts. Never make assumptions.
> Always dive deep and ensure you know the problem you're solving.
> Make use of search as needed.
> Measure 3x, cut once.
> No fallback. No stub (todo later) code.
> Just pure excellence, done the right way the entire time.
> Chesterton's fence: always understand the current implementation fully before changing it.

**Status:** Accepted — Updated with Round 2 findings
**Date:** 2026-03-17 (Round 1), 2026-03-17 (Round 2)
**Authors:** Robert E. Lee
**Supersedes:** None
**Related:** ADR-008 (v0.2.0 Release Readiness Audit), ADR-009 (Broken Windows Elimination)

---

## Context

After completing the ADR-008 audit (33 findings, all resolved) and ADR-009 polish pass
(19 broken windows, all fixed), an independent deep evaluation was conducted using an
8-agent parallel swarm. Each agent read every line of its assigned modules — no sampling,
no assumptions. The goal: verify that when a user runs `npm install claude-telegram-mirror`
and types `ctm start`, the system actually works end-to-end.

### Codebase State at Round 1 Audit Start

| Metric | Value |
|--------|-------|
| Rust source files | 24 (across 16 modules) |
| Lines of Rust | 14,097 |
| Integration test files | 8 |
| Total tests | 286 |
| Test result | **286/286 passing** |
| Clippy | Clean (zero warnings) |
| Build (debug + release) | Clean |
| Binary | `ctm 0.2.0` — all CLI subcommands respond |

### Round 1 Audit (initial discovery)

| # | Agent | Model | Scope | Duration |
|---|-------|-------|-------|----------|
| 1 | Queen Coordinator | Opus | Architecture, cross-cutting concerns, migration completeness, dead code | 161s |
| 2 | Core Reviewer | Sonnet | config.rs, types.rs, error.rs, session.rs, socket.rs, lib.rs | 205s |
| 3 | Bot & Daemon Reviewer | Sonnet | bot/*, daemon/* (~5,000 lines — largest modules) | 175s |
| 4 | CLI & Service Reviewer | Sonnet | main.rs, setup.rs, doctor.rs, installer.rs, hook.rs, injector.rs, service/* | 213s |
| 5 | Formatting Reviewer | Sonnet | formatting.rs, summarize.rs, colors.rs | 118s |
| 6 | Security Auditor | Sonnet | Full codebase security sweep (all 24 .rs files) | 200s |
| 7 | Packaging Reviewer | Sonnet | Cargo.toml, package.json, npm scripts, CI workflow, platform packages | 232s |
| 8 | Test Coverage Analyst | Sonnet | All 8 test files vs all 24 source modules | 200s |

### Round 1 Results → Fixes Applied

Round 1 found 13 blockers (S-1 through S-4, C-1 through C-6, U-1, U-2, P-2, P-3).
All 13 were fixed in commits `9bd80e5`, `18b1677`, `f5a5cf6`, `4e43e40`.

### Codebase State at Round 2 Audit Start

| Metric | Value |
|--------|-------|
| Rust source files | 24 (across 16 modules) |
| Lines of Rust | 14,400 |
| Integration test files | 8 |
| Total tests | 387 (101 unit x2 lib+bin, 7 CLI, 7 concurrency, 12 config, 30 formatting, 13 hook, 18 session, 5 socket, 87 summarize, 6 doc-tests) |
| Test result | **387/387 passing** |
| Clippy | **1 error** (`hook.rs:606` question-mark lint) |
| Fmt | **5 files diverged** (cosmetic only) |
| Build (release) | Clean — zero warnings |
| Binary | `ctm 0.2.0` |

### Round 2 Audit (post-fix verification + deep dive)

| # | Agent | Model | Scope | Duration |
|---|-------|-------|-------|----------|
| 2 | Build & Test | Sonnet | cargo build/test/clippy/fmt + dead code scan | 163s |
| 3 | Core Module Reviewer | Sonnet | bot/, daemon/, config, types, error, socket | 196s |
| 4 | CLI & Commands Reviewer | Sonnet | main.rs, hook, installer, setup, doctor, service/ | 172s |
| 5 | Formatting & Session Reviewer | Sonnet | formatting.rs, summarize.rs, colors.rs, session.rs + test gaps | 149s |
| 6 | Security Auditor | Sonnet | Full codebase security sweep | 196s |
| 7 | NPM Distribution Reviewer | Sonnet | package.json, wrapper, resolve-binary, postinstall, npm-packages | pending |
| 8 | CI/CD & Release Reviewer | Sonnet | .github/workflows/, bump-version.sh, version alignment | 154s |

### Round 2 Overall Verdict

| Category | R1 Score | R2 Score | Notes |
|----------|----------|----------|-------|
| Compilation | 10/10 | 10/10 | Clean release build |
| Unit Tests | 9/10 | 9/10 | 387/387 pass; 1 clippy + 5 fmt nits block CI gate |
| **Behavioral Test Coverage** | **3/10** | **3/10** | **Entire daemon, hook pipeline, bot, service layers still untested** |
| Code Quality | 7/10 | 8/10 | Original 13 blockers fixed; 7 new blockers found |
| Security | 6/10 | 8/10 | S-1 through S-4 fixed; 0 critical, 3 medium remain |
| Unicode Handling | 4/10 | 8/10 | U-1/U-2 fixed; residual ANSI stripper gap |
| npm Distribution | 5/10 | 6/10 | P-2/P-3 fixed; new CI and version management gaps |
| Migration Completeness | 10/10 | 10/10 | Zero TS remnants |

**Release readiness verdict: NOT READY — 7 new blockers, 0 security criticals, ~30 new warnings discovered.**
**Progress: Round 1 blockers fully resolved. Round 2 findings are less severe but still need fixes.**

---

## Decision

Resolve all blockers before cutting v0.2.0. Organized into fix phases by dependency order.

---

# ROUND 1 FINDINGS (all resolved)

## Phase 1 — Security (RESOLVED)

### S-1: Path Traversal via `transcript_path` — Arbitrary File Read

- **Files:** `hook.rs:562`, `socket_handlers.rs:575`, `daemon/mod.rs:596`
- **Severity:** CRITICAL
- **Category:** Security — path traversal
- **Found by:** Agent #6 (Security Auditor)

**Problem:** The `transcript_path` field arrives from Claude Code hook stdin as
attacker-supplied JSON. It is passed directly to `fs::File::open()` with only an
existence check. No validation that the path is absolute, under a permitted directory,
or free of traversal sequences.

```rust
// hook.rs:562-574
let path = Path::new(transcript_path);
if !path.exists() { return None; }
let file = fs::File::open(path).ok()?;
```

A crafted hook payload with `"transcript_path": "/etc/passwd"` causes the daemon to
read arbitrary files. The JSONL parser silently skips non-JSON lines but returns any
line that happens to be valid JSON.

**Fix:** Validate `transcript_path` before use:
1. Require the path to be absolute
2. Resolve with `Path::canonicalize()`
3. Assert it starts with a known safe prefix (`~/.claude/` or the project directory)
4. Reject paths containing null bytes

**Effort:** 30 minutes. Apply in all three locations.

---

### S-2: Approval Workflow Forgery — Any Socket Client Can Bypass Telegram

- **Files:** `hook.rs:679-734`, `daemon/mod.rs:742-756`
- **Severity:** CRITICAL
- **Category:** Security — authentication bypass
- **Found by:** Agent #6 (Security Auditor)

**Problem:** `send_and_wait()` accepts the first `ApprovalResponse` whose `session_id`
matches. The Unix socket server broadcasts responses to **all** connected clients. Any
process connected to the same socket can inject an `ApprovalResponse` with
`content: "approve"`, and the hook will accept it — bypassing Telegram approval entirely.

The socket is mode 0o600 (same-UID access only), so the attacker must be running as the
same user. This limits the blast radius but does not eliminate it: any Claude Code hook
process running concurrently can forge approvals for other sessions.

**Fix:** Include a server-signed nonce (UUID) in the approval request that must be echoed
in the response. Alternatively, route approval responses only to the specific client that
submitted the request (use the client ID from the socket server's client map instead of
broadcasting).

**Effort:** 1-2 hours. Requires protocol change.

---

### S-3: `db_op` Panics on Task Cancellation — Daemon Crash

- **File:** `daemon/mod.rs:418`
- **Severity:** CRITICAL
- **Category:** Security — denial of service
- **Found by:** Agent #6 (Security Auditor), corroborated by Agent #3

**Problem:** Any Tokio shutdown or task cancellation during a `spawn_blocking` call
causes an unrecoverable `panic!`, terminating the entire daemon:

```rust
.unwrap_or_else(|e| {
    panic!("db_op: spawn_blocking task was cancelled or panicked: {e}");
})
```

A flurry of approvals or session starts during daemon shutdown can race this condition.
Panics in async Rust propagate in ways that may corrupt in-flight state.

**Fix:** Return `Err(AppError::...)` instead of panicking. Callers already handle
`Result` types. Add `R: Default` bound or use `Option<R>` for the shutdown case.

**Effort:** 30 minutes.

---

### S-4: `bot_token` Exposed via `#[derive(Debug)]` on Config

- **File:** `config.rs:68`
- **Severity:** WARNING (promoted due to defense-in-depth failure)
- **Category:** Security — information disclosure
- **Found by:** Agent #2 (Core Reviewer)

**Problem:** `Config` derives `Debug`. Any `tracing` event at `DEBUG` level that formats
`{:?}` on a `Config` instance will print the raw bot token, bypassing `ScrubWriter`
(which only scrubs the `/bot<token>/` URL pattern).

**Fix:** Implement `Debug` manually for `Config`, redacting `bot_token` to `"[REDACTED]"`.

**Effort:** 15 minutes.

---

## Phase 2 — Correctness (RESOLVED)

### C-1: Echo Prevention Broken — Key Format Mismatch

- **Files:** `daemon/mod.rs:869` vs `daemon/socket_handlers.rs:406`
- **Severity:** BLOCKER
- **Category:** Correctness — logic bug
- **Found by:** Agent #1 (Queen), corroborated by Agent #3

**Problem:** `add_echo_key()` stores keys as `"{session_id}\0{text}"` (NUL separator).
`handle_user_input()` checks for `"{session_id}:{text}"` (colon separator). These keys
never match, so echo prevention is completely broken — every Telegram-originated message
is echoed back to the Telegram chat when it arrives as a `user_input` socket message.

**Fix:** Unify separator to `\0` in both locations. Extract a `fn echo_key(session_id, text) -> String` helper to prevent future divergence.

**Effort:** 15 minutes.

---

### C-2: Queue Permanently Stalls on Panic — No RAII Guard

- **File:** `bot/queue.rs:28-74`
- **Severity:** BLOCKER
- **Category:** Correctness — resource leak
- **Found by:** Agent #3 (Bot & Daemon Reviewer)

**Problem:** `process_queue` sets `*processing = true` on entry and resets to `false`
at line 73. If any `await` inside the loop panics or is cancelled, the flag stays `true`
permanently. All subsequent `enqueue` calls see `*processing == true` and return
immediately. The queue is permanently stalled for the remainder of the daemon lifetime.

**Fix:** Add an RAII drop guard:
```rust
struct ProcessingGuard(Arc<Mutex<bool>>);
impl Drop for ProcessingGuard {
    fn drop(&mut self) {
        if let Ok(mut g) = self.0.try_lock() { *g = false; }
    }
}
```

**Effort:** 20 minutes.

---

### C-3: TOPIC_CLOSED Reopen Drops Messages Silently

- **File:** `bot/queue.rs:109-136`
- **Severity:** BLOCKER
- **Category:** Correctness — silent data loss
- **Found by:** Agent #3 (Bot & Daemon Reviewer)

**Problem:** When `TOPIC_CLOSED` is detected and `reopen_forum_topic` returns
`Ok(false)` (HTTP success but Telegram reports failure), the code logs
`"Failed to reopen topic"` and falls through to check for entity parse errors against
the original TOPIC_CLOSED description — which never matches. The message is silently
retried three more times with an unrecoverable error, then dropped.

**Fix:** Return `Err(AppError::Telegram(...))` immediately after the failed reopen
attempt. Do not fall through to the parse-error retry logic.

**Effort:** 15 minutes.

---

### C-4: `end_session` Not Atomic — Two SQL Statements Without Transaction

- **File:** `session.rs:380-399`
- **Severity:** BLOCKER
- **Category:** Correctness — data integrity
- **Found by:** Agent #2 (Core Reviewer)

**Problem:** `end_session` executes two separate `execute()` calls: one to set session
status, one to expire pending approvals. No transaction wraps them. A crash between the
two leaves the session marked "ended" but approvals still in "pending" status — an
inconsistency that can only be repaired manually.

**Fix:** Wrap both statements in `conn.transaction(|tx| { ... })`.

**Effort:** 15 minutes.

---

### C-5: Session ID Not Validated Before Database Write

- **File:** `session.rs:206-244`
- **Severity:** BLOCKER
- **Category:** Correctness — input validation
- **Found by:** Agent #2 (Core Reviewer)

**Problem:** `create_session` accepts a raw `session_id: &str` and writes it directly to
SQLite without calling `is_valid_session_id()`. The validator exists in `types.rs` and is
exported, but is never invoked at the persistence boundary. Empty strings, 10,000-character
strings, or strings with control characters will be silently persisted.

Same gap exists in `create_approval`, `set_session_thread`, `end_session`, and
`resolve_approval`.

**Fix:** Call `is_valid_session_id()` at the top of `create_session` and `create_approval`.
Return `Err(AppError::Validation(...))` on failure.

**Effort:** 20 minutes.

---

### C-6: Arbitrary Status Strings Accepted Into Database

- **File:** `session.rs:380`, `session.rs:539`
- **Severity:** BLOCKER
- **Category:** Correctness — domain constraint violation
- **Found by:** Agent #2 (Core Reviewer)

**Problem:** `end_session` and `resolve_approval` accept `status: &str` as caller-supplied
values written directly to the database. Valid values (`"active"`, `"ended"`, `"aborted"`,
`"pending"`, `"approved"`, `"denied"`, `"expired"`) are an implicit convention with no
enforcement. A typo like `"ENDED"` silently corrupts every query that filters by
`status = 'active'`.

**Fix:** Define `SessionStatus` and `ApprovalStatus` enums. Change function signatures to
accept the enum. Implement `AsRef<str>` for SQL binding.

**Effort:** 30 minutes.

---

## Phase 3 — Unicode / Multibyte Safety (RESOLVED)

### U-1: Message Chunking Uses Byte Length — Panics on Non-ASCII

- **Files:** `formatting.rs:489`, `formatting.rs:504`, `formatting.rs:592-612`
- **Severity:** BLOCKER
- **Category:** Unicode — runtime panic
- **Found by:** Agent #5 (Formatting Reviewer)

**Problem:** Four related bugs in `chunk_message_with_options`:

1. **Guard uses `.len()` (bytes)** at lines 489 and 504 instead of `.chars().count()`.
   A string of 4,000 CJK characters (12,000 bytes) bypasses the chunking logic entirely,
   violating Telegram's 4,096-character limit.

2. **`find_best_split_point` slices at byte offsets** at lines 592-594. `&text[search_start..search_end]`
   will panic with `byte index X is not a char boundary` for any multibyte character
   straddling the offset. The fallback `return target` at line 612 passes a raw byte
   offset back to the caller, where `remaining[..split]` panics for the same reason.

3. **Part headers added after chunking** at lines 516-523. The header
   `"📄 *Part N/M*\n\n"` (~20 chars) is added to chunks already split at `max_length`,
   pushing them over the 4,096-char limit.

4. **`offset` tracking wrong after `trim_start()`** at lines 510-512. Trimmed bytes are
   discarded but `offset` only increments by `split`, causing code-block detection to
   drift further left with every chunk.

**Fix:**
- Replace `.len()` with `.chars().count()` in all guard conditions
- Use `char_indices()` for split point detection instead of byte slicing
- Reserve `header_size` characters per chunk before splitting
- Track trimmed byte count in the offset accumulator

**Effort:** 1-2 hours. This is the most complex fix — the chunking loop needs a rewrite
with char-boundary-aware arithmetic throughout.

---

### U-2: Topic Name and Filename Truncation at Byte Offsets

- **Files:** `socket_handlers.rs:646`, `daemon/files.rs:32`
- **Severity:** BLOCKER
- **Category:** Unicode — runtime panic
- **Found by:** Agent #3, Agent #5

**Problem:** Two locations truncate strings at byte offsets:

```rust
// socket_handlers.rs:646 — topic name
let new_name = &new_name[..std::cmp::min(128, new_name.len())];

// files.rs:32 — filename
safe.truncate(200);
```

Both will panic at runtime if the byte offset falls inside a multibyte UTF-8 character.
Telegram forum topic names with emoji or CJK characters will crash the daemon.

**Fix:** Replace with char-boundary-safe truncation:
```rust
let new_name: String = new_name.chars().take(128).collect();
```

**Effort:** 15 minutes for both locations.

---

## Phase 4 — Packaging & Distribution (PARTIALLY RESOLVED)

### P-1: Platform Packages Unpublished on npm

- **File:** `package.json` (optionalDependencies)
- **Severity:** BLOCKER
- **Category:** Packaging — non-functional install
- **Found by:** Agent #7 (Packaging Reviewer)

**Problem:** All four platform packages (`@agidreams/ctm-linux-x64`,
`@agidreams/ctm-linux-arm64`, `@agidreams/ctm-darwin-arm64`,
`@agidreams/ctm-darwin-x64`) return 404 on npm. The main package references them at
version `0.2.0` as optional dependencies. npm silently skips 404 optional deps.
`resolveBinary()` returns `null`. Every end user hits "No native ctm binary found."

**Fix:** Build and publish platform packages via CI before or simultaneously with the
main package.

**Effort:** CI pipeline work — covered by existing `release.yml` once P-3 is fixed.

---

### P-2: Platform Package `bin/` Directories Contain Only `.gitkeep`

- **File:** `npm-packages/*/bin/.gitkeep`
- **Severity:** BLOCKER
- **Category:** Packaging — empty payload
- **Found by:** Agent #7 (Packaging Reviewer)

**Problem:** The `bin/` directories are populated by CI during the release workflow. In
the working tree they contain only `.gitkeep`. A manual `npm publish` from the working
tree would ship empty packages. Must ensure CI is the only publishing path.

**Fix:** Add a pre-publish check in each platform package's `package.json`:
```json
"scripts": { "prepublishOnly": "test -f bin/ctm || (echo 'ERROR: bin/ctm not found' && exit 1)" }
```

**Effort:** 15 minutes.

---

### P-3: `release.yml` Missing `actions/setup-node` — Provenance Signing Fails

- **File:** `.github/workflows/release.yml` (publish job)
- **Severity:** BLOCKER
- **Category:** Packaging — CI broken
- **Found by:** Agent #7 (Packaging Reviewer)

**Problem:** The `publish` job uses `npm publish --provenance` and has
`permissions: id-token: write`, but there is no `actions/setup-node` step with
`registry-url: 'https://registry.npmjs.org'`. npm provenance via OIDC requires
setup-node to configure the registry and inject the auth token. Without this,
`--provenance` will error.

**Fix:** Add before the publish step:
```yaml
- uses: actions/setup-node@v4
  with:
    node-version: '20'
    registry-url: 'https://registry.npmjs.org'
```
Remove the manual `~/.npmrc` write.

**Effort:** 15 minutes.

---

## Phase 5 — Test Coverage (DEFERRED — post-release)

### T-1: Test Coverage Assessment

- **Found by:** Agent #8 (Test Coverage Analyst)
- **Category:** Quality — behavioral verification

The 286 passing tests are **misleadingly comprehensive**. They verify the data model,
type serialization, and pure helper functions. The entire runtime is untested:

| Layer | Coverage | Risk |
|-------|----------|------|
| Types, validators, serde | Well tested | Low |
| Formatting, summarize | Well tested (117 tests) | Low |
| Session DB operations | Mostly tested (18 tests) | Medium |
| Socket connect/disconnect | Partially tested | Medium |
| **Hook processing pipeline** | **Zero tests** | **Critical** |
| **Daemon event loop** | **Zero tests** | **Critical** |
| **All 16 socket handlers** | **Zero tests** | **Critical** |
| **All Telegram handlers** | **Zero tests** | **Critical** |
| **All callback handlers** | **Zero tests** | **Critical** |
| **Bot API operations** | **Zero tests** | **Critical** |
| **Approval workflow e2e** | **Zero tests** | **Critical** |
| **Service management** | **Zero tests** | **Critical** |
| **Setup wizard** | **Zero tests** | **Medium** |

**Specific critical untested functions:**

1. `process_hook()` — the entire hook stdin pipeline (main entry point)
2. `build_messages()` for Stop events — JSONL transcript I/O, summary extraction
3. `SocketClient::send_and_wait()` — approval workflow blocking call with 300s timeout
4. `Daemon::start()` / `stop()` — full lifecycle
5. All 16 functions in `daemon/socket_handlers.rs`
6. `scrub_bot_token()` — security-critical token redaction (no test at all)
7. `expire_old_approvals()` — cleanup of expired approvals
8. `inject()` with valid tmux target — the Telegram-to-Claude input path
9. `get_pending_approvals()` — approval display
10. `SocketServer::broadcast()` — test exists but only checks `client_count()`, never sends data

**Test quality issues:**
- `server_broadcast_reaches_all_clients` — named misleadingly; checks connection count, never sends a message
- `check_config_dir_creates_when_fix` — creates a path variable but never calls the function under test
- Concurrency tests use sequential `SessionManager` (which is `!Send`), not actual concurrent access

**Recommendation:** Before v0.3.0, add integration tests for the hook pipeline, daemon
socket handlers, and approval workflow using mock socket connections. These are the
highest-risk untested paths.

---

## Warnings — Full Inventory

### Security Warnings

| ID | Issue | File:Line |
|----|-------|-----------|
| SW-1 | Token scrubbing regex requires trailing `/` — misses non-URL appearances | `bot/mod.rs:39-40` |
| SW-2 | Telegram caption injected verbatim into tmux — prompt injection vector | `telegram_handlers.rs:248-252` |
| SW-3 | PID file TOCTOU in `ctm stop` — could signal wrong process on PID reuse | `main.rs:299-317` |
| SW-4 | `sh -c` in launchd status check — shell injection precedent | `service/launchd.rs:215-222` |
| SW-5 | No per-connection read timeout on socket — 64 slow-drip clients = full DoS | `socket.rs:282-327` |
| SW-6 | Bot token in plain `String` — not zeroed on drop | `bot/client.rs:7-8` |
| SW-7 | `starts_with` prefix match in `resolve_pending_key` — key confusion risk | `daemon/mod.rs:833-838` |
| SW-8 | `config.json` file permissions not explicitly set to 0o600 | `config.rs:227-228` |
| SW-9 | Full binary path embedded in settings.json — path disclosure | `installer.rs:64-70` |
| SW-10 | Plist file with embedded bot token not set to 0o600 | `service/launchd.rs:135` |
| SW-11 | `cmd_config --test` interpolates token into URL visible in error messages | `main.rs:502-527` |

### Correctness Warnings

| ID | Issue | File:Line |
|----|-------|-----------|
| CW-1 | `find_claude_code_session` matches any session containing "code" (e.g. "vscode") | `injector.rs:338-345` |
| CW-2 | `handle_abort` removes `session_tmux` but not `session_threads` — stale cache | `telegram_handlers.rs:775` |
| CW-3 | Stale tmux target race in free-text/callback answer handlers | `telegram_handlers.rs:857-872` |
| CW-4 | `is_tmux_target_owned_by_other` swallows DB row errors as "owned" | `session.rs:636` |
| CW-5 | SQLite foreign key enforcement disabled (no `PRAGMA foreign_keys = ON`) | `session.rs:128-166` |
| CW-6 | `HookEvent` has no forward-compatibility variant (unlike `MessageType`) | `types.rs:5-14` |
| CW-7 | `chat_id.parse::<i64>().unwrap_or(0)` silently writes invalid config | `setup.rs:639` |
| CW-8 | Double write-lock TOCTOU on `pending_q` cleanup in 3 locations | `telegram_handlers.rs:874-878`, `callback_handlers.rs:396-399,576-579` |
| CW-9 | Echo key formed from uncapped text, defeating dedup for long messages | `daemon/mod.rs:867`, `telegram_handlers.rs:157` |
| CW-10 | `check_for_session_rename` does sync file I/O on async Tokio thread | `socket_handlers.rs:571-600` |

### Reliability Warnings

| ID | Issue | File:Line |
|----|-------|-----------|
| RW-1 | reqwest timeout (30s) matches Telegram long-poll (30s) — spurious timeouts | `bot/client.rs:33` |
| RW-2 | Unbounded `read_line()` — OOM before `MAX_LINE_BYTES` check fires | `socket.rs:293-310` |
| RW-3 | `download_file` loads entire file into memory (up to 20MB) | `bot/client.rs:462-477` |
| RW-4 | `send_photo`/`send_document` read entire file into memory (up to 50MB) | `bot/client.rs:494,539` |
| RW-5 | Event loop spawns unbounded tasks per message — no backpressure | `event_loop.rs:46,69,84` |
| RW-6 | `cleanup_stale_files` silently ignores removal errors | `main.rs:631-640` |
| RW-7 | PID file write failure silently ignored | `socket.rs:170` |
| RW-8 | `launchctl stop` doesn't unload — `KeepAlive` immediately restarts | `service/launchd.rs:195-209` |
| RW-9 | macOS restart aborts on stop failure instead of proceeding to start | `service/mod.rs:211-216` |
| RW-10 | `systemctl daemon-reload` and `enable` errors silently discarded | `systemd.rs:92-98` |
| RW-11 | Stale session cleanup sends message then immediately deletes topic (race) | `daemon/cleanup.rs:103-131` |

### Formatting / Display Warnings

| ID | Issue | File:Line |
|----|-------|-----------|
| FW-1 | `escape_markdown_v2` regex doesn't handle unclosed backtick spans | `formatting.rs:38` |
| FW-2 | `find_code_blocks` regex degrades on unclosed triple-backtick sequences | `formatting.rs:558` |
| FW-3 | `format_tool_details` embeds raw content inside ` ``` ` without escaping inner backticks | `formatting.rs:86-93` |
| FW-4 | `find_meaningful_command` splits on literal `&&` inside quoted strings | `summarize.rs:46` |
| FW-5 | Approval JSON built via `format!` instead of `serde_json` — malformed JSON risk | `hook.rs:456-472` |

### Packaging Warnings

| ID | Issue | File:Line |
|----|-------|-----------|
| PW-1 | `package-lock.json` missing `@agidreams/ctm-linux-arm64` entry | `package-lock.json` |
| PW-2 | Registry propagation wait loop doesn't fail — publishes main package even if platforms unavailable | `release.yml:203-217` |
| PW-3 | `bump-version.sh` updates 6 files but doesn't commit or verify | `scripts/bump-version.sh` |
| PW-4 | `bump-version.sh` and `get-chat-id.sh` published to npm inside `scripts/` | `package.json` files array |
| PW-5 | `.npmignore` is redundant with `files` field — confusing maintenance | `.npmignore` |
| PW-6 | `write_settings` not atomic — interrupted write corrupts Claude Code settings | `installer.rs:49-58` |
| PW-7 | `uninstall_hooks` only removes from global settings, not project-level | `installer.rs:400-449` |

### Platform Compatibility Warnings

| ID | Issue | File:Line |
|----|-------|-----------|
| PCW-1 | Unix signal handling and `nix` crate used without `#[cfg(unix)]` guards | `main.rs:253-264,314-327` |
| PCW-2 | `systemd` unit sets `PrivateTmp=false` with incorrect justification | `systemd.rs:41` |
| PCW-3 | `check_tmux` uses `which` instead of `tmux -V` (not available in all containers) | `doctor.rs:462-468` |
| PCW-4 | `detect_groups` in setup wizard swallows all errors — no network error feedback | `setup.rs:119-153` |
| PCW-5 | `toggle --on --off` accepted simultaneously with silent first-wins | `main.rs:579-585` |

---

# ROUND 2 FINDINGS (post-fix verification)

## Phase 6 — CI Gate Blockers (trivial fixes)

### R2-B1: Clippy Error — `hook.rs:606` question-mark lint

- **File:** `hook.rs:606-609`
- **Severity:** BLOCKER (CI gate)
- **Category:** Lint — `cargo clippy -- -D warnings` fails
- **Found by:** Agent #2 (Build & Test)

**Problem:** `match validate_transcript_path(...) { Some(p) => p, None => return None }` can be replaced with `?`.

**Fix:** Replace the match with `validate_transcript_path(transcript_path)?`.

**Effort:** 1 minute.

---

### R2-B2: Formatting Divergence — 5 Files

- **Files:** `config.rs:105`, `daemon/mod.rs:504,901,955`, `socket.rs:155`, `types.rs:228`
- **Severity:** BLOCKER (CI gate)
- **Category:** Formatting — `cargo fmt --check` fails
- **Found by:** Agent #2 (Build & Test)

**Problem:** 5 files have cosmetic whitespace/line-length divergence from `rustfmt` defaults.

**Fix:** Run `cargo fmt`.

**Effort:** 1 minute.

---

## Phase 7 — Correctness (Round 2 discoveries)

### R2-B3: Rate Limit Default Is 1 msg/sec Instead of 20

- **File:** `config.rs:316-319`
- **Severity:** BLOCKER
- **Category:** Correctness — severe UX degradation
- **Found by:** Agent #3 (Core Module Reviewer)

**Problem:** `rate_limit` defaults to `1` message/second. The TS implementation defaulted to 20.
Test configs use `rate_limit: 20`. With default of 1, any moderately active session causes extreme
message queue buildup — messages arrive 20x slower than generated. Users will see multi-minute
delays on all Telegram notifications.

**Fix:** Change the `unwrap_or(1)` at line 319 to `unwrap_or(20)`.

**Effort:** 1 minute.

---

### R2-B4: `ProcessingGuard::drop` Uses `try_lock()` — Permanent Queue Deadlock

- **File:** `bot/queue.rs:8-16`
- **Severity:** BLOCKER
- **Category:** Correctness — resource leak
- **Found by:** Agent #3 (Core Module Reviewer)

**Problem:** The `ProcessingGuard` added to fix C-2 uses `try_lock()` in its `Drop` impl.
If the async mutex is held by another task at drop time (reading the `processing` flag),
`try_lock()` silently fails. The flag remains `true` permanently, deadlocking the message
queue for the rest of the daemon lifetime. No new messages will ever be sent.

**Fix:** Store the `MutexGuard` inside the guard struct rather than the `Arc<Mutex>`, or use
`block_on(self.0.lock())` in a `spawn_blocking` context. Alternatively, use an `AtomicBool`
instead of `Mutex<bool>` — the flag is a simple boolean that doesn't need mutual exclusion.

**Effort:** 30 minutes.

---

### R2-B5: Unquoted Binary Path in Hook Command — Spaces in Path Break Hooks

- **File:** `installer.rs:64-69`
- **Severity:** BLOCKER
- **Category:** Correctness — install failure on common paths
- **Found by:** Agent #4 (CLI & Commands Reviewer)

**Problem:** `ctm_hook_command()` produces `format!("{} hook", exe.display())`. If the binary
is installed in a path with spaces (e.g. `/home/user/my tools/ctm`), Claude Code's shell
invocation splits the path and the hook silently fails. All mirroring stops working.

**Fix:** Quote the path: `format!("\"{}\" hook", exe.display())`.

**Effort:** 1 minute.

---

### R2-B6: No PID File Locking — Double-Start Race

- **Files:** `main.rs` (cmd_start), `socket.rs` (PID file write)
- **Severity:** BLOCKER
- **Category:** Correctness — concurrent start race
- **Found by:** Agent #4 (CLI & Commands Reviewer)

**Problem:** No advisory lock (`flock`) on the PID file. Two concurrent `ctm start` invocations
both pass the `!pid_file.exists()` check, both create daemons, and the second overwrites the PID
file. The first daemon runs untracked and cannot be stopped via `ctm stop`.

**Fix:** Use `flock(LOCK_EX | LOCK_NB)` on the PID file at daemon startup. If the lock fails,
print "Daemon already running" and exit.

**Effort:** 30 minutes.

---

### R2-B7: Registry Propagation Loop Has No Failure Exit

- **File:** `.github/workflows/release.yml:206-220`
- **Severity:** BLOCKER (CI — broken release)
- **Category:** Packaging — silent failure
- **Found by:** Agent #8 (CI/CD Reviewer)

**Problem:** The wait loop polls for 30 iterations. If platform packages are still not visible
on the npm registry after 150 seconds, execution falls through silently and the main package
publishes. Users who `npm install` during this window get broken installs (binary not found).

**Fix:** After the loop, check `$ALL_AVAILABLE` and `exit 1` if still false.

**Effort:** 5 minutes.

---

## Phase 8 — Security (Round 2 — no criticals, medium only)

### R2-SEC-1: Group Chat Members Can Interact With Bot (BY DESIGN)

- **Files:** `daemon/callback_handlers.rs`, `daemon/telegram_handlers.rs`
- **Severity:** MEDIUM → **ACCEPTED BY DESIGN**
- **Category:** Auth — per-user authorization within configured chat
- **Found by:** Agent #6 (Security Auditor)

**Problem:** The `from` field of Telegram `CallbackQuery` is never checked. If
`TELEGRAM_CHAT_ID` is a group chat, any group member can press approval buttons.

**Decision:** This is by design. The `chat_id` check is the authorization boundary.
Group owners are responsible for membership. Private 1:1 chat is the recommended
deployment model. No code change needed.

---

### R2-SEC-2: No Per-Connection Rate Limit on Unix Socket

- **File:** `socket.rs:29`
- **Severity:** MEDIUM
- **Category:** DoS — socket flooding
- **Found by:** Agent #6 (Security Auditor)

**Problem:** A compromised local process can flood the socket with messages, starving the
Telegram polling loop. Existing mitigation: 500-item outbound queue cap + 64 connection limit.

**Recommended fix:** Add per-connection token-bucket rate limiter using `governor` crate.
Track for v0.3.0.

---

### R2-SEC-3: No Socket Authentication Between Same-UID Processes

- **File:** `socket.rs:181-223`
- **Severity:** MEDIUM
- **Category:** Auth — Unix socket trust boundary
- **Found by:** Agent #6 (Security Auditor)

**Problem:** Any process running as the same user can connect to the socket and send
arbitrary messages (fake sessions, fake approvals, toggle commands).

**Decision:** Acceptable for v0.2.0 — the trust boundary is the OS user account.
Document as known limitation. Consider `SO_PEERCRED` or per-session HMAC for v1.0.

---

## Round 2 — New Warnings

### Security Warnings (new)

| ID | Issue | File:Line |
|----|-------|-----------|
| SW-12 | Setup wizard `getUpdates` URL leaks bot token via `println!` (bypasses ScrubWriter) | `setup.rs:497-498` |
| SW-13 | `download_file` accepts any `&str` dest path with no validation | `bot/client.rs:425` |

### Correctness Warnings (new)

| ID | Issue | File:Line |
|----|-------|-----------|
| CW-11 | `get_updates` silently drops Telegram API `ok:false` errors — returns empty vec | `bot/client.rs:590` |
| CW-12 | `remove_keyboard` is a no-op — missing `reply_markup: {}` in editMessageReplyMarkup | `bot/client.rs:370-381` |
| CW-13 | Plain-text retry fallback loses `reply_to_message_id` threading context | `bot/queue.rs:157-179` |
| CW-14 | `short_session_id` byte slice (not char slice) — latent panic if validation relaxed | `socket_handlers.rs:744-745` |
| CW-15 | `cleanup_stale_files` hardcodes `bridge.sock` — ignores `TELEGRAM_BRIDGE_SOCKET` override | `main.rs:631-639` |
| CW-16 | Setup wizard hardcodes `"verbose": true` in all written configs | `setup.rs:644` |
| CW-17 | `escape_markdown_v1` only escapes backtick — `*`, `_`, `[` can break rendering | `daemon/mod.rs:840-851` |
| CW-18 | `cc` command silently rejected with no user feedback on invalid slash command | `telegram_handlers.rs:97-104` |
| CW-19 | Photo/document download proceeds without size guard when `file_size` is None | `telegram_handlers.rs:229-234` |
| CW-20 | `doctor.rs` check counter comments stale (`[1/9]` vs actual `[1/10]`) | `doctor.rs:688-691` |
| CW-21 | `doctor.rs` reimplements hook detection instead of calling `installer::is_ctm_command` | `doctor.rs:254-388` |
| CW-22 | `is_ctm_command` substring fallback matches `notctm hook` | `installer.rs:105` |
| CW-23 | `require_manual` can underflow if `into_fixed` invariant breaks | `doctor.rs:765` |
| CW-24 | `bot_sessions` HashMap unbounded — no eviction of stale entries | `telegram_handlers.rs:595` |
| CW-25 | `recent_inputs` HashSet unbounded — grows without bound under flood | `daemon/mod.rs:120` |

### Reliability Warnings (new)

| ID | Issue | File:Line |
|----|-------|-----------|
| RW-12 | Setup wizard `process::exit(1)` skips terminal state cleanup from `dialoguer` | `setup.rs:329-334,381-386` |
| RW-13 | Approval timeout conflates socket write failures with user non-response | `hook.rs:469-475` |
| RW-14 | `hook.rs:18-28` off-by-one: `take(MAX)` then `>= MAX` drops exact-length valid input | `hook.rs:18-28` |
| RW-15 | macOS `restart_service` is stop-then-start (non-atomic, leaves gap) | `service/mod.rs:210-223` |
| RW-16 | `handle_service_command` uses `process::exit(1)` instead of returning `Err` | `service/mod.rs:240-283` |
| RW-17 | Launchd `install` doesn't `launchctl load` — inconsistent with systemd auto-enable | `service/launchd.rs:113-148` |
| RW-18 | SIGKILL aftermath not confirmed — stale files possible after force-kill | `main.rs:329` |
| RW-19 | `cmd_restart` tail-calls `cmd_start` which blocks on signal — surprising for scripts | `main.rs:382` |

### Formatting / Display Warnings (new)

| ID | Issue | File:Line |
|----|-------|-----------|
| FW-6 | `format_*` helpers don't call `escape_markdown_v2` — messages with special chars fail to send | `formatting.rs:70-73` |
| FW-7 | ANSI stripper regex misses OSC sequences (`\x1b]...`) — garbage in Telegram messages | `formatting.rs:15` |
| FW-8 | `summarize.rs`: `env VAR=val cmd` produces wrong summary ("Running `VAR=val`") | `summarize.rs:190` |
| FW-9 | `summarize.rs`: `timeout --preserve-status cmd` mishandles flag as command | `summarize.rs` |

### Packaging Warnings (new)

| ID | Issue | File:Line |
|----|-------|-----------|
| PW-8 | No `cargo audit` in CI or before release — known vulnerabilities can ship | `.github/workflows/ci.yml` |
| PW-9 | `workflow_dispatch` publishes to npm without requiring a tag — creates unanchored release | `release.yml:6` |
| PW-10 | `macos-latest` unpinned — ABI compatibility may silently regress | `release.yml:47` |
| PW-11 | `bump-version.sh` `sed` silent non-match — version bump appears to succeed when pattern not found | `scripts/bump-version.sh:29-47` |
| PW-12 | `bump-version.sh` doesn't update `Cargo.lock` after `Cargo.toml` change | `scripts/bump-version.sh` |
| PW-13 | `npm-packages/*/bin/ctm` not in `.gitignore` — compiled binaries can be accidentally committed | `.gitignore` |
| PW-14 | Systemd env file value quoting doesn't escape `"` — service fails with certain token chars | `service/env.rs:85-91` |

### Platform Compatibility Warnings (new)

| ID | Issue | File:Line |
|----|-------|-----------|
| PCW-6 | Injector `set_target` uses string `..` check instead of canonicalize — false positives | `injector.rs:33-46` |
| PCW-7 | Injector spawns 3 separate `tmux display-message` processes instead of 1 | `injector.rs:251-263` |
| PCW-8 | Launchd NVM path filtering rejects any PATH component containing `.nvm` substring | `service/launchd.rs:18-20` |

### Test Coverage Gaps (new — from Round 2 test analysis)

| ID | Issue | Source File |
|----|-------|-------------|
| TG-1 | No test for `format_tool_details` (all tool types) | `formatting.rs` |
| TG-2 | No test for `chunk_message("")` (empty input) | `formatting.rs` |
| TG-3 | No test for single word longer than `max_length` (fallback split) | `formatting.rs` |
| TG-4 | No test for Unicode emoji at exact chunk boundary | `formatting.rs` |
| TG-5 | No test for `expire_old_approvals()` | `session.rs` |
| TG-6 | No test for `get_pending_approvals()` | `session.rs` |
| TG-7 | No test for invalid session ID rejection at `create_session` boundary | `session.rs` |
| TG-8 | No test for `reactivate_session` on non-existent session (silent no-op) | `session.rs` |
| TG-9 | No test for `env VAR=val cmd` wrapper stripping | `summarize.rs` |
| TG-10 | No test for `timeout --preserve-status cmd` wrapper stripping | `summarize.rs` |

---

## Positive Findings

The audit identified significant strengths worth preserving:

1. **Token scrubbing via `ScrubWriter`** — defense-in-depth that scrubs all tracing output
2. **Socket permissions** — 0o600 file, 0o700 directory, flock for single-instance
3. **`Command::arg()` everywhere** — no shell interpolation in tmux commands
4. **Tmux key whitelist** — `ALLOWED_TMUX_KEYS` prevents arbitrary key injection
5. **Slash command character whitelist** — prevents shell metacharacter injection
6. **IDOR checks on every callback handler** — chat ID verified on all Telegram inputs
7. **`MAX_LINE_BYTES` consistently applied** — 1 MiB limit on NDJSON reads
8. **Chat ID authorization** — silent drop for unauthorized chats (correct posture)
9. **Rate limiting** — `governor` crate on API client, 64-connection socket cap
10. **Forward-compatible `MessageType`** — `#[serde(other)]` on `Unknown` variant
11. **Documented lock ordering** — `DaemonState` comments specify acquisition order
12. **All `unwrap()` calls provably safe** — clamped values, constant regexes, prior None-checks
13. **Zero hardcoded secrets** — all credentials flow from env vars or config files
14. **Migration 100% complete** — no TS remnants, no TODOs, all ADR items resolved
15. **Parameterized SQL everywhere** — zero SQL injection risk (Round 2 confirmed)
16. **TLS enforced via rustls** — no HTTP connections to Telegram possible
17. **`NoNewPrivileges=true`** — no privilege escalation in systemd unit
18. **Binary integrity verification** — SHA-256 checksums in npm distribution pipeline
19. **All 387 tests pass** — 101 more tests than Round 1 baseline

---

## Appendix A: Round 1 Fix Summary

| Item | Status | Commit |
|------|--------|--------|
| S-1: Path traversal | FIXED | `9bd80e5` |
| S-2: Approval routing | FIXED | `9bd80e5` |
| S-3: `db_op` panic | FIXED | `9bd80e5` |
| S-4: Debug redaction | FIXED | `9bd80e5` |
| C-1: Echo key | FIXED | `9bd80e5` |
| C-2: Queue RAII guard | FIXED (but see R2-B4) | `9bd80e5` |
| C-3: TOPIC_CLOSED reopen | FIXED | `9bd80e5` |
| C-4: Atomic `end_session` | FIXED | `f5a5cf6` |
| C-5: Session ID validation | FIXED | `f5a5cf6` |
| C-6: Status validation | FIXED | `f5a5cf6` |
| U-1: Char-boundary chunking | FIXED | `18b1677` |
| U-2: Char-safe truncation | FIXED | `9bd80e5` |
| P-2: `prepublishOnly` guard | FIXED | `4e43e40` |
| P-3: `setup-node` for provenance | FIXED | `4e43e40` |

## Appendix B: Round 2 Recommended Fix Order

| Priority | Phase | Items | Effort |
|----------|-------|-------|--------|
| P0 | CI gate | R2-B1 (clippy), R2-B2 (fmt) | ~2 minutes |
| P1 | Critical correctness | R2-B3 (rate limit), R2-B4 (queue guard), R2-B5 (path quoting) | ~35 minutes |
| P2 | Race condition | R2-B6 (PID file locking) | ~30 minutes |
| P3 | Release pipeline | R2-B7 (registry propagation exit) | ~5 minutes |
| P4 | Top warnings | CW-11, CW-15, CW-17, RW-12, PW-8, PW-13, PW-14 | ~3 hours |
| P5 | Test coverage gaps | TG-1 through TG-10 | ~4 hours |
| **Total** | | | **~8 hours** |
