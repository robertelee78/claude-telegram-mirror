---
stepsCompleted: [1, 2, 3]
inputDocuments:
  - docs/adr/ADR-008-v020-release-readiness-audit.md
trackingIssue: https://github.com/robertelee78/claude-telegram-mirror/issues/3
lastUpdated: 2026-03-17
---

# claude-telegram-mirror — Epic & Story Breakdown (ADR-008: v0.2.0 Release Readiness)

## Overview

This document provides the complete epic and story breakdown for ADR-008, covering all
27 findings from the v0.2.0 release readiness audit. All items — including those
originally deferred to v0.3.0 — ship with v0.2.0 per decision on 2026-03-17.

Eight epics organized by dependency order. Epics 1 is foundational (do first). Epics
2, 3, 5 depend on Epic 1. Epics 4 and 6 are independent and can run in parallel with
anything. Epics 7 and 8 are final-phase quality gates.

## Requirements Inventory

### Functional Requirements

FR1: No `Box::leak` in production code — all string allocations must be owned or borrowed within scope
FR2: No `unsafe` blocks when safe alternatives exist in the project's minimum Rust edition (2021)
FR3: All string truncation must be UTF-8 safe (no byte-index slicing on user-facing strings)
FR4: `BridgeMessage.msg_type` uses the `MessageType` enum, not String, with serde wire compatibility
FR5: All platform packages (linux-x64, linux-arm64, darwin-arm64, darwin-x64) wired end-to-end: optionalDependencies, CI build, bin/ directory
FR6: Single publish workflow with dynamic version detection
FR7: Canonical lock ordering documented and enforced across all daemon handlers
FR8: `Config` wrapped in `Arc` — no deep clones on the hot path
FR9: SQLite operations use `spawn_blocking` — no synchronous I/O on the tokio runtime
FR10: HTTP client has a timeout — no indefinite blocking on Telegram API
FR11: Message queue uses O(1) dequeue — no `Vec::remove(0)`
FR12: No `#[allow(dead_code)]` unless justified with a comment explaining why
FR13: No duplicate utility functions — shared code extracted to common modules
FR14: `home_dir()` centralized — single implementation used by all modules
FR15: daemon.rs decomposed into 7 focused modules, each under 500 lines
FR16: bot.rs decomposed into 3 modules, service.rs decomposed into 3 modules
FR17: Integration tests cover CLI, socket, daemon lifecycle, config, and hooks
FR18: Binary integrity verified via SHA-256 checksums at resolve time
FR19: npm provenance enabled via `--provenance` flag on all publish commands
FR20: README build instructions reference Rust toolchain, not TypeScript
FR21: All documentation URLs consistent with package.json
FR22: No orphaned TypeScript artifacts in working tree
FR23: Deprecated chrono methods replaced with non-deprecated equivalents
FR24: Doc-tests on key public API functions

### NonFunctional Requirements

NFR1: All 211+ existing tests pass after every story completion
NFR2: `cargo clippy -- -W clippy::all` produces zero warnings
NFR3: `cargo fmt --check` produces no formatting issues
NFR4: No new `unsafe` blocks introduced
NFR5: Each decomposed module is under 500 lines (per CLAUDE.md rule)
NFR6: Use "whitelist" terminology, never "allowlist"

### FR Coverage Map

| FR | Epic | Stories |
|----|------|---------|
| FR1 | Epic 2 | 2.1 |
| FR2 | Epic 2 | 2.2 |
| FR3 | Epic 2 | 2.3 |
| FR4 | Epic 3 | 3.1 |
| FR5 | Epic 4 | 4.1, 4.2, 4.3 |
| FR6 | Epic 4 | 4.4, 4.5 |
| FR7 | Epic 3 | 3.2 |
| FR8 | Epic 3 | 3.3 |
| FR9 | Epic 3 | 3.4 |
| FR10 | Epic 2 | 2.5 |
| FR11 | Epic 2 | 2.4 |
| FR12 | Epic 5 | 5.1 |
| FR13 | Epic 5 | 5.2 |
| FR14 | Epic 5 | 5.3 |
| FR15 | Epic 1 | 1.1 |
| FR16 | Epic 1 | 1.2, 1.3 |
| FR17 | Epic 7 | 7.1–7.5 |
| FR18 | Epic 8 | 8.1 |
| FR19 | Epic 8 | 8.2 |
| FR20 | Epic 6 | 6.2 |
| FR21 | Epic 6 | 6.3 |
| FR22 | Epic 6 | 6.1 |
| FR23 | Epic 5 | 5.4 |
| FR24 | Epic 6 | 6.4 |

