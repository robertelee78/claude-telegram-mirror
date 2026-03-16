# ADR-001: TypeScript Security Hardening and UX Improvements

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

| Item | Status | Commit |
|------|--------|--------|
| 1. sendSlashCommand spawn + whitelist | DONE | `53fd412` |
| 2. Bot token scrubbing | DONE | `8e7ff2a` |
| 3. Session ID validation | DONE | `2048371` |
| 4. Socket path traversal check | DONE | `2048371` |
| 5. Config dir permissions | DONE | `dc2c59c` |
| 6. Drop file logging | DONE | `8e7ff2a` |
| 7. NDJSON line limit | DONE | `2048371` |
| 8. Connection limit | DONE | `2048371` |
| 9. Tool summarizer | DONE | `89be6d4` |
| 10. Topic augmentation via /rename | DONE | `f922ccc` |
| 11. Photo/document download | DONE | `5fcdb18` |
| 12. IDOR approval check | DONE | `d0a8bb3` |
| 13. Doctor --fix | DONE | `d505ce0` |
| 14. SECURITY.md + ADRs | DONE | `34a9ba6` |
| 15. CI workflow | DONE | `f35613f` |
| 16. Remove inquirer | DONE | `0ae6e88` |
| 17. AskUserQuestion in Telegram | DONE | `eec8b0f` |

---

## Context

On 2026-03-15, DreamLab-AI forked our repository, deleted all TypeScript source, and rewrote the project in Rust. Their PRD claimed 10 security vulnerabilities in our codebase. On 2026-03-16, a CFA swarm analysis validated some of their claims and refuted others.

This ADR captures the TypeScript fixes we are adopting -- informed by their analysis but executed our way. These fixes stand on their own: they ship first and have value regardless of whether a Rust migration (ADR-002) happens. We are not reacting to a fork; we are hardening our own codebase based on a thorough audit of actual issues.

The 16 items below fall into three categories: security hardening (8 items), UX improvements (4 items), and operational improvements (4 items). Every item has been verified against our source code with specific file paths and line numbers.

---

## Decision

Adopt all 16 items described below. Reject wholesale Rust rewrite adoption. Defer 6 items to the Rust migration where they are more naturally expressed.

---

## Security Hardening (Items 1-8)

### 1. Switch `sendSlashCommand` to `spawn()` + Character Whitelist

**Problem:** `src/bridge/injector.ts:355` uses `execSync` with string interpolation to build a tmux command. The `command` parameter is interpolated directly into the shell string with no validation and no `-l` flag. A crafted slash command like `/clear; rm -rf /` would execute arbitrary shell commands.

```typescript
// injector.ts:355 — current code
execSync(`tmux ${socketFlag} send-keys -t "${this.tmuxSession}" ${command}`, {
  stdio: 'ignore'
});
```

The same pattern exists in `injectViaTmux()` at line 159 and `sendKey()` at line 333, though those are less exploitable because `injectViaTmux` uses `-l` flag with escaped text and `sendKey` uses a fixed keymap.

**Why it matters:** This is an active shell injection vector. Any Telegram user with access to the configured chat can trigger it by sending text that routes to `sendSlashCommand`.

**Fix:**
- Switch from `execSync()` with string interpolation to `child_process.spawn()` with an argument array. This eliminates shell interpretation entirely.
- Add the `-l` (literal) flag so tmux does not interpret key names in the command text.
- Add a character whitelist before the spawn call: `/^[a-zA-Z0-9_\- \/]+$/`. Reject any command that does not match.
- Add unit tests that verify rejection of inputs containing `;`, `$()`, backticks, `|`, `>`, `<`, and `&&`.

**What NOT to do:**
- Do not use a blocklist of dangerous characters. Blocklists are always incomplete. Use a whitelist.
- Do not simply escape the string and keep `execSync`. The argument array approach with `spawn()` is categorically safer because it never invokes a shell.

---

### 2. Bot Token Scrubbing from Error Logs

**Problem:** grammY embeds the full Bot API URL (including the bot token) in `GrammyError.description`. Our error handler in `src/bot/telegram.ts:146-159` logs these descriptions raw:

```typescript
// telegram.ts:151-155
if (error instanceof GrammyError) {
  logger.error('Telegram API error', {
    code: error.error_code,
    description: error.description  // Contains bot token in URL
  });
}
```

