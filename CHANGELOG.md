# Changelog

All notable changes to this project will be documented in this file.

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