---

## Epic List

### Epic 1: Structural Decomposition — ✅ DONE (2026-03-17)
Decompose daemon.rs (3,741 lines), bot.rs (1,129 lines), and service.rs (928 lines) into focused modules under 500 lines each. This is foundational work — all subsequent daemon.rs fixes (Epics 2, 3, 5) land in the decomposed modules rather than the monolith.
**FRs covered:** FR15, FR16
**NFRs covered:** NFR1, NFR2, NFR3, NFR5
**Dependencies:** None (do first)
**Effort:** 7-12 hours

### Epic 2: Runtime Safety Fixes
Fix memory leak, eliminate unsafe block, fix UTF-8 panic, add reqwest timeout, fix queue performance. All runtime-observable bugs.
**FRs covered:** FR1, FR2, FR3, FR10, FR11
**NFRs covered:** NFR1, NFR2, NFR4
**Dependencies:** Epic 1 (B-1 lands in daemon/files.rs, S-8 in bot/queue.rs, S-9 in bot/client.rs)
**Effort:** 1 hour

### Epic 3: Type Safety & Concurrency Hardening
Convert BridgeMessage to enum dispatch, establish lock ordering, wrap Config in Arc, migrate SQLite to spawn_blocking.
**FRs covered:** FR4, FR7, FR8, FR9
**NFRs covered:** NFR1, NFR2
**Dependencies:** Epic 1 (lock ordering and Config changes span multiple daemon/ modules)
**Effort:** 7-8 hours

### Epic 4: npm Distribution Pipeline — ✅ DONE
Wire linux-arm64 end-to-end, fix CI versioning, eliminate conflicting publish workflow, fix Cargo.toml warning.
**FRs covered:** FR5, FR6
**NFRs covered:** NFR1
**Dependencies:** None (independent — can parallel with Epic 1)
**Effort:** 45 minutes
**Completed:** 2026-03-17. All 6 stories done. cargo check clean.

### Epic 5: Code Hygiene & DRY
Audit dead_code annotations, extract duplicate functions, centralize home_dir, fix deprecated chrono methods.
**FRs covered:** FR12, FR13, FR14, FR23
**NFRs covered:** NFR1, NFR2
**Dependencies:** Epic 1 (dead_code audit spans daemon/ modules)
**Effort:** 2-3 hours

### Epic 6: Documentation & Artifacts — ✅ DONE (6.1-6.3), 6.4 staged
Delete orphaned dist/, update README, fix postinstall URL, add doc-tests.
**FRs covered:** FR20, FR21, FR22, FR24
**NFRs covered:** NFR1
**Dependencies:** None (independent — can parallel with anything)
**Effort:** 3-5 hours
**Completed (6.1-6.3):** 2026-03-17. dist/ deleted, README updated, postinstall URL fixed. Story 6.4 (doc-tests) staged for after decomposition.

### Epic 7: Integration Test Suite
Add end-to-end tests for CLI, socket communication, daemon lifecycle, config loading, and hook pipeline.
**FRs covered:** FR17
**NFRs covered:** NFR1, NFR2
**Dependencies:** Epics 1-3 (test the decomposed, hardened codebase)
**Effort:** 8-12 hours

### Epic 8: Binary Integrity Verification
Add SHA-256 checksums to platform packages, verify at resolve time, enable npm provenance in CI.
**FRs covered:** FR18, FR19
**NFRs covered:** None
**Dependencies:** Epic 4 (build pipeline must be correct first)
**Effort:** 4-6 hours

---

## Dependency Graph

```
Epic 4 (npm pipeline) ──────────────────────────────────┐
Epic 6 (docs & artifacts) ──────────────────────────────┤
                                                        │
Epic 1 (structural decomposition) ──┬── Epic 2 (safety) │
                                    ├── Epic 3 (types)  ├── Epic 7 (integration tests)
                                    └── Epic 5 (hygiene)│         │
                                                        │         v
                                        Epic 8 (integrity) ◄──────┘
```

**Parallelism opportunities:**
- Epics 1, 4, 6 can all start simultaneously
- Epics 2, 3, 5 can start as soon as Epic 1 completes (and can run in parallel with each other)
- Epic 7 should start after Epics 1-3 are complete
- Epic 8 should start after Epic 4 is complete

---

## Epic 1: Structural Decomposition

Decompose the three oversized modules into focused sub-modules. This is the foundation
that makes all subsequent work cleaner to implement and review.

### Story 1.1: Decompose daemon.rs into 7 Modules

As a developer working on the daemon,
I want daemon.rs split into focused modules,
So that I can reason about, review, and modify each concern independently.