The token also appears in `error.message` on line 148 via `String(error)`.

**Why it matters:** Bot tokens are credentials. Leaking them into log files (especially the `/tmp` file transport -- see Item 6) means anyone with read access to those logs can impersonate the bot.

**Fix:**
- Create a scrubbing function: `function scrubBotToken(text: string): string` that applies `text.replace(/bot\d+:[A-Za-z0-9_-]+\//g, 'bot<REDACTED>/')`.
- Apply it in the grammY error handler in `telegram.ts` on both `error.description` and `error.message`.
- Also add it as a winston format transform in `logger.ts` so that any code path that accidentally logs a token gets scrubbed. The transform should run on the `message` field and on all string values in the `meta` object.

**What NOT to do:**
- Do not rely solely on the winston transform. Apply scrubbing at the source (grammY handler) as well. Defense in depth.
- Do not try to scrub only specific log calls. The winston transform catches everything.

---

### 3. Session ID Validation

**Problem:** The daemon accepts session IDs of arbitrary length and content from the Unix socket. In `src/hooks/handler.ts:498`, the handler passes `event.session_id` directly from parsed JSON to the `HookHandler` constructor, which forwards it to the daemon via the socket. The daemon in `src/bridge/daemon.ts` uses it as a Map key and SQLite primary key without validation.

A malicious or buggy hook script could send a multi-megabyte session ID, causing unbounded memory growth in the `sessionThreads`, `sessionTmuxTargets`, `topicCreationPromises`, and `topicCreationResolvers` Maps.

**Why it matters:** Input validation at system boundaries is a fundamental security principle. The Unix socket is a trust boundary -- hook scripts run in the context of Claude Code sessions, which may be operating on untrusted repositories.

**Fix:**
- In `src/bridge/daemon.ts`, in the socket message handler (`setupSocketHandlers`), validate the `sessionId` field of every incoming `BridgeMessage`:
  - Maximum 128 characters
  - Character whitelist: `[a-zA-Z0-9_-]` only
  - Drop messages with invalid session IDs and log a warning
- Claude Code's native session IDs match `[a-f0-9-]` (UUIDs). Our generated IDs use `[a-z0-9-]`. The whitelist of `[a-zA-Z0-9_-]` is generous enough for both.

**What NOT to do:**
- Do not validate only in the hook handler (`handler.ts`). The daemon is the trust boundary; validate there. The hook handler runs in the client process and can be bypassed.

---

### 4. Socket Path Traversal Check

**Problem:** `src/bridge/injector.ts:386-392` (`setTmuxSession`) accepts a socket path from the caller without any validation:

```typescript
// injector.ts:386-392
setTmuxSession(session: string, socket?: string): void {
  this.tmuxSession = session;
  this.tmuxSocket = socket || null;
  if (session) {
    this.method = 'tmux';
  }
}
```

The socket path is later interpolated into tmux commands (lines 115, 158, 332). Similarly, `src/utils/config.ts:156-159` accepts `socketPath` from environment variables and config files without path validation.

**Why it matters:** A path containing `..` segments or non-absolute paths could cause tmux to connect to or create a socket in an unintended location, potentially enabling socket hijacking on multi-user systems.

**Fix:**
- In `setTmuxSession()`: reject socket paths containing `..`, reject non-absolute paths (must start with `/`), reject paths longer than 256 characters. Throw an error on violation.
- In `loadConfig()`: apply the same validation to `socketPath`. Reject and fall back to the default socket path if validation fails.
- The validation function should be a shared utility: `function validateSocketPath(path: string): boolean`.

**What NOT to do:**
- Do not use `path.resolve()` as a substitute for validation. Resolving `../../../etc/foo` still gives a valid absolute path -- the issue is that the intent was malicious. Check for the literal presence of `..` segments.

---

### 5. Centralize Config Dir Permissions

**Problem:** At least 4 code paths create `~/.config/claude-telegram-mirror/` but only 2 set secure permissions (`mode: 0o700`):

| File | Line | Sets mode? |
|------|------|------------|
| `src/bridge/socket.ts` | 131 | Yes (`0o700`) |
| `src/service/manager.ts` | 71 | Yes (`0o700`) |
| `src/bridge/session.ts` | 37 | **No** |
| `src/service/setup.ts` | 509 | **No** |
| `src/service/manager.ts` | 362, 368 | **No** |

