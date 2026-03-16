---
stepsCompleted: [1, 2, 3]
inputDocuments:
  - docs/adr/ADR-001-typescript-security-and-ux.md
---

# claude-telegram-mirror - Epic Breakdown

## Overview

This document provides the complete epic and story breakdown for claude-telegram-mirror, decomposing the requirements from ADR-001 (TypeScript Security Hardening and UX Improvements) into implementable stories.

## Requirements Inventory

### Functional Requirements

FR1: Switch `sendSlashCommand` from `execSync` to `spawn()` with argument array, add `-l` flag, add character whitelist `/^[a-zA-Z0-9_\- \/]+$/`. Add unit tests for rejection of `;`, `$()`, backticks, `|`, `>`, `<`, `&&`.
FR2: Create `scrubBotToken(text)` function applying `text.replace(/bot\d+:[A-Za-z0-9_-]+\//g, 'bot<REDACTED>/')`. Apply in grammY error handler on `error.description` and `error.message`. Add as winston format transform on `message` and all string `meta` values.
FR3: Validate session IDs on socket ingress in `daemon.ts` `setupSocketHandlers`: max 128 chars, character whitelist `[a-zA-Z0-9_-]` only, drop invalid messages with warning log.
FR4: Validate socket paths: reject paths containing `..`, non-absolute paths, paths >256 chars. Create shared `validateSocketPath()` utility. Apply in `setTmuxSession()` and `loadConfig()`.
FR5: Create `ensureConfigDir()` in `config.ts` with `mkdirSync` + `chmodSync(dir, 0o700)`. Replace all bare `mkdirSync(CONFIG_DIR, ...)` in `session.ts:37`, `setup.ts:509`, `manager.ts:362,368`, `socket.ts:131`. Set `0o600` on `config.json` writes and `sessions.db`.
FR6: Remove winston `File` transport from `logger.ts:32-38`. All log output to stderr via Console transport. Stdout reserved for programmatic output (hook JSON, status).
FR7: Cap NDJSON lines at 1,048,576 bytes in `socket.ts` `handleData()` before `JSON.parse`. Cap `handler.ts` `main()` stdin accumulation at same limit. Drop oversized, log warning, reset buffer.
FR8: Limit concurrent socket connections to 64 in `socket.ts` `handleConnection`. Check `this.clients.size >= 64` before processing. Reject with `socket.destroy()` and warning log.
FR9: Create `src/utils/summarize.ts` with `summarizeToolAction(tool, input)`, `summarizeToolResult(tool, output)`, `findMeaningfulCommand(command)`. Include 30+ patterns for cargo, git, npm, docker, make, tsc, vitest, eslint, python, pip, go, rustc, kubectl, terraform, curl, wget, ssh, tar, chmod, mkdir, rm, cp, mv, grep, find. Call from `handleToolStart()`. Augment raw tool info (summary headline + raw underneath). Include unit tests.
FR10: Detect Claude Code `/rename` event via hooks (requires research spike). Add `editForumTopic(threadId, name)` to `telegram.ts`. Prepend user-chosen label to existing topic name. Also support `/rename` as Telegram command. Preserve original topic details after separator.
FR11: Register `message:photo` and `message:document` handlers on grammY bot. Download via `api.getFile()`. Save to `~/.config/claude-telegram-mirror/downloads/` with `0o600`. Sanitize filenames (strip path separators, reject `..`, prepend UUID). Inject path string into tmux. Cap at 20MB. Cleanup files >24h old on stale session interval.
FR12: After `getApproval(approvalId)`, retrieve associated session and check `session.chatId === config.chatId`. Reject with security warning if mismatch.
FR13: Add `--fix` flag to `doctor` command. Auto-remediate: config dir permissions (`chmod 0o700`), stale socket files (delete), missing dirs (`ensureConfigDir()`), stale PID files. Print suggestions (not auto-fix) for hooks and service configuration.
FR14: Write `SECURITY.md` with threat model (Mermaid diagram), per-vulnerability descriptions, file permission summary, security checklist, responsible disclosure policy. Write ADRs for dual hook handlers, tmux-only injection, SQLite sessions. Write AFTER all security fixes ship.
FR15: Create `.github/workflows/ci.yml` with typecheck (`tsc --noEmit`), test (`vitest --run`), lint (`eslint src/`) jobs. Trigger on push to master and PRs. Node.js 18+20 matrix. `npm ci` for reproducible builds.
FR16: Remove `"inquirer": "^8.2.5"` from `dependencies` in `package.json`. Run `npm install` to update lock file. Verify build.
FR17: Detect `AskUserQuestion` tool in `handleToolStart()`. Render question text and options as inline keyboard buttons with callback data `answer:{sessionId}:{questionIndex}:{optionIndex}`. Handle button tap: inject selected option into tmux, edit message to show selection, remove keyboard. Handle free-text "Other" via normal text injection path. Handle `multiSelect` with toggle buttons + Submit. Handle 1-4 questions per invocation.

