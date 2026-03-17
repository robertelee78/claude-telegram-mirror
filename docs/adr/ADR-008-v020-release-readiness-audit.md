# ADR-008: v0.2.0 Release Readiness Audit

> **DO NOT BE LAZY. We have plenty of time to do it right.**
> No shortcuts. Never make assumptions.
> Always dive deep and ensure you know the problem you're solving.
> Make use of search as needed.
> Measure 3x, cut once.
> No fallback. No stub (todo later) code.
> Just pure excellence, done the right way the entire time.
> Chesterton's fence: always understand the current implementation fully before changing it.

**Status:** Accepted (In Progress)
**Date:** 2026-03-17
**Authors:** Robert E. Lee
**Supersedes:** None
**Related:** ADR-002 (Phased Rust Migration), ADR-006 (Migration Gap Audit)
**Tracking:** https://github.com/robertelee78/claude-telegram-mirror/issues/3

### Execution Progress (updated 2026-03-17)

| Epic | Status | Stories |
|------|--------|---------|
| 1: Structural Decomposition | ✅ DONE | 3/3 |
| 2: Runtime Safety Fixes | Staged | 0/5 |
| 3: Type Safety & Concurrency | Staged | 0/4 |
| 4: npm Distribution Pipeline | ✅ DONE | 6/6 |
| 5: Code Hygiene & DRY | Staged | 0/4 |
| 6: Documentation & Artifacts | ✅ PARTIAL | 3/4 |
| 7: Integration Test Suite | Staged | 0/5 |
| 8: Binary Integrity | Staged | 0/2 |

---

## Context

CTM v0.2.0 represents the first release after the complete TypeScript-to-Rust migration
(ADR-002 Phase 1–4). The migration itself was verified by six revision gap audits
(ADR-006 Rev 1–6) confirming all 109 gaps resolved and 17+ Rust-only improvements.

This ADR documents a comprehensive release readiness audit conducted on 2026-03-17
using four parallel analysis agents covering: (1) Rust code quality, (2) npm
distribution pipeline, (3) build/test status, and (4) migration completeness.

### Audit Methodology