**Acceptance Criteria:**

**Given** the current daemon.rs at 3,741 lines
**When** the decomposition is complete
**Then** a `daemon/` directory exists with these modules:

| Module | Contents | Max Lines |
|--------|----------|-----------|
| `daemon/mod.rs` | `Daemon` struct, `start()`, `stop()`, `HandlerContext`, public re-exports | 300 |
| `daemon/event_loop.rs` | `run_event_loop()`, tokio select loop, timer management | 400 |
| `daemon/socket_handlers.rs` | All `handle_*` functions dispatched from socket messages | 500 |
| `daemon/telegram_handlers.rs` | Telegram command handlers, message processing | 500 |
| `daemon/callback_handlers.rs` | Callback query dispatching, approval flows | 500 |
| `daemon/cleanup.rs` | Stale session cleanup, topic deletion scheduling | 400 |
| `daemon/files.rs` | Document upload/download, `handle_telegram_document` | 400 |

**And** `mod daemon;` replaces `mod daemon;` (single file) in the module tree
**And** all 211 existing tests pass (`cargo test`)
**And** `cargo clippy -- -W clippy::all` produces zero warnings
**And** no public API changes — all existing callers compile without modification
**And** each module has its `#[cfg(test)]` section with the tests that were originally in daemon.rs for that domain

**Technical Notes:**
- This is a purely mechanical refactor: move functions, update imports, verify tests
- `HandlerContext` must be in `mod.rs` since all handler modules need it
- The 13 existing daemon tests should be distributed to the module that contains their subject
- Maintain the same function signatures — no behavioral changes

**Risks:**
- Import cycles between daemon sub-modules. Mitigation: `HandlerContext` and shared types live in `mod.rs`, handler modules only depend on `mod.rs`, not on each other.

---

### Story 1.2: Decompose bot.rs into 3 Modules

As a developer working on the Telegram bot,
I want bot.rs split into focused modules,
So that types, HTTP client logic, and queue management are separated.

**Acceptance Criteria:**

**Given** the current bot.rs at 1,129 lines
**When** the decomposition is complete
**Then** a `bot/` directory exists with:

| Module | Contents | Max Lines |
|--------|----------|-----------|
| `bot/mod.rs` | `TelegramBot` struct, public API, re-exports | 200 |
| `bot/types.rs` | API response types, `Update`, `Message`, `CallbackQuery`, etc. | 300 |
| `bot/client.rs` | HTTP methods (`send_message`, `send_photo`, `send_document`, polling) | 400 |
| `bot/queue.rs` | `MessageQueue`, rate limiting, `process_queue` | 300 |

**And** all tests pass, zero clippy warnings
**And** no public API changes

---

### Story 1.3: Decompose service.rs into 3 Modules

As a developer working on service management,
I want service.rs split by platform,
So that systemd and launchd logic are isolated.

**Acceptance Criteria:**

**Given** the current service.rs at 928 lines
**When** the decomposition is complete
**Then** a `service/` directory exists with:

| Module | Contents | Max Lines |
|--------|----------|-----------|
| `service/mod.rs` | `ServiceManager`, platform dispatch, public API | 200 |
| `service/env.rs` | Environment file parsing and generation | 200 |
| `service/systemd.rs` | systemd unit file generation and lifecycle | 300 |
| `service/launchd.rs` | launchd plist generation and lifecycle | 300 |

**And** all tests pass, zero clippy warnings
**And** no public API changes

---

## Epic 2: Runtime Safety Fixes

Fix all runtime-observable bugs identified in the audit. These are the changes users
would actually notice.

### Story 2.1: Fix Box::leak Memory Leak (B-1)

As a user sending unnamed documents via Telegram,
I want the daemon to not leak memory,
So that long-running daemon instances remain stable.

**Acceptance Criteria:**