### NonFunctional Requirements

NFR1: All security fixes (FR1-FR8) must include unit tests verifying both the fix and rejection of malicious input.
NFR2: Tool summarizer (FR9) must be a pure function with zero external dependencies or API calls.
NFR3: Photo/document downloads (FR11) capped at 20MB (Telegram API limit).
NFR4: CI (FR15) must pass before any merge to master.
NFR5: Security documentation (FR14) written AFTER fixes ship — describe current state, not planned future state.

### Additional Requirements

- ADR-001 Implementation Order must be respected: security first, then UX features, then operational, then documentation
- All existing tests must continue to pass after each change
- Chesterton's fence: read and understand existing code before modifying
- Use "whitelist" terminology, never "allowlist"
- No stub/TODO code — every change must be complete and production-ready

### UX Design Requirements

N/A — no UX design document exists. UX requirements are embedded in the functional requirements (FR9, FR10, FR11, FR17).

### FR Coverage Map

| FR | Epic | Description |
|----|------|------------|
| FR1 | Epic 1 | sendSlashCommand spawn + whitelist |
| FR2 | Epic 1 | Bot token scrubbing |
| FR3 | Epic 1 | Session ID validation |
| FR4 | Epic 1 | Socket path traversal check |
| FR5 | Epic 1 | Config dir permissions |
| FR6 | Epic 1 | Drop file logging |
| FR7 | Epic 1 | NDJSON line limit |
| FR8 | Epic 1 | Connection concurrency limit |
| FR9 | Epic 2 | Tool summarizer |
| FR10 | Epic 5 | Topic augmentation via /rename |
| FR11 | Epic 4 | Photo/document download |
| FR12 | Epic 1 | IDOR approval check |
| FR13 | Epic 6 | Doctor --fix |
| FR14 | Epic 7 | SECURITY.md + ADRs |
| FR15 | Epic 6 | CI workflow |
| FR16 | Epic 6 | Remove inquirer |
| FR17 | Epic 3 | AskUserQuestion in Telegram |

All 17 FRs covered. No gaps.

## Epic List

### Epic 1: Secure the Bridge
Users can trust the bridge daemon with their bot token on shared systems. Shell injection eliminated, credentials scrubbed from logs, input validation at all boundaries, file permissions enforced consistently.
**FRs covered:** FR1, FR2, FR3, FR4, FR5, FR6, FR7, FR8, FR12
**NFRs covered:** NFR1

### Epic 2: Understand What Claude Is Doing
Users monitoring Claude from Telegram see human-readable tool descriptions ("Running tests", "Editing src/config.ts") instead of generic "Running: Bash". No need to tap Details for basic understanding.
**FRs covered:** FR9
**NFRs covered:** NFR2

### Epic 3: Answer Claude's Questions from Telegram
When Claude asks a multi-choice question, users see the options as tappable buttons in Telegram and can respond without leaving their phone. Completes the full remote control story.
**FRs covered:** FR17

### Epic 4: Share Files with Claude from Telegram
Users can send photos, screenshots, and documents from Telegram directly to their Claude session. Claude receives the file path and can work with it.
**FRs covered:** FR11
**NFRs covered:** NFR3

### Epic 5: Name Your Sessions
Users can give meaningful names to forum topics via Claude's `/rename` command or a Telegram `/rename` command, making it easy to find the right session when multiple are open.
**FRs covered:** FR10

### Epic 6: Operational Excellence
Contributors get CI catching regressions on every push. Users get `doctor --fix` for self-healing common issues. Dead dependencies cleaned up.
**FRs covered:** FR13, FR15, FR16
**NFRs covered:** NFR4

### Epic 7: Document the Security Model
Contributors and users have a clear security reference with threat model, architectural decisions, and security checklist.
**FRs covered:** FR14
**NFRs covered:** NFR5

