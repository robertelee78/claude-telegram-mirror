# ADR-009: Release Polish — Broken Windows Elimination

**Status:** Accepted
**Date:** 2026-03-17
**Authors:** Robert E. Lee
**Related:** ADR-008 (v0.2.0 Release Readiness Audit)

## Context

After the comprehensive v0.2.0 audit (ADR-008), a second pass was performed to
eliminate every remaining "broken window" — minor issues that individually are
non-blocking but collectively affect the impression of production quality. This
ADR documents the 19 fixes applied.

## Decision

Fix all identified quality issues regardless of severity. A codebase intended
as a portfolio artifact should have zero known broken windows.

## Changes

### Critical (would cause runtime failures)

1. **Process-global umask race eliminated** — `SocketServer::listen()` used
   `umask(0o177)` to set socket permissions. Since umask is per-process (not
   per-thread), concurrent code creating temp directories would inherit the
   restrictive mask, causing `Permission denied` errors. Replaced with
   post-bind `chmod(0o600)` which is thread-safe.

2. **Socket path validation tightened** — Limit changed from 256 to 104 bytes
   to match the AF_UNIX `sun_path` field size on Linux (108 minus overhead).
   Paths exceeding this would silently fail at `bind(2)`.

3. **Topic creation race condition fixed** — The check-then-insert pattern for
   topic creation locks used a read lock for checking and a separate write lock
   for inserting, allowing two concurrent handlers to both pass the check and
   create duplicate forum topics. Changed to a single atomic write lock.

### High (incorrect behavior)

4. **Rate limiter bounds** — Clamped to `[1, 30]` msgs/sec. Telegram enforces
   ~30 msgs/sec per bot; the previous code allowed unbounded values that would
   trigger 429 rate-limit errors.

5. **Message queue bounded** — Added `MAX_QUEUE_SIZE = 500` with oldest-message
   eviction. Previously unbounded, which could OOM under sustained send failures.

6. **Retry backoff overflow-safe** — Used `saturating_mul` and capped shift to
   prevent integer overflow if max retries were ever increased.

### Medium (silent failures, inconsistencies)

7. **Mirror status writes now log errors** — Previously silently discarded all
   I/O and serialization errors, making state drift invisible.

8. **Config parse functions log warnings** — Invalid env var values (e.g.
   `TELEGRAM_CHAT_ID=abc`) previously fell back to defaults silently. Now
   emit `tracing::warn` with the invalid value and the default used.

9. **`estimate_chunks` / `needs_chunking` use char count** — Were using byte
   length while `truncate()` and Telegram both use character count.

10. **`truncate()` handles `max_len < 4`** — Previously returned just `"..."`
    for any input when max_len was 3 or less. Now returns what fits without
    the ellipsis.

11. **Echo prevention key uses null separator** — Changed from `:` to `\0`.
    While session IDs can't contain `:`, using a character that is guaranteed
    impossible in both session IDs and UTF-8 text eliminates the theoretical
    collision class entirely.

### Low (code hygiene, DRY)

12. **Removed duplicate `truncate_path`** — `daemon/mod.rs` had its own
    `truncate_path()` identical to `formatting::short_path()`. Replaced all
    call sites with the shared function.

13. **Renamed `escape_markdown` → `escape_markdown_v1`** — Clarifies this is
    Telegram Markdown v1 escaping (backticks only), not MarkdownV2 which
    requires escaping 19 special characters.

14. **State file cleanup** — `.last_line_{session_id}` transcript tracking
    files are now cleaned up both on session Stop events and during periodic
    stale session cleanup. Previously accumulated indefinitely.

15. **Removed 4 duplicate `short_path` tests** — `summarize_tests.rs`
    contained copies of tests already in `formatting_tests.rs`.

16. **Enhanced truncation test coverage** — `truncate_emoji` test now covers
    `max_len < 4`, exact fit, and ellipsis boundary behavior.

### Test infrastructure

17-19. **All socket tests hardened** — Every test creating a `SocketServer`
    now explicitly sets tempdir permissions to `0o700`, providing defense-in-depth
    against any future process-global state leaks.

## Verification

- 387 tests, 0 failures, 3 consecutive clean runs
- 0 clippy warnings
- 6 doc tests pass
- All 13 CLI subcommands verified at runtime

## Consequences

- Test suite is now deterministic — no more flaky failures from umask races
- All logging paths produce actionable output on failure
- Internal consistency between formatting functions (char count everywhere)
- No dead code, no duplicate code, no misleading function names
