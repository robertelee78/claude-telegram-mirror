# Setup Guide

Complete guide to setting up Claude Code Rust Telegram (CTM) from scratch.

## Prerequisites

```mermaid
graph LR
    RUST[Rust 1.75+] --> BUILD[cargo build]
    TMUX[tmux] --> INJECT[Input injection]
    TELEGRAM[Telegram Account] --> BOT[Bot creation]
    CLAUDE[Claude Code CLI] --> HOOKS[Hook integration]

    style RUST fill:#f96,stroke:#333
    style TMUX fill:#9f6,stroke:#333
    style TELEGRAM fill:#69f,stroke:#333
    style CLAUDE fill:#f9f,stroke:#333
```

| Requirement | Purpose | Check |
|-------------|---------|-------|
| Rust 1.75+ | Build CTM binary | `rustc --version` |
| tmux | Bidirectional CLI communication | `tmux -V` |
| Claude Code | The CLI being mirrored | `claude --version` |
| Telegram account | Mobile control interface | - |

## Step 1: Build CTM

```bash
git clone https://github.com/DreamLab-AI/Claude-Code-Rust-Telegram.git
cd Claude-Code-Rust-Telegram
cargo build --release
```

The binary is at `target/release/ctm`. Optionally install it:

```bash
cp target/release/ctm ~/.local/bin/
# or
sudo cp target/release/ctm /usr/local/bin/
```

## Step 2: Create a Telegram Bot

```mermaid
sequenceDiagram
    participant You
    participant BotFather as @BotFather
    participant Bot as Your New Bot

    You->>BotFather: /newbot
    BotFather->>You: Choose a name
    You->>BotFather: Claude Mirror Bot
    BotFather->>You: Choose a username
    You->>BotFather: claude_mirror_bot
    BotFather->>You: Bot token: 123456:ABC...

    You->>BotFather: /mybots
    You->>BotFather: Select bot -> Settings
    You->>BotFather: Group Privacy -> Turn OFF

    Note over You,BotFather: Privacy mode must be OFF<br/>for the bot to see all messages
```

1. Open Telegram (desktop app or mobile — **not** a self-chat)
2. Use the **search bar** to find `BotFather` — select the one with the **blue verified checkmark**
3. Start a direct chat with @BotFather and send `/newbot`
4. BotFather will ask for a **display name** (e.g. "Claude Mirror") and a **username** (must end in `bot`, e.g. `claude_mirror_bot`)
5. Copy the **bot token** it gives you (format: `123456789:ABCdefGHIjklMNOpqrsTUVwxyz`)
6. **Disable privacy mode**: `/mybots` -> Select bot -> Bot Settings -> Group Privacy -> **Turn off**

> **Note:** @BotFather is a real Telegram bot you message directly — it is not a settings page or a chat with yourself.

## Step 3: Create a Supergroup with Topics

```mermaid
graph TB
    CREATE[Create Telegram Group] --> ADD[Add your bot]
    ADD --> SETTINGS[Group Settings]
    SETTINGS --> TOPICS[Enable Topics/Forums]
    TOPICS --> ADMIN[Make bot an Admin]
    ADMIN --> PERMS[Grant permissions]

    PERMS --> P1[Manage Topics]
    PERMS --> P2[Post Messages]
    PERMS --> P3[Delete Messages]
```

1. Create a new group in Telegram
2. **Add your bot during group creation**: in the "Add Members" screen, search for your bot's username (e.g. `TheDreamLabBot`) and select it
3. Go to Group Settings -> Enable **Topics** to convert it to a forum supergroup
4. Make the bot an **Administrator** with these permissions:
   - Manage Topics
   - Post Messages
   - Delete Messages (optional, for topic cleanup)

> **Telegram Web limitation:** Enabling Topics (converting a group to a forum) may not be available in Telegram Web. Use the **Telegram desktop app** or **mobile app** to enable Topics in group settings. Once enabled, the forum works fine in Telegram Web.
>
> **Adding the bot:** If the group already exists, open the group -> click the group name header -> **Add Members** -> search for your bot username.

## Step 4: Get Your Chat ID

The chat ID identifies your supergroup. Supergroup IDs start with `-100`.

### Easiest method

1. Send any message in the group where your bot is a member
2. Run this command (replace `YOUR_TOKEN` with your bot token):

```bash
curl -s "https://api.telegram.org/botYOUR_TOKEN/getUpdates" | python3 -m json.tool
```

3. Look for `"chat": {"id": -100XXXXXXXXXX}` in the response — that negative number is your chat ID

### Using the helper script

```bash
./scripts/get-chat-id.sh YOUR_BOT_TOKEN
```

> **Tip:** If `getUpdates` returns an empty result, make sure you've sent a message in the group **after** adding the bot. The bot only sees messages sent after it joined.

## Step 5: Configure

### Option A: Config file (recommended)

Create `~/.config/claude-telegram-mirror/config.json`:

```bash
mkdir -p ~/.config/claude-telegram-mirror
chmod 700 ~/.config/claude-telegram-mirror
```

```json
{
  "bot_token": "123456789:ABCdefGHIjklMNOpqrsTUVwxyz",
  "chat_id": -1001234567890,
  "enabled": true,
  "approvals": true,
  "use_threads": true,
  "rate_limit": 20,
  "session_timeout": 1800,
  "auto_delete_topics": false
}
```

```bash
chmod 600 ~/.config/claude-telegram-mirror/config.json
```

Or use the interactive wizard which validates your token and chat ID:

```bash
ctm setup
```

### Option B: Environment variables

Create `~/.telegram-env`:

```bash
export TELEGRAM_BOT_TOKEN="123456789:ABCdefGHIjklMNOpqrsTUVwxyz"
export TELEGRAM_CHAT_ID="-1001234567890"
export TELEGRAM_MIRROR=true
```

Source it in your shell profile (`~/.bashrc` or `~/.zshrc`):

```bash
[[ -f ~/.telegram-env ]] && source ~/.telegram-env
```

> **Note:** Environment variables take precedence over the config file. You can mix both — use the config file for stable settings and env vars for overrides.

### All Configuration Options

```mermaid
graph TD
    subgraph "Required"
        TOKEN[TELEGRAM_BOT_TOKEN]
        CHATID[TELEGRAM_CHAT_ID]
        MIRROR[TELEGRAM_MIRROR=true]
    end

    subgraph "Optional"
        VERBOSE[TELEGRAM_MIRROR_VERBOSE<br/>default: false]
        THREADS[TELEGRAM_USE_THREADS<br/>default: true]
        AUTODEL[TELEGRAM_AUTO_DELETE_TOPICS<br/>default: false]
        DELAY[TELEGRAM_TOPIC_DELETE_DELAY<br/>default: 5 minutes]
        SOCKET[TELEGRAM_BRIDGE_SOCKET<br/>default: auto]
    end

    TOKEN --> CONFIG[Config]
    CHATID --> CONFIG
    MIRROR --> CONFIG
    VERBOSE --> CONFIG
    THREADS --> CONFIG
    AUTODEL --> CONFIG
    DELAY --> CONFIG
    SOCKET --> CONFIG
```

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `TELEGRAM_BOT_TOKEN` | Yes | - | Bot API token from @BotFather |
| `TELEGRAM_CHAT_ID` | Yes | - | Supergroup chat ID (starts with -100) |
| `TELEGRAM_MIRROR` | Yes | `false` | Enable the bridge |
| `TELEGRAM_MIRROR_VERBOSE` | No | `false` | Show tool start/result messages |
| `TELEGRAM_USE_THREADS` | No | `true` | Create forum topics per session |
| `TELEGRAM_AUTO_DELETE_TOPICS` | No | `false` | Delete topics when sessions end |
| `TELEGRAM_TOPIC_DELETE_DELAY` | No | `5` | Minutes before topic deletion |
| `TELEGRAM_BRIDGE_SOCKET` | No | auto | Custom socket path |

## Step 6: Install Claude Code Hooks

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

### Hook Architecture

```mermaid
graph LR
    subgraph "Claude Code Hooks"
        PRE[PreToolUse]
        POST[PostToolUse]
        NOTIF[Notification]
        STOP[Stop]
        USER[UserPromptSubmit]
        SSTART[SessionStart]
        SEND[SessionEnd]
    end

    subgraph "CTM Handler"
        HOOK[ctm hook<br/>reads stdin JSON]
    end

    subgraph "Bridge"
        SOCKET[Unix Socket]
    end

    PRE -->|stdin| HOOK
    POST -->|stdin| HOOK
    NOTIF -->|stdin| HOOK
    STOP -->|stdin| HOOK
    USER -->|stdin| HOOK
    SSTART -->|stdin| HOOK
    SEND -->|stdin| HOOK

    HOOK -->|NDJSON| SOCKET
    HOOK -->|stdout passthrough| PRE
```

Each hook:
1. Receives JSON on **stdin** from Claude Code
2. Parses the event and forwards to the bridge via unix socket
3. Passes through the original JSON on **stdout** (so Claude Code is unaffected)

## Step 7: Start the Daemon

```bash
# Start in foreground
ctm start

# Or in a tmux window
tmux new-window -n ctm 'ctm start'
```

## Step 8: Verify

```bash
ctm doctor
```

Expected output:
```
Claude Telegram Mirror - Doctor
================================

[1/6] Binary...
  OK: ctm binary running

[2/6] Config directory...
  OK: Config directory exists with secure permissions

[3/6] Environment variables...
  OK: All environment variables set

[4/6] tmux...
  OK: tmux is available
  OK: tmux session detected: workspace:0.0

[5/6] Socket...
  OK: Bridge socket exists at ~/.config/claude-telegram-mirror/bridge.sock

[6/6] Database...
  OK: Database accessible (0 sessions, 0 approvals)

================================
All checks passed!
```

## Step 9: Use It

```bash
# Start Claude Code in tmux
tmux new -s claude
claude
```

Now any Claude Code activity will be mirrored to your Telegram supergroup. Reply in the forum topic to send input back to Claude.

## Supervisor Integration (Docker/Server)

For running as a managed service:

```ini
[program:ctm]
command=/usr/local/bin/ctm start
directory=/home/devuser
user=devuser
autostart=true
autorestart=true
environment=TELEGRAM_MIRROR="true",RUST_LOG="info"
stdout_logfile=/home/devuser/.config/claude-telegram-mirror/supervisor.log
stdout_logfile_maxbytes=10MB
```

## Multi-System Setup

```mermaid
graph TB
    subgraph "System A"
        BOT_A[Bot A Token]
        DAEMON_A[ctm start]
        BOT_A --> DAEMON_A
    end

    subgraph "System B"
        BOT_B[Bot B Token]
        DAEMON_B[ctm start]
        BOT_B --> DAEMON_B
    end

    subgraph "Telegram"
        GROUP[Shared Supergroup]
        TOPIC_A[System A Topics]
        TOPIC_B[System B Topics]
        GROUP --> TOPIC_A
        GROUP --> TOPIC_B
    end

    DAEMON_A -->|creates & manages| TOPIC_A
    DAEMON_B -->|creates & manages| TOPIC_B
```

Each system needs its own bot token (Telegram only allows one polling connection per token). All bots share the same supergroup, and each daemon only manages topics it created.