When `session.ts:37` creates the directory first (which happens on first daemon start before the socket server), it uses `mkdirSync(CONFIG_DIR, { recursive: true })` with no mode, resulting in the default `0o755` -- world-readable and world-executable.

Additionally, `setup.ts:523` writes `config.json` (containing the bot token) with `writeFileSync` and no mode option, inheriting the process umask -- typically `0o644`, world-readable.

**Why it matters:** On multi-user systems, another user can read the bot token from `config.json` or the session database from `sessions.db`. This is a real credential exposure.

**Fix:**
- Create `ensureConfigDir()` in `src/utils/config.ts`:
  ```typescript
  export function ensureConfigDir(): string {
    if (!existsSync(CONFIG_DIR)) {
      mkdirSync(CONFIG_DIR, { recursive: true, mode: 0o700 });
    } else {
      chmodSync(CONFIG_DIR, 0o700);
    }
    return CONFIG_DIR;
  }
  ```
- Replace ALL bare `mkdirSync(CONFIG_DIR, ...)` calls across `session.ts`, `setup.ts`, `manager.ts`, and `socket.ts` with `ensureConfigDir()`.
- When writing `config.json` in `setup.ts`, add `{ mode: 0o600 }` to the `writeFileSync` call.
- When writing `sessions.db`, better-sqlite3 does not support mode on creation, so call `chmodSync(DB_PATH, 0o600)` immediately after opening.

**What NOT to do:**
- Do not rely on umask. Different environments have different umasks. Explicit mode is the only reliable approach.
- Do not only fix the `mkdirSync` calls and forget about file permissions. Both the directory and its contents need protection.

---

### 6. Drop File Logging, Stderr-Only

**Problem:** `src/utils/logger.ts:32-38` adds a winston `File` transport writing to `/tmp/claude-telegram-mirror.log` when `NODE_ENV=production`:

```typescript
// logger.ts:32-38
if (process.env.NODE_ENV === 'production') {
  logger.add(new winston.transports.File({
    filename: '/tmp/claude-telegram-mirror.log',
    maxsize: 5242880, // 5MB
    maxFiles: 3
  }));
}
```

No `mode` option is set, so the file is created with default permissions (typically `0o644`). The `/tmp` directory is world-readable on most systems. The log file may contain error messages with bot tokens (see Item 2), session IDs, and project paths.

**Why it matters:** World-readable log files in `/tmp` on multi-user systems expose credentials and session metadata to all local users.

**Fix:**
- Remove the `File` transport entirely. All log output goes to stderr via the existing `Console` transport (which writes to stderr by default in winston).
- Stdout is reserved for programmatic output: hook responses (JSON written by `handler.ts:515`), status JSON, and similar structured data.
- Debug logging is controlled via `LOG_LEVEL=debug` environment variable, which already works with the existing Console transport.
- The systemd service file already captures stderr via `StandardError=journal`. The launchd plist already captures stderr to `~/.config/claude-telegram-mirror/daemon.err.log` (in a 0o700 directory).

**What NOT to do:**
- Do not simply add `mode: 0o600` to the File transport. The `/tmp` location is still problematic -- it is a shared namespace and the filename is predictable. Removing the transport entirely is simpler and more secure.
- Do not redirect logs to a file in the config directory from Node.js. Let the service manager (systemd/launchd) handle log persistence.

---

### 7. NDJSON Line Size Limit

**Problem:** `src/bridge/socket.ts:231-248` (`handleData`) and `src/hooks/handler.ts:480-486` (`main`) read data without any size limit:

```typescript
// socket.ts:231-248
private handleData(clientId: string, data: Buffer): void {
  let buffer = this.buffer.get(clientId) || '';
  buffer += data.toString();
  // ... no size check before accumulating or parsing
}

// handler.ts:480-486
for await (const chunk of process.stdin) {
  input += chunk;  // No size limit
}
```

A malicious client can send a single line of arbitrary size, causing memory exhaustion.

**Why it matters:** Memory exhaustion is a denial-of-service vector. The Unix socket is accessible to any process running as the same user, including potentially compromised Claude Code sessions operating on untrusted repositories.

