# ADR-013: Session Hierarchy and tmux Reliability

**Status:** Implemented (all gaps closed — GAP-7 resolved via temporal correlation fallback)
**Date:** 2026-03-18
**Authors:** Robert, Claude

DO NOT BE LAZY. We have plenty of time to do it right. No short cuts. Never make assumptions. Always dive deep and ensure you know the problem you're solving. Make use of search as needed.  Measure 3x, cut once.  No fallback. No stub (todo later) code.  Just pure excellence, done the right way the entire time. Also recall Chesterton's fence; always understand current fully before changing it.

## Context

Two critical UX problems in the Claude Code ↔ Telegram bridge:

1. **tmux injection is brittle** — 90% of sessions have `tmux_target = NULL` in the DB. When a Telegram user sends a message back to Claude Code, the injection silently fails or targets the wrong pane.

2. **Sub-agent topic sprawl** — When the parent session spawns sub-agents via the Agent tool (e.g., CFA research swarms), each sub-agent creates a new Telegram forum topic. The user sees dozens of topics when they expect one.

## Decision

### Part A: tmux Reliability (6 Fixes)

#### F1: Hook environment — `$TMUX` not set
**Problem:** `detect_tmux_session()` reads `$TMUX` env var. If the hook process doesn't inherit tmux context (90% of sessions), no tmuxTarget is sent.
**Fix:** In `get_tmux_target()`, when both cache and DB return None, actively call `InputInjector::find_claude_code_session()` as a runtime fallback. Store the result.

#### F2: UPDATE-before-INSERT race
**Problem:** `check_and_update_tmux_target()` runs before `handle_session_start()` creates the DB row. The UPDATE touches zero rows. Silent.
**Fix:** Use `INSERT OR REPLACE` semantics in `set_tmux_info()`, or move `check_and_update_tmux_target()` to run after session creation.

#### F3: `create_session` idempotency guard discards tmux_target
**Problem:** When `create_session()` finds an existing session, it only calls `update_activity()`. tmux_target, hostname, project_dir are discarded.
**Fix:** When the session exists and tmux_target is provided, call `set_tmux_info()` in the early-return path.

#### F4: Session dedup skips `handle_session_start`
**Problem:** Active sessions skip `handle_session_start` entirely (dedup at mod.rs:590). tmux metadata from the hook is lost.
**Fix:** Mitigated by F2 fix (`check_and_update_tmux_target` runs first and stores correctly after F2/F3).

#### F5: `session_end` clears tmux cache
**Problem:** `handle_session_end` removes from in-memory cache but DB stays NULL.
**Fix:** Acceptable behavior. Session has ended — tmux cache should be cleared.

#### F6: No runtime re-detection fallback
**Problem:** `get_tmux_target()` returns None when cache and DB are empty. No fallback.
**Fix:** Add live detection as final fallback in `get_tmux_target()`:
```rust
// Fallback: try to detect tmux pane at runtime
if let Some(info) = InputInjector::detect_tmux_session() {
    // Store in cache and DB, then return
}
if let Some(session) = InputInjector::find_claude_code_session() {
    // Store in cache and DB, then return
}
```

#### F7: Startup default causes wrong-pane injection
**Problem:** If `get_tmux_target` returns None but injector has a startup default, it injects to the wrong pane.
**Fix:** F6 eliminates this case — get_tmux_target always returns a valid target or None. The injector's startup default should only be used as part of F6's fallback chain, not as a silent global default.

#### F8: Daemon restart loses in-memory state
**Problem:** After restart, `session_tmux_targets` cache is empty.
**Fix:** On startup, after tmux auto-detection, iterate active sessions from DB and populate the in-memory cache with any stored tmux_target values.

### Part B: Parent-Child Session Routing

#### Discovery: transcript_path contains parent session_id (same-cwd only)

Sub-agent transcripts follow the path pattern **when the sub-agent shares the parent's `cwd`**:
```
~/.claude/projects/{project-key}/{parentSessionId}/subagents/agent-{agentId}.jsonl
```

The `transcript_path` is already transmitted to the daemon in every hook message's metadata. Extraction:

```rust
fn extract_parent_session_id(path: &str) -> Option<&str> {
    let (parent_part, _) = path.split_once("/subagents/")?;
    parent_part.rsplit('/').next()
}
```

This provides deterministic parent-child mapping **only when the sub-agent's `cwd` maps to the same project key as the parent**.