**Given** a Telegram document message with no filename and mime_type `application/pdf`
**When** `handle_telegram_document` processes it
**Then** the generated filename `unnamed.pdf` is an owned `String`, not a `Box::leak`'d `&'static str`
**And** no memory is permanently leaked
**And** the function's caller accepts `String` or `Cow<str>` instead of requiring `&str`

**File:** `rust-crates/ctm/src/daemon/files.rs` (post-decomposition)

**Test:**
- Existing document handling tests continue to pass
- Add a test that processes 1000 unnamed documents and verifies no panic or OOM signal

---

### Story 2.2: Replace unsafe Block with Safe Alternative (B-2)

As a maintainer,
I want zero `unsafe` blocks when safe alternatives exist,
So that the codebase is auditable and panic-safe.

**Acceptance Criteria:**

**Given** the `acquire_flock` function in socket.rs
**When** converting a `File` to `OwnedFd`
**Then** use `let owned_fd: OwnedFd = lock_file.into();` (safe, stable since Rust 1.63)
**And** remove the `unsafe` block and the `use std::os::unix::io::FromRawFd` import
**And** `grep -r "unsafe" rust-crates/ctm/src/` returns zero results

**File:** `rust-crates/ctm/src/socket.rs:331`

---

### Story 2.3: Fix UTF-8 Panic in summarize.rs Truncate (B-8)

As a user with non-ASCII content in tool results,
I want truncation to handle multi-byte characters safely,
So that the daemon doesn't panic on Japanese filenames, emoji URLs, etc.

**Acceptance Criteria:**

**Given** a string containing multi-byte UTF-8 characters (e.g., `"こんにちは世界"`)
**When** `truncate(s, 5)` is called
**Then** it returns the first 5 *characters* (not bytes) without panicking
**And** the `truncate` function in `summarize.rs` is replaced with a call to the safe version from `formatting.rs` (see also Story 5.2)

**Test:**
- Add test: `truncate("こんにちは世界", 5)` returns `"こんにちは"`
- Add test: `truncate("hello🌍world", 6)` returns `"hello🌍"`
- Add test: `truncate("ascii", 10)` returns `"ascii"` (no change when under limit)

---

### Story 2.4: Replace Vec::remove(0) with VecDeque (S-8)

As a bot processing many queued messages,
I want O(1) dequeue performance,
So that message processing doesn't degrade with queue depth.

**Acceptance Criteria:**

**Given** the message queue in bot (post-decomposition: `bot/queue.rs`)
**When** dequeueing a message
**Then** `VecDeque::pop_front()` is used instead of `Vec::remove(0)`
**And** the queue type is `VecDeque<QueuedMessage>`
**And** `use std::collections::VecDeque` is added

**File:** `rust-crates/ctm/src/bot/queue.rs` (post-decomposition)

---

### Story 2.5: Add reqwest Client Timeout (S-9)

As a daemon operator,
I want HTTP requests to have a timeout,
So that a hung Telegram API doesn't block the daemon indefinitely.

**Acceptance Criteria:**

**Given** the `TelegramBot` HTTP client construction
**When** creating the `reqwest::Client`
**Then** use `Client::builder().timeout(Duration::from_secs(30)).build()?`
**And** `use std::time::Duration` is added if not present

**File:** `rust-crates/ctm/src/bot/client.rs` (post-decomposition)

---

## Epic 3: Type Safety & Concurrency Hardening

Eliminate an entire class of string-matching bugs, prevent deadlocks, and fix
performance issues in the hot path.

### Story 3.1: Convert BridgeMessage.msg_type to MessageType Enum (B-9)

As a developer adding new message types,
I want the compiler to catch message type mismatches,
So that typos in string literals can't silently drop messages.

**Acceptance Criteria:**

**Given** `BridgeMessage` in `types.rs` with `msg_type: String`
**When** the conversion is complete
**Then** `BridgeMessage.msg_type` is `MessageType` (the existing enum at types.rs:118)
**And** `MessageType` has `#[serde(rename_all = "snake_case")]` for wire compatibility
**And** the `#[serde(rename = "type")]` attribute is on the field for JSON key `"type"`
**And** all `msg.msg_type.as_str()` match arms in daemon socket handlers are replaced with pattern matching on `MessageType` variants
**And** the `_` catch-all arm uses `MessageType` exhaustive matching (no silent drops)
**And** all existing tests pass

**Files:** `types.rs`, `daemon/socket_handlers.rs`, `daemon/event_loop.rs`

**Technical Notes:**
- Verify the existing `MessageType` enum has all variants used in daemon.rs match arms
- Add any missing variants discovered during conversion
- Use `#[serde(other)]` on a catch-all variant for forward compatibility with unknown message types from the socket

---

### Story 3.2: Establish Canonical Lock Ordering (B-11)

As a daemon handling concurrent events,
I want consistent lock acquisition order,
So that deadlocks cannot occur under concurrent handler execution.

**Acceptance Criteria:**

**Given** the daemon has multiple shared state locks (`sessions`, `session_threads`, `session_tmux`, etc.)
**When** any handler acquires multiple locks
**Then** locks are always acquired in this canonical order:
1. `sessions` (SQLite `Mutex`)
2. `session_threads` (`RwLock`)
3. `session_tmux` (`RwLock`)
4. All remaining `RwLock`s in struct declaration order

