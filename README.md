# Claude Code Rust Telegram

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)

Bidirectional communication between Claude Code CLI and Telegram, written in Rust. Control your Claude Code sessions from your phone.

**Supported platforms:** Linux, macOS

## Installation

### From source

```bash
git clone https://github.com/DreamLab-AI/Claude-Code-Rust-Telegram.git
cd Claude-Code-Rust-Telegram
cargo build --release
# Binary at target/release/ctm
```

### Quick start

```bash
# 1. Build
cargo build --release

# 2. Run setup wizard
./target/release/ctm setup

# 3. Start the bridge daemon
./target/release/ctm start

# 4. Run Claude Code in tmux
tmux new -s claude
claude
```

## Features

- **CLI to Telegram**: Mirror Claude's responses, tool usage, and notifications
- **Telegram to CLI**: Send prompts from Telegram directly to Claude Code
- **Human-Readable Summaries**: Tool actions shown as natural language ("Running tests", "Editing config.rs") instead of raw operations, with optional LLM fallback for unknown tools
- **Stop/Interrupt**: Type `stop` in Telegram to send Escape and halt Claude mid-process
- **Kill**: Type `kill` to send Ctrl-C and exit Claude entirely
- **Session Threading**: Each Claude session gets its own Forum Topic
- **Tool Approval**: Approve/reject tool executions via inline keyboard buttons
- **Compaction Notifications**: Get notified when Claude summarizes context
- **Multi-System Support**: Run separate daemons on multiple machines with shared group
- **Auto-cleanup**: Stale sessions and forum topics are cleaned up automatically

## Security

This is a complete Rust rewrite that fixes 10 security vulnerabilities (3 CRITICAL, 4 HIGH, 3 MEDIUM) from the original TypeScript implementation:

| Fix | Description |
|-----|-------------|
| No shell interpolation | All tmux commands use `Command::new().arg()` - zero injection surface |
| Restrictive file permissions | Config: 0o600, directories: 0o700 |
| Atomic PID locking | `flock(2)` eliminates TOCTOU race conditions |
| Chat ID validation | Verified on ALL update types including callbacks |
| Rate limiting | Token-bucket via `governor` crate |
| Safe JSON parsing | All `serde_json` calls return `Result`, no panics |

See [docs/PRD.md](docs/PRD.md) for the full vulnerability matrix and [docs/adr/](docs/adr/) for architectural decision records.

## CLI Commands

```bash
ctm start              # Start bridge daemon (foreground)
ctm hook               # Process hook events from stdin (called by Claude Code)
ctm status             # Show daemon status and configuration
ctm doctor             # Run diagnostics and validate configuration
ctm doctor --fix       # Auto-fix issues where possible
ctm setup              # Interactive setup wizard
```

## Telegram Commands

Once connected, control Claude from Telegram:

| Input | Action |
|-------|--------|
| Any text | Sends to Claude as input |
| `stop` / `esc` / `escape` | Sends Escape to pause Claude |
| `kill` / `exit` / `quit` / `ctrl-c` | Sends Ctrl-C to exit Claude |
| `cc clear` | Sends `/clear` to Claude |
| `cc compact` | Sends `/compact` to Claude |
| `/status` | Show active sessions |
| `/help` | Show available commands |
| `/ping` | Health check |

### Tool Approval Buttons

When Claude requests tool permission, you'll see inline keyboard buttons:

| Button | Action |
|--------|--------|
| Approve | Allow the tool to execute |
| Reject | Deny this specific tool execution |
| Abort | Stop the entire Claude session |
| Details | View full tool input parameters |

## Architecture

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│   Claude Code   │────>│  Bridge Daemon  │────>│    Telegram     │
│      CLI        │<────│   (Rust/ctm)    │<────│      Bot        │
└─────────────────┘     └─────────────────┘     └─────────────────┘
        │                       │
        │ hooks                 │ Unix socket (NDJSON)
        v                       v
