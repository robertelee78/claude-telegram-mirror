# ADR-003: Dual Hook Handlers

**Status:** Accepted
**Date:** 2026-03-16

## Context

Claude Code's hook system invokes configured scripts on lifecycle events
(PreToolUse, PostToolUse, Stop, Notification, UserPromptSubmit, etc.). The
bridge needs to forward these events to the Telegram bot daemon for real-time
mirroring.

Two constraints pull in opposite directions:

1. **Most hook events must be fast and fire-and-forget.** Claude Code waits
   for hook scripts to complete before continuing. A slow hook blocks the
   entire CLI session. PostToolUse, Notification, Stop, and UserPromptSubmit
   hooks should finish in single-digit milliseconds.

2. **PreToolUse approval must block synchronously.** Claude Code's permission
   system supports `hookSpecificOutput` on PreToolUse, allowing a hook to
   return `permissionDecision: "allow" | "deny" | "ask"`. The hook handler
   must wait for the user to respond on Telegram (up to 5 minutes) before
   returning a decision. A bash script cannot do this because it must exit
   quickly.

A single handler cannot satisfy both requirements.

## Decision

Two hook handlers run concurrently, each handling different aspects:

### Bash Script (`scripts/telegram-hook.sh`)

- Registered in `.claude/settings.json` for all hook events.
- Reads JSON from stdin, extracts `hook_event_name` and `session_id`.
- Formats an NDJSON message and sends it to the daemon via `netcat` over the
  Unix domain socket.
- Exits immediately after sending. Total latency is under 5 ms.
- Handles: PreToolUse (fire-and-forget notification), PostToolUse, Stop,
  Notification, UserPromptSubmit, PreCompact.
- On Stop events, reads the transcript JSONL to extract new assistant text
  since the last turn.
- Passes through the original stdin to stdout so Claude Code sees unmodified
  input.

### Node.js Handler (`src/hooks/handler.ts`)

- Registered in `.claude/settings.json` specifically for PreToolUse.
- Reads JSON from stdin (with 1 MiB size limit).
- Connects to the daemon socket as a `SocketClient`.
- Calls `sendAndWait()` with a 5-minute timeout to request approval from the
  Telegram user.
- Returns `hookSpecificOutput` JSON on stdout with `permissionDecision` and
  `permissionDecisionReason`.
- If the daemon is not running or the timeout expires, returns
  `permissionDecision: "ask"` to fall back to the CLI prompt.

### Concurrency on PreToolUse

Both handlers run on PreToolUse events because Claude Code invokes all
registered hooks for each event type. The bash script fires the notification
to Telegram (so the user sees what tool is about to run), while the Node.js
handler blocks waiting for the approval response. This is safe because:

- The bash script exits immediately and does not write to stdout (no
  `hookSpecificOutput`), so it does not interfere with the Node.js handler's
  permission decision.
- The Node.js handler is the only one that returns `hookSpecificOutput`.
- The daemon deduplicates the tool notification (one from bash, one from
  Node.js) based on `tool_use_id`.

## Consequences

### Positive

- Hook latency for non-approval events is under 5 ms (bash + netcat).
- The approval workflow blocks Claude Code synchronously, giving the
  Telegram user time to review and decide.
- If the bridge daemon is not running, both handlers exit cleanly and Claude
  Code continues normally.
- The bash script uses only standard Unix tools (jq, netcat) with no Node.js
  startup cost.

### Negative

- Two separate codepaths handle overlapping event types, requiring care to
  keep them consistent.
- The bash script depends on `jq` and `nc` (netcat) being available on the
  system.
- PreToolUse events generate two socket messages (one from each handler),
  requiring deduplication in the daemon.

### Neutral

- Both handlers read the same stdin JSON. Claude Code provides identical
  input to all registered hooks for a given event.
- The Node.js handler's 5-minute timeout is a deliberate tradeoff between
  giving the user time to respond and not blocking Claude Code indefinitely.