**And** a comment block at the top of `daemon/mod.rs` documents the lock ordering:
```rust
// LOCK ORDERING (acquire in this order to prevent deadlocks):
// 1. sessions (Mutex<SessionManager>)
// 2. session_threads (RwLock<HashMap>)
// 3. session_tmux (RwLock<HashMap>)
// 4. All other RwLocks in HandlerContext field declaration order
```

**And** `get_tmux_target` is refactored to acquire locks in the canonical order
**And** all other handlers with multi-lock acquisition are verified or fixed
**And** all existing tests pass

**Files:** `daemon/mod.rs`, `daemon/socket_handlers.rs`, `daemon/telegram_handlers.rs`

---

### Story 3.3: Wrap Config in Arc (S-1)

As a daemon processing high-frequency events,
I want Config to be shared by reference,
So that the hot path doesn't deep-clone strings on every event.

**Acceptance Criteria:**

**Given** `HandlerContext` constructs `Config` via `.clone()` on every event
**When** the fix is applied
**Then** `Config` is wrapped in `Arc<Config>` in the daemon's startup code
**And** `HandlerContext.config` is `Arc<Config>` instead of `Config`
**And** a single `HandlerContext` template is pre-constructed and `.clone()`'d (16 atomic increments, zero string copies)
**And** all handlers access config via `ctx.config.field` (transparent through Arc deref)
**And** all existing tests pass

**File:** `daemon/mod.rs`, `daemon/event_loop.rs`

---

### Story 3.4: Migrate SQLite to spawn_blocking (S-2)

As a daemon under concurrent load,
I want database I/O off the tokio runtime threads,
So that synchronous SQLite operations don't block async event processing.

**Acceptance Criteria:**

**Given** `SessionManager` wraps `rusqlite::Connection` in `tokio::sync::Mutex`
**When** any handler calls a SessionManager method
**Then** the call is wrapped in `tokio::task::spawn_blocking`
**And** the `tokio::sync::Mutex` is replaced with `std::sync::Mutex` (spawn_blocking runs on a blocking thread, not the async runtime)
**And** double-locking patterns (lock, read, drop, lock, write) are consolidated into single lock acquisitions where possible
**And** all 17 session tests pass
**And** all daemon tests pass

**Files:** `session.rs`, `daemon/socket_handlers.rs`, `daemon/telegram_handlers.rs`, `daemon/cleanup.rs`

**Technical Notes:**
- `spawn_blocking` moves the closure to a dedicated thread pool — `std::sync::Mutex` is appropriate here
- The `SessionManager` struct remains the same; only the call sites change
- Consider creating a `SessionManager::with<F, R>(&self, f: F) -> R` helper that handles the spawn_blocking + mutex dance

**Risks:**
- Changing `tokio::sync::Mutex` to `std::sync::Mutex` means `.lock()` returns `Result` (poison) instead of a future. All call sites must be updated.

---

## Epic 4: npm Distribution Pipeline

Wire linux-arm64 end-to-end and fix the broken CI/CD pipeline. This epic is
independent of the Rust code changes and can run in parallel with Epic 1.

### Story 4.1: Wire linux-arm64 into optionalDependencies (B-3)

As an ARM64 Linux user (Raspberry Pi, Graviton, Ampere),
I want `npm install -g claude-telegram-mirror` to install my platform binary,
So that I can use CTM without building from source.

**Acceptance Criteria:**

**Given** `package.json` optionalDependencies
**When** the fix is applied
**Then** `"@agidreams/ctm-linux-arm64": "0.2.0"` is present alongside the other 3 packages
**And** `npm install` on linux-arm64 resolves the correct package

---

### Story 4.2: Create linux-arm64 bin/ Directory (B-5)

As the CI pipeline building linux-arm64,
I want the platform package to have a bin/ directory,
So that `npm pack` produces a non-empty package and the binary has a place to land.

**Acceptance Criteria:**

**Given** `npm-packages/ctm-linux-arm64/`
**When** the fix is applied
**Then** `npm-packages/ctm-linux-arm64/bin/.gitkeep` exists
**And** the directory structure matches the other 3 platform packages

---

### Story 4.3: Add linux-arm64 CI Build Job (B-4)

As a release engineer,
I want the CI pipeline to build linux-arm64 binaries,
So that the platform package contains an actual binary on publish.

**Acceptance Criteria:**

**Given** `.github/workflows/release.yml`
**When** the linux-arm64 job is added
**Then** a `build-linux-arm64` job exists that:
  - Uses `ubuntu-latest` (or `ubuntu-24.04-arm` if available)
  - Installs `aarch64-unknown-linux-gnu` target via `rustup target add`
  - Installs the `gcc-aarch64-linux-gnu` cross-linker
  - Builds with `cargo build --release --target aarch64-unknown-linux-gnu`
  - Uploads the binary as a GitHub Actions artifact
