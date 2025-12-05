# Claude Code Telegram Mirror

Bidirectional communication between Claude Code CLI and Telegram. Control your Claude Code sessions from your phone.

## Features

- **CLI → Telegram**: Mirror Claude's responses to a Telegram chat
- **Telegram → CLI**: Send prompts from Telegram directly to Claude Code
- **Session Threading**: Each Claude session gets its own Forum Topic
- **Real-time Sync**: See what Claude is doing as it happens
- **tmux Integration**: Works with Claude Code running in tmux sessions

## How It Works

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

1. **Claude Code Hooks** capture events (user input, responses, tool use)
2. **Hook Script** formats events as JSON and sends via Unix socket
3. **Bridge Daemon** receives events and forwards to Telegram
4. **Telegram Bot** displays messages in Forum Topics per session
5. **Telegram → CLI** input is injected via `tmux send-keys`

## Prerequisites

- Node.js 18+
- Claude Code CLI installed
- tmux (for bidirectional communication)
- A Telegram account

## Telegram Setup

### Step 1: Create a Telegram Bot

1. Open Telegram and search for [@BotFather](https://t.me/botfather)
2. Send `/newbot` to create a new bot
3. Choose a name (e.g., "Claude Code Mirror")
4. Choose a username (must end in `bot`, e.g., `my_claude_mirror_bot`)
5. **Save the API token** - you'll need this (looks like `123456789:ABCdefGHIjklMNOpqrsTUVwxyz`)

### Step 2: Create a Telegram Group with Forum Topics

1. Open Telegram and create a new group
2. Add at least one other member (can remove later) or make it a public group temporarily
3. Go to **Group Settings** (tap group name → Edit)
4. Scroll down and enable **"Topics"** (this enables Forum Topics)
5. The group will now show a "General" topic

### Step 3: Add Your Bot to the Group

1. In the group, tap **Add Members**
2. Search for your bot by username (e.g., `@my_claude_mirror_bot`)
3. Add the bot to the group

### Step 4: Make the Bot an Admin

The bot needs admin permissions to create Forum Topics:

1. Go to **Group Settings** → **Administrators**
2. Tap **Add Administrator**
3. Select your bot
4. Enable these permissions:
   - **Manage Topics** (required for creating session threads)
   - **Post Messages**
   - **Delete Messages** (optional, for cleanup)
5. Save changes

### Step 5: Get Your Chat ID

1. Send any message in the group (e.g., "test")
2. Open this URL in your browser (replace `YOUR_BOT_TOKEN`):
   ```
   https://api.telegram.org/botYOUR_BOT_TOKEN/getUpdates
   ```
3. Look for `"chat":{"id":-100XXXXXXXXXX}` in the response
4. **Copy the full ID including the `-100` prefix** (e.g., `-1001234567890`)

### Step 6: Disable Privacy Mode (Important!)

By default, bots only see messages that start with `/` or mention the bot. To receive all messages:

1. Go back to [@BotFather](https://t.me/botfather)
2. Send `/mybots`
3. Select your bot
4. Go to **Bot Settings** → **Group Privacy**
5. Select **Turn off**

Now your bot can see all messages in the group.

## Installation

```bash
# Clone the repository
git clone https://github.com/yourusername/claude-telegram-mirror.git
cd claude-telegram-mirror

# Install dependencies
npm install

# Build
npm run build

# Install Claude Code hooks
node dist/cli.js install-hooks
```

## Configuration

Set environment variables in your shell profile (`~/.bashrc` or `~/.zshrc`):

```bash
# Required
export TELEGRAM_BOT_TOKEN="123456789:ABCdefGHIjklMNOpqrsTUVwxyz"
export TELEGRAM_CHAT_ID="-1001234567890"

# Optional
export TELEGRAM_MIRROR=true               # Enable/disable mirroring
export TELEGRAM_MIRROR_VERBOSE=false      # Show tool execution details
```

Then reload your shell:
```bash
source ~/.bashrc  # or source ~/.zshrc
```

### Test Your Configuration

```bash
# Test the Telegram connection
node dist/cli.js config --test
```

You should see:
```
✅ Bot connected: @your_bot_username
✅ Test message sent to chat
```

## Usage

### Start the Bridge

```bash
# Run in foreground
claude-telegram-mirror start

# Or use the CLI directly
node dist/cli.js start
```

### Run Claude Code in tmux

```bash
# Start a tmux session
tmux new -s claude

# Run Claude Code
claude

# The bridge will auto-detect the tmux session
```

### Commands

```bash
claude-telegram-mirror start          # Start the bridge daemon
claude-telegram-mirror status         # Show bridge status
claude-telegram-mirror install-hooks  # Install Claude Code hooks
claude-telegram-mirror uninstall-hooks # Remove hooks
claude-telegram-mirror hooks          # Show hook status
claude-telegram-mirror config         # Show configuration
claude-telegram-mirror config --test  # Test Telegram connection
```

## How Mirroring Works

### CLI → Telegram

| Event | What's Mirrored |
|-------|-----------------|
| User types in CLI | "👤 User (cli): ..." |
| Claude responds | "🤖 Claude: ..." |
| Tool execution | Tool name and output (verbose mode) |
| Session start | Creates new Forum Topic |
| Session end | Closes the topic |

### Telegram → CLI

When you send a message in the session's Forum Topic:
1. Bridge receives the message
2. Injects text into tmux via `send-keys`
3. Submits with Enter key
4. Claude processes and responds
5. Response mirrors back to Telegram

## Architecture

```
packages/claude-telegram-mirror/
├── src/
│   ├── bot/
│   │   ├── telegram.ts      # Telegram bot wrapper (grammy)
│   │   ├── commands.ts      # Bot commands and handlers
│   │   └── formatting.ts    # Message formatting
│   ├── bridge/
│   │   ├── daemon.ts        # Main bridge orchestrator
│   │   ├── socket.ts        # Unix socket server
│   │   ├── session.ts       # Session management (SQLite)
│   │   ├── injector.ts      # tmux input injection
│   │   └── types.ts         # TypeScript types
│   ├── hooks/
│   │   └── installer.ts     # Hook installation
│   ├── utils/
│   │   ├── config.ts        # Configuration loading
│   │   └── logger.ts        # Logging (pino)
│   └── cli.ts               # CLI entry point
├── scripts/
│   └── telegram-hook.sh     # Hook script (called by Claude Code)
└── dist/                    # Compiled JavaScript
```

## Technical Details

### Session Tracking

- Uses Claude's native `session_id` (UUID) for consistent tracking
- Sessions stored in SQLite (`~/.config/claude-telegram-mirror/sessions.db`)
- Each session maps to a Telegram Forum Topic

### Hook Events

The bridge captures these Claude Code hook events:

- `UserPromptSubmit` - User entered a prompt
- `Stop` - Claude finished responding (extracts response from transcript)
- `Notification` - System notifications (filtered to reduce noise)
- `PreToolUse` / `PostToolUse` - Tool execution (verbose mode only)

### Response Extraction

Since Claude Code doesn't have an "AssistantResponse" hook, we extract responses by:
1. Reading the transcript file (`.jsonl`) on `Stop` event
2. Parsing the last assistant message with text content
3. Forwarding to Telegram

### Deduplication

Messages sent from Telegram are tracked to prevent echo:
1. Input text + session ID stored in a Set
2. When hook fires `UserPromptSubmit`, check against Set
3. Skip mirroring if match found (was from Telegram)
4. Auto-expire tracking after 10 seconds

## Troubleshooting

### Bridge not receiving events

```bash
# Check if socket exists
ls -la /tmp/claude-telegram-bridge.sock

# Check hook debug log
cat /tmp/telegram-hook-debug.log

# Verify hooks are installed
claude-telegram-mirror hooks
```

### Messages going to wrong topic

```bash
# Clear session tracking
rm ~/.config/claude-telegram-mirror/sessions.db
rm ~/.config/claude-telegram-mirror/.session_active_*
```

### Bot not responding

```bash
# Test Telegram connection
claude-telegram-mirror config --test

# Check for multiple bot instances (409 error)
pkill -f "node.*dist/cli"
```

### tmux injection not working

```bash
# Verify tmux session detection
tmux list-sessions

# Check bridge logs for tmux target
# Should show: "Session tmux target stored"
```

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `TELEGRAM_BOT_TOKEN` | Bot token from BotFather | Required |
| `TELEGRAM_CHAT_ID` | Target chat/group ID | Required |
| `TELEGRAM_MIRROR` | Enable mirroring | `false` |
| `TELEGRAM_MIRROR_VERBOSE` | Show tool execution | `false` |
| `TELEGRAM_BRIDGE_SOCKET` | Socket path | `/tmp/claude-telegram-bridge.sock` |

## License

MIT

## Credits

Built as part of the claude-mobile project for remote Claude Code interaction.
