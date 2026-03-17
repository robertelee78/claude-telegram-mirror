# ADR-007: Runtime Mirroring Toggle and Outbound File Transfer

> **DO NOT BE LAZY. We have plenty of time to do it right.**
> No shortcuts. Never make assumptions.
> Always dive deep and ensure you know the problem you're solving.
> Make use of search as needed (goalie).
> Measure 3x, cut once.
> No fallback. No stub (todo later) code.
> Just pure excellence, done the right way the entire time.
> Chesterton's fence: always understand the current implementation fully before changing it.

**Status:** Accepted (Complete)
**Date:** 2026-03-16
**Authors:** Robert E. Lee

### Progress

| Item | Rust | TypeScript | Notes |
|------|------|-----------|-------|
| 1.1 MirrorStatus persistence | DONE | DONE | `config.rs` / `config.ts` — `MirrorStatus`, `read/write_mirror_status()` |
| 1.2 Daemon gating | DONE | DONE | `daemon.rs` / `daemon.ts` — message gating with safety bypass |
| 1.3 Command handler | DONE | DONE | `handle_command()` for toggle/enable/disable |
| 1.4 CLI `toggle` subcommand | DONE | DONE | `cmd_toggle()` with `--on`/`--off` flags |
| 1.5 `/toggle` Telegram command | DONE | DONE | `/status` updated + `formatHelp()` updated |
| 2.1 `SendImage` message type | DONE | DONE | `SendImage` variant / `'send_image'` union member |
| 2.2 Bot upload methods | DONE | DONE | `send_photo()`, `send_document()` via multipart / grammy InputFile |
| 2.3 Path validation | DONE | DONE | Absolute path, no `..`, exists, ≤50 MB |
| 2.4 `send_image` handler | DONE | DONE | Extension-based routing, toggle gating |

**Verification:** Rust 200 tests passing, TypeScript 195 tests passing, both builds clean.

---

## Context

A comparative audit of `/opt/Claude-Code-Rust-Telegram` (the DreamLab-AI fork) against
our codebase revealed two features worth adopting. Both are straightforward to implement,
carry low risk, and fill genuine usability gaps.

**Feature 1 — Runtime Mirroring Toggle (`ctm toggle`)**

Currently, disabling mirroring requires either:
- Stopping the daemon (`ctm stop`) and restarting without `TELEGRAM_MIRROR=true`, or
- Editing the config file and restarting.

Both approaches kill active sessions, lose in-memory state (thread mappings, tool caches,
dedup sets), and require the user to remember to re-enable later. A runtime toggle would
let users silence mirroring for a sensitive conversation without disrupting the daemon.

**Feature 2 — Outbound File Transfer (CLI → Telegram)**

ADR-001 Item 11 implemented *inbound* file transfer: photos and documents sent from
Telegram are downloaded and injected as file paths into the Claude session. This works in
both TS (`src/bridge/daemon.ts:559-614`) and Rust (`rust-crates/ctm/src/daemon.rs:1605-1863`).

The *outbound* direction — sending files from the CLI to Telegram — does not exist. If a
Claude session generates a chart, screenshot, diagram, or build artifact, there is no way
to push it to the Telegram topic. The fork supports this via a `send_image` socket message
type with `sendPhoto`/`sendDocument` Telegram API methods.

---

## Decision

Implement both features in TypeScript and Rust, maintaining the existing parity discipline
from ADR-002. Both features are additive (no existing behavior changes) and isolated (no
cross-cutting concerns).

---

## Feature 1: Runtime Mirroring Toggle

### 1.1 Design

Three-layer implementation:

| Layer | Component | Responsibility |
|-------|-----------|----------------|
| **Persistence** | `status.json` | Survives daemon restart |
| **In-Memory** | `AtomicBool` (Rust) / `boolean` field (TS) | Lock-free hot-path check |
| **CLI** | `ctm toggle` subcommand | User-facing control |

**State file:** `~/.config/claude-telegram-mirror/status.json`

```json
{
  "enabled": true,
  "pid": 12345,
  "toggled_at": "2026-03-16T14:30:00Z"
}
```

- Permissions: `0o600` (consistent with all other config files)
- Default when missing: `enabled: true` (mirrors current behavior)
- `pid` is advisory — records which daemon wrote the state

### 1.2 CLI Subcommand

```
ctm toggle
```