#### Limitation: Cross-cwd sub-agents (GAP-7)

Claude Code derives the project key from `cwd` by replacing `/` with `-`. When the Agent tool spawns a sub-agent that operates in a different directory (e.g., parent at `/opt/project` → sub-agent at `/home/user`), the transcript is stored as a **flat top-level session** under the new project key:

```
# Same-cwd sub-agent (detected correctly):
~/.claude/projects/-opt-project/{parentId}/subagents/agent-{id}.jsonl

# Cross-cwd sub-agent (NOT detected — no /subagents/ segment):
~/.claude/projects/-home-user/{sub-agent-uuid}.jsonl
```

The `extract_parent_session_id()` function finds no `/subagents/` to split on and returns `None`. The sub-agent is treated as an independent top-level session and creates its own Telegram topic.

**Evidence (2026-03-18 production investigation):**
- Database query: `SELECT COUNT(*) FROM sessions WHERE parent_session_id IS NOT NULL` → **0 rows**. Parent detection has never successfully fired in production.
- All sub-agent sessions observed had `project_dir = '/home/robert'` while the parent was at `/opt/claude-telegram-mirror`.
- Daemon logs showed `transcript_path canonicalization failed` warnings, but these are **unrelated** to parent detection — `validate_transcript_path()` is only called for JSONL content extraction, not for the `/subagents/` check.
- The `/subagents/` directory convention IS real and IS used by Claude Code — but only within the same project key scope.

#### Session DB Schema Changes

```sql
ALTER TABLE sessions ADD COLUMN parent_session_id TEXT;
ALTER TABLE sessions ADD COLUMN agent_id TEXT;
```

#### Routing Logic

In `handle_session_start()`, before creating a new topic:

1. Parse `transcript_path` from metadata for `/subagents/` pattern
2. If present, extract `parent_session_id` and `agent_id`
3. Look up the parent session's `thread_id`
4. If found, reuse the parent's `thread_id` for this child session (no new topic)
5. Store `parent_session_id` and `agent_id` in the child's DB row

#### Message Routing

When a child session sends messages (agent_response, tool_start, etc.):

- Route to the parent's Telegram topic (using parent's thread_id)
- Prefix messages with a sub-agent label: `"🤖 [Agent: {agent_type}] {content}"`

### Part C: Sub-Agent UX in Parent Topic

#### One-liner Summary

When a sub-agent starts, send a one-line message in the parent topic:
```
🤖 Agent spawned: researcher-1 (Explore)
```

When it completes (`SubagentStop`), update to:
```
✅ Agent completed: researcher-1 (Explore)
[Details] button
```

#### Details Expansion

Tapping "Details" sends:
1. A reply message with a summary (first ~500 chars of `last_assistant_message`)
2. A `.md` file attachment with the full sub-agent output via `send_document`

This uses the existing file transfer capability (`bot.send_document`).

#### Sub-Agent Identity

Each sub-agent is identified by:
- **PID equivalent:** `agent_id` from the transcript path (e.g., `agent-abc123`)
- **PPID equivalent:** `parent_session_id` from the transcript path
- **Type:** `agent_type` from `SubagentStop` event (e.g., `"Explore"`, `"researcher"`)

### Part D: UX Requirements (from PM Q&A)

#### D1: Silent failure is unacceptable
When a Telegram reply fails to inject into Claude Code, the user MUST see feedback. No silent drops.

#### D2: Warning on every failed reply
Every time a Telegram message fails to inject, show: `"⚠️ Reply failed — tmux not detected. Start Claude Code inside tmux for bidirectional chat."`

#### D3: tmux status indicator in session start
When a session starts, the session info message should include tmux connectivity:
- `🟢 tmux: connected (0:0.0)` — when tmux_target is known
- `🔴 tmux: not detected — replies disabled` — when no tmux target

#### D4: Auto-healing
When the user exits Claude Code, starts tmux, and resumes (`claude --resume`), the next hook event carries `$TMUX`. The daemon updates the target, and subsequent Telegram replies work. The status indicator updates automatically on the next message.

#### D5: Re-detect on cache miss, not proactively
`get_tmux_target()` checks: in-memory cache → DB → **live detection fallback** (~100ms). Only the cache miss path pays the detection cost. When tmux is cached, zero overhead.

#### D6: No-tmux is view-only with clear warning
Without tmux, Telegram is read-only (Claude→Telegram works via hooks+socket). The user sees a clear warning, not silence.