**Fix:**
- In `socket.ts` `handleData()`: before `JSON.parse`, check if the assembled line exceeds 1,048,576 bytes (1 MiB). If so, drop the line, log a warning including the `clientId`, and reset the buffer for that client.
- In `handler.ts` `main()`: check if `input.length` exceeds 1,048,576 bytes during accumulation. If so, log a warning to stderr and `process.exit(0)` (graceful exit, not error).
- The 1 MiB limit is generous. The largest legitimate messages are approval requests with tool input previews, which are well under 100 KiB.

**What NOT to do:**
- Do not set the limit too low. Tool outputs and transcript summaries can be large. 1 MiB is a safe floor.
- Do not forget to reset the buffer after dropping an oversized line. Otherwise subsequent data on the same connection will be corrupted.

---

### 8. Connection Concurrency Limit

**Problem:** `src/bridge/socket.ts` `SocketServer` accepts unlimited connections. The `handleConnection` method (line 202) adds every new connection to the `clients` Map with no upper bound.

**Why it matters:** Without a connection limit, a local process can exhaust file descriptors or memory by opening thousands of connections to the Unix socket, causing denial of service for legitimate hook handlers.

**Fix:**
- Add an `activeConnections` counter (or use `this.clients.size`) in the `handleConnection` method.
- Before processing a new connection, check: `if (this.clients.size >= 64) { socket.destroy(); return; }`.
- Log a warning when a connection is rejected due to the limit.
- The limit of 64 is generous. Normal operation involves 1-2 concurrent connections per active Claude Code session, and we support at most a handful of sessions simultaneously.

**What NOT to do:**
- Do not use a limit that is too low. During session start, multiple hook events fire in rapid succession, and each one opens a connection. A limit below 32 could cause legitimate connections to be rejected.

---

## UX Improvements (Items 9-12)

### 9. Rule-Based Tool Summarizer

**Problem:** Tool notifications in Telegram show generic messages like "Running: Bash" or "Running: Edit" -- useless for monitoring from mobile. The user must tap the Details button to see what the tool is actually doing.

Currently, `src/bot/formatting.ts` `formatToolExecution()` generates these messages, and `src/bridge/daemon.ts` sends them in the PostToolUse handler.

**Why it matters:** The primary value proposition of this project is mobile monitoring. If every notification requires a tap to understand, the user experience is poor.

**Fix:**
- Create `src/utils/summarize.ts` with two pure functions:
  - `summarizeToolAction(tool: string, input: Record<string, unknown>): string` -- generates a human-readable one-liner from tool name and input.
  - `summarizeToolResult(tool: string, output: string): string` -- generates a human-readable one-liner from tool output, including error detection.
  - `findMeaningfulCommand(command: string): string` -- for Bash tool, extracts the meaningful command from chained commands (pipes, `&&`, `;`).
- Include 30+ patterns covering: `cargo`, `git`, `npm`, `docker`, `make`, `tsc`, `vitest`, `eslint`, `python`, `pip`, `go`, `rustc`, `kubectl`, `terraform`, `curl`, `wget`, `ssh`, `scp`, `tar`, `zip`, `unzip`, `chmod`, `chown`, `mkdir`, `rm`, `cp`, `mv`, `cat`, `grep`, `find`, `sed`, `awk`.
- For Edit/Write tools: show file path and a brief description of the change.
- For Read tool: show file path.
- Call `summarizeToolAction()` from the PostToolUse handler in `daemon.ts`.
- **AUGMENT** the existing raw tool info, do not replace it. Show the summary as a headline, then the raw tool name and command underneath. The Details button remains available for full output.
- Include unit tests for all patterns.

**What NOT to do:**
- Do not use an LLM for summarization. No external API calls. Rule-based is sufficient and deterministic.
- Do not replace the raw tool information. The summary is an addition, not a replacement.

---

### 10. Session Topic Augmentation via `/rename`

**Problem:** Forum topics are created with generic names: `"{hostname} - {sessionId.slice(0,8)}"`. When a user has multiple sessions, these are hard to distinguish in the Telegram sidebar.

**Why it matters:** With multiple active sessions (common for developers running Claude on different projects), navigation becomes frustrating.