Behavior:
1. Read current state from `status.json` (default: `true`)
2. Flip to `!current`
3. Write new state to `status.json` with `0o600` permissions
4. If the bridge socket exists, send a `Command` message with content `"toggle"` via
   NDJSON, then disconnect immediately (fire-and-forget)
5. Print confirmation: `Telegram mirroring: ON` or `Telegram mirroring: OFF`

Additionally support explicit set:
```
ctm toggle --on
ctm toggle --off
```

These bypass the flip logic and set the state directly. Useful for scripts and service
hooks.

### 1.3 Daemon Handling

On startup:
- Read `status.json` to initialize `mirroring_enabled` (default: `true` if file missing)

On receiving `Command` message with content `"toggle"`, `"enable"`, `"on"`, `"disable"`,
or `"off"`:
1. Compute new state
2. Store in `AtomicBool` / boolean field (lock-free)
3. Write `status.json` with current PID
4. Send one confirmation message to Telegram (even when disabling — so the user knows
   it took effect)
5. Log at `info` level

When `mirroring_enabled == false`:
- **Skip** all outbound message processing (tool_start, tool_result, agent_response, etc.)
- **Continue** processing: system commands (toggle/enable/disable), Telegram polling
  (so the user can re-enable via Telegram), and approval requests (safety-critical)
