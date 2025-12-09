# Claude Code Telegram Mirror

[![npm version](https://img.shields.io/npm/v/claude-telegram-mirror.svg)](https://www.npmjs.com/package/claude-telegram-mirror)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Bidirectional communication between Claude Code CLI and Telegram. Control your Claude Code sessions from your phone.

**Supported platforms:** Linux (verified), macOS (untested)

## Installation

```bash
npm install -g claude-telegram-mirror
ctm setup    # Interactive setup wizard
```

The setup wizard guides you through:
1. Creating a Telegram bot via @BotFather
2. Disabling privacy mode (critical for group messages)
3. Setting up a supergroup with Topics
4. Verifying bot permissions
5. Installing hooks and the system service

## Features

- **CLI to Telegram**: Mirror Claude's responses, tool usage, and notifications
- **Telegram to CLI**: Send prompts from Telegram directly to Claude Code
- **Stop/Interrupt**: Type `stop` in Telegram to send Ctrl+C and halt Claude mid-process
- **Session Threading**: Each Claude session gets its own Forum Topic
- **Multi-System Support**: Run separate daemons on multiple machines
- **Compaction Notifications**: Get notified when Claude summarizes context

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

# Daemon control
ctm start              # Start daemon (foreground mode)
ctm stop               # Stop running daemon
ctm stop --force       # Force stop if graceful shutdown fails
ctm restart            # Restart daemon
ctm status             # Show daemon status, config, and hooks
ctm config --test      # Test Telegram connection

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

**Note:** The top-level `ctm stop` and `ctm restart` commands work for both direct daemon mode and OS service mode. They automatically detect how the daemon is running and use the appropriate method.

## Telegram Commands

Once connected, you can control Claude from Telegram:

| Command | Action |
|---------|--------|
| Any text | Sends to Claude as input |
| `stop` | Sends Escape to pause Claude |
| `kill` | Sends Ctrl-C to exit Claude entirely |

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for additional command aliases.

### Tool Approval Buttons

When Claude requests to use a tool that requires permission (Write, Edit, Bash with non-safe commands), you'll see approval buttons in Telegram:

| Button | Action |
|--------|--------|
| **Approve** | Allow the tool to execute |
| **Reject** | Deny this specific tool execution |
| **Abort** | Stop the entire Claude session |
| **Details** | View full tool input parameters |

**Note:** Approval buttons only appear when running Claude in normal mode. They do not appear when using `--dangerously-skip-permissions` mode. If you don't respond within 5 minutes, Claude will fall back to asking for approval in the CLI terminal.

## Architecture

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│   Claude Code   │────▶│  Bridge Daemon  │────▶│    Telegram     │
│      CLI        │◀────│                 │◀────│      Bot        │
└─────────────────┘     └─────────────────┘     └─────────────────┘
        │                       │
        │ hooks                 │ Unix socket
        ▼                       ▼
┌─────────────────┐     ┌─────────────────┐
│  PreToolUse:    │────▶│  Socket Server  │
│  Node.js handler│◀────│  (bidirectional)│
│  (with approval)│     │                 │
├─────────────────┤     │                 │
│  Other hooks:   │────▶│                 │
│  Bash script    │     │                 │
│  (fire & forget)│     │                 │
└─────────────────┘     └─────────────────┘
```

**Flow:**
1. Claude Code hooks capture events (prompts, responses, tool use)
2. PreToolUse: Node.js handler sends approval request, waits for Telegram response
3. Other hooks: Bash script sends JSON and exits immediately (faster)
4. Bridge forwards messages to Telegram Forum Topic
5. Telegram replies are injected into CLI via `tmux send-keys`
6. Stop commands send `Ctrl-C` to interrupt Claude

## Multi-System Architecture

When running Claude Code on multiple machines, each system needs its own bot to avoid Telegram API conflicts (error 409: only one polling connection per bot token is allowed).

**The model:**
- **One daemon per host** - Each machine runs its own bridge daemon
- **One bot per daemon** - Each daemon uses a unique Telegram bot
- **Multiple sessions per host** - One daemon handles all Claude sessions on that machine
- **Shared supergroup** - All bots post to the same Telegram supergroup

### Setup for Multiple Systems

1. **Create one bot per system** via [@BotFather](https://t.me/botfather)
   - System A: `@claude_mirror_system_a_bot`
   - System B: `@claude_mirror_system_b_bot`

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

- Node.js 18+
- Claude Code CLI
- tmux (for bidirectional communication)
- jq (JSON processing)
- nc (netcat, for socket communication)
- Telegram account

## Telegram Setup

### 1. Create a Bot

1. Message [@BotFather](https://t.me/botfather) → `/newbot`
2. Choose name and username (must end in `bot`)
3. Save the API token

### 2. Create Supergroup with Topics

1. Create a new group in Telegram
2. Add your bot to the group
3. Group Settings → Enable **Topics**

### 3. Make Bot an Admin

1. Group Settings → Administrators → Add your bot
2. Enable: **Manage Topics**, **Post Messages**

### 4. Get Chat ID

1. Send any message in the group
2. Run the helper script:
   ```bash
   ./scripts/get-chat-id.sh YOUR_BOT_TOKEN
   ```
   Or manually: `https://api.telegram.org/botYOUR_TOKEN/getUpdates`
3. Copy the chat ID (supergroups start with `-100`)

### 5. Disable Privacy Mode

1. [@BotFather](https://t.me/botfather) → `/mybots` → Select bot
2. Bot Settings → Group Privacy → **Turn off**

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
# Checks: Node.js, config, hooks, socket, tmux, systemd, Telegram API
```

## Project-Level Hooks

If your project has `.claude/settings.json` with custom hooks, global hooks are ignored. Install hooks to the project:

```bash
cd /path/to/your/project
ctm install-hooks --project
# or shorthand:
ctm install-hooks -p
```

The installer will prompt you to set up project-level hooks during installation. You can also add them later to any project.

## How Messages Flow

| Direction | Event | Display |
|-----------|-------|---------|
| CLI → Telegram | User types | 👤 User (cli): ... |
| CLI → Telegram | Tool starts | 🔧 Running: Bash |
| CLI → Telegram | Claude responds | 🤖 Claude: ... |
| CLI → Telegram | Session starts | New Forum Topic created |
| CLI → Telegram | Context compacting | ⏳ Notification sent |
| Telegram → CLI | User sends message | Injected via tmux |
| Telegram → CLI | User types "stop" | Sends Ctrl+C interrupt |

## Technical Details

- **Session storage**: SQLite at `~/.config/claude-telegram-mirror/sessions.db`
- **Socket path**: `~/.config/claude-telegram-mirror/bridge.sock`
- **Response extraction**: Reads Claude's transcript `.jsonl` on Stop event
- **Deduplication**: Telegram-originated messages tracked to prevent echo
- **Topic routing**: Each daemon only processes topics it created (multi-bot safe)
- **Compaction alerts**: PreCompact hook sends notification before context summarization

## Troubleshooting

Run the diagnostic tool first:

```bash
ctm doctor
```

This checks all common issues and provides fix suggestions.

### Common Issues

**Hooks not firing?**
- Check if project has local `.claude/settings.json` overriding globals
- Run `ctm install-hooks -p` from project directory
- Restart Claude Code after installing hooks

**409 Conflict error?**
- Only one polling connection per bot token is allowed
- If running multiple systems, each needs its own bot (see Multi-System Architecture)
- Kill duplicate daemons: `pkill -f "claude-telegram-mirror"`

**Bridge not receiving events?**
- Check socket: `ls -la ~/.config/claude-telegram-mirror/bridge.sock`
- Enable debug: `export TELEGRAM_HOOK_DEBUG=1` then retry
- Check debug log: `cat ~/.config/claude-telegram-mirror/hook-debug.log`

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
- Check permissions: Ensure Terminal has Accessibility access

## Manual Setup (for developers)

<details>
<summary>Click to expand manual installation steps</summary>

For developers who want to work on the source code:

```bash
# 1. Clone and build
git clone https://github.com/robertelee78/claude-telegram-mirror.git
cd claude-telegram-mirror && npm install && npm run build

# 2. Create a Telegram bot via @BotFather, get the token

# 3. Create a supergroup with Topics enabled, add your bot as admin

# 4. Get your chat ID
./scripts/get-chat-id.sh YOUR_BOT_TOKEN

# 5. Configure environment
cat > ~/.telegram-env << 'EOF'
export TELEGRAM_BOT_TOKEN="your-token-here"
export TELEGRAM_CHAT_ID="-100your-chat-id"
export TELEGRAM_MIRROR=true
EOF

# 6. Install hooks
node dist/cli.js install-hooks                    # Global install
# OR for projects with custom .claude/settings.json:
cd /path/to/project && node dist/cli.js install-hooks --project

# 7. Start daemon (choose one)
node dist/cli.js start                            # Foreground (for testing)
node dist/cli.js service install && \
node dist/cli.js service start                    # As system service (recommended)
```

**Note:** When using npm install, use `ctm` instead of `node dist/cli.js`.

</details>

## License

MIT

## Credits

Built for remote Claude Code interaction from mobile devices.