**Fix:**
- Detect Claude Code's `/rename` event via hooks. This requires a **research spike** first to determine what hook event `/rename` produces (likely a `UserPromptSubmit` with `/rename` as the prompt, or possibly a separate event).
- Add `editForumTopic()` method to `src/bot/telegram.ts` that calls the Telegram Bot API `editForumTopic` endpoint.
- When a rename event is detected, prepend the user-chosen label to the existing topic name: `"User Label -- originalTopicName"`.
- Also support `/rename` as a Telegram command: user sends `/rename My Feature Work` in a topic, and the topic name is updated.
- Preserve the original topic name details (hostname, session ID snippet) after the separator.

**What NOT to do:**
- Do not replace the original topic name entirely. The hostname and session ID are useful for debugging.
- Do not implement this without the research spike. The hook event for `/rename` needs to be verified empirically.

---

### 11. Photo/Document Download from Telegram

**Problem:** Photos and documents sent in Telegram topics are silently ignored. The bot only handles `message:text` events (registered in `src/bot/telegram.ts:430`). There is no handler for `message:photo`, `message:document`, or other media types.

**Why it matters:** This is a long-wanted feature. Users want to share screenshots, error logs, and reference documents with Claude from mobile. Currently, they must type descriptions instead of simply dropping an image.