- **Continue** session tracking (so sessions aren't orphaned during a mute period)

### 1.4 Telegram Command

Add `/toggle` as a Telegram bot command:
- Sends a `Command` message internally (same path as CLI toggle)
- Responds with current state after toggle
- No arguments = flip; `/toggle on` or `/toggle off` for explicit set

### 1.5 Implementation Scope

**TypeScript:**

| File | Changes |
|------|---------|
| `src/utils/config.ts` | Add `readMirrorStatus()`, `writeMirrorStatus()` functions |
| `src/bridge/daemon.ts` | Add `mirroringEnabled` field, check before `sendToTelegram()`, handle `Command` messages |
| `src/bot/commands.ts` | Add `/toggle` command handler |
| `src/cli.ts` | Add `toggle` subcommand with `--on`/`--off` flags |

**Rust:**

| File | Changes |
|------|---------|
| `rust-crates/ctm/src/config.rs` | Add `MirrorStatus` struct, `read_mirror_status()`, `write_mirror_status()` |
| `rust-crates/ctm/src/daemon.rs` | Add `mirroring_enabled: Arc<AtomicBool>`, gate outbound messages, handle `Command` |
| `rust-crates/ctm/src/main.rs` | Add `Toggle` variant to `Commands` enum, implement `cmd_toggle()` |

**Tests:**

| Test | Validates |
|------|-----------|
| `status.json` read/write round-trip | Persistence layer |
| Default when file missing | Graceful degradation |
| Toggle flips state | Core logic |
| Explicit `--on`/`--off` | Override behavior |
| Messages skipped when disabled | Gating logic |
| Approvals still work when disabled | Safety invariant |
| System commands processed when disabled | Re-enable path |

### 1.6 Edge Cases

| Scenario | Behavior |
|----------|----------|
| Daemon not running, `ctm toggle` | Writes `status.json` only; next daemon start reads it |
| Multiple daemons | Each reads/writes own `status.json`; last-writer wins (acceptable — multi-daemon is already advisory) |
| Toggle during active approval | Approval still completes (not gated) |
| `status.json` corrupted/invalid | Default to `enabled: true` |
| Toggle, then daemon restart | Reads persisted state from `status.json` |

---

## Feature 2: Outbound File Transfer (CLI → Telegram)

### 2.1 Design

New socket message type `send_image` enables the CLI to push files to the Telegram topic
associated with the sending session.

**Message format:**

```json
{
  "type": "send_image",
  "sessionId": "session-abc123",
  "timestamp": "2026-03-16T14:30:00Z",
  "content": "/absolute/path/to/file.png",
  "metadata": {
    "caption": "Build output diagram"
  }
}
```

| Field | Required | Description |
|-------|----------|-------------|
| `type` | Yes | Must be `"send_image"` |
| `sessionId` | Yes | Session that owns the target forum topic |
| `content` | Yes | Absolute path to the file on the local filesystem |
| `metadata.caption` | No | Optional caption text sent with the file |

### 2.2 File Type Detection

Detect by extension, not MIME sniffing (consistent with Telegram API expectations):

| Extension | Telegram API | Method |
|-----------|-------------|--------|
| `.jpg`, `.jpeg`, `.png`, `.gif`, `.webp`, `.bmp` | `sendPhoto` | Photo with preview |
| Everything else (`.pdf`, `.zip`, `.log`, `.txt`, etc.) | `sendDocument` | Document with filename |

Photos get an inline preview in the Telegram chat. Documents get a download link with
the original filename.

### 2.3 Security Constraints

| Check | Rationale |
|-------|-----------|
| Path must be absolute | Prevents relative path confusion |
| Path must not contain `..` | Prevents directory traversal |
| File must exist | Prevents error spam |
| File must be ≤50 MB | Telegram API limit |
| File must be readable by daemon | Permission check before API call |

All checks happen **before** any Telegram API call. Failures are logged at `warn` level
and silently dropped (no error message to Telegram — the sender is a hook/script, not a
human waiting for feedback).

### 2.4 Bot API Surface

Add two methods to the Telegram bot wrapper:

**`send_photo(path, caption, thread_id)`**

```
POST /bot<token>/sendPhoto
Content-Type: multipart/form-data

chat_id: <chat_id>
photo: <file upload>
caption: <optional>
message_thread_id: <optional>
```

**`send_document(path, caption, thread_id)`**

```
POST /bot<token>/sendDocument
Content-Type: multipart/form-data

chat_id: <chat_id>
document: <file upload>
caption: <optional>
message_thread_id: <optional>
```

Both methods:
- Use multipart form upload (not file_id or URL)
- Respect the existing rate limiter (governor token bucket)
- Go through the message queue with retry logic
- Sanitize the filename for the `Content-Disposition` header (replace `/`, `\` with `_`)

### 2.5 Implementation Scope

**TypeScript:**

| File | Changes |
|------|---------|
| `src/bot/telegram.ts` | Add `sendPhoto(path, caption, threadId)` and `sendDocument(path, caption, threadId)` methods using grammy's `InputFile` |
| `src/bridge/daemon.ts` | Add `send_image` handler in message router; validate path, detect type, call bot |
| `src/bridge/types.ts` | Add `send_image` to `MessageType` enum (if separate from `types.ts`) |

**Rust:**

| File | Changes |
|------|---------|
| `rust-crates/ctm/src/bot.rs` | Add `send_photo(&self, path, caption, thread_id)` and `send_document(&self, path, caption, thread_id)` using `reqwest` multipart |
| `rust-crates/ctm/src/daemon.rs` | Add `handle_send_image()` in message router; validate path, detect type, call bot |
| `rust-crates/ctm/src/types.rs` | Add `SendImage` variant to `MessageType` enum |

**Tests:**

| Test | Validates |
|------|-----------|
| Path validation rejects relative paths | Security |
| Path validation rejects `..` | Security |
| Extension detection (jpg → photo, pdf → document) | Routing |
| Unknown extension → document | Default behavior |
| Case-insensitive extension matching | `.PNG` == `.png` |
| Missing file handled gracefully | No panic, no Telegram error |
| Filename sanitization | `/` and `\` replaced with `_` |

### 2.6 Usage Scenarios

**Scenario 1: Hook sends a screenshot**

A PostToolUse hook for a browser automation tool captures a screenshot. The hook script
sends a `send_image` message via the socket:

```bash
echo '{"type":"send_image","sessionId":"'$SESSION_ID'","timestamp":"'$(date -u +%FT%TZ)'","content":"/tmp/screenshot.png","metadata":{"caption":"Browser screenshot"}}' | socat - UNIX-CONNECT:~/.config/claude-telegram-mirror/bridge.sock
```

**Scenario 2: Claude generates a diagram**

A custom tool or agent produces a Mermaid-rendered PNG. The PostToolUse hook detects the
output path and forwards it:

```json
{
  "type": "send_image",
  "sessionId": "session-xyz",
  "content": "/tmp/diagram.png",
  "metadata": {"caption": "Architecture diagram"}
}
```

**Scenario 3: Build artifact notification**

After a successful `cargo build`, a hook sends the binary as a document:

```json
{
  "type": "send_image",
  "sessionId": "session-xyz",
  "content": "/target/release/ctm",
  "metadata": {"caption": "Release build (9.3 MB)"}
}
```

### 2.7 Edge Cases

| Scenario | Behavior |
|----------|----------|
| File deleted between validation and upload | `reqwest`/`grammy` returns error; logged, no retry |
| File >50 MB | Rejected at validation; logged at `warn` |
| Symlink | Followed (consistent with `Path::exists()` behavior) |
| Broken symlink | Rejected (doesn't exist) |
| Session has no forum topic | Message dropped with `debug` log |
| Mirroring disabled (toggle OFF) | Message dropped (gated by toggle) |
| Rate limit hit | Queued behind other messages; governor handles backpressure |
| Binary file as "photo" | Extension check prevents this — only known image extensions go to `sendPhoto` |

---

## Implementation Order

| Step | Feature | Effort | Risk |
|------|---------|--------|------|
| 1 | Toggle: config layer (`MirrorStatus` read/write) | 30 min | Low |
| 2 | Toggle: daemon gating + `Command` handler | 30 min | Low |
| 3 | Toggle: CLI subcommand | 15 min | Low |
| 4 | Toggle: `/toggle` Telegram command | 15 min | Low |
| 5 | Toggle: tests | 30 min | Low |
| 6 | Outbound: `send_photo`/`send_document` bot methods | 1-2 hr | Medium |
| 7 | Outbound: `send_image` daemon handler + validation | 1 hr | Low |
| 8 | Outbound: `SendImage` message type | 15 min | Low |
| 9 | Outbound: tests | 45 min | Low |

**Total estimated effort:** ~5-6 hours (both TS and Rust)

Steps 1-5 (toggle) and 6-9 (outbound) are independent and can be developed in parallel.

---

## Alternatives Considered

### Toggle: Telegram-only (no CLI command)

Rejected. The CLI toggle is essential for scripts, service hooks, and situations where
Telegram is unreachable. The Telegram `/toggle` command is a convenience layer on top.

### Toggle: Environment variable reload (SIGHUP)

Rejected. `SIGHUP` is a Unix convention for config reload, but it doesn't provide
feedback to the user (no "ON/OFF" confirmation). The socket-based approach gives
bidirectional feedback and works from both CLI and Telegram.

### Outbound: Base64-encode files in socket messages

Rejected. Files can be up to 50 MB. Encoding them as JSON strings would double memory
usage and break NDJSON line limits. File paths are the right abstraction — the daemon
reads the file only when uploading.

### Outbound: Use Telegram file_id or URL instead of local path

Rejected. The file is local to the machine running the daemon. There is no HTTP URL to
reference. `file_id` is for files already on Telegram's servers. Multipart upload is the
correct API for local files.

### Outbound: Separate message types for photos vs documents

Rejected. The fork uses a single `send_image` type and detects by extension. This is
simpler for callers (they don't need to know the distinction) and consistent with the
inbound path (which also auto-detects). One message type, one handler.

---

## Consequences

### Positive

- Users can mute mirroring for sensitive conversations without killing the daemon
- Toggle state persists across daemon restarts
- Hooks and scripts can push files to Telegram programmatically
- Bidirectional file transfer completes the inbound work from ADR-001 Item 11
- No breaking changes — both features are purely additive

### Negative

- `status.json` adds one more file to the config directory
- `send_photo`/`send_document` require multipart form upload, which is more complex than
  JSON-only API calls (new dependency surface in the bot layer)
- Toggle state can drift if multiple daemons share a config directory (acceptable — this
  is already a known limitation of multi-daemon setups)

### Risks

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Toggle accidentally left OFF | Low | `/status` command should show toggle state prominently |
| Large file upload blocks message queue | Low | Rate limiter + queue already handles backpressure |
| Path traversal in `send_image` | Prevented | Absolute path + no `..` validation |

---

## References

- ADR-001 Item 11: Photo/document download (inbound) — `5fcdb18`
- ADR-002: Phased Rust migration — feature parity discipline
- ADR-006: Rust migration gap audit — completeness methodology
- `/opt/Claude-Code-Rust-Telegram/src/bridge.rs:661-694` — fork's `handle_send_image`
- `/opt/Claude-Code-Rust-Telegram/src/bridge.rs:1284-1318` — fork's toggle handler
- `/opt/Claude-Code-Rust-Telegram/src/main.rs:106-134` — fork's `cmd_toggle`
- `/opt/Claude-Code-Rust-Telegram/src/config.rs:295-333` — fork's `MirrorStatus`