#### D7: Sub-agent Details via file transfer
"Details" button sends: summary reply (~500 chars) + `.md` file attachment of full output via `send_document`. No topic sprawl.

### Part E: Session ↔ Topic Lifecycle (from PM Q&A)

#### E1: 1:1 session-to-topic mapping
Every Claude Code session gets exactly one Telegram topic. Sub-agents route to their parent session's topic (not new topics). Two independent Claude sessions in the same project get separate topics.

#### E2: Stale topic auto-cleanup (two triggers)
- **Session ended:** Topic deleted 15 minutes after the session ends.
- **Inactivity:** Topic deleted after 12 hours (720 minutes) of no activity, even if the session is still technically "active" in the DB.

Both thresholds should be configurable.

#### E3: Auto-heal on resume/reactivation
If a topic was deleted (stale) and the user later resumes the session (`claude --resume`) or a new hook event arrives for that session_id:
- Create a fresh topic
- Send a "Session resumed" context message (custom title, duration, last activity from the old session)
- Re-associate the session with the new thread_id in the DB

This is already partially implemented (ensure_session_exists creates topics on-demand). The enhancement is the context message.

#### E4: Topic title follows Claude Code renames
Keep existing behavior: when Claude Code assigns a custom title (auto or manual `/rename`), the Telegram topic title updates. Experimental: prepend active sub-agent count (e.g., `"[2 agents] Fix auth bug"`). Can be toggled off if too noisy.

#### E5: Closed vs deleted
Prefer CLOSE over DELETE for the 15-minute post-session-end window (preserves history, topic is hidden from list). DELETE after the 12-hour inactivity threshold (full cleanup).

## Implementation Plan

### Phase 1: tmux Reliability (Fixes F1-F4, F6-F8)
- **Files:** `mod.rs`, `session.rs`, `socket_handlers.rs`, `telegram_handlers.rs`
- **Estimated:** ~100 lines changed
- **Risk:** Low — fixes silent failures, no behavior change for working paths

### Phase 2: Session DB Schema + Parent Detection
- **Files:** `session.rs` (migration), `types.rs`, `hook.rs`, `socket_handlers.rs`
- **Estimated:** ~80 lines changed
- **Risk:** Low — additive schema change, backward compatible

### Phase 3: Child-to-Parent Topic Routing
- **Files:** `socket_handlers.rs`, `mod.rs`
- **Estimated:** ~60 lines changed
- **Risk:** Medium — changes topic creation logic, needs careful testing

### Phase 4: Sub-Agent UX (One-liner + Details + File)
- **Files:** `callback_handlers.rs`, `socket_handlers.rs`
- **Estimated:** ~120 lines changed
- **Risk:** Medium — new callback type, new message format

## Alternatives Considered

### `/rename` trick to encode parent_session_id
Rejected. Sub-agents cannot trigger rename on the parent's topic. The `SubagentStop` handler in the hook binary is a no-op. Even if it worked, the rename would be attributed to the sub-agent's session_id.

### Heuristic grouping by host + time window
Previously rejected as primary approach due to false positive risk. Now **adopted as fallback** for GAP-7 (cross-cwd sub-agents). Implemented with safeguards: requires no-tmux on child (sub-agents are background processes), tmux on parent (real user session), same hostname, configurable time window (default 60s), excludes other sub-agents from matching. False positive risk is low in practice — you'd need two independent Claude Code sessions starting within 60s on the same machine, one with tmux and one without.

### Separate topics with naming convention
Rejected. Causes the exact topic sprawl the user complained about. Telegram topics are flat — no nesting.

### Wait for upstream `parent_session_id`
Not viable as sole strategy. GitHub issues #7881, #14859, #16424 are open with no timeline. The transcript_path approach works today.

## Implementation Audit (2026-03-18)

A five-agent CFA swarm audited the implementation against every requirement in this ADR. Four researchers examined Parts A–E independently; a queen coordinator synthesized the findings.

### Per-Part Grades (post-remediation)