┌─────────────────┐     ┌─────────────────┐
│  Hook handler   │────>│  Socket Server  │
│  (ctm hook)     │     │  flock PID lock │
├─────────────────┤     │  0o600 perms    │
│  Bash script    │────>│                 │
│  (fire & forget)│     │                 │
└─────────────────┘     └─────────────────┘
```

### Components

| Module | Responsibility |
|--------|---------------|
| `main.rs` | CLI entrypoint with clap subcommands |
| `bridge.rs` | Central orchestrator: socket + telegram + sessions |
| `bot.rs` | Telegram API wrapper with rate limiting (governor) |
| `socket.rs` | Unix socket server with flock(2) PID locking |
| `session.rs` | SQLite session management (rusqlite) |
| `hook.rs` | Claude Code hook event processing |
| `injector.rs` | tmux command injection (Command::arg, no shell) |
| `config.rs` | Configuration: env > file > defaults, secure perms |
| `formatting.rs` | Message formatting, ANSI stripping, tool summaries |
| `summarizer.rs` | LLM-backed fallback summarizer (optional, for unknown tools) |
| `types.rs` | Shared types: BridgeMessage, HookEvent, Session |
| `error.rs` | Error types via thiserror |

### Message Flow

1. Claude Code hooks capture events (prompts, responses, tool use)
2. `ctm hook` reads stdin JSON, forwards to bridge via unix socket
3. Bridge routes messages to the correct Telegram Forum Topic
4. Telegram replies are injected into CLI via `tmux send-keys`
5. Stop/kill commands send Escape or Ctrl-C to interrupt Claude

## Prerequisites

- Rust 1.75+ (for building from source)
- Claude Code CLI
- tmux (for bidirectional communication)
- Telegram account

## Telegram Setup

### 1. Create a Bot

1. Message [@BotFather](https://t.me/botfather) -> `/newbot`
2. Choose name and username (must end in `bot`)
3. Save the API token

### 2. Create Supergroup with Topics

1. Create a new group in Telegram
2. Add your bot to the group
3. Group Settings -> Enable **Topics**

### 3. Make Bot an Admin

1. Group Settings -> Administrators -> Add your bot
2. Enable: **Manage Topics**, **Post Messages**

### 4. Disable Privacy Mode

1. [@BotFather](https://t.me/botfather) -> `/mybots` -> Select bot
2. Bot Settings -> Group Privacy -> **Turn off**

### 5. Get Chat ID

```bash
./scripts/get-chat-id.sh YOUR_BOT_TOKEN
```

Or manually visit: `https://api.telegram.org/botYOUR_TOKEN/getUpdates`

Supergroup chat IDs start with `-100`.

## Configuration

### Environment Variables

```bash
export TELEGRAM_BOT_TOKEN="123456789:ABCdefGHIjklMNOpqrsTUVwxyz"
export TELEGRAM_CHAT_ID="-1001234567890"
export TELEGRAM_MIRROR=true

# Optional:
# export TELEGRAM_MIRROR_VERBOSE=true       # Show tool start/result messages
# export TELEGRAM_USE_THREADS=true          # Use forum topics (default: true)
# export TELEGRAM_AUTO_DELETE_TOPICS=true   # Delete topics on session end
# export TELEGRAM_TOPIC_DELETE_DELAY=5      # Minutes before topic deletion
# export TELEGRAM_BRIDGE_SOCKET=~/.config/claude-telegram-mirror/bridge.sock
# export CTM_LLM_SUMMARIZE_URL=https://api.anthropic.com/v1/messages
# export CTM_LLM_API_KEY=sk-ant-...        # For LLM-powered tool summaries
```

### Config File (Alternative)

`~/.config/claude-telegram-mirror/config.json`:

```json
{
  "bot_token": "your-token",
  "chat_id": -1001234567890,
  "enabled": true,
  "verbose": true,
  "use_threads": true,
  "auto_delete_topics": false,
  "llm_summarize_url": "https://api.anthropic.com/v1/messages",
  "llm_api_key": "sk-ant-..."
}
```

Both `snake_case` and `camelCase` field names are accepted.

