# Claude Telegram Mirror (CTM)

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)
[![Tests](https://img.shields.io/badge/Tests-21%20passing-green.svg)]()
[![Clippy](https://img.shields.io/badge/Clippy-0%20warnings-green.svg)]()

Monitor and control Claude Code from your phone. CTM is a Rust daemon that bridges Claude Code CLI sessions to Telegram, giving you a real-time mobile interface to what Claude is doing.

```
You (phone)                    Your machine
┌──────────┐                  ┌──────────────────────┐
│ Telegram  │◄────────────────│  CTM daemon          │
│          │                  │    ↕                  │
│ "Running │  Telegram API    │  Claude Code (tmux)   │
│  tests"  │────────────────►│    ↕                  │
│          │                  │  Your codebase        │
│ [Approve]│                  │                       │
└──────────┘                  └──────────────────────┘
```

**What you see in Telegram:**
- "Running tests" (not `🔧 Running: Bash cargo test`)
- "Editing config.rs" (not `🔧 Running: Edit .../config.rs`)
- "Searching for 'auth'" (not `🔧 Running: Grep auth`)
- Approve/reject tool executions with inline buttons
- Each Claude session gets its own Forum Topic thread
- Type prompts, stop, or kill Claude directly from chat

## Quick Start

```bash
# Build
git clone https://github.com/DreamLab-AI/Claude-Code-Rust-Telegram.git
cd Claude-Code-Rust-Telegram
cargo build --release

# Configure (interactive)
./target/release/ctm setup

# Start daemon
./target/release/ctm start &

# Run Claude in tmux — everything mirrors to Telegram automatically
tmux new -s claude
claude
```

## Why Rust?

This is a security-focused rewrite of the original TypeScript version. The TypeScript version had **10 security vulnerabilities** including 3 CRITICAL command injection flaws. The Rust version fixes all of them:

| Vulnerability | TypeScript | Rust |
|--------------|-----------|------|
| Command injection | `execSync(\`tmux ... ${input}\`)` | `Command::new("tmux").arg(input)` |
| World-readable secrets | Default file perms | `0o600` files, `0o700` dirs |
| PID race conditions | Check-then-write | `flock(2)` atomic locking |
| Unvalidated callbacks | Chat ID skipped | Validated on ALL update types |
| No rate limiting | Unlimited | `governor` token-bucket |
| JSON panics | `.unwrap()` on user input | All parsing returns `Result` |

## Features

### Mobile Monitoring
- **Human-readable summaries** — Tool actions described in plain English, not raw operations
- **Session threads** — Each Claude session gets its own Telegram Forum Topic
- **Auto-cleanup** — Stale sessions and dead topics deleted automatically

### Bidirectional Control
- **Send prompts** — Type in Telegram, text appears in Claude's CLI
- **Stop/interrupt** — Send `stop` to press Escape, `kill` to send Ctrl-C
- **Slash commands** — `cc clear`, `cc compact` forwarded to Claude

### Tool Approval
- **Inline keyboards** — Approve, Reject, or Abort with one tap
- **Details button** — Expand to see full tool input parameters
- **Timeout fallback** — If you don't respond, Claude falls back to CLI prompts

### Smart Summaries
Tool actions are summarized in natural language with a two-tier system:
1. **Rule-based** (zero latency) — Covers cargo, git, npm, docker, pip, file ops, search, and 15+ system commands
2. **LLM fallback** (optional) — Unknown tools get summarized via Haiku/Claude API

| What Claude Does | What You See |
|-----------------|-------------|
| `Bash: cargo test` | Running tests |
| `Bash: cargo build --release` | Building project (release) |
| `Bash: git push` | Pushing to remote |
| `Edit: /home/user/src/config.rs` | Editing config.rs |
| `Grep: pattern "authentication"` | Searching for 'authentication' |
| `Task: {desc: "Explore auth"}` | Delegating: Explore auth |

## Setup

### 1. Create a Telegram Bot

1. Message [@BotFather](https://t.me/botfather) on Telegram — it's a real bot, message it directly
2. Send `/newbot`, choose a name and username (must end in `bot`)
3. Save the API token

### 2. Create a Supergroup

1. Create a new group in Telegram
2. Add your bot to the group
3. Make the bot an **Admin** (needs: Manage Topics, Post Messages, Delete Messages)
4. Enable **Topics** in group settings (requires Telegram desktop or mobile app, not web)

### 3. Disable Bot Privacy Mode

1. Back in [@BotFather](https://t.me/botfather): `/mybots` -> Select bot -> Bot Settings -> Group Privacy -> **Turn off**

This lets the bot see all messages in the group, not just commands.

### 4. Get Your Chat ID

```bash
# After sending a message in the group:
curl -s "https://api.telegram.org/botYOUR_TOKEN/getUpdates" | python3 -m json.tool | grep '"id"'
```

Supergroup chat IDs start with `-100` (e.g., `-1001234567890`).

### 5. Configure CTM

Run the interactive wizard:
```bash
ctm setup
```

Or create `~/.config/claude-telegram-mirror/config.json` manually:
```json
{
  "bot_token": "123456789:ABCdefGHIjklMNOpqrsTUVwxyz",
  "chat_id": -1001234567890,
  "enabled": true,
  "use_threads": true,
  "verbose": true
}
```

Both `snake_case` and `camelCase` field names are accepted.

### 6. Install Claude Code Hooks

Add to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [{ "hooks": [{ "type": "command", "command": "ctm hook", "timeout": 5000 }] }],
    "PostToolUse": [{ "hooks": [{ "type": "command", "command": "ctm hook", "timeout": 5000 }] }],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "ctm hook", "timeout": 5000 }] }],
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "ctm hook", "timeout": 5000 }] }],
    "SessionEnd": [{ "hooks": [{ "type": "command", "command": "ctm hook", "timeout": 5000 }] }],
    "Notification": [{ "hooks": [{ "type": "command", "command": "ctm hook", "timeout": 5000 }] }],
    "Stop": [{ "hooks": [{ "type": "command", "command": "ctm hook", "timeout": 5000 }] }]
  }
}
```

### 7. Start

```bash
ctm start &          # Start daemon in background
ctm status           # Verify it's running
ctm doctor           # Run diagnostics
```

## CLI Reference

```bash
ctm start              # Start bridge daemon (foreground)
ctm hook               # Process hook events from stdin (called by hooks)
ctm status             # Show daemon status and active sessions
ctm doctor             # Run diagnostics
ctm doctor --fix       # Auto-fix permission issues
ctm setup              # Interactive setup wizard
```

## Telegram Commands

| Input | Action |
|-------|--------|
| Any text | Sends as input to Claude |
| `stop` / `esc` | Sends Escape (pause Claude) |
| `kill` / `ctrl-c` | Sends Ctrl-C (exit Claude) |
| `cc clear` | Sends `/clear` to Claude |
| `cc compact` | Sends `/compact` to Claude |
| `/status` | Show active sessions |
| `/help` | Show commands |
| `/ping` | Health check |

## Configuration Reference

### Environment Variables

```bash
# Required
export TELEGRAM_BOT_TOKEN="your-token"
export TELEGRAM_CHAT_ID="-1001234567890"
export TELEGRAM_MIRROR=true