| Part | Grade | Verdict |
|------|-------|---------|
| **A** — Tmux Reliability (F1-F8) | **A** | All 8 fixes implemented. Three-tier lookup (cache → DB → live detection) is excellent. |
| **B** — Parent-Child Routing | **A-** | ~~D~~ → A-. GAP-7 resolved: temporal correlation fallback detects cross-cwd sub-agents (same host, no tmux, within 60s window). Path heuristic retained for same-cwd cases. 8 unit tests pass. |
| **C** — Sub-Agent UX | **A-** | ~~B~~ → A-. Spawn notification includes agent_type when available. Details button + file transfer work. Path traversal fixed. |
| **D** — Telegram UX (D1-D7) | **A** | ~~A-~~ → A. Resume confirmation message now sent ("🟢 tmux: reconnected"). |
| **E** — Topic Lifecycle (E1-E5) | **A-** | ~~B~~ → A-. Default corrected to 15min. Inactivity threshold now runtime-configurable. Close fallback added. Temp file cleanup added. |

### Critical Gaps (7 items — ALL RESOLVED)

#### GAP-7: Cross-cwd sub-agents bypass parent detection entirely
- **Severity:** Critical (Part B — defeats the purpose of ADR-013)
- **Location:** `types.rs:551` — `extract_parent_session_id()`, `socket_handlers.rs:67-75` — parent detection block
- **Problem:** The `/subagents/` path heuristic only works when the sub-agent's `cwd` matches the parent's, because Claude Code organizes transcripts by project key (derived from `cwd`). When the Agent tool spawns a sub-agent with a different `cwd` — which happens frequently (e.g., worktree agents, agents that `cd` to home, general-purpose agents) — the transcript lands in a different project directory as a flat `{uuid}.jsonl` file with no `/subagents/` segment. Parent detection returns `None`, and the sub-agent creates its own topic. In production, **zero sessions** have ever had `parent_session_id` set, meaning this heuristic has a 0% success rate for the observed workload.
- **Root cause:** ADR-013 assumed all sub-agent transcripts would be nested under `{parentId}/subagents/`. This assumption is incorrect — it only holds when parent and child share the same `cwd`.
- **Additional finding:** The `validate_transcript_path()` canonicalization failure (logged as WARN) is a **red herring** — it only affects JSONL content extraction, not parent detection. Parent detection reads the raw unvalidated `transcript_path` string.
- **Fix implemented:** Option 2 — daemon-side temporal correlation fallback (commit 5384a44).
  - New method `SessionManager::find_likely_parent()` queries for the most recent active session on the same hostname with tmux + thread_id + no parent_session_id, within a configurable time window.
  - In `handle_session_start()`, when the `/subagents/` path heuristic returns `None` AND the session has no tmux target, the fallback fires and links the sub-agent to the detected parent.
  - New config: `TELEGRAM_SUBAGENT_DETECTION_WINDOW_SECS` (default: 60).
  - Safety guards: excludes self, excludes other sub-agents, requires tmux on parent, requires thread_id on parent, time-windowed.
  - 8 unit tests cover all edge cases (basic detection, self-exclusion, tmux/thread_id/hostname requirements, sub-agent exclusion, most-recent tiebreaking, time window enforcement).
  - Files changed: `session.rs`, `socket_handlers.rs`, `config.rs`, + 4 test config fixes.
- **Status:** RESOLVED (2026-03-18, commit 5384a44)

#### GAP-1: SECURITY — Path traversal in Details callback
- **Severity:** Security
- **Location:** `callback_handlers.rs` — `handle_subagent_details_callback`
- **Problem:** `agent_id` from Telegram callback_data is used directly to construct `/tmp/ctm-subagent-{agent_id}.md` file path. An authorized group member could craft callback data like `subagentdetails:../../etc/passwd` to read arbitrary files accessible to the daemon process. The write side in `socket_handlers.rs` uses hook-sourced (trusted) agent_id, but the read side uses user-controlled callback_data.
- **Fix:** Validate `agent_id` against path-safe character whitelist (e.g., `is_valid_session_id()` or reject any `/` chars) before constructing the file path. Apply on both read and write paths.

#### GAP-2: `agent_type` not tracked anywhere
- **Severity:** Functional gap (Part B + C)
- **Problem:** The ADR specifies a three-key identity model: `agent_id` + `parent_session_id` + `agent_type`. The `agent_type` dimension is completely absent — not in the DB schema, not in hook metadata, not in any struct. `SubagentStopEvent` has no `agent_type` field. Hook messages carry `agentId` but no `agentType`.
- **Fix:** Add `agent_type TEXT` column to sessions table. Populate from hook metadata (requires adding `agent_type` to `SubagentStop` event or inferring from transcript path structure). Display in spawn notifications and child message prefixes.