Environment variables take precedence over config file values.

### Claude Code Hook Installation

Add to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [{ "command": "ctm hook" }],
    "PostToolUse": [{ "command": "ctm hook" }],
    "Notification": [{ "command": "ctm hook" }],
    "Stop": [{ "command": "ctm hook" }],
    "UserPromptSubmit": [{ "command": "ctm hook" }]
  }
}
```

Or use the fire-and-forget bash script for non-approval hooks:

```json
{
  "hooks": {
    "PreToolUse": [{ "command": "ctm hook" }],
    "PostToolUse": [{ "command": "scripts/telegram-hook.sh" }],
    "Notification": [{ "command": "scripts/telegram-hook.sh" }],
    "Stop": [{ "command": "scripts/telegram-hook.sh" }],
    "UserPromptSubmit": [{ "command": "scripts/telegram-hook.sh" }]
  }
}
```

## Multi-System Architecture

When running Claude Code on multiple machines:

- **One daemon per host** - Each machine runs its own bridge daemon
- **One bot per daemon** - Each daemon uses a unique Telegram bot token
- **Shared supergroup** - All bots post to the same Telegram supergroup
- **Isolated topics** - Each daemon only processes topics it created

Create one bot per system via [@BotFather](https://t.me/botfather) and add all bots to the same supergroup with admin permissions.

## Technical Details

- **Session storage**: SQLite at `~/.config/claude-telegram-mirror/sessions.db`
- **Socket path**: `~/.config/claude-telegram-mirror/bridge.sock`
- **PID lock**: `~/.config/claude-telegram-mirror/bridge.pid` (flock-based)
- **Protocol**: NDJSON over Unix domain socket
- **Rate limiting**: 25 requests/second via governor token-bucket
- **Deduplication**: Telegram-originated messages tracked to prevent echo
- **Stale cleanup**: Sessions without tmux after 1h, dead panes after 24h

## Troubleshooting

```bash
ctm doctor       # Run all diagnostics
ctm doctor --fix # Auto-fix permissions and config issues
```

### Common Issues

**Bridge not starting?**
- Check for existing instance: another daemon may hold the flock
- Run `ctm doctor` to diagnose

**Messages not appearing in Telegram?**
- Verify bot token and chat ID: `ctm doctor`
- Check bot has admin permissions in the supergroup
- Ensure Topics are enabled in group settings

**tmux injection not working?**
- Verify tmux session: `tmux list-sessions`
- Ensure Claude Code is running inside tmux

**409 Conflict error?**
- Only one polling connection per bot token is allowed
- Each machine needs its own bot token

## Development

```bash
# Build
cargo build

# Build release
cargo build --release

# Run tests
cargo test

# Run clippy
cargo clippy

# Format
cargo fmt
```

## Documentation

| Document | Description |
|----------|-------------|
| [Architecture](docs/ARCHITECTURE.md) | System diagrams, message flows, concurrency model |
| [Setup Guide](docs/SETUP.md) | Step-by-step installation and configuration |
| [Development](docs/DEVELOPMENT.md) | Building, testing, module deep dives |
| [Security](docs/SECURITY.md) | Threat model, vulnerability fixes, audit checklist |
| [Product Requirements](docs/PRD.md) | Full PRD with security vulnerability matrix |

### Architecture Decision Records

| ADR | Decision |
|-----|----------|
| [ADR-001](docs/adr/ADR-001-rust-rewrite.md) | Rust over TypeScript |
| [ADR-002](docs/adr/ADR-002-teloxide.md) | teloxide for Telegram API |
| [ADR-003](docs/adr/ADR-003-bash-hook-retention.md) | Bash fire-and-forget hooks |
| [ADR-004](docs/adr/ADR-004-flock-pid-locking.md) | flock(2) atomic PID locking |
| [ADR-005](docs/adr/ADR-005-governor-rate-limiting.md) | governor token-bucket rate limiting |
| [ADR-006](docs/adr/ADR-006-single-binary-clap.md) | Single binary with clap CLI |

## License

MIT