---

## Epic 1: Secure the Bridge

Users can trust the bridge daemon with their bot token on shared systems. Shell injection eliminated, credentials scrubbed from logs, input validation at all boundaries, file permissions enforced consistently.

### Story 1.1: Eliminate Shell Injection in Injector

As a user running the bridge on a shared system,
I want tmux commands to be executed without shell interpolation,
So that no crafted input can execute arbitrary commands on my machine.

**Acceptance Criteria:**

**Given** a slash command containing shell metacharacters (`;`, `$()`, `` ` ``, `|`, `>`, `<`, `&&`)
**When** `sendSlashCommand()` is called with that input
**Then** the command is rejected before reaching tmux
**And** a warning is logged with the rejected command

**Given** a valid slash command like `/clear` or `/compact`
**When** `sendSlashCommand()` is called
**Then** the command is sent to tmux via `spawn()` with argument array (no shell)
**And** the `-l` flag is used for literal mode
**And** tmux receives the command correctly

**Given** the existing `injectViaTmux()` and `sendKey()` methods
**When** they execute
**Then** they also use `spawn()` with argument array instead of `execSync`

### Story 1.2: Scrub Bot Tokens and Drop File Logging

As a user who might share logs or run on a multi-user system,
I want bot tokens removed from all log output and logs kept out of world-readable locations,
So that my Telegram bot credentials are never accidentally exposed.

**Acceptance Criteria:**

**Given** a grammY error containing a bot token in the URL pattern `bot12345:ABCxyz/`
**When** the error is logged
**Then** the token is replaced with `bot<REDACTED>/` in all logged fields

**Given** any log message containing a bot token pattern
**When** it passes through the winston logger
**Then** the format transform scrubs the token from `message` and all string `meta` values

**Given** the daemon starts in any environment
**When** winston initializes
**Then** no `File` transport exists — all output goes to stderr via Console transport
**And** stdout remains clean for programmatic output (hook JSON responses)

**Given** `LOG_LEVEL=debug` is set
**When** debug messages are logged
**Then** they appear on stderr, not stdout

### Story 1.3: Input Validation at System Boundaries

As a user running the bridge,
I want all inputs from the Unix socket validated before processing,
So that malformed or malicious payloads cannot cause memory exhaustion or path traversal.

**Acceptance Criteria:**

**Given** a socket message with a session ID longer than 128 characters
**When** the daemon receives it
**Then** the message is dropped and a warning is logged

**Given** a socket message with a session ID containing characters outside `[a-zA-Z0-9_-]`
**When** the daemon receives it
**Then** the message is dropped and a warning is logged

**Given** a socket message with a valid Claude Code UUID session ID
**When** the daemon receives it
**Then** the message is processed normally

**Given** a tmux socket path containing `..`
**When** `setTmuxSession()` or `loadConfig()` processes it
**Then** the path is rejected and an error is thrown (injector) or default is used (config)

**Given** a non-absolute tmux socket path
**When** `setTmuxSession()` or `loadConfig()` processes it
**Then** the path is rejected

**Given** a tmux socket path longer than 256 characters
**When** `setTmuxSession()` or `loadConfig()` processes it
**Then** the path is rejected

**Given** an NDJSON line exceeding 1,048,576 bytes on the socket
**When** `handleData()` assembles the line
**Then** the line is dropped, a warning is logged with the client ID, and the buffer is reset

**Given** hook stdin input exceeding 1,048,576 bytes
**When** `handler.ts` `main()` accumulates input
**Then** it logs a warning to stderr and exits gracefully

**Given** 64 active socket connections
**When** a 65th connection attempts to connect
**Then** the connection is destroyed immediately and a warning is logged

### Story 1.4: Enforce File Permissions Consistently

As a user on a multi-user system,
I want the config directory and all sensitive files to have restrictive permissions,
So that other users cannot read my bot token or session data.

**Acceptance Criteria:**

**Given** the config directory does not exist
**When** any module calls `ensureConfigDir()`
**Then** the directory is created with mode `0o700`

**Given** the config directory exists with mode `0o755`
**When** any module calls `ensureConfigDir()`
**Then** the directory permissions are corrected to `0o700`

**Given** `config.json` is written by the setup wizard
**When** `writeFileSync` is called
**Then** the file mode is `0o600`

**Given** `sessions.db` is opened by better-sqlite3
**When** the database is first created
**Then** `chmodSync(dbPath, 0o600)` is called immediately after

**Given** any of `session.ts`, `setup.ts`, `manager.ts`, or `socket.ts` need the config directory
**When** they initialize
**Then** they call `ensureConfigDir()` instead of bare `mkdirSync`

### Story 1.5: IDOR Check on Approval Callbacks

As a user with multiple bridge deployments,
I want approval callbacks verified against the session's chat ID,
So that an approval from one context cannot affect another.

**Acceptance Criteria:**

**Given** an approval callback arrives for approval ID `abc123`
**When** the associated session has `chatId` matching `config.chatId`
**Then** the approval is resolved normally

**Given** an approval callback arrives for approval ID `abc123`
**When** the associated session has a different `chatId` than `config.chatId`
**Then** the approval is rejected and a security warning is logged

---

## Epic 2: Understand What Claude Is Doing

Users monitoring Claude from Telegram see human-readable tool descriptions instead of generic "Running: Bash".

### Story 2.1: Create Rule-Based Tool Summarizer

As a user monitoring Claude from Telegram,
I want tool notifications to show what Claude is actually doing,
So that I can understand progress without tapping Details on every message.

**Acceptance Criteria:**

**Given** a Bash tool invocation with command `cargo test --release`
**When** `summarizeToolAction('Bash', {command: 'cargo test --release'})` is called
**Then** it returns `"Running tests (release)"`

**Given** a Bash tool invocation with command `cd /tmp && npm install && npm run build`
**When** `findMeaningfulCommand()` parses the chained command
**Then** it skips `cd` and returns the meaningful command `npm install`
**And** `summarizeToolAction` returns `"Installing dependencies"`

**Given** a Read tool invocation with `file_path: '/opt/project/src/config.ts'`
**When** `summarizeToolAction('Read', {file_path: '/opt/project/src/config.ts'})` is called
**Then** it returns `"Reading .../src/config.ts"`

**Given** an Edit tool invocation with a file path
**When** `summarizeToolAction('Edit', {file_path: '...'})` is called
**Then** it returns `"Editing .../filename.ts"`

**Given** a tool result containing `error[E0433]` or `FAILED`
**When** `summarizeToolResult('Bash', output)` is called
**Then** it returns a string indicating failure with the error snippet

**Given** a tool with no matching pattern (e.g., a custom MCP tool)
**When** `summarizeToolAction('CustomTool', input)` is called
**Then** it returns `"Using CustomTool"` as fallback

**Given** any call to `summarizeToolAction` or `summarizeToolResult`
**When** executed
**Then** no external API calls, network requests, or dependencies outside Node.js stdlib are used

**And** unit tests cover all 30+ patterns including cargo (build/test/clippy/fmt/run/add/install/clean/doc/publish/bench), git (clone/commit/push/pull/checkout/merge/rebase/stash/diff/log/status/branch/tag/fetch/reset), npm/npx/yarn/pnpm/bun, pip/pytest, docker/docker-compose, make, tsc, vitest, eslint, curl, wget, ssh, tar, chmod, mkdir, rm, cp, mv, grep, find, and chained command parsing.

### Story 2.2: Integrate Tool Summarizer into Telegram Notifications

As a user monitoring Claude from Telegram,
I want tool notifications to show the summary headline with raw details underneath,
So that I get both the quick understanding and the full context.

**Acceptance Criteria:**

**Given** a `tool_start` message arrives in the daemon for tool `Bash` with command `git push origin main`
**When** `handleToolStart()` processes it
**Then** the Telegram message shows the summary as headline and raw tool+command underneath
**And** a "Details" button is still present when tool input exists

**Given** a `tool_start` message for tool `Read` with file_path `/src/utils/config.ts`
**When** `handleToolStart()` processes it
**Then** the Telegram message shows the summary with the file path

**Given** a `tool_result` message with output containing error patterns
**When** `handleToolResult()` processes it (in verbose mode)
**Then** the result summary is included in the notification

**Given** the existing tool notification format
**When** the summarizer is integrated
**Then** all existing functionality is preserved — the raw tool name, the Details button, the tool input cache for `tooldetails:` callbacks all continue to work

---

## Epic 3: Answer Claude's Questions from Telegram

When Claude asks a multi-choice question, users see the options as tappable buttons in Telegram and can respond without leaving their phone.

### Story 3.1: Detect and Render AskUserQuestion in Telegram

As a user monitoring Claude from my phone,
I want to see Claude's questions displayed with tappable option buttons,
So that I can understand what Claude is asking without switching to a terminal.

**Acceptance Criteria:**

**Given** a `tool_start` message with `tool_name === 'AskUserQuestion'`
**When** `handleToolStart()` processes it
**Then** the question text is displayed as the message body
**And** each option is rendered as an inline keyboard button with `label` as button text
**And** each option's `description` is shown in the message text below the question
**And** the callback data follows the pattern `answer:{sessionId}:{questionIndex}:{optionIndex}`
**And** the `header` field is shown as a bold label above the question

**Given** an `AskUserQuestion` with 4 options
**When** rendered in Telegram
**Then** all 4 options appear as buttons
**And** button layout uses 1 button per row for readability on mobile

**Given** an `AskUserQuestion` event
**When** the question is rendered
**Then** a free-text hint is included: "Or type your answer to respond with 'Other'"

### Story 3.2: Handle Single-Select Responses

As a user viewing Claude's question in Telegram,
I want to tap an option button and have my answer sent to Claude,
So that I can respond to questions without typing.

**Acceptance Criteria:**

**Given** a rendered AskUserQuestion with single-select options
**When** the user taps option button "Option A"
**Then** the text "Option A" is injected into the tmux session via `send-keys`
**And** the Telegram message is edited to show "Selected: Option A"
**And** the inline keyboard is removed to prevent double-taps

**Given** a rendered AskUserQuestion
**When** the user types free text in the topic instead of tapping a button
**Then** the text is injected into tmux as the "Other" response via the normal text injection path
**And** the question message keyboard is removed

**Given** a rendered AskUserQuestion
**When** the user taps a button after already responding (race condition)
**Then** the tap is ignored gracefully (answer the callback query with "Already answered")

**Given** an `answer:` callback query
**When** the session's tmux target is not available
**Then** an error message is shown: "Cannot send response — no tmux session found"
**And** the keyboard remains so the user can retry after reconnection

### Story 3.3: Handle Multi-Select and Multi-Question

As a user receiving a complex question from Claude,
I want to select multiple options or answer multiple questions in sequence,
So that I can fully respond to Claude's information needs from Telegram.

**Acceptance Criteria:**

**Given** an `AskUserQuestion` with `multiSelect: true`
**When** rendered in Telegram
**Then** each option button toggles between selected (prefixed with a checkmark) and unselected
**And** a "Submit" button appears below the options
**And** tapping an option does NOT immediately submit — it toggles the selection state

**Given** a multi-select question with options A, B, C selected
**When** the user taps "Submit"
**Then** the selected options are injected into tmux in the format Claude expects
**And** the message is edited to show "Selected: A, B, C"
**And** the keyboard is removed

**Given** an `AskUserQuestion` with 2 or more questions in the `questions` array
**When** rendered in Telegram
**Then** each question is displayed as a clearly labeled section with its own set of option buttons
**And** questions are numbered (e.g., "Q1:", "Q2:")

**Given** a multi-question invocation where the user answers Q1
**When** Q1 is answered
**Then** Q1's keyboard is removed and shows the selection
**And** Q2 remains active for the user to answer

---

## Epic 4: Share Files with Claude from Telegram

Users can send photos, screenshots, and documents from Telegram directly to their Claude session.

### Story 4.1: File Download Pipeline and Storage

As a user who wants to share files with Claude,
I want a secure download and storage system for Telegram files,
So that files are saved safely and cleaned up automatically.

**Acceptance Criteria:**

**Given** the downloads directory `~/.config/claude-telegram-mirror/downloads/` does not exist
**When** the first file download is triggered
**Then** the directory is created with mode `0o700` via `ensureConfigDir()` pattern

**Given** a file downloaded from Telegram
**When** it is saved to the downloads directory
**Then** the file has mode `0o600`
**And** the filename is `{uuid}_{sanitized_original_name}`
**And** path separators (`/`, `\`) in the original name are replaced with `_`
**And** filenames starting with `.` are prefixed with `_`
**And** filenames containing `..` are rejected

**Given** the file size reported by Telegram exceeds 20MB
**When** the download is requested
**Then** the download is skipped and a message is sent to the topic: "File too large (max 20MB)"

**Given** the stale session cleanup interval runs (every 5 minutes)
**When** files in the downloads directory are older than 24 hours
**Then** they are deleted

**Given** a network error during file download
**When** `api.getFile()` or the HTTP fetch fails
**Then** a warning is logged and a message is sent to the topic: "Failed to download file"

### Story 4.2: Photo Download and Injection

As a user who wants to share a screenshot with Claude,
I want to send a photo in a Telegram topic and have Claude receive the file path,
So that Claude can read and analyze the image.

**Acceptance Criteria:**

**Given** a user sends a photo in a forum topic linked to an active session
**When** the `message:photo` handler fires
**Then** the highest-resolution photo variant is downloaded (last element in `msg.photo` array)
**And** saved to the downloads directory with `.jpg` extension
**And** the text `[Image from Telegram: /full/path/to/uuid_photo.jpg]` is injected into the tmux session
**And** a confirmation message is sent to the topic: "Photo sent to Claude"

**Given** a photo is sent with a caption
**When** the handler processes it
**Then** the caption is appended to the injection: `[Image from Telegram: /path/to/file.jpg] Caption: user's caption text`

**Given** a photo is sent in a topic with no active session
**When** the handler processes it
**Then** the photo is not downloaded and a message is sent: "No active session for this topic"

**Given** a photo is sent in the General topic (no thread ID)
**When** the handler processes it
**Then** the photo is silently ignored (consistent with BUG-005 behavior)

### Story 4.3: Document Download and Injection

As a user who wants to share a file with Claude,
I want to send a document in a Telegram topic and have Claude receive the file path,
So that Claude can read and work with the file.

**Acceptance Criteria:**

**Given** a user sends a document (PDF, text file, log file, etc.) in a forum topic
**When** the `message:document` handler fires
**Then** the file is downloaded and saved with its original extension preserved
**And** the text `[Document from Telegram: /full/path/to/uuid_filename.pdf]` is injected into the tmux session
**And** a confirmation message is sent to the topic: "Document sent to Claude"

**Given** a document with a filename containing path separators
**When** the filename is sanitized
**Then** `/` and `\` are replaced with `_` and the file saves successfully

**Given** a document with no filename (Telegram allows this)
**When** the handler processes it
**Then** a default name `uuid_unnamed` with the MIME-type-based extension is used

**Given** a document with a caption
**When** the handler processes it
**Then** the caption is appended to the injection string

---

## Epic 5: Name Your Sessions

Users can give meaningful names to forum topics for easy navigation.

### Story 5.1: Research Spike — Claude Code /rename Hook Event

As a developer planning the topic rename feature,
I want to understand exactly what hook event Claude Code's `/rename` command produces,
So that we implement the correct detection logic.

**Acceptance Criteria:**

**Given** a Claude Code session running in tmux
**When** the user types `/rename My Feature Work` in the CLI
**Then** the hook events fired are documented (event type, payload structure, timing)

**Given** the research is complete
**When** the findings are documented
**Then** the document specifies: which hook type fires (UserPromptSubmit, Notification, or other), what the payload looks like, whether the new name appears in a specific field, and whether there are edge cases (empty rename, special characters)

**Given** `/rename` does NOT produce a detectable hook event
**When** the research concludes
**Then** an alternative approach is documented (e.g., Telegram-only `/rename` command, or parsing UserPromptSubmit for `/rename` prefix)

### Story 5.2: Implement Topic Name Augmentation

As a user with multiple active sessions,
I want to give my forum topics meaningful names,
So that I can quickly find the right session in the Telegram sidebar.

**Acceptance Criteria:**

**Given** a Claude Code `/rename` event is detected (based on Story 5.1 findings)
**When** the daemon processes the event for a session with an active forum topic
**Then** the topic name is updated to `"User Label — originalTopicName"`
**And** the original topic name (hostname, session ID) is preserved after the separator

**Given** a user sends `/rename My Feature` in a Telegram topic
**When** the bot command handler processes it
**Then** the topic is renamed to `"My Feature — originalTopicName"`
**And** a confirmation message is sent: "Topic renamed"

**Given** a rename with a very long label (>80 characters)
**When** the rename is processed
**Then** the label is truncated at a word boundary to fit within Telegram's 128-char topic name limit
**And** the original details are still appended if space allows

**Given** a rename attempt in a topic with no active session
**When** the command is processed
**Then** the user sees: "No active session for this topic"

**Given** the topic has already been renamed
**When** a second rename is requested
**Then** the previous label is replaced with the new one, preserving the original topic details

---

## Epic 6: Operational Excellence

Contributors get CI catching regressions. Users get `doctor --fix` for self-healing. Dead deps cleaned up.

### Story 6.1: GitHub Actions CI

As a contributor to the project,
I want automated checks on every push and PR,
So that regressions are caught before they reach users.

**Acceptance Criteria:**

**Given** a push to the `master` branch
**When** GitHub Actions triggers
**Then** three jobs run: typecheck (`tsc --noEmit`), test (`vitest --run`), lint (`eslint src/`)

**Given** a pull request targeting `master`
**When** GitHub Actions triggers
**Then** the same three jobs run

**Given** the CI workflow
**When** it executes
**Then** it tests on both Node.js 18 and Node.js 20
**And** dependencies are installed with `npm ci` for reproducibility

**Given** any job fails
**When** the PR is viewed on GitHub
**Then** the failure is visible and the check is marked as failed

**Given** the workflow file
**When** reviewed
**Then** it contains no deployment steps, coverage thresholds, or other non-essential complexity

### Story 6.2: Doctor --fix Flag

As a user encountering configuration issues,
I want `ctm doctor --fix` to automatically repair common problems,
So that I don't have to manually run remediation commands.

**Acceptance Criteria:**

**Given** the config directory has mode `0o755`
**When** `ctm doctor --fix` runs
**Then** the permissions are corrected to `0o700` and the fix is reported

**Given** a stale socket file exists (daemon not running)
**When** `ctm doctor --fix` runs
**Then** the stale socket is deleted and the fix is reported

**Given** the config directory does not exist
**When** `ctm doctor --fix` runs
**Then** it is created with `0o700` via `ensureConfigDir()` and the fix is reported

**Given** a stale PID file references a dead process
**When** `ctm doctor --fix` runs
**Then** the PID file is removed and the fix is reported

**Given** hooks are missing or outdated
**When** `ctm doctor --fix` runs
**Then** a suggestion is printed: "Run `ctm install-hooks` to fix" — NOT auto-fixed

**Given** the system service is not installed
**When** `ctm doctor --fix` runs
**Then** a suggestion is printed: "Run `ctm service install` to set up" — NOT auto-fixed

**Given** `ctm doctor` runs WITHOUT `--fix`
**When** issues are found
**Then** behavior is identical to current: report only, no changes

### Story 6.3: Remove Unused inquirer Dependency

As a contributor to the project,
I want dead dependencies removed,
So that install size is smaller and supply chain risk is reduced.

**Acceptance Criteria:**

**Given** `package.json` currently lists `"inquirer": "^8.2.5"` in `dependencies`
**When** the dependency is removed and `npm install` is run
**Then** `package-lock.json` is updated
**And** `npm run build` succeeds
**And** `npm test` passes
**And** no file in `src/` imports `inquirer`

---

## Epic 7: Document the Security Model

Contributors and users have a clear security reference with threat model, architectural decisions, and security checklist.

### Story 7.1: Write SECURITY.md and Architectural ADRs

As a contributor or user evaluating this project,
I want a clear security reference documenting the threat model and architectural decisions,
So that I can trust the tool and understand what's safe to change.

**Acceptance Criteria:**

**Given** all security fixes from Epic 1 have shipped
**When** SECURITY.md is written
**Then** it includes a Mermaid threat model diagram showing trust boundaries (Telegram API, Unix socket, tmux, file system, hook scripts)
**And** per-vulnerability descriptions referencing the fixes from ADR-001 Items 1-8
**And** a file permission summary table (which files, what mode, why)
**And** a security checklist for contributors (what to check before modifying security-sensitive code)
**And** a responsible disclosure policy

**Given** the need for architectural decision documentation
**When** ADRs are written
**Then** ADR-003 documents the dual hook handler architecture (bash fire-and-forget + Node.js approval blocking) — why both exist, what each handles, why not consolidate
**And** ADR-004 documents the tmux-only injection decision — why tmux was chosen over PTY and FIFO, why the others are dead code
**And** ADR-005 documents the SQLite session storage decision — why not files or in-memory, what the schema provides

**Given** any ADR
**When** it is written
**Then** it follows standard ADR format: Title, Status, Date, Context, Decision, Consequences
**And** it references specific code paths and file locations

**Given** the current state of the codebase
**When** the documentation is reviewed
**Then** it accurately describes what IS, not what was planned or what will be
