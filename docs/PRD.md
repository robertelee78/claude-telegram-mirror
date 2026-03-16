# Product Requirements Document: Claude Telegram Mirror (Rust)

## Overview

Bidirectional bridge between Claude Code CLI sessions and Telegram, rewritten in Rust for memory safety, zero-runtime overhead, and elimination of 10 security vulnerabilities found in the TypeScript implementation.

## Problem Statement

The TypeScript implementation has 3 CRITICAL, 4 HIGH, and 3 MEDIUM security vulnerabilities including command injection via `execSync`, world-readable secrets, and TOCTOU race conditions in PID locking. A Rust rewrite eliminates entire vulnerability classes through type safety and `Command::arg()` API.

## Security Vulnerabilities (10 total)

| # | Severity | Description | Root Cause |
|---|----------|-------------|------------|
| 1 | CRITICAL | Command injection in tmux slash commands | `execSync` with string interpolation |
| 2 | CRITICAL | FIFO path injection | User-controlled path in shell command |
| 3 | CRITICAL | World-readable config files with bot token | Missing file permission enforcement |
| 4 | HIGH | Log files in /tmp with default permissions | Public temp directory, no mode set |
| 5 | HIGH | Chat ID bypass on callback queries | Filter only on message updates |
| 6 | HIGH | Config directory created without restrictive perms | Missing mkdir mode |
| 7 | HIGH | Tmux target interpolated in shell | String concatenation in exec |
| 8 | MEDIUM | TOCTOU in PID file locking | check-then-write race condition |
| 9 | MEDIUM | No rate limiting on input injection | Unbounded message processing |
| 10 | MEDIUM | JSON parse panics on malformed input | `unwrap()` on untrusted data |

## Feature Requirements

### Preserved from TypeScript
- Forum topic creation per Claude Code session
- Inline keyboard approval buttons (Approve/Reject/Abort)
- Message queue with rate limiting and exponential backoff
- Session management with SQLite persistence
- Tmux input injection for remote control
- Hook integration with Claude Code (pre/post tool use, stop, compact)
- Stale session cleanup with differentiated timeouts
- Topic auto-deletion with configurable delay
- Message chunking for Telegram's 4096 char limit
- MarkdownV2 formatting with code block preservation
- Multi-session support with thread mapping

### New in Rust Version
- Single static binary (`ctm`) with clap subcommands
- `flock(2)` atomic PID locking
- `governor` token-bucket rate limiting per chat_id
- All file operations via `OpenOptions::mode()`
- Zero `unsafe` blocks
- No shell interpolation anywhere
- **Bidirectional image/file transfer** — photos/documents from Telegram downloaded and injected into Claude; images/files sent to Telegram via bridge socket
- **Human-readable tool summaries** — rule-based with optional LLM fallback
- **Stale topic auto-cleanup** — dead sessions and their forum topics cleaned up automatically

## Non-Functional Requirements

- Hook latency: <5ms for fire-and-forget events
- File permissions: 0o600 for files, 0o700 for directories
- PID locking: atomic via flock(2)
- Rate limiting: configurable per chat_id
- Binary size: <20MB static release build
- Memory: <10MB RSS at idle
- Startup: <100ms to accepting connections

## CLI Interface

```
ctm start              # Run daemon in foreground
ctm stop [--force]     # Stop daemon
ctm hook               # Process hook event from stdin
ctm setup              # Interactive setup wizard
ctm doctor             # Run diagnostics
ctm status             # Show daemon status
```

## Configuration

Priority: Environment variables > config file > defaults

| Variable | Default | Description |
|----------|---------|-------------|
| TELEGRAM_BOT_TOKEN | (required) | Bot token from @BotFather |
| TELEGRAM_CHAT_ID | (required) | Target chat ID |
| TELEGRAM_MIRROR | true | Enable/disable mirroring |
| TELEGRAM_MIRROR_VERBOSE | true | Show tool input/output |
| TELEGRAM_MIRROR_APPROVALS | true | Enable approval buttons |
| TELEGRAM_USE_THREADS | true | Enable forum topics |
| TELEGRAM_CHUNK_SIZE | 4000 | Max message chunk size |
| TELEGRAM_RATE_LIMIT | 1 | Messages per second |
| TELEGRAM_SESSION_TIMEOUT | 30 | Session timeout minutes |
| TELEGRAM_STALE_SESSION_TIMEOUT_HOURS | 72 | Stale cleanup hours |
| TELEGRAM_AUTO_DELETE_TOPICS | true | Auto-delete topics |
| TELEGRAM_TOPIC_DELETE_DELAY_MINUTES | 1440 | Deletion delay |

## Success Criteria

1. `cargo build --release` with zero warnings
2. `cargo clippy` clean
3. All 10 security vulnerabilities eliminated
4. Feature parity with TypeScript version
5. All unit and integration tests passing
6. Container integration via supervisor