#### GAP-3: Child messages not prefixed with agent label
- **Severity:** Functional gap (Part B)
- **Location:** `socket_handlers.rs` — all handlers (`handle_agent_response`, `handle_tool_start`, etc.)
- **Problem:** The ADR Part B "Message Routing" section explicitly requires: "Prefix messages with a sub-agent label: `🤖 [Agent: {agent_type}] {content}`". Only the `SubagentStop` completion message has any labeling. Regular in-progress messages (tool calls, partial responses) arrive in the parent topic with no visual distinction from the parent session's own messages.
- **Fix:** In each handler, check if the session has a `parent_session_id`. If so, prepend the agent label to the message content. Depends on GAP-2 for agent_type; can use agent_id alone as interim.

#### GAP-4: Parent-before-child race condition
- **Severity:** Correctness (Part B)
- **Location:** `socket_handlers.rs:57-75` — parent thread_id lookup
- **Problem:** If a child session's `session_start` arrives before the parent's topic is created (parent session row exists but `thread_id = NULL`), `parent_thread_id` resolves to `None`. The child falls through to `create_forum_topic`, creating its own independent topic — exactly the topic sprawl ADR-013 is designed to prevent. This race is real in fast swarm spawning scenarios (e.g., CFA launches 8 agents simultaneously).
- **Fix:** Options: (a) if parent exists but has no thread_id, trigger parent topic creation on behalf of the parent before proceeding; (b) queue the child's session_start and retry after a short delay; (c) use a per-parent mutex/condvar so children block until the parent's topic is ready.

#### GAP-5: Orphan child sessions on parent end
- **Severity:** Correctness (Part B)
- **Location:** `socket_handlers.rs` — `handle_session_end`
- **Problem:** When a parent session ends, child sub-agent sessions remain in `active` status with no cascade, warning, or cleanup. Children with the parent's `thread_id` may post to a now-closed topic (silently fails at Telegram API layer). They eventually age out via stale session cleanup (hours later).
- **Fix:** In `handle_session_end`, query child sessions by `parent_session_id`. Either end them explicitly (`end_session`), or log a warning and let them age out naturally. At minimum, send a "Parent session ended" notification in the topic.

#### GAP-6: Lifecycle defaults don't match spec
- **Severity:** Spec violation (Part E)
- **Locations:**
  - `config.rs:347` — `topic_delete_delay_minutes` defaults to `1440` (24 hours). ADR specifies 15 minutes for the stage-1 close.
  - `cleanup.rs:13` — `INACTIVITY_DELETE_THRESHOLD_MINUTES = 720` is a compile-time `const`. ADR says "Both thresholds should be configurable."
  - `cleanup.rs:386-408` — `cleanup_inactive_topics` silently no-ops when `auto_delete_topics = false`. No close fallback (compare: `handle_session_end` at `socket_handlers.rs:279` correctly closes when auto-delete is off).