**And** the `publish` job's `needs` array includes `build-linux-arm64`
**And** the platform publish loop includes `ctm-linux-arm64`

---

### Story 4.4: Fix Hardcoded Version in Release Pipeline (B-6)

As a release engineer publishing v0.2.0+,
I want the registry propagation check to use the actual version,
So that the publish pipeline doesn't check the wrong version.

**Acceptance Criteria:**

**Given** `.github/workflows/release.yml` propagation check
**When** the fix is applied
**Then** the version is extracted dynamically:
```yaml
VERSION=$(node -p "require('./package.json').version")
```
**And** all `npm view` commands use `@$VERSION` instead of `@0.1.0`

---

### Story 4.5: Delete Conflicting publish.yml Workflow (B-7)

As a release engineer,
I want a single publish workflow,
So that releases don't trigger race conditions between competing pipelines.

**Acceptance Criteria:**

**Given** `.github/workflows/publish.yml` exists and conflicts with `release.yml`
**When** the fix is applied
**Then** `publish.yml` is deleted
**And** `release.yml` is the sole publish workflow
**And** `release.yml` handles: build all 4 platforms → publish platform packages → wait for propagation → publish main package

---

### Story 4.6: Remove Duplicate [[bin]] Target (B-10)

As a developer running cargo commands,
I want zero Cargo.toml warnings,
So that real warnings aren't drowned out by noise.

**Acceptance Criteria:**

**Given** two `[[bin]]` entries in `rust-crates/ctm/Cargo.toml` both pointing at `src/main.rs`
**When** the fix is applied
**Then** only the `ctm` bin target remains
**And** the `claude-telegram-mirror` bin target is removed (the npm wrapper provides this alias)
**And** `cargo check 2>&1 | grep -i warning` returns nothing

---

## Epic 5: Code Hygiene & DRY

Clean up dead code annotations, extract duplicate functions, and fix deprecated API usage.

### Story 5.1: Audit and Remove #[allow(dead_code)] (S-3)

As a maintainer,
I want the compiler to catch unused code,
So that dead code doesn't accumulate silently.

**Acceptance Criteria:**

**Given** `#[allow(dead_code)]` on 6+ modules in main.rs, all of error.rs, and TelegramBot impl
**When** the audit is complete
**Then** each annotation is either:
  - **Removed** (code is actually used — was suppressing a false positive from lib/bin split)
  - **Removed along with the dead code** (code is genuinely unused)
  - **Retained with a justifying comment** (rare — only for intentional future API surface)
**And** `cargo check` passes with no dead_code warnings (or only justified ones)
**And** the lib.rs public API is configured correctly so lib consumers don't trigger warnings

**Technical Notes:**
- The lib/bin split likely causes many false positives: code used by the binary but not exported from the library. Fix by either making the lib export them or by removing the lib target if it's not independently consumed.

---

### Story 5.2: Extract Duplicate Utility Functions (S-5)

As a maintainer,
I want shared logic in one place,
So that bug fixes apply everywhere and there's no behavioral divergence.

**Acceptance Criteria:**

**Given** duplicate functions across modules:
- `short_path`: `formatting.rs:404` and `summarize.rs:11`
- `truncate`: `formatting.rs:376` and `summarize.rs:33`
- Color helpers (`cyan`, `green`, `yellow`, `red`, `gray`, `bold`): `doctor.rs:16-33` and `setup.rs:18-35`

**When** extraction is complete
**Then:**
- `short_path` is `pub` in `formatting.rs`, imported by `summarize.rs`
- `truncate` is `pub` in `formatting.rs` (the UTF-8 safe version), imported by `summarize.rs` (the unsafe version is deleted — see Story 2.3)
- Color helpers are in a new `colors.rs` module, imported by `doctor.rs` and `setup.rs`
**And** all tests pass
**And** `grep -rn "fn short_path" rust-crates/ctm/src/` returns exactly 1 result
**And** `grep -rn "fn truncate" rust-crates/ctm/src/` returns exactly 1 result

---

### Story 5.3: Centralize home_dir() Fallback (S-10)

As a maintainer,
I want `home_dir()` defined once,
So that the fallback behavior is consistent and changeable in one place.

**Acceptance Criteria:**