# Optional
export TELEGRAM_MIRROR_VERBOSE=true             # Show tool start/result (default: true)
export TELEGRAM_USE_THREADS=true                # Forum topics per session (default: true)
export TELEGRAM_AUTO_DELETE_TOPICS=true          # Delete topics on session end
export TELEGRAM_TOPIC_DELETE_DELAY_MINUTES=1440  # Delay before topic deletion (default: 24h)
export TELEGRAM_SESSION_TIMEOUT=1800             # Seconds before session goes stale
export TELEGRAM_RATE_LIMIT=20                    # Messages per second

# LLM-powered summaries (optional)
export CTM_LLM_SUMMARIZE_URL=https://api.anthropic.com/v1/messages
export CTM_LLM_API_KEY=sk-ant-...
```

Environment variables override config file values.

### Config File Fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `bot_token` | string | required | Telegram bot API token |
| `chat_id` | integer | required | Telegram supergroup chat ID |
| `enabled` | bool | `false` | Enable mirroring |
| `verbose` | bool | `true` | Show tool start/result messages |
| `use_threads` | bool | `true` | Create Forum Topics per session |
| `auto_delete_topics` | bool | `true` | Delete topics when sessions end |
| `topic_delete_delay_minutes` | integer | `1440` | Minutes to wait before deleting |
| `session_timeout` | integer | `30` | Seconds of inactivity before stale |
| `rate_limit` | integer | `1` | Max messages per second |
| `llm_summarize_url` | string | none | LLM endpoint for summary fallback |
| `llm_api_key` | string | none | API key for LLM endpoint |

## Multi-Machine Setup

Run CTM on multiple machines with a shared Telegram group:

1. Create **one bot per machine** via [@BotFather](https://t.me/botfather)
2. Add all bots to the **same supergroup** with admin permissions
3. Each daemon uses its own bot token and only manages its own topics

## Architecture

```
┌─────────────┐     hooks      ┌─────────────┐    NDJSON     ┌─────────────┐
│ Claude Code │───(stdin/out)──│  ctm hook   │───(socket)───│   Bridge    │
│   (tmux)    │                └─────────────┘               │   Daemon    │
│             │◄──(send-keys)──────────────────────────────│             │
└─────────────┘                                              │  ┌───────┐ │
                                                             │  │  Bot  │ │
