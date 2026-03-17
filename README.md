# Claude Code Telegram Mirror

[![npm version](https://img.shields.io/npm/v/claude-telegram-mirror.svg)](https://www.npmjs.com/package/claude-telegram-mirror)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
![Rust](https://img.shields.io/badge/Built_with-Rust-dea584.svg)

Bidirectional communication between Claude Code CLI and Telegram. Control your Claude Code sessions from your phone.

**Supported platforms:** Linux x64, Linux arm64, macOS ARM64, macOS Intel x64

## Installation

```bash
npm install -g claude-telegram-mirror
ctm setup    # Interactive setup wizard
```

This installs a native Rust binary (`ctm`) via platform-specific optional packages. No Node.js runtime is needed to run the binary itself — Node.js 18+ is only required as the npm distribution mechanism.

## Features

- **CLI to Telegram**: Mirror Claude's responses, tool usage, and notifications
- **Telegram to CLI**: Send prompts from Telegram directly to Claude Code
- **Tool Summarizer**: Human-readable summaries for 30+ command patterns ("Running tests" instead of "Running: Bash")
- **AskUserQuestion Rendering**: Inline buttons for Claude's interactive questions
- **Photo & Document Upload**: Send images/files from Telegram, path injected into Claude
- **Stop/Interrupt**: Type `stop` to send Escape, `kill` to send Ctrl-C
- **Session Threading**: Each Claude session gets its own Forum Topic
- **Session Rename**: `/rename` syncs with Claude Code's session title
- **Multi-System Support**: Run separate daemons on multiple machines
- **Compaction Notifications**: Get notified when Claude summarizes context
- **Governor Rate Limiting**: MessageQueue with retry and exponential backoff
- **Doctor Auto-Fix**: `ctm doctor --fix` auto-remediates common issues
- **Token Scrubbing**: Global regex-based scrubbing prevents bot tokens from leaking to logs
- **Atomic PID Locking**: `flock(2)` prevents duplicate daemon instances

## Quick Start

```bash
# 1. Install globally
npm install -g claude-telegram-mirror

# 2. Run interactive setup (creates bot, configures everything)
ctm setup

# 3. Start the daemon
ctm start

# 4. Run Claude in tmux
tmux new -s claude
claude
```

## CLI Commands

```bash
# Setup & diagnostics
ctm setup              # Interactive setup wizard
ctm doctor             # Diagnose configuration issues
ctm doctor --fix       # Auto-fix detected issues

# Daemon control
ctm start              # Start daemon (foreground mode)
ctm stop               # Stop running daemon
ctm stop --force       # Force stop if graceful shutdown fails
ctm restart            # Restart daemon
ctm status             # Show daemon status, config, and hooks
ctm config --test      # Test Telegram connection
ctm toggle             # Toggle mirroring on/off
ctm toggle --on        # Force mirroring ON
ctm toggle --off       # Force mirroring OFF

# Hook management
ctm install-hooks      # Install global hooks
ctm install-hooks -p   # Install to current project's .claude/
ctm uninstall-hooks    # Remove hooks
ctm hooks              # Show hook status

# OS service management (optional, for auto-start on boot)
ctm service install    # Install as systemd/launchd service
ctm service uninstall  # Remove system service
ctm service start      # Start via service manager
ctm service stop       # Stop via service manager
ctm service restart    # Restart via service manager
ctm service status     # Show service status
```

**Note:** `ctm stop` and `ctm restart` auto-detect whether the daemon is running directly or via a system service and use the appropriate method.

## Telegram Commands

| Command | Action |
|---------|--------|
| Any text | Sends to Claude as input |
| `stop` | Sends Escape to pause Claude |
| `kill` | Sends Ctrl-C to exit Claude entirely |
| `cc <cmd>` | Sends `/<cmd>` as a slash command to Claude |
| `/status` | Show active sessions and mirroring state |
| `/sessions` | List active sessions with age and project dir |
| `/rename <name>` | Rename session (syncs with Claude Code) |
| `/attach <id>` | Attach to a session for updates |
| `/detach` | Detach from current session |
| `/mute` / `/unmute` | Suppress/resume agent response notifications |
| `/toggle` | Toggle mirroring on/off |
| `/abort` | Abort the attached session |
| `/ping` | Measure round-trip latency |
| `/help` | Show all commands |

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for additional details and command aliases.

### Tool Approval Buttons

When Claude requests to use a tool that requires permission (Write, Edit, Bash with non-safe commands), you'll see approval buttons in Telegram:

| Button | Action |
|--------|--------|
| **Approve** | Allow the tool to execute |
| **Reject** | Deny this specific tool execution |
| **Abort** | Stop the entire Claude session |
| **Details** | View full tool input parameters |

Approval buttons only appear in normal mode, not with `--dangerously-skip-permissions`. If you don't respond within 5 minutes, Claude falls back to CLI approval.

## Architecture

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│   Claude Code   │────▶│   ctm daemon    │────▶│    Telegram     │
│      CLI        │◀────│  (Rust binary)  │◀────│      Bot        │
└─────────────────┘     └─────────────────┘     └─────────────────┘
        │                       │
        │ hooks                 │ Unix socket
        ▼                       ▼
┌─────────────────┐     ┌─────────────────┐
│  ctm hook       │────▶│  Socket Server  │
│  (same binary,  │◀────│  (bidirectional)│
│   hook mode)    │     │                 │
└─────────────────┘     └─────────────────┘
```

**Flow:**
1. Claude Code hooks invoke `ctm hook`, which reads the event from stdin
2. PreToolUse: sends approval request via socket, waits for Telegram response
3. Other hooks: sends JSON to daemon via socket and exits immediately
4. Daemon forwards messages to Telegram Forum Topic with summarized tool actions
5. Telegram replies are injected into CLI via `tmux send-keys`
6. Stop/kill commands send Escape or Ctrl-C to interrupt Claude

## Multi-System Architecture

When running Claude Code on multiple machines, each system needs its own bot to avoid Telegram API conflicts (error 409: only one polling connection per bot token is allowed).

**The model:**
- **One daemon per host** - Each machine runs its own bridge daemon
- **One bot per daemon** - Each daemon uses a unique Telegram bot
- **Multiple sessions per host** - One daemon handles all Claude sessions on that machine
- **Shared supergroup** - All bots post to the same Telegram supergroup

### Setup for Multiple Systems

1. **Create one bot per system** via [@BotFather](https://t.me/botfather)
2. **Add all bots to the same supergroup** with admin permissions
3. **Configure each system** with its own bot token:
   ```bash
   # On System A (~/.telegram-env)
   export TELEGRAM_BOT_TOKEN="token-for-system-a-bot"
   export TELEGRAM_CHAT_ID="-100shared-group-id"

   # On System B (~/.telegram-env)
   export TELEGRAM_BOT_TOKEN="token-for-system-b-bot"
   export TELEGRAM_CHAT_ID="-100shared-group-id"  # Same group!
   ```
4. **Each daemon creates topics for its sessions** - Messages route correctly because each daemon only processes topics it created.

## Prerequisites

- Node.js 18+ (for npm installation only)
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

### 4. Get Chat ID

1. Send any message in the group
2. Visit `https://api.telegram.org/botYOUR_TOKEN/getUpdates`
3. Copy the chat ID (supergroups start with `-100`)

### 5. Disable Privacy Mode

1. [@BotFather](https://t.me/botfather) -> `/mybots` -> Select bot
2. Bot Settings -> Group Privacy -> **Turn off**

## Configuration

### Environment Variables

Create `~/.telegram-env`:

```bash
export TELEGRAM_BOT_TOKEN="123456789:ABCdefGHIjklMNOpqrsTUVwxyz"
export TELEGRAM_CHAT_ID="-1001234567890"
export TELEGRAM_MIRROR=true
# Optional:
# export TELEGRAM_MIRROR_VERBOSE=true
# export TELEGRAM_BRIDGE_SOCKET=~/.config/claude-telegram-mirror/bridge.sock
# export TELEGRAM_STALE_SESSION_TIMEOUT_HOURS=72  # Auto-cleanup dead sessions (default: 72)
```

Source in your shell profile (`~/.bashrc` or `~/.zshrc`):

```bash
[[ -f ~/.telegram-env ]] && source ~/.telegram-env
```

### Config File (Alternative)

The `ctm setup` wizard creates `~/.config/claude-telegram-mirror/config.json`:

```json
{
  "botToken": "your-token",
  "chatId": -1001234567890,
  "enabled": true,
  "verbose": true
}
```

Environment variables take precedence over config file values.

### Test Connection

```bash
ctm doctor
# Checks: config, hooks, socket, tmux, systemd/launchd, Telegram API
```

## Project-Level Hooks

If your project has `.claude/settings.json` with custom hooks, global hooks are ignored. Install hooks to the project:

```bash
cd /path/to/your/project
ctm install-hooks --project
```

## How Messages Flow

| Direction | Event | Display |
|-----------|-------|---------|
| CLI -> Telegram | User types | User (cli): ... |
| CLI -> Telegram | Tool starts | Running tests (summarized) |
| CLI -> Telegram | Claude responds | Claude: ... |
| CLI -> Telegram | Session starts | New Forum Topic created |
| CLI -> Telegram | Context compacting | Notification sent |
| CLI -> Telegram | AskUserQuestion | Inline buttons rendered |
| Telegram -> CLI | User sends message | Injected via tmux |
| Telegram -> CLI | User sends photo | Downloaded, path injected |
| Telegram -> CLI | User types "stop" | Sends Escape interrupt |

## Technical Details

- **Binary**: Single native Rust executable (`ctm`), ~10 MB
- **Session storage**: SQLite at `~/.config/claude-telegram-mirror/sessions.db`
- **Socket path**: `~/.config/claude-telegram-mirror/bridge.sock`
- **PID file**: `~/.config/claude-telegram-mirror/bridge.pid` (flock-guarded)
- **Downloads**: `~/.config/claude-telegram-mirror/downloads/` (0700 permissions)
- **Response extraction**: Reads Claude's transcript `.jsonl` on Stop event
- **Deduplication**: Telegram-originated messages tracked to prevent echo
- **Topic routing**: Each daemon only processes topics it created (multi-bot safe)
- **Rate limiting**: Governor-based with exponential backoff retry queue
- **Token scrubbing**: All log output filtered through regex to strip bot tokens
- **Test suite**: 387 Rust tests (unit + 8 integration test files)

## Troubleshooting

Run the diagnostic tool first:

```bash
ctm doctor
ctm doctor --fix   # Auto-fix common issues
```

### Common Issues

**Hooks not firing?**
- Check if project has local `.claude/settings.json` overriding globals
- Run `ctm install-hooks -p` from project directory
- Restart Claude Code after installing hooks

**409 Conflict error?**
- Only one polling connection per bot token is allowed
- If running multiple systems, each needs its own bot (see Multi-System Architecture)
- Kill duplicate daemons: `ctm stop --force`

**Bridge not receiving events?**
- Check socket: `ls -la ~/.config/claude-telegram-mirror/bridge.sock`
- Check daemon logs for errors
- Run `ctm status` to verify daemon is running

**tmux injection not working?**
- Verify tmux session: `tmux list-sessions`
- Check daemon logs for "Session tmux target stored"

**Messages going to wrong topic?**
- Clear session DB: `rm ~/.config/claude-telegram-mirror/sessions.db`

**Service not starting (Linux)?**
- Check status: `systemctl --user status claude-telegram-mirror`
- View logs: `journalctl --user -u claude-telegram-mirror -f`
- Enable linger: `loginctl enable-linger $USER`

**Service not starting (macOS)?**
- Check status: `launchctl list | grep claude`
- View logs: `cat ~/Library/Logs/claude-telegram-mirror.*.log`

## Build from Source

<details>
<summary>Click to expand</summary>

For developers who want to build from source or contribute:

```bash
# 1. Clone and build
git clone https://github.com/robertelee78/claude-telegram-mirror.git
cd claude-telegram-mirror/rust-crates
cargo build --release
# Binary at: rust-crates/target/release/ctm

# 2. Run tests
cargo test

# 3. Use the binary directly
./target/release/ctm setup
./target/release/ctm start
```

### Project Structure (30 source files)

```
rust-crates/ctm/src/
  main.rs           # CLI entry point (clap)
  lib.rs            # Library re-exports
  hook.rs           # Hook event processing
  config.rs         # Configuration loading (env > file > defaults)
  error.rs          # Centralized error types (thiserror)
  types.rs          # Shared types, validation, security constants
  session.rs        # SQLite session management
  socket.rs         # Unix socket server/client (flock, NDJSON)
  injector.rs       # tmux input injection
  formatting.rs     # Message formatting, chunking, ANSI stripping
  summarize.rs      # Tool action summarizer (30+ patterns)
  colors.rs         # ANSI color helpers for terminal output
  doctor.rs         # Diagnostic checks with --fix
  installer.rs      # Hook installer
  setup.rs          # Interactive setup wizard
  bot/              # Telegram API client (client.rs, queue.rs, types.rs)
  daemon/           # Bridge daemon (mod.rs, event_loop.rs, socket_handlers.rs,
                    #   telegram_handlers.rs, callback_handlers.rs, cleanup.rs, files.rs)
  service/          # OS service management (mod.rs, systemd.rs, launchd.rs, env.rs)

rust-crates/ctm/tests/   # 8 integration test files
```

</details>

## License

MIT

## Credits

Built for remote Claude Code interaction from mobile devices.