| Agent | Scope | Duration |
|-------|-------|----------|
| Code Analyzer | All 17 `.rs` files (14,598 lines), architecture, error handling, security, concurrency | 144s |
| Distribution Reviewer | package.json, postinstall.cjs, npm-packages/*, CI workflows, wrapper scripts | 99s |
| Build & Test | cargo check, cargo test, cargo clippy, test coverage analysis | 64s |
| Migration Explorer | dist/ artifacts, TS remnants, README accuracy, ADR verification | 85s |

### Overall Score

| Category | Score | Verdict |
|----------|-------|---------|
| Compilation | 9/10 | Clean (1 Cargo.toml warning) |
| Tests | 8/10 | 211/211 pass, 0 clippy warnings, no integration tests |
| Code Quality | 7.5/10 | 27 issues found (3 critical, 5 high, 10 medium) |
| Migration | 9.5/10 | Complete, clean, well-tracked via ADRs |
| Security | 8/10 | Strong posture, 1 memory leak, no binary verification |
| npm Distribution | 4/10 | linux-arm64 broken, CI race conditions, stale versions |
| Documentation | 6/10 | README has dead commands, postinstall URL wrong |

**Release readiness verdict: NOT READY — 11 blockers, 10 should-fix items.**

---

## Decision

Resolve **all** findings before cutting v0.2.0. No deferrals.

- **Tier 1 (Release Blockers):** Critical and high-severity items with runtime or
  distribution impact. Includes daemon.rs decomposition as foundational work that
  de-risks all subsequent daemon.rs fixes.
- **Tier 2 (Should Fix):** Architectural debt, performance, documentation, and DRY.
- **Tier 3 (Quality & Hardening):** Integration tests, binary integrity verification,
  doc-tests, and remaining module splits. Previously deferred to v0.3.0; promoted to
  active scope per decision on 2026-03-17.

All items are tracked in `_bmad-output/planning-artifacts/epics-adr008.md`.

---

## Tier 1 — Release Blockers

### B-1: `Box::leak` Memory Leak in daemon.rs

- **File:** `rust-crates/ctm/src/daemon.rs:2067`
- **Severity:** CRITICAL
- **Category:** Runtime bug

**Problem:** `Box::leak(format!("unnamed.{ext}").into_boxed_str())` permanently leaks
memory every time a Telegram document without a filename is received. Used to produce a
`&'static str` where only a local borrow is needed.

```rust
// CURRENT (leaks)
doc.mime_type
    .as_deref()
    .and_then(|m| m.split('/').next_back())
    .map(|ext| Box::leak(format!("unnamed.{ext}").into_boxed_str()) as &str)
    .unwrap_or("unnamed.bin")
```

**Fix:** Use an owned `String` variable. Change the consuming function to accept
`String` or `Cow<str>` instead of requiring `&str` with `'static` lifetime.

**Effort:** 15 minutes.

---

### B-2: `unsafe` Block in socket.rs Replaceable with Safe Code

- **File:** `rust-crates/ctm/src/socket.rs:331`
- **Severity:** CRITICAL
- **Category:** Code safety

**Problem:** Uses `unsafe { OwnedFd::from_raw_fd(raw_fd) }` to convert a `File` into
an `OwnedFd`. The `into_raw_fd()` + `from_raw_fd()` round-trip is the classic
fd-leak-on-panic pattern. A safe alternative has been stable since Rust 1.63
(edition 2021, which this project targets).

```rust
// CURRENT (unsafe)
let raw_fd = lock_file.into_raw_fd();
let owned_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };

// FIX (safe)
let owned_fd: OwnedFd = lock_file.into();
```

**Effort:** 5 minutes.

---

### B-3: linux-arm64 Missing from `optionalDependencies`

- **File:** `package.json:49-53`
- **Severity:** CRITICAL
- **Category:** Distribution

**Problem:** The `ctm-linux-arm64` platform package exists in `npm-packages/` and the
binary resolver (`resolve-binary.cjs:30`) maps `linux-arm64`, but `package.json`
`optionalDependencies` only lists three packages:

```json
"optionalDependencies": {
    "@agidreams/ctm-linux-x64": "0.2.0",
    "@agidreams/ctm-darwin-arm64": "0.2.0",
    "@agidreams/ctm-darwin-x64": "0.2.0"
}
```

`@agidreams/ctm-linux-arm64` is missing. npm will never auto-install it. ARM64 Linux
users (Raspberry Pi, AWS Graviton, Ampere) silently get "No native ctm binary found."

**Fix:** Add `"@agidreams/ctm-linux-arm64": "0.2.0"` to optionalDependencies.

**Effort:** 1 minute.

---

### B-4: linux-arm64 Missing CI Build Job

- **File:** `.github/workflows/release.yml`
- **Severity:** CRITICAL
- **Category:** CI/CD

**Problem:** `release.yml` defines build jobs for `linux-x64`, `darwin-arm64`, and
`darwin-x64`. There is no `build-linux-arm64` job. The `publish` job's `needs` array
and the platform publish loop both omit `ctm-linux-arm64`.

**Fix:** Add a `build-linux-arm64` job using cross-compilation target
`aarch64-unknown-linux-gnu` (or `runs-on: ubuntu-24.04-arm` if available). Add it to
the `needs` array and publish loop.

**Effort:** 30 minutes.

---

### B-5: linux-arm64 Package Missing `bin/` Directory

- **File:** `npm-packages/ctm-linux-arm64/`
- **Severity:** CRITICAL
- **Category:** Distribution

**Problem:** The `ctm-linux-arm64` package has only a `package.json` — no `bin/`
directory, no `.gitkeep`. Its `files` array declares `["bin/"]`, so `npm pack` produces
an empty package. All other platform packages have `bin/.gitkeep`.

**Fix:** Create `npm-packages/ctm-linux-arm64/bin/.gitkeep`.

**Effort:** 1 minute.

---

### B-6: Hardcoded Version `0.1.0` in Release Pipeline

- **File:** `.github/workflows/release.yml:129`
- **Severity:** CRITICAL
- **Category:** CI/CD

**Problem:** Registry propagation check is hardcoded:

```yaml
if npm view @agidreams/ctm-linux-x64@0.1.0 version 2>/dev/null; then
```

On v0.2.0+, this checks the wrong version. It either succeeds immediately (if 0.1.0
was previously published) or loops for 150s checking the wrong version.

**Fix:** Extract version dynamically:

```yaml
VERSION=$(node -p "require('./package.json').version")
npm view @agidreams/ctm-linux-x64@$VERSION version
```

**Effort:** 5 minutes.

---

### B-7: Conflicting Publish Workflows (Race Condition)

- **Files:** `.github/workflows/publish.yml`, `.github/workflows/release.yml`
- **Severity:** CRITICAL
- **Category:** CI/CD

**Problem:** `publish.yml` triggers on GitHub release events. `release.yml` triggers on
`v*` tags. Creating a GitHub release creates a tag, so both workflows fire
simultaneously. One fails with "version already exists."

Additionally, `publish.yml` runs `npm run build` — a script that does not exist in
`package.json`. This workflow would fail even without the race condition.

**Fix:** Delete `publish.yml` entirely. `release.yml` already handles the complete
build → publish chain for all platforms and the main package.

**Effort:** 5 minutes.

---

### B-8: UTF-8 Panic in `summarize.rs` Truncate

- **File:** `rust-crates/ctm/src/summarize.rs:33-38`
- **Severity:** HIGH
- **Category:** Runtime bug

**Problem:** The `truncate` function slices on byte indices (`&s[..max_len]`), which
panics on multi-byte UTF-8 characters (Japanese filenames, emoji URLs, etc.). The same
function in `formatting.rs` correctly uses `.chars().take()`.

```rust
// CURRENT (panics on multi-byte)
fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len { s } else { &s[..max_len] }
}

// FIX (UTF-8 safe)
fn truncate(s: &str, max_len: usize) -> String {
    s.chars().take(max_len).collect()
}
```

**Fix:** Unify both `truncate` implementations to use the safe version from
`formatting.rs`. See also S-5 (duplicate functions).

**Effort:** 10 minutes.

---

### B-9: `BridgeMessage.msg_type` is String, Not Enum

- **File:** `rust-crates/ctm/src/types.rs:139-141`
- **Severity:** HIGH
- **Category:** Type safety

**Problem:** `BridgeMessage.msg_type` is a `String` despite having a well-defined
`MessageType` enum at line 118 of the same file. The daemon dispatches on
`msg.msg_type.as_str()` with string match arms (daemon.rs:644-694). Any typo silently
falls through to the `_` arm. The `MessageType` enum exists but is unused.

**Fix:** Change `BridgeMessage.msg_type` to `MessageType` with
`#[serde(rename = "type")]` for wire compatibility. Update all match sites.

**Effort:** 1 hour.

---

### B-10: Duplicate Cargo.toml `[[bin]]` Targets

- **File:** `rust-crates/ctm/Cargo.toml`
- **Severity:** HIGH
- **Category:** Build configuration

**Problem:** Two `[[bin]]` entries (`ctm` and `claude-telegram-mirror`) both point at
`src/main.rs`. Cargo emits a warning on every build:

> `file src/main.rs found to be present in multiple build targets`

**Fix:** Remove one `[[bin]]` entry. The npm wrapper script already provides the
`claude-telegram-mirror` alias — the binary only needs the `ctm` name.

**Effort:** 2 minutes.

---

### B-11: Inconsistent Lock Ordering (Deadlock Risk)

- **File:** `rust-crates/ctm/src/daemon.rs` (multiple locations)
- **Severity:** HIGH
- **Category:** Concurrency

**Problem:** Several handlers acquire `ctx.sessions.lock().await` (SQLite mutex) and
then `ctx.session_threads.write().await` or `ctx.session_tmux.write().await`. But the
ordering is inconsistent: `handle_session_start` locks sessions → session_threads, while
`get_tmux_target` locks session_tmux → sessions. With tokio's cooperative scheduling,
this creates a potential deadlock under concurrent handler execution.

**Fix:** Establish and document a canonical lock ordering:
1. `sessions` (SQLite)
2. `session_threads`
3. `session_tmux`
4. All other `RwLock`s in declaration order

Consider consolidating related state into a single `RwLock<DaemonState>` struct.

**Effort:** 2 hours.

---

## Tier 2 — Should Fix Before Release

### S-1: `Config` Deep-Cloned on Every Event

- **File:** `rust-crates/ctm/src/daemon.rs:386-476`
- **Severity:** HIGH
- **Category:** Performance

**Problem:** `HandlerContext` is constructed inline three times in the event loop (socket
message, Telegram update, cleanup timer). Each construction clones 16 `Arc`s plus a
deep copy of `Config` (all strings). Under heavy traffic, this is measurable overhead.

**Fix:** Wrap `Config` in `Arc<Config>`. Pre-construct a single `HandlerContext`
template and `.clone()` it (16 atomic increments, no string copies).

**Effort:** 1 hour.

---

### S-2: Synchronous SQLite on Async Runtime

- **File:** `rust-crates/ctm/src/session.rs`, `daemon.rs:103`
- **Severity:** HIGH
- **Category:** Performance / Concurrency

**Problem:** `rusqlite::Connection` wrapped in `tokio::sync::Mutex`. Every DB operation
holds the mutex during synchronous I/O on the tokio thread pool. Under load, this blocks
the async runtime. Double-locking pattern visible at lines 762-773, 787-793.

**Fix:** Use `tokio::task::spawn_blocking` for all database operations. At minimum,
reduce critical section size by reading data, dropping the lock, then re-acquiring to
write.

**Effort:** 3-4 hours.

---

### S-3: `#[allow(dead_code)]` on 6+ Modules

- **Files:** `main.rs:4-26`, `error.rs:1`, `bot.rs:198`
- **Severity:** HIGH
- **Category:** Code hygiene

**Problem:** Six of 15 modules in `main.rs` have `#[allow(dead_code)]`. The entire
`error.rs` is gated. `TelegramBot` impl block has it. This suggests `AppError` and many
public APIs are not actually used, or the lib/bin split is misconfigured.

**Fix:** Audit all `#[allow(dead_code)]` annotations. Remove genuinely unused code.
For code used only via `lib.rs`, configure the lib crate's public API properly.

**Effort:** 1-2 hours.

---

### S-4: Delete Orphaned `dist/` Directory

- **File:** `/opt/claude-telegram-mirror/dist/`
- **Severity:** MEDIUM
- **Category:** Migration cleanup

**Problem:** 896 KB of old compiled TypeScript outputs (`.js`, `.js.map`, `.d.ts`).
Untracked in git, not in package.json `files` field, not used by anything. Will not
ship to npm but clutters the working tree.

**Fix:** `rm -rf dist/`

**Effort:** 1 minute.

---

### S-5: Duplicate Utility Functions

- **Files:** `formatting.rs:404` + `summarize.rs:11` (`short_path`), `formatting.rs:376` + `summarize.rs:33` (`truncate`), `doctor.rs:16-33` + `setup.rs:18-35` (color helpers)
- **Severity:** MEDIUM
- **Category:** DRY violation

**Problem:** `short_path`, `truncate`, and 6 color helper functions (`cyan`, `green`,
`yellow`, `red`, `gray`, `bold`) are copy-pasted across modules.

**Fix:** Extract shared utilities:
- `short_path` and `truncate` → `formatting.rs` (make pub, import elsewhere)
- Color helpers → new `colors.rs` or use the `colored` crate

**Effort:** 30 minutes.

---

### S-6: Update README Build Instructions

- **File:** `README.md:340-369`
- **Severity:** MEDIUM
- **Category:** Documentation

**Problem:** Still references `npm run build` and `node dist/cli.js` — commands that
no longer exist. First-impression killer for developers cloning the repo.

**Fix:** Replace with Rust build instructions:
```bash
cd rust-crates && cargo build --release
# Binary at: rust-crates/target/release/ctm
```

Clarify that end users should use `npm install -g claude-telegram-mirror`.

**Effort:** 15 minutes.

---

### S-7: Fix postinstall.cjs Documentation URL

- **File:** `postinstall.cjs:44`
- **Severity:** MEDIUM
- **Category:** Documentation

**Problem:** Points users to `github.com/robertelee78/claude-mobile` instead of
`github.com/robertelee78/claude-telegram-mirror`. Inconsistent with `package.json`
homepage and repository fields.

**Fix:** Update URL to match package.json.

**Effort:** 1 minute.

---

### S-8: `Vec::remove(0)` — O(n) Dequeue

- **File:** `rust-crates/ctm/src/bot.rs:374`
- **Severity:** MEDIUM
- **Category:** Performance

**Problem:** `q.remove(0)` shifts all remaining elements on every dequeue. The message
queue should use `VecDeque` for O(1) front removal.

**Fix:** Replace `Vec<QueuedMessage>` with `VecDeque<QueuedMessage>` and use
`pop_front()`.

**Effort:** 15 minutes.

---

### S-9: No `reqwest` Timeout

- **File:** `rust-crates/ctm/src/bot.rs:213`
- **Severity:** MEDIUM
- **Category:** Reliability

**Problem:** `Client::new()` creates an HTTP client with no timeout. A hung Telegram API
call would block indefinitely.

**Fix:** `Client::builder().timeout(Duration::from_secs(30)).build()?`

**Effort:** 5 minutes.

---

### S-10: `home_dir()` Fallback Duplicated in 4 Files

- **Files:** `service.rs`, `setup.rs`, `installer.rs`, `doctor.rs`
- **Severity:** LOW
- **Category:** DRY violation

**Problem:** `dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))` is
copy-pasted in 4 modules.

**Fix:** Centralize as `pub fn home_dir()` in `config.rs`.

**Effort:** 10 minutes.

---

## Tier 3 — Quality & Hardening

> Originally deferred to v0.3.0. Promoted to active scope on 2026-03-17 — all work
> ships with v0.2.0. No deferrals.

### D-1: Split daemon.rs (God Object) — PROMOTED TO TIER 1

- **File:** `rust-crates/ctm/src/daemon.rs` (3,741 lines)
- **Severity:** CRITICAL (structural) — **now foundational for Tier 1 fixes**
- **Category:** Architecture

**Problem:** Exceeds the project's 500-line limit by 7.5x. Contains the entire daemon
runtime: event loop, 15+ socket message handlers, all Telegram command handlers, all
callback query handlers, session lifecycle management, topic deletion scheduling,
AskUserQuestion flow, cleanup routines, and file upload/download handling. The
`run_event_loop` function takes 16 parameters.

**Rationale for promotion:** B-1 (Box::leak), B-9 (enum conversion), B-11 (lock
ordering), S-1 (Arc Config), and S-2 (spawn_blocking) all modify daemon.rs. Doing
those fixes in a 3,741-line monolith versus focused 300-600 line modules is a
different experience entirely. Split first, fix after.

**Proposed Split:**

| Module | Contents | Est. Lines |
|--------|----------|------------|
| `daemon/mod.rs` | `Daemon` struct, `start()`, `stop()`, `HandlerContext` | 200 |
| `daemon/event_loop.rs` | Main select loop, timer management | 300 |
| `daemon/socket_handlers.rs` | All `handle_*` functions for socket messages | 800 |
| `daemon/telegram_handlers.rs` | Command and message handlers | 600 |
| `daemon/callback_handlers.rs` | Callback query dispatching | 500 |
| `daemon/cleanup.rs` | Stale session cleanup, topic deletion scheduling | 300 |
| `daemon/files.rs` | Document upload/download handling | 300 |

**Effort:** 4-8 hours.

---

### D-2: Add Integration Tests

- **Severity:** HIGH (testing)
- **Category:** Quality

**Problem:** All 211 tests are unit tests in `#[cfg(test)]` modules. No integration
tests exercise the binary, cross-module interactions, or daemon lifecycle.

**Proposed Tests:**

| Test | Description |
|------|-------------|
| CLI smoke test | Verify `ctm --help`, `ctm --version`, invalid args |
| Socket round-trip | Start socket server, connect client, send/receive message |
| Daemon lifecycle | Start daemon, verify pidfile, send shutdown, verify cleanup |
| Config validation | End-to-end config load from real file with edge cases |
| Hook processing | Simulate Claude Code hook events through the full pipeline |

**Effort:** 8-12 hours.

---

### D-3: Binary Integrity Verification

- **File:** `scripts/resolve-binary.cjs:72-74`
- **Severity:** MAJOR
- **Category:** Security

**Problem:** No checksum or signature verification on resolved binaries. A compromised
`node_modules` or npm registry supply-chain attack could substitute a malicious binary.
Code acknowledges this: "See ADR-006 L3.8."

**Proposed Fix:** Add SHA-256 checksums in platform package manifests, verify at
resolve time. Add `--provenance` to all `npm publish` commands in CI.

**Effort:** 4-6 hours.

---

### D-4: Migrate Deprecated `chrono::Duration` Methods

- **Files:** `session.rs:557`, `daemon.rs:3357`
- **Severity:** LOW
- **Category:** Maintenance

**Problem:** `chrono::Duration::hours()` and `chrono::Duration::days()` are deprecated
in newer chrono versions in favor of `chrono::TimeDelta::try_hours()`.

**Effort:** 10 minutes.

---

### D-5: Add Doc-Tests

- **Severity:** LOW
- **Category:** Documentation

**Problem:** Zero doc-tests across the entire codebase. Public API functions have no
executable documentation.

**Effort:** 2-4 hours.

---

### D-6: Split Oversized Modules (bot.rs, service.rs, setup.rs)

- **Files:** `bot.rs` (1,129), `service.rs` (928), `setup.rs` (940)
- **Severity:** MEDIUM
- **Category:** Architecture

**Problem:** Three additional modules exceed the 500-line limit. Lower priority than
daemon.rs because they are 2x (not 7.5x) the limit.

**Proposed Splits:**
- `bot.rs` → `bot/types.rs`, `bot/client.rs`, `bot/queue.rs`
- `service.rs` → `service/env.rs`, `service/systemd.rs`, `service/launchd.rs`
- `setup.rs` — acceptable as single-file interactive wizard

**Effort:** 3-4 hours.

---

## Implementation Plan

All work ships with v0.2.0. Eight epics, sequenced so foundational work lands first.
Full breakdown in `_bmad-output/planning-artifacts/epics-adr008.md`.

### Epic 1: Structural Decomposition (foundational — do first)

| Item | Fix | Effort |
|------|-----|--------|
| D-1 | Split daemon.rs into 7 focused modules | 4-8 hr |
| D-6 | Split bot.rs (3 modules), service.rs (3 modules) | 3-4 hr |

### Epic 2: Runtime Safety Fixes (unblocked after Epic 1)

| Item | Fix | Effort |
|------|-----|--------|
| B-1 | Fix `Box::leak` memory leak (now in daemon/files.rs) | 15 min |
| B-2 | Replace `unsafe` with safe `File::into()` | 5 min |
| B-8 | Fix UTF-8 truncate panic | 10 min |
| S-8 | Replace `Vec` with `VecDeque` for queue (now in bot/queue.rs) | 15 min |
| S-9 | Add reqwest timeout (now in bot/client.rs) | 5 min |

### Epic 3: Type Safety & Concurrency Hardening (unblocked after Epic 1)

| Item | Fix | Effort |
|------|-----|--------|
| B-9 | Convert `BridgeMessage.msg_type` to enum | 1 hr |
| B-11 | Establish lock ordering, document it | 2 hr |
| S-1 | Wrap `Config` in `Arc` | 1 hr |
| S-2 | `spawn_blocking` for SQLite ops | 3-4 hr |

### Epic 4: npm Distribution Pipeline (independent — can parallel)

| Item | Fix | Effort |
|------|-----|--------|
| B-3 | Add linux-arm64 to optionalDependencies | 1 min |
| B-4 | Add linux-arm64 CI build job | 30 min |
| B-5 | Create `npm-packages/ctm-linux-arm64/bin/.gitkeep` | 1 min |
| B-6 | Fix hardcoded version in release.yml | 5 min |
| B-7 | Delete `publish.yml` | 5 min |
| B-10 | Remove duplicate `[[bin]]` target | 2 min |

### Epic 5: Code Hygiene & DRY (unblocked after Epic 1)

| Item | Fix | Effort |
|------|-----|--------|
| S-3 | Audit and remove `#[allow(dead_code)]` | 1-2 hr |
| S-5 | Extract duplicate functions | 30 min |
| S-10 | Centralize `home_dir()` | 10 min |
| D-4 | Migrate deprecated chrono methods | 10 min |

### Epic 6: Documentation & Artifacts (independent — can parallel)

| Item | Fix | Effort |
|------|-----|--------|
| S-4 | Delete orphaned `dist/` | 1 min |
| S-6 | Update README build instructions | 15 min |
| S-7 | Fix postinstall.cjs URL | 1 min |
| D-5 | Add doc-tests for public API | 2-4 hr |

### Epic 7: Integration Test Suite

| Item | Fix | Effort |
|------|-----|--------|
| D-2 | CLI smoke, socket round-trip, daemon lifecycle, config, hooks | 8-12 hr |

### Epic 8: Binary Integrity Verification

| Item | Fix | Effort |
|------|-----|--------|
| D-3 | SHA-256 checksums + npm provenance | 4-6 hr |

---

## Verification Criteria

### Pre-Release Gate

- [ ] `cargo check` — zero warnings (including Cargo.toml)
- [ ] `cargo test` — all tests pass
- [ ] `cargo clippy -- -W clippy::all` — zero warnings
- [ ] `cargo fmt --check` — no formatting issues
- [ ] No `unsafe` blocks remain
- [ ] No `Box::leak` in production code
- [ ] `optionalDependencies` lists all 4 platform packages
- [ ] All 4 platform packages have `bin/` directory
- [ ] Single publish workflow (`release.yml` only)
- [ ] `release.yml` builds all 4 platforms
- [ ] `release.yml` uses dynamic version in propagation check
- [ ] `npm pack` on root package includes only: `scripts/`, `postinstall.cjs`, `README.md`, `SECURITY.md`
- [ ] `npm pack` on each platform package includes `bin/` with `.gitkeep`
- [ ] README build instructions reference Rust toolchain
- [ ] `dist/` directory does not exist
- [ ] Lock ordering documented in code comments

### Smoke Test Checklist

- [ ] `npm install -g claude-telegram-mirror` succeeds on linux-x64
- [ ] `ctm --version` prints `0.2.0`
- [ ] `ctm --help` shows all commands
- [ ] `ctm setup` interactive wizard completes
- [ ] `ctm doctor` reports healthy state
- [ ] `ctm daemon` starts and creates pidfile
- [ ] Telegram bot responds to `/start`

---

## Consequences

### Positive

- v0.2.0 ships with zero known runtime bugs (memory leak, UTF-8 panic, unsafe block all fixed)
- linux-arm64 users (growing market: Graviton, Pi, Ampere) get first-class support
- CI/CD pipeline is reliable and race-condition-free
- Type safety eliminates string-matching bug class in message dispatch
- Lock ordering documentation prevents future deadlocks

### Negative

- Significant upfront investment (~30-45 hours total) before release
- daemon.rs split (Epic 1) is a large refactor that must be done carefully to avoid
  regressions — all 211 tests must continue to pass after every intermediate step
- Integration tests (Epic 7) add CI time and maintenance burden

### Risks

- The daemon.rs split is the highest-risk item. Mitigation: split is purely mechanical
  (move functions between files, update imports), verified by the existing 211 unit
  tests passing at each step. No behavioral changes.
- Synchronous SQLite migration to spawn_blocking (S-2) changes concurrency semantics.
  Mitigation: existing session tests + new integration tests verify correctness.
