# Changelog

All notable changes to this project will be documented in this file.

## [0.1.17] - 2025-12-09

### Fixed
- **BUG-009: Reactivate ended sessions on new hook events** - Sessions marked as 'ended' are now automatically reactivated when new hook events arrive
  - Fixes issue where Telegram → CLI input silently failed after session was incorrectly marked ended
  - Added `reactivateSession()` method to SessionManager
  - `ensureSessionExists()` now checks session status and reactivates if needed

## [0.1.16] - 2025-12-09

### Added
- **FEAT-001: CLI lifecycle commands** - New `ctm stop` and `ctm restart` commands
  - `ctm stop` - Gracefully stop the running daemon (sends SIGTERM, waits up to 5s)
  - `ctm stop --force` - Force kill if graceful shutdown fails
  - `ctm restart` - Stop and restart the daemon in one command
  - Commands auto-detect if running as OS service and delegate appropriately
  - Cleans up stale PID and socket files automatically

- **Enhanced `ctm status` command** - Now shows daemon running state
  - Shows PID when daemon is running directly
  - Shows "(via system service)" when running under systemd/launchd
  - Shows socket file status
  - Detects stale PID files

### Changed
- `isServiceInstalled()` function exported from service manager for CLI use
- README.md updated with complete CLI command documentation

## [0.1.15] - 2025-12-09

### Fixed
- **BUG-005: Ignore General topic messages** - Messages in the forum's General topic are now ignored
  - Only messages in specific forum topics (with threadId) are routed to Claude sessions
  - Daemon can still write to General topic (startup/shutdown notifications)
  - Prevents confusion when user accidentally posts in General instead of session topic

- **BUG-006: Remove file-based session tracking** - Daemon SQLite is now single source of truth
  - Removed `.session_active_*` file tracking from both bash hook and Node handler
  - Hooks are now stateless - they just forward events to daemon
  - Eliminates inconsistency between bash (kept tracking on Stop) and Node (cleared on Stop)
  - Daemon's `ensureSessionExists()` handles all session creation via SQLite

## [0.1.14] - 2025-12-09

### Fixed
- **BUG-003: Stale session cleanup** - Sessions with dead tmux panes are now automatically cleaned up
  - New `staleSessionTimeoutHours` config (default 72 hours, configurable via env or config file)
  - Cleanup only triggers when: `lastActivity > 72h` AND (pane gone OR pane reassigned to another session)
  - Sends "Session ended (terminal closed)" message before closing forum topic
  - Prevents stale "active" sessions from accumulating indefinitely

- **BUG-004: Stop command sends wrong key** - Fixed interrupt behavior for Claude Code
  - `sendKey` method now includes `-S socket` flag for correct tmux server targeting
  - **Interrupt commands** (`stop`, `cancel`, `abort`, `esc`, `escape`) now send **Escape** to pause Claude
  - **Kill commands** (`kill`, `exit`, `quit`, `ctrl+c`, `ctrl-c`, `^c`) send **Ctrl-C** to exit Claude entirely
  - All commands work with or without leading `/` (e.g., `stop` or `/stop`)

### Added
- `TELEGRAM_STALE_SESSION_TIMEOUT_HOURS` environment variable for configuring stale session cleanup
- New kill command category for exiting Claude entirely (vs just interrupting)

## [0.1.13] - 2025-12-08

### Fixed
- **BUG-002: Race condition in topic creation** - Messages no longer leak to General topic when events arrive out-of-order
  - Added promise-based topic lock with 5-second timeout
  - All handlers now await topic creation before sending messages
  - Explicit failure (error log + drop message) on timeout instead of silent misdirection

- **Closed topic auto-reopen** - Bot automatically reopens topics closed by user in Telegram
  - Detects `TOPIC_CLOSED` error and calls `reopenForumTopic()`
  - Sends "Topic reopened" notification after recovery
  - Retries original message after successful reopen

- **PreToolUse regression: Missing tool details** - Restored detailed tool call information in Telegram
  - PreToolUse now runs BOTH bash script (tool details) AND Node.js handler (approvals) in parallel
  - Safe tools (ls, cat, pwd, etc.) now appear in Telegram - they were silently skipped before
  - Rich expandable context restored for all tool invocations

### Changed
- **Smart hook installer** - Auto-fixes configuration without `--force` flag
  - Compares existing CTM hooks with expected configuration
  - Only updates hooks that need changes, preserves user's other hooks
  - Reports what changed: `added`, `updated`, or `unchanged`
  - Removed `--force` option (no longer needed)

## [0.1.11] - 2025-12-08

### Fixed
- **Respect bypass permissions mode** - Skip Telegram approval prompts when Claude Code is in `bypassPermissions` mode
- Deployed with bypass fix included (0.1.10 was missing the fix)

## [0.1.9] - 2025-12-08

### Fixed
- **Critical: Telegram approval buttons now work correctly**
  - Fixed hook event type mismatch: Claude Code sends `hook_event_name` but handler was checking `type`
  - PreToolUse hooks now properly send `approval_request` messages to daemon
  - Approval buttons (Approve/Reject/Abort) now appear in Telegram for dangerous operations

- **Fixed message update after approval**
  - Changed to plain text mode to avoid Markdown parsing conflicts
  - Message now correctly updates to show decision after clicking approval button

### Changed
- Updated `types.ts` to use `hook_event_name` instead of `type` to match Claude Code's actual JSON format
- Added fallback timestamps for hook events where timestamp is optional
- Added additional Claude Code fields to hook types: `transcript_path`, `cwd`, `permission_mode`

## [0.1.8] - 2025-12-07

### Added
- Initial release with Telegram approval buttons feature
- Bidirectional Claude Code ↔ Telegram integration
- Session mirroring with forum topics
- Tool execution notifications
- Input injection from Telegram to CLI
