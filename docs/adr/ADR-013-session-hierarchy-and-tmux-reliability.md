# ADR-013: Session Hierarchy and tmux Reliability

**Status:** Proposed
**Date:** 2026-03-18
**Authors:** Robert, Claude

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

#### Discovery: transcript_path contains parent session_id

Sub-agent transcripts follow the path pattern:
```
~/.claude/projects/{project}/{parentSessionId}/subagents/agent-{agentId}.jsonl
```

The `transcript_path` is already transmitted to the daemon in every hook message's metadata. Extraction:

```rust
fn extract_parent_session_id(path: &str) -> Option<&str> {
    let (parent_part, _) = path.split_once("/subagents/")?;
    parent_part.rsplit('/').next()
}
```

This provides deterministic parent-child mapping with no heuristics.

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

### Heuristic grouping by project_dir + time window
Rejected as primary approach. False positives when two independent sessions use the same project. However, could serve as a fallback when transcript_path doesn't contain `/subagents/`.

### Separate topics with naming convention
Rejected. Causes the exact topic sprawl the user complained about. Telegram topics are flat — no nesting.

### Wait for upstream `parent_session_id`
Not viable as sole strategy. GitHub issues #7881, #14859, #16424 are open with no timeline. The transcript_path approach works today.

## References

- [Claude Code Hooks Reference](https://code.claude.com/docs/en/hooks)
- [GitHub Issue #7881 — SubagentStop shared session_id](https://github.com/anthropics/claude-code/issues/7881)
- [Telegram Forum API](https://core.telegram.org/api/forum)
- ADR-012: AskUserQuestion tentative selection