**Given** `dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))` duplicated in `service.rs`, `setup.rs`, `installer.rs`, `doctor.rs`
**When** centralization is complete
**Then** `pub fn home_dir() -> PathBuf` exists in `config.rs`
**And** all 4 call sites import and use `config::home_dir()`
**And** `grep -rn "dirs::home_dir" rust-crates/ctm/src/` returns exactly 1 result (in config.rs)

---

### Story 5.4: Migrate Deprecated chrono Methods (D-4)

As a maintainer,
I want no deprecated API usage,
So that future dependency upgrades don't break the build.

**Acceptance Criteria:**

**Given** `chrono::Duration::hours()` at session.rs:557 and `chrono::Duration::days()` at daemon (post-decomposition: cleanup.rs)
**When** migration is complete
**Then** replaced with `chrono::TimeDelta::try_hours().unwrap()` and `chrono::TimeDelta::try_days().unwrap()`
**And** `cargo check 2>&1 | grep -i deprecated` returns nothing

---

## Epic 6: Documentation & Artifacts

Clean up migration artifacts and ensure all documentation is accurate for a Rust-only project.

### Story 6.1: Delete Orphaned dist/ Directory (S-4)

As a developer cloning the repo,
I want no confusing TypeScript artifacts,
So that the working tree reflects the actual Rust-only project.

**Acceptance Criteria:**

**Given** `/opt/claude-telegram-mirror/dist/` contains 896 KB of orphaned .js/.d.ts files
**When** the cleanup is complete
**Then** the `dist/` directory does not exist
**And** `.gitignore` includes `dist/` to prevent re-creation

---

### Story 6.2: Update README Build Instructions (S-6)

As a developer building from source,
I want accurate build instructions,
So that I can build and run CTM without guessing.

**Acceptance Criteria:**

**Given** README.md lines 340-369 reference `npm run build` and `node dist/cli.js`
**When** the update is complete
**Then** the "Development / From Source" section shows:
```bash
cd rust-crates && cargo build --release
# Binary at: rust-crates/target/release/ctm
./rust-crates/target/release/ctm --help
```
**And** the "Installation" section clarifies: `npm install -g claude-telegram-mirror`
**And** no references to `npm run build` or `node dist/` remain in README.md

---

### Story 6.3: Fix postinstall.cjs Documentation URL (S-7)

As a user seeing the postinstall message,
I want the URL to point to the correct repository,
So that I can find documentation and report issues.

**Acceptance Criteria:**

**Given** `postinstall.cjs:44` points to `github.com/robertelee78/claude-mobile`
**When** the fix is applied
**Then** the URL is `github.com/robertelee78/claude-telegram-mirror`
**And** it matches `package.json` homepage field

---

### Story 6.4: Add Doc-Tests for Public API (D-5)

As a library consumer or contributor,
I want executable examples in doc comments,
So that I can understand API usage and the examples stay correct.

**Acceptance Criteria:**

**Given** zero doc-tests exist
**When** doc-tests are added
**Then** at minimum these functions have `/// # Examples` with runnable doc-tests:
- `formatting::escape_markdown_v2`
- `formatting::truncate`
- `formatting::short_path`
- `config::load_config`
- `config::home_dir`
- `types::is_valid_session_id`
- `types::is_valid_slash_command`
- `summarize::summarize_tool_result` (at least 1 example)
**And** `cargo test --doc` passes

---

## Epic 7: Integration Test Suite

Add end-to-end tests that exercise the binary and cross-module interactions.

### Story 7.1: CLI Smoke Tests

As a QA engineer,
I want tests that verify the CLI interface,
So that argument parsing and help output don't regress.

**Acceptance Criteria:**

**Given** the `ctm` binary built with `cargo build`
**When** integration tests run
**Then** these cases pass:
- `ctm --help` exits 0 and contains "Claude Telegram Mirror"
- `ctm --version` exits 0 and prints the version from Cargo.toml
- `ctm invalid-command` exits non-zero with an error message
- `ctm daemon --help` exits 0 and shows daemon-specific options
- `ctm setup --help` exits 0 and shows setup-specific options

**File:** `rust-crates/ctm/tests/cli_smoke.rs`

---

### Story 7.2: Socket Round-Trip Tests

As a QA engineer,
I want tests that verify socket communication,
So that the daemon↔CLI message path doesn't regress.

**Acceptance Criteria:**

**Given** a socket server started in a temp directory
**When** a client connects and sends a `BridgeMessage`
**Then** the server receives the exact message
**And** the client receives any response the server sends
**And** the flock mechanism prevents duplicate server instances
**And** cleanup removes the socket file on shutdown

**File:** `rust-crates/ctm/tests/socket_roundtrip.rs`

---