**Fix:**
- Register handlers for `message:photo` and `message:document` on the grammY bot instance.
- Use grammY `api.getFile()` to get the file path, then download via the Telegram file API.
- Save to `~/.config/claude-telegram-mirror/downloads/` with `0o600` permissions.
- Sanitize filenames: strip path separators, reject any name containing `..`, prepend a UUID to prevent collisions and predictable names.
- Inject a path string into tmux via the existing injector: `[Image from Telegram: /path/to/file.jpg (800x600)]` for photos, `[Document from Telegram: /path/to/file.pdf]` for documents.
- Cap at 20 MB (Telegram's own API limit for bot file downloads).
- Cleanup files older than 24 hours on the existing stale session cleanup interval.
- For outbound: detect file paths in tool outputs. Route photos (`.jpg`, `.jpeg`, `.png`, `.gif`, `.webp`) to `send_photo`, documents (all other extensions) to `send_document`.

**What NOT to do:**
- Do not store files in `/tmp`. Use the config directory with proper permissions (see Item 5).
- Do not skip filename sanitization. Files from Telegram can have arbitrary names.
- Do not assume all files are small. Enforce the 20 MB cap before downloading.

---

### 12. IDOR Check on Approval Callbacks

**Problem:** The approval handler in `src/bot/commands.ts` and `src/bridge/daemon.ts` resolves approvals without verifying that the callback came from the session's configured chat. When an approval callback arrives, the code looks up the approval by ID and resolves it, but does not check whether the Telegram chat ID matches the session's `chat_id`.

In `src/bridge/session.ts`, the `resolveApproval` method (line 359-376) updates the approval status based solely on the approval ID. No ownership check is performed.

**Why it matters:** This is a belt-and-suspenders defense. In practice, the scenario cannot occur with proper Telegram bot token isolation (each deployment has its own bot token, and grammY's middleware checks `chat_id` against the config). However, defense-in-depth is warranted for approval flows because they control permission decisions for file writes and command execution.

**Fix:**
- After `getApproval(approvalId)`, retrieve the associated session via `getSession(approval.sessionId)`.
- Check that `session.chatId === config.chatId`.
- If mismatch, log a security warning and reject the approval.
- This adds approximately 5 lines of code to the approval resolution path.

**What NOT to do:**
- Do not skip this because "it can't happen." Defense-in-depth is the point. If the outer chatId check in `telegram.ts:120-127` is ever bypassed (middleware ordering change, bug, etc.), this catch prevents escalation.

---

## Operational Improvements (Items 13-16)

### 13. Doctor `--fix` Flag

**Problem:** `src/service/doctor.ts` runs diagnostic checks and reports issues, but the user must manually fix every problem. For many common issues (wrong directory permissions, stale socket files, missing directories), the fix is mechanical and safe.

**Why it matters:** Reduces new user friction. The setup process has multiple moving parts (config dir, permissions, socket, hooks, service), and manual remediation is tedious.

**Fix:**
- Add a `--fix` flag to the `doctor` command (already parsed by commander in `cli.ts`).
- Safe auto-remediation (applied when `--fix` is passed):
  - Config directory permissions: `chmodSync(CONFIG_DIR, 0o700)`
  - Stale socket files: `unlinkSync(SOCKET_PATH)` when socket status is `'stale'`
  - Missing config directory: `ensureConfigDir()` (from Item 5)
  - Stale PID files: remove PID files where the referenced process is not running
- Unsafe remediation (printed as suggestions, never auto-applied):
  - Hook installation/modification (modifies `~/.claude/settings.json`)
  - Service installation/start (modifies system service config)
  - Bot token or chat ID configuration (requires user input)
- After each auto-fix, re-run the relevant check and print the result.

**What NOT to do:**
- Do not auto-fix hooks or service configuration. These modify external config files that may have user customizations.
- Do not auto-fix without printing what was fixed. Always log each remediation action.

---

### 14. SECURITY.md + ADRs

**Problem:** No security documentation exists. Users and contributors have no reference for the threat model, known mitigations, or architectural decisions.

**Why it matters:** Security documentation is essential for maintaining trust, onboarding contributors, and demonstrating due diligence.

**Fix:**
- Write `SECURITY.md` with:
  - Threat model (Mermaid diagram showing trust boundaries: Telegram API, Unix socket, tmux, file system)
  - Per-vulnerability descriptions referencing the items in this ADR
  - File permission summary table (which files, what mode, why)
  - Security checklist for contributors
  - Responsible disclosure policy
- Write ADRs for key architectural decisions:
  - ADR-001: This document (TypeScript security and UX)
  - ADR-002: Rust migration decision (separate document)
  - ADR-003: Dual hook handlers (bash + Node.js) -- why both exist
  - ADR-004: Tmux-only injection -- why we chose tmux over PTY/FIFO
  - ADR-005: SQLite for sessions -- why not files or in-memory
- **Write AFTER security fixes ship.** Document what is, not what will be.

**What NOT to do:**
- Do not write security documentation before the fixes land. The documentation should describe the current state, not a planned future state.

---

### 15. GitHub Actions CI

**Problem:** There is no CI at all. The project has `vitest`, `eslint`, and `tsc` configured in `package.json` but nothing enforces them on push or PR.

**Why it matters:** Without CI, regressions go undetected until they reach users. The security fixes in this ADR will add new tests that must pass on every change.

**Fix:**
- Create `.github/workflows/ci.yml` with the following jobs:
  - **typecheck**: `tsc --noEmit` -- catches type errors
  - **test**: `vitest --run` -- runs the test suite
  - **lint**: `eslint src/` -- enforces code style
- Trigger on push to `master` and on pull requests targeting `master`.
- Node.js version matrix: 18 and 20 (matching `engines.node >= 18.0.0` in package.json).
- Install dependencies with `npm ci` for reproducible builds.

**What NOT to do:**
- Do not add complex CI features (coverage thresholds, deployment, etc.) in this initial PR. Start with the basics and iterate.
- Do not skip the Node.js matrix. We support Node 18+, so we test on both 18 and 20.

---

### 16. Remove Unused `inquirer` Dependency

**Problem:** `inquirer` is listed in `package.json:69` as a production dependency, but `src/service/setup.ts` uses Node.js `readline` directly (line 10, `createInterface` from `readline`). No file in the codebase imports `inquirer`.

**Why it matters:** Dead dependencies increase install size, increase attack surface (supply chain risk), and confuse contributors.

**Fix:**
- Remove `"inquirer": "^8.2.5"` from `dependencies` in `package.json`.
- Run `npm install` to update `package-lock.json`.
- Verify build still succeeds.
- One line change.

**What NOT to do:**
- Do not remove it from `devDependencies` if it were there (it is not). Only remove from `dependencies`.

---

### 17. AskUserQuestion Rendering in Telegram

**Problem:** When Claude Code uses the `AskUserQuestion` tool to ask the user a multi-choice question (e.g., "Which approach should I take?" with 3-4 options), our hook handler does not recognize it as requiring special treatment. The tool is not in the dangerous tools list (`Write`, `Edit`, `Bash`, `MultiEdit`), so it passes through without any Telegram interaction. The user sees a generic "Claude needs your attention" notification and must SSH + tmux into the session to answer. This breaks the "control Claude entirely from your phone" promise.

**Why it matters:** `AskUserQuestion` is Claude's primary mechanism for interactive decision-making. It fires whenever Claude needs clarification, wants the user to choose between approaches, or needs to confirm a direction. Without Telegram support, every multi-choice question forces the user off their phone and onto a terminal. This is the single biggest gap in our bidirectional communication story.

**The data is already available.** The `PreToolUse` hook event for `AskUserQuestion` contains:
- `tool_input.questions` — array of question objects, each with:
  - `question` — the question text
  - `header` — short label (max 12 chars)
  - `options` — array of `{label, description}` objects (2-4 options)
  - `multiSelect` — boolean indicating if multiple options can be selected
- Claude always allows an "Other" free-text response in addition to the listed options

**Fix:**

1. **Detect `AskUserQuestion` in the daemon's `handleToolStart()`.** When a `tool_start` message arrives with `tool_name === 'AskUserQuestion'`, render the question in Telegram instead of a generic tool notification.

2. **Render the question with inline keyboard buttons.** For each option in `tool_input.questions[0].options`, create an inline button with the option's `label` as the button text and a callback data prefix like `answer:{sessionId}:{questionIndex}:{optionIndex}`. Display the question text and option descriptions as message text above the buttons.

3. **Handle button tap.** When the user taps an option button, the daemon:
   - Injects the selected option's label (or index number, depending on what Claude expects) into the tmux session via `send-keys`
   - Edits the Telegram message to show which option was selected (same pattern as approval decisions)
   - Removes the keyboard to prevent double-taps

4. **Handle free-text "Other" response.** If the user types text in the topic instead of tapping a button (within a reasonable window after the question was posted), treat it as the "Other" response and inject it into tmux. This uses the existing Telegram → CLI injection path — no new mechanism needed.

5. **Handle `multiSelect` questions.** When `multiSelect` is true, buttons should toggle on/off rather than immediately submitting. Add a "Submit" button that sends the selected options once the user is done choosing.

6. **Handle multiple questions.** `AskUserQuestion` supports 1-4 questions in a single invocation. Render them sequentially or as a single message with clearly labeled sections.

**What NOT to do:**
- Do not try to use `hookSpecificOutput` / the permission system to respond. `AskUserQuestion` is not a permission decision — it's user input. The response goes through tmux injection, not the hook output.
- Do not assume Claude always sends exactly one question with exactly 3 options. Read the actual `questions` array and handle 1-4 questions with 2-4 options each.
- Do not forget the "Other" path. Users must be able to type a custom response instead of choosing a listed option.

---

## Decision Log

### Adopted

| # | Item | Decision | Rationale |
|---|------|----------|-----------|
| 1 | sendSlashCommand spawn + whitelist | ADOPT | Active shell injection vector |
| 2 | Bot token scrubbing | ADOPT | Credential leak in logs, zero functionality loss |
| 3 | Session ID validation | ADOPT | Input validation at system boundary |
| 4 | Socket path traversal check | ADOPT | Prevents socket hijacking |
| 5 | Config dir permissions | ADOPT | Real vulnerability, inconsistent across modules |
| 6 | Drop file logging | ADOPT | World-readable logs in /tmp |
| 7 | NDJSON line limit | ADOPT | Memory exhaustion protection |
| 8 | Connection limit | ADOPT | DoS protection |
| 9 | Tool summarizer | ADOPT | Transforms monitoring UX |
| 10 | Topic augmentation via /rename | ADOPT | Navigation improvement |
| 11 | Photo/document download | ADOPT | Killer mobile feature, long-wanted |
| 12 | IDOR approval check | ADOPT | Belt-and-suspenders defense |
| 13 | Doctor --fix | ADOPT | Reduces new user friction |
| 14 | SECURITY.md + ADRs | ADOPT | Security documentation gap |
| 15 | CI workflow | ADOPT | No CI is a gap |
| 16 | Remove inquirer | ADOPT | Dead dependency |
| 17 | AskUserQuestion in Telegram | ADOPT | Completes bidirectional control story — multi-choice questions rendered as inline buttons |

### Rejected

| Item | Decision | Rationale |
|------|----------|-----------|
| Rust rewrite (wholesale adoption of fork) | REJECT | DreamLab-AI's rewrite lost ~60% of features (forum topics, approval flow, session management, tmux socket targeting, stale session cleanup, topic auto-deletion, markdown fallback, message chunking, etc.). We do our own migration on our own terms. See ADR-002. |
| LLM summarizer fallback | REJECT | No external API calls for tool summarization. Adding an LLM call to summarize tool output introduces latency, cost, and a dependency on an external service. Rule-based summarization (Item 9) is sufficient, deterministic, and free. |
| `cc <command>` shorthand | REJECT | Already exists in our codebase. The fork's PRD claimed this was missing; it is not. |

### Deferred to Rust Migration (ADR-002)

| Item | Decision | Rationale |
|------|----------|-----------|
| `flock(2)` PID locking | DEFERRED | No clean Node.js equivalent. `fs.flock` does not exist in the standard library. Third-party packages (`proper-lockfile`, `lockfile`) add complexity. Rust has `flock(2)` via `nix` or `libc` crate natively. Our current PID file approach (socket.ts:68-87) is adequate for now. |
| `umask` on socket bind | DEFERRED | `process.umask()` is process-global in Node.js -- setting it for socket creation affects all subsequent file operations. Rust can set umask per-operation via `unsafe { libc::umask() }` scoped to the bind call. Our current approach of `chmodSync` after bind (socket.ts:188) is a workable interim. |
| Governor token-bucket rate limiting | DEFERRED | Must include BOTH token-bucket AND retry/backoff to be correct. Implementing this properly in TypeScript is possible but the Rust `governor` crate provides a battle-tested implementation. Our current simple rate limiter (telegram.ts:30-83) handles the common case. |
| UTF-8-safe truncation | DEFERRED | Slicing a `string` in JavaScript/TypeScript can split multi-byte characters. Rust's `str` type guarantees UTF-8 validity and `char_indices()` provides safe truncation points natively. In TypeScript, this requires `Array.from(text).slice(0, n).join('')` which is correct but awkward. |
| Typed error enum | DEFERRED | Idiomatic in Rust with `thiserror` derive macro and exhaustive `match`. TypeScript's closest equivalent (discriminated unions) works but lacks exhaustiveness checking at the type level for error handling. |
| `HookEvent` typed union | DEFERRED | Rust's `enum` with data variants and exhaustive `match` is the natural fit. TypeScript's discriminated unions work (we use them in `hooks/types.ts`) but Rust's compiler guarantees are stronger for this pattern. |

---

## Implementation Order

1. **Security hardening (Items 1-8)** -- Ship as one commit or PR. These are the highest priority and have zero feature risk. Each fix is small and surgical.
2. **Tool summarizer (Item 9)** -- Standalone feature. New file, new tests, minimal changes to existing code. Can be reviewed independently.
3. **AskUserQuestion in Telegram (Item 17)** -- High-value UX feature that completes the bidirectional control story. Builds on existing approval button patterns.
4. **Photo/document download (Item 11)** -- Standalone feature. Larger scope but self-contained. Requires new handlers and the download pipeline.
5. **Topic augmentation (Item 10)** -- Requires a research spike first to determine what hook event Claude Code's `/rename` produces. Do not implement until the spike is complete.
6. **Operational improvements (Items 13, 15, 16)** -- Can be done in parallel. Each is independent. Item 16 is a one-line change. Item 15 is a new file. Item 13 adds a flag to existing code.
7. **SECURITY.md + ADRs (Item 14)** -- Write AFTER security fixes ship. Document what is, not what will be.

---

## Consequences

### Positive

- All identified security vulnerabilities addressed in TypeScript before any Rust migration
- Shell injection vector eliminated (Item 1)
- Credential leakage in logs eliminated (Items 2, 6)
- Input validation at all system boundaries (Items 3, 4, 7, 8)
- Consistent file permissions across all code paths (Item 5)
- Mobile monitoring UX transformed with tool summaries (Item 9)
- Full bidirectional control from Telegram -- multi-choice questions rendered as tappable buttons (Item 17)
- Photo/document sharing enables new workflows (Item 11)
- CI prevents regressions going forward (Item 15)
- Dead dependency removed (Item 16)
- Codebase is in a strong state whether or not Rust migration proceeds

### Negative

- Development effort required before new features
- Research spike needed for Item 10 (topic augmentation)
- Some items (tool summarizer) require ongoing maintenance as new tools are added

### Neutral

- Six items deferred to Rust migration are acknowledged gaps but have adequate interim mitigations
- Fork's analysis was useful input despite being adversarial in framing
