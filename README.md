# Claude Code Telegram Mirror

Bidirectional communication between Claude Code CLI and Telegram. Control your Claude Code sessions from your phone.

**Supported platforms:** Linux, macOS

## Quick Start

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
cd /path/to/project && node /path/to/claude-telegram-mirror/dist/cli.js install-hooks --project

# 7. Start daemon
./scripts/start-daemon.sh
```

## Features

- **CLI → Telegram**: Mirror Claude's responses, tool usage, and notifications
- **Telegram → CLI**: Send prompts from Telegram directly to Claude Code
- **Session Threading**: Each Claude session gets its own Forum Topic
- **Multi-System Support**: Run separate daemons on multiple machines

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
│  telegram-hook  │────▶│  Socket Server  │
│     (bash)      │     │                 │
└─────────────────┘     └─────────────────┘
```

**Flow:**
1. Claude Code hooks capture events (prompts, responses, tool use)
2. Hook script sends JSON to bridge daemon via Unix socket
3. Bridge forwards messages to Telegram Forum Topic
4. Telegram replies are injected into CLI via `tmux send-keys`

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
# export TELEGRAM_BRIDGE_SOCKET=/tmp/claude-telegram-bridge.sock
```

Source in your shell profile (`~/.bashrc` or `~/.zshrc`):

```bash
[[ -f ~/.telegram-env ]] && source ~/.telegram-env
```

### Test Connection

```bash
node dist/cli.js config --test
# ✅ Bot connected: @your_bot_username
# ✅ Test message sent to chat
```

## Usage

### Start the Bridge

```bash
# Foreground (recommended for first run)
./scripts/start-daemon.sh

# Background
nohup ./scripts/start-daemon.sh > /tmp/telegram-daemon.log 2>&1 &
```

### Run Claude in tmux

```bash
tmux new -s claude
claude
# Bridge auto-detects tmux session
```

### CLI Commands

```bash
node dist/cli.js start              # Start daemon
node dist/cli.js status             # Show status
node dist/cli.js config --test      # Test connection
node dist/cli.js install-hooks      # Install global hooks
node dist/cli.js install-hooks -p   # Install to current project
node dist/cli.js hooks              # Show hook status
```

## Project-Level Hooks

If your project has `.claude/settings.json` with custom hooks, global hooks are ignored. Install hooks to the project:

```bash
cd /path/to/your/project
node /path/to/claude-telegram-mirror/dist/cli.js install-hooks --project
```

## How Messages Flow

| Direction | Event | Display |
|-----------|-------|---------|
| CLI → Telegram | User types | 👤 User (cli): ... |
| CLI → Telegram | Tool starts | 🔧 Running: Bash |
| CLI → Telegram | Claude responds | 🤖 Claude: ... |
| CLI → Telegram | Session starts | New Forum Topic created |
| Telegram → CLI | User sends message | Injected via tmux |

## Technical Details

- **Session storage**: SQLite at `~/.config/claude-telegram-mirror/sessions.db`
- **Response extraction**: Reads Claude's transcript `.jsonl` on Stop event
- **Deduplication**: Telegram-originated messages tracked to prevent echo
- **Topic routing**: Each daemon only processes topics it created (multi-bot safe)

## Troubleshooting

**Hooks not firing?**
- Check if project has local `.claude/settings.json` overriding globals
- Run `node dist/cli.js install-hooks --project` from project directory
- Restart Claude Code after installing hooks

**409 Conflict error?**
- Only one polling connection per bot token is allowed
- If running multiple systems, each needs its own bot (see Multi-System Architecture)
- Kill duplicate daemons: `pkill -f "node.*dist/cli"`

**Bridge not receiving events?**
- Check socket: `ls -la /tmp/claude-telegram-bridge.sock`
- Check debug log: `cat /tmp/telegram-hook-debug.log`

**tmux injection not working?**
- Verify tmux session: `tmux list-sessions`
- Check daemon logs for "Session tmux target stored"

**Messages going to wrong topic?**
- Clear session DB: `rm ~/.config/claude-telegram-mirror/sessions.db`

## License

MIT

## Credits

Built as part of the claude-mobile project for remote Claude Code interaction.