### Story 7.3: Daemon Lifecycle Tests

As a QA engineer,
I want tests that verify daemon start/stop,
So that pidfile management and signal handling don't regress.

**Acceptance Criteria:**

**Given** a test environment with temp config directory
**When** the daemon starts
**Then** a pidfile is created with the correct PID
**And** the socket file is created
**When** SIGTERM is sent
**Then** the daemon shuts down gracefully
**And** the pidfile is removed
**And** the socket file is removed

**File:** `rust-crates/ctm/tests/daemon_lifecycle.rs`

---

### Story 7.4: Config Validation Tests

As a QA engineer,
I want end-to-end config loading tests,
So that edge cases in real config files are caught.

**Acceptance Criteria:**

**Given** real config files in a temp directory
**When** config is loaded
**Then** these cases pass:
- Valid config with all fields loads successfully
- Missing config file returns appropriate error
- Config with missing required fields returns specific error
- Config with invalid bot token format returns specific error
- Config file with wrong permissions (0o644 instead of 0o600) loads with a warning

**File:** `rust-crates/ctm/tests/config_validation.rs`

---

### Story 7.5: Hook Pipeline Tests

As a QA engineer,
I want tests that verify hook event processing end-to-end,
So that Claude Code integration doesn't regress.

**Acceptance Criteria:**

**Given** a running socket server (simulated daemon)
**When** a hook event JSON is sent via the socket client
**Then** the server correctly parses the hook event
**And** the correct handler is dispatched based on event type
**And** approval requests generate the expected response format

**File:** `rust-crates/ctm/tests/hook_pipeline.rs`

---

## Epic 8: Binary Integrity Verification

Add supply-chain security measures to the npm binary distribution.

### Story 8.1: Add SHA-256 Checksums to Platform Packages

As a security-conscious user,
I want the binary resolver to verify checksums,
So that a compromised npm registry can't serve malicious binaries.

**Acceptance Criteria:**

**Given** each platform package (linux-x64, linux-arm64, darwin-arm64, darwin-x64)
**When** the CI pipeline builds a release
**Then** a `checksums.json` file is generated alongside the binary in each platform package:
```json
{
  "ctm": {
    "sha256": "<hex digest>",
    "size": <bytes>
  }
}
```
**And** `resolve-binary.cjs` reads `checksums.json` from the resolved package
**And** the resolved binary's SHA-256 is computed and compared
**And** a mismatch aborts with a clear error: `"Binary integrity check failed"`
**And** missing checksums.json falls back to current behavior with a warning

**Files:** `scripts/resolve-binary.cjs`, `.github/workflows/release.yml`

---

### Story 8.2: Enable npm Provenance in CI

As a supply-chain security engineer,
I want npm provenance attestation on all published packages,
So that users can verify packages were built from the claimed source.

**Acceptance Criteria:**

**Given** `.github/workflows/release.yml` publish steps
**When** provenance is enabled
**Then** all `npm publish` commands include `--provenance`
**And** the workflow has `permissions: id-token: write` at the job level
**And** published packages show provenance on npmjs.com

---

## Verification Gate

All stories are complete when the ADR-008 Pre-Release Gate checklist passes:

- [ ] `cargo check` — zero warnings (including Cargo.toml)
- [ ] `cargo test` — all tests pass (unit + integration)
- [ ] `cargo test --doc` — all doc-tests pass
- [ ] `cargo clippy -- -W clippy::all` — zero warnings
- [ ] `cargo fmt --check` — no formatting issues
- [ ] No `unsafe` blocks remain
- [ ] No `Box::leak` in production code
- [ ] No `#[allow(dead_code)]` without justifying comment
- [ ] `optionalDependencies` lists all 4 platform packages
- [ ] All 4 platform packages have `bin/` directory
- [ ] Single publish workflow (`release.yml` only)
- [ ] `release.yml` builds all 4 platforms with dynamic versioning
- [ ] README build instructions reference Rust toolchain
- [ ] `dist/` directory does not exist
- [ ] Lock ordering documented in daemon/mod.rs
- [ ] All duplicate functions extracted to single source
- [ ] Binary integrity verification in resolve-binary.cjs
- [ ] npm provenance enabled in release.yml

### Smoke Test Checklist

- [ ] `npm install -g claude-telegram-mirror` succeeds on linux-x64
- [ ] `ctm --version` prints `0.2.0`
- [ ] `ctm --help` shows all commands
- [ ] `ctm setup` interactive wizard completes
- [ ] `ctm doctor` reports healthy state
- [ ] `ctm daemon` starts and creates pidfile
- [ ] Telegram bot responds to `/start`
