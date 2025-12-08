# Changelog

All notable changes to this project will be documented in this file.

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