┌─────────────┐                                              │  │(telo- │ │
│  Sessions   │◄────────(SQLite)──────────────────────────│  │ xide) │ │
│    (.db)    │                                              │  └───┬───┘ │
└─────────────┘                                              └─────┼─────┘
                                                                   │
                                                          Telegram API
                                                                   │
                                                             ┌─────┴─────┐
                                                             │ Telegram  │
                                                             │   Group   │
                                                             │ (Topics)  │
                                                             └───────────┘
```

### Modules

| Module | Lines | Responsibility |
|--------|-------|---------------|
| `bridge.rs` | ~1100 | Central orchestrator: routes messages between all components |
| `bot.rs` | ~300 | Telegram API: send/receive, forums, inline keyboards, rate limiting |
| `socket.rs` | ~250 | Unix socket server with flock PID locking, NDJSON protocol |
| `session.rs` | ~250 | SQLite persistence: sessions, approvals, stale cleanup |
| `formatting.rs` | ~700 | Tool summaries, message formatting, ANSI stripping, chunking |
| `summarizer.rs` | ~150 | Optional LLM fallback for unknown tool summarization |
| `hook.rs` | ~250 | Converts Claude Code hook events to bridge messages |
| `injector.rs` | ~200 | Shell-safe tmux command injection via `Command::arg()` |
| `config.rs` | ~300 | Config loading: env vars > file > defaults, permission enforcement |
| `types.rs` | ~200 | Shared types: BridgeMessage, HookEvent, Session, enums |
| `error.rs` | ~50 | Error types via thiserror |

## Troubleshooting

```bash
ctm doctor           # Run all diagnostics
ctm doctor --fix     # Auto-fix permissions and config
```

| Problem | Solution |
|---------|----------|
| Bridge won't start | Another instance holds the flock. Kill it or check `ctm status` |
| No messages in Telegram | Run `ctm doctor`. Check bot token, chat ID, admin perms, Topics enabled |
| tmux injection broken | Ensure Claude Code runs inside tmux: `tmux list-sessions` |
| 409 Conflict | Two daemons polling same bot token. Each machine needs its own bot |
| getUpdates timeout | Network issue. Outbound messages still work; inbound polling retries automatically |
| Bot can't see messages | Disable Group Privacy via @BotFather |

## Development

```bash
cargo build              # Debug build
cargo build --release    # Optimized release build (~8MB binary)
cargo test               # 21 tests
cargo clippy             # 0 warnings
cargo fmt --check        # Check formatting
RUST_LOG=debug ctm start # Verbose logging
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