- **Fix:** (a) Change `topic_delete_delay_minutes` default to `15`. (b) Add `inactivity_delete_threshold_minutes` field to `Config`, read from `TELEGRAM_INACTIVITY_DELETE_THRESHOLD_MINUTES` env var, default 720. (c) In `cleanup_inactive_topics`, when `auto_delete_topics = false`, close (don't delete) inactive topics instead of skipping entirely.

### Minor Issues (8 items — 7 RESOLVED, 1 deferred)

#### MINOR-1: Stale comment in mod.rs
- **Location:** `mod.rs:602`
- **Problem:** Comment references "lines 528-539" but the actual code lives at lines 680+. Logic is correct; only the comment reference is stale.

#### MINOR-2: Unused helper dead code
- **Location:** `session.rs` — `get_active_sessions_with_tmux()`
- **Problem:** Specialized helper for F8 cache warming exists but is never called. Startup uses `get_active_sessions()` and filters inline instead.

#### MINOR-3: No tmux confirmation on session resume (D4 visual gap)
- **Location:** `mod.rs:600-617` — dedup guard
- **Problem:** When a session auto-heals via `claude --resume`, the dedup guard prevents re-running `handle_session_start`, so no fresh "🟢 tmux: connected" message is sent. Functional healing works; only the confirmation is absent.

#### MINOR-4: Spawn/completion as two messages instead of edit
- **Location:** `socket_handlers.rs:94-113` (spawn), `socket_handlers.rs:335-353` (completion)
- **Problem:** ADR specifies spawn message should be edited to become the completion message. Implementation sends two separate messages because spawn message_id is not tracked. Cosmetic deviation, not functional.

#### MINOR-5: Temp file accumulation
- **Location:** `socket_handlers.rs:319` — writes `/tmp/ctm-subagent-{agent_id}.md`
- **Problem:** Files are never cleaned up. Long-running daemon accumulates stale files in `/tmp` indefinitely. The existing `cleanup_old_downloads` function handles downloads but not sub-agent temp files.
- **Fix:** Add cleanup for `/tmp/ctm-subagent-*.md` files with a 24-48 hour TTL.

#### MINOR-6: Fragile `.unwrap()` in cleanup
- **Location:** `cleanup.rs:175`
- **Problem:** `tmux_target.as_deref().unwrap()` is guarded by an early-continue 17 lines above, but the guard and unwrap are far enough apart to be a maintenance hazard.
- **Fix:** Replace with `let Some(target) = tmux_target.as_deref() else { continue; };`

#### MINOR-7: Stage-2 deletion cache leak
- **Location:** `cleanup.rs:483-487`
- **Problem:** Stage-2 topic deletion removes `session_threads` cache entry but not `session_tmux` or `custom_titles`. Compare: `handle_stale_session_cleanup` (lines 251-253) correctly clears all three. Entries persist in memory until next cleanup cycle.

#### MINOR-8: No integration tests for new functionality
- **Problem:** Only unit tests exist (`extract_parent_session_id`, `extract_agent_id` in `types.rs`). No integration tests for:
  - Parent-child topic routing end-to-end
  - `handle_subagent_details_callback` (Details button flow)
  - F8 startup cache warm-up from DB
  - Two-stage topic lifecycle (close then delete)
  - Orphan session behavior

### Implementation Evidence (file locations)

| Requirement | File | Lines | Status |
|-------------|------|-------|--------|
| F1: Runtime fallback | `daemon/mod.rs` | 1083-1128 | Implemented |
| F2: UPDATE race | `daemon/mod.rs` | 581; `session.rs` 563-604 | Implemented |
| F3: Idempotency guard | `session.rs` | 283-319 | Implemented |
| F4: Dedup mitigation | `daemon/mod.rs` | 600-617, 679-715 | Implemented (via F2/F3) |
| F5: Cache clear on end | `daemon/socket_handlers.rs` | 285 | Implemented |
| F6: Live detection | `daemon/mod.rs` | 1080-1131 | Implemented |
| F7: No stale global default | `daemon/mod.rs` | 299-320 | Implemented |
| F8: Startup cache warm | `daemon/mod.rs` | 322-345 | Implemented |
| B: Parent-child schema | `session.rs` | 151-153, 211-248 | Implemented |
| B: extract_parent_session_id | `types.rs` | 526-533 | Implemented (same-cwd path heuristic) |
| B: find_likely_parent (GAP-7) | `session.rs` | 682-735 | Implemented (cross-cwd temporal correlation fallback) |
| B: Parent thread_id reuse | `daemon/socket_handlers.rs` | 67-125 | Implemented (3-branch: path heuristic → temporal fallback → none) |
| B: Child message prefix | `daemon/socket_handlers.rs` | via get_child_prefix() | Implemented |
| B: agent_type tracking | `session.rs`, `types.rs`, `hook.rs` | schema + hook metadata | Implemented |
| C: Spawn notification | `daemon/socket_handlers.rs` | 94-113 | Partial (no type) |
| C: Completion + Details button | `daemon/socket_handlers.rs` | 317-353 | Implemented |
| C: Details summary + file | `daemon/callback_handlers.rs` | 384-502 | Implemented |
| D1: No silent drops | `daemon/telegram_handlers.rs` | 97-119, 208-223 | Implemented |
| D2: Warning every failure | `daemon/telegram_handlers.rs` | 111-118, 215-223 | Implemented |
| D3: tmux status indicator | `daemon/socket_handlers.rs` | 188-194 | Implemented |
| D4: Auto-healing | `daemon/mod.rs` | 679-715 | Implemented |
| D5: Cache-miss-only detect | `daemon/mod.rs` | 1057-1132 | Implemented |
| D6: View-only warning | `daemon/telegram_handlers.rs` | 101-119 | Implemented |
| D7: Details file transfer | `daemon/callback_handlers.rs` | 473-502 | Implemented |
| E1: 1:1 mapping | `daemon/socket_handlers.rs` | 49-177 | Implemented |
| E2: Two-trigger cleanup | `daemon/cleanup.rs` | 341-495 | Partial (GAP-6) |
| E3: Auto-heal on resume | `daemon/mod.rs` | 727-878 | Implemented |
| E4: Title renames | `daemon/socket_handlers.rs` | 700-795 | Partial (no agent count) |
| E5: Close then delete | `daemon/cleanup.rs` | 438-495 | Implemented |

### Remediation Plan — COMPLETE (ALL GAPS RESOLVED)

5-agent CFA swarm executed Phases 5-6 in parallel (commit 6d18ac7). GAPs 1-6 closed. 13 files changed, 361 insertions.

**GAP-7 resolved** (commit 5384a44). Discovered via 4-agent research swarm that traced the 0% parent detection rate to cross-cwd transcript path divergence. Fixed with daemon-side temporal correlation fallback. 2-agent CFA swarm implemented the fix: 7 files changed, 602 insertions, 8 new unit tests. All tests pass.

**Phase 5: Security + Critical Gaps** (all done)

| Step | Gap | Files | Estimated | Risk |
|------|-----|-------|-----------|------|
| 5a | GAP-1: Path traversal fix | `callback_handlers.rs`, `socket_handlers.rs` | ~10 lines | Low |
| 5b | GAP-2: `agent_type` schema + tracking | `session.rs`, `types.rs`, `hook.rs` | ~60 lines | Low |
| 5c | GAP-3: Child message prefix | `socket_handlers.rs` (all handlers) | ~40 lines | Low |
| 5d | GAP-6: Lifecycle defaults + configurability | `config.rs`, `cleanup.rs` | ~30 lines | Low |
| 5e | GAP-4: Parent-before-child race | `socket_handlers.rs` | ~40 lines | Medium |
| 5f | GAP-5: Orphan child cascade | `socket_handlers.rs` | ~30 lines | Medium |

**Phase 6: Minor Cleanup**

| Step | Minor | Files | Estimated |
|------|-------|-------|-----------|
| 6a | MINOR-1: Fix stale comment | `mod.rs` | 1 line |
| 6b | MINOR-2: Remove dead code | `session.rs` | ~20 lines removed |
| 6c | MINOR-5: Temp file cleanup | `cleanup.rs` | ~20 lines |
| 6d | MINOR-6: Replace unwrap | `cleanup.rs` | 1 line |
| 6e | MINOR-7: Stage-2 cache clear | `cleanup.rs` | ~5 lines |
| 6f | MINOR-4: Message edit pattern | `socket_handlers.rs` | ~30 lines |
| 6g | MINOR-3: Resume tmux confirmation | `mod.rs` or `socket_handlers.rs` | ~15 lines |

**Phase 7: Test Coverage**

| Test | Scope |
|------|-------|
| Integration: parent-child routing | Simulate parent + child session_start, verify shared thread_id |
| Integration: Details callback | Simulate callback with valid and invalid agent_ids |
| Integration: F8 cache warm-up | Create sessions with tmux_target, restart, verify cache populated |
| Integration: two-stage lifecycle | End session, verify close at stage-1 and delete at stage-2 |
| Integration: orphan behavior | End parent, verify child cascade |
| Security: path traversal rejection | Callback with `../` in agent_id, verify rejection |

**Phase 8: Cross-cwd Parent Detection (GAP-7)** — COMPLETED (2026-03-18, commit 5384a44)

| Step | Description | Files | Lines | Status |
|------|-------------|-------|-------|--------|
| 8a | Research + design (4-agent research swarm) | ADR-013 | — | Done |
| 8b | Temporal correlation fallback in `handle_session_start()` | `socket_handlers.rs` | 124 changed | Done |
| 8c | `find_likely_parent()` query | `session.rs` | 51 added | Done |
| 8d | Config: `subagent_detection_window_secs` | `config.rs` | 28 added | Done |
| 8e | Unit tests (8 cases) | `session.rs` | 461 added | Done |

Part B restored from **D** to **A-**.

## References

- [Claude Code Hooks Reference](https://code.claude.com/docs/en/hooks)
- [GitHub Issue #7881 — SubagentStop shared session_id](https://github.com/anthropics/claude-code/issues/7881)
- [Telegram Forum API](https://core.telegram.org/api/forum)
- ADR-012: AskUserQuestion tentative selection
